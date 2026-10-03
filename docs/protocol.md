# Protocol

The wire protocol between room members, and the pipe between the main process and `couch-viewer`.

**Status:**
- M1 implemented and validated the control envelope with `Hello` and `KeyframeRequest`, video epoch streams, receiver reports and the viewer IPC.
- M2 added input: the input stream, the gamepad, mouse and rumble datagrams, and the seat messages.
- The rest is a draft for later milestones.

## Versioning

- ALPN `couch-coapp/0` until the first public release, then `/1`. Incompatible changes need a new ALPN.
- Both sides send `Hello { proto_minor, app_version, features }` first on the control stream. Optional behaviour is negotiated through `features` bits: send a message type only if the peer advertised it.
- **Released payloads are frozen.** Extend the protocol by adding message types, never by changing an existing payload. Decoders reject trailing bytes, so an accidental change fails loudly.

## Control-stream envelope

```
u32 len (big-endian) | u16 type | payload (postcard)
```

- `len` counts the type field plus the payload, and is capped at 64 KiB.
- **High bit set** (`0x8000`): the message must be understood. An unknown must-understand type closes the connection with the `version` code.
- **High bit clear**: the message is optional, and unknown optional types are skipped.

| Type | Message | Notes |
|---|---|---|
| `0x8001` | `Hello` | First message in both directions |
| `0x8101` | `KeyframeRequest { epoch, reason: Start \| DecoderError }` | Viewer to host: decoding can't go on without a keyframe |
| `0x8201` | `SeatAssigned { seat u8, seat_epoch u32, permissions u8 }` | Host to member: you hold this seat; tag input with this epoch. Permissions: bit 0 gamepad, 1 keyboard, 2 mouse |
| `0x8202` | `SeatReleased { seat u8, seat_epoch u32 }` | Host to member: your hold on the seat (with that epoch) ended |

Message families still to come, all on this stream:
- **Admission:** `AdmitStart`, `AdmitReply`, `AdmitConfirm`, `AdmitResult`.
- **Room:** `RoomSnapshot`, `MemberJoined`/`MemberLeft`, `Presence`, `MemberAddrUpdate`, `RoomPolicy`.
- **Session directory and participation:** `SessionAnnounce`, `SessionUpdate`, `SessionEnded{reason}`, `SessionJoin`, `SessionWelcome`, `SessionLeave`.
- **Control:** `ControlRequest`, `ControlCancel`, `ControlDecision`, `ControlRevoked`.
- **Media control:** `VideoConfig`, `KeyframeRequest`, `BitrateHint`.
- **Liveness:** `Ping`/`Pong` every second, `Goodbye{reason}`.

## Channels

Each pair of room members shares one iroh connection.

| Channel | QUIC primitive | Direction | Content |
|---|---|---|---|
| Control | Bidirectional stream, opened first by the dialer | both | Envelope messages above |
| Video | One unidirectional stream per epoch | host to viewer | Preamble `{kind 0x01, version u8, session u64, epoch u32, codec u8 (1 = H.264), width u16, height u16, fps u16}`, then frames `{seq u32, flags u8 (bit 0 = keyframe), capture_us u64, encode_us u32, len u32 (at most 8 MiB), Annex-B access unit}` |
| Input | One unidirectional stream, priority 100 (video streams have 0) | viewer to host | Kind byte `0x02`, then messages framed like the control stream (see [Input](#input)) |
| `0x01` Audio | Datagram | host to viewer | `{seq, ts (48 kHz), Opus 10 ms}` |
| `0x02` Gamepad | Datagram | viewer to host | `{seat u8, seat_epoch u32, seq u32, pad}`, on change and every 100 ms while holding a seat. `pad` is `{buttons u16 (XInput bits), left_trigger u8, right_trigger u8, left_x, left_y, right_x, right_y i16 (up positive)}` |
| `0x03` Mouse motion | Datagram | viewer to host | `{seat, seat_epoch, totals}`, on change and every 100 ms. `totals` is `{version u32, x, y, wheel_x, wheel_y i32}` |
| `0x04` Receiver report | Datagram | viewer to host | Every 50 ms, postcard: `{epoch, epoch_bytes_received, total_bytes_received, frames_received, delays: up to 32 × (seq, arrival − capture µs), decode_errors}`. Audio fields come in M3. |
| `0x05` Rumble | Datagram | host to viewer | `{seat u8, low u8, high u8}`, on change |

- **Epochs.** An epoch is one keyframe plus the frames that depend on it. With an infinite GOP, every frame depends on the one before, so frames must be decoded in order anyway. Resetting a whole epoch is the only useful discard. See [ADR 0003](adr/0003-epoch-streams-for-video.md).
- **Datagram payloads** are postcard after the tag byte; trailing bytes are rejected. One reader per connection hands each datagram to the subscriber for its tag (`couch_net::datagrams`).

## Input

Code: `couch_input::forward` on the viewer's side, `couch_input::router` on the host's, `couch_net::input` between them.

**Input stream messages** (`u32 len | u16 type | postcard`, like the control stream)

| Type | Message | Notes |
|---|---|---|
| `0x8301` | `Key { seat, seat_epoch, usage u16, down }` | A USB HID keyboard usage (page 7), 0x04 to 0xE7: a physical key position, never a character |
| `0x8302` | `MouseButton { seat, seat_epoch, button u8, down, totals }` | Buttons: 1 left, 2 right, 3 middle, 4 back, 5 forward |
| `0x8303` | `Held { seat, seat_epoch, keys [u8; 32], buttons u8 }` | Every second: a bitmap of held usages, and held buttons (bit `button − 1`) |

**Encoding rules**
- **Gamepad as full snapshots.** A lost datagram can never leave a button stuck. `seq` grows per datagram within a seat epoch; the host drops older ones.
- **Mouse motion as running totals** since the seat epoch began, wrapping. A lost datagram's motion arrives with the next one. `version` counts changes: the host applies totals only if they are newer than the last it applied (wrapping comparison), so a late datagram never moves the pointer backwards. Button events carry the totals too, so the pointer moves to where the click happened first.
- **The wheel** counts in 1/120 notches (Windows' `WHEEL_DELTA`), up and right positive.
- **The pad datagram is the heartbeat.** It goes every 100 ms while holding a seat, whatever the permissions. It stands for the viewer process, not the main process: the viewer sends `Alive` over IPC every 100 ms from its window loop, and after 250 ms without anything from the viewer the main process stops the heartbeat. A hung viewer therefore trips the host's dead-man timer.
- **Rumble** goes only to the seat's holder. A running rumble is repeated every 500 ms, and the viewer runs each one for 1 s, so a lost "stop" ends within a second. Losing the seat stops it.

**What the host accepts** (the seat router). Input reaches the game only if all hold:
- it comes from the seat's holder, tagged with the seat's current epoch;
- the holder has the permission for that kind of input;
- for keyboard and mouse, the captured game window (not just its process) is in the foreground. The host checks right before each input, not only on its 10 ms tick;
- for a mouse button press, the pointer is over the game window too. Otherwise a click could reach the taskbar or another app. Motion and releases still go through;
- it isn't blocked: the Windows keys always; Tab while Alt is held; Esc while Alt or Ctrl is held. Held modifiers count across every seat, so two holders can't combine into Alt+Tab. The pad's Guide bit is cleared.

**What the host releases**
- Releases (key up, button up) only ever let go of something the router pressed, so keys held when a seat is granted stay ignored until released.
- `Held` only releases: anything pressed that isn't in it goes up. It never presses.
- **Dead-man:** nothing from the holder for 300 ms suspends the seat. Every key and button goes up and the pad goes neutral. Input resumes it. The first input after a grant arms the timer.
- **Lapses:** suspended for 10 s, or granted and never heard from in 10 s, the seat returns to the host. So does a third suspension within 5 minutes; the member needs a new grant.
- Every grant, revocation, disconnect and loss of foreground releases first. So does stopping the host (Ctrl+C, Ctrl+Break, closing its console): Windows keeps injected keys down after the process that pressed them exits.

**Injection on Windows** (`couch-platform-win`)
- Keys go in as set-1 scancodes through `SendInput`, so the host's layout decides what they type. Pause and Num Lock go by virtual-key code. Media and power keys are not sent.
- The mouse moves relatively; the wheel goes as `MOUSEEVENTF_WHEEL` and `HWHEEL`.
- The pad is a ViGEmBus Xbox 360 controller. Development builds give it USB ids 1209:0001 (pid.codes' test range), which a viewer on the same machine skips so the host's own pad isn't captured and sent back.

## Bitrate control (per viewer, 20 times a second)

The rate controller is the congestion control. QUIC's own controller only keeps the sender within twice what the path recently delivered ([ADR 0005](adr/0005-media-congestion-control.md)). Code: `couch_media::ratecontrol`.

**Inputs** (`FeedbackTracker`)
- From receiver reports:
  - **Delivery rate:** growth of `total_bytes_received` over the last 300 ms.
  - **Outstanding bytes:** sent in the current epoch minus that epoch's `epoch_bytes_received`.
  - **One-way delay trend:** the smallest arrival-minus-capture delay of the last 400 ms, minus the smallest of the last 10 s. The clock offset cancels out. Taking the minimum ignores frames held up behind a retransmission, since those delay only some frames while a queue delays all of them.
  - **Report age.**
- From the host itself: the **send rate**, video handed to the transport over the last 300 ms. It falls below the bitrate when the encoder has little to send, as in a static scene.
- From iroh's selected path:
  - Smoothed round trip, and its 10-second minimum.
  - Loss rate over the last second (`lost_packets` / `udp_tx.datagrams`, from 50 packets up).
  - Whether the path is relayed. A switch between relay and direct clears the round-trip and delay minimums, since a new path's longer baseline isn't a queue.

**Control law** (`RateController::tick`, in this order)
1. **Outage.** No report for 500 ms with video outstanding: abandon the epoch, halve the bitrate and stop encoding (`Pause`). A report fresher than 100 ms: resume with one keyframe (`Resume`). The resume counts as a catch-up for the cooldown, so data queued behind a transport that is still waking up doesn't cause a second keyframe.
2. **Catch-up.** Queued video = outstanding ÷ max(bitrate, send rate) − (round trip + report age). The bytes went out at the send rate, which right after a cut is still above the new bitrate. If that exceeds max(150 ms, 2 × minimum round trip) for two ticks, at most once a second:
   - reset the epoch stream (`SKIP_AHEAD`) and start a new one with a keyframe;
   - if the path shows congestion (round-trip inflation or a delay trend as below, or loss above 2%), new bitrate = 0.85 × delivery rate, kept between a quarter and half of the old bitrate;
   - otherwise keep the bitrate: the queue is ours, for example the transport's window still opening at the start of a stream.
3. **Overuse.** Any of these:
   - delay trend above 10 ms;
   - round trip above its minimum by more than max(15 ms, 0.5 × minimum);
   - loss above 10%.

   Response, then hold for 500 ms:
   - if the path delivers less than 90% of the send rate, target = 0.85 × min(delivery rate, target);
   - otherwise target = 0.85 × target. A path that keeps up says nothing about its capacity; the sender may just have little to send.
4. **Otherwise** grow 8% per second, unless holding or loss is above 2%.
5. **Clamp** between the minimum (1.5 Mbit/s) and the cap. The cap is the user maximum (20 Mbit/s), or the relay maximum (2 Mbit/s) on a relayed path. This viewer's share of an upload cap comes in M5.
6. **Apply.**
   - Decreases apply at once.
   - Increases of more than 3% apply too, except where every bitrate change costs a keyframe (FFmpeg's NVENC, below). There, increases go in steps of at least 15%, at most once a second.

**QUIC settings**
- Timeouts: idle timeout 15 s, keep-alive 2 s.
- Streams and windows: 64 concurrent unidirectional streams; stream window 8 MB, connection window 16 MB, send window 16 MB.
- Datagrams: 64 KB send buffer, so stale audio and input drop rather than queue. noq puts datagrams in each packet ahead of stream data. In M1's test, audio-sized datagrams gained under 1 ms while a 1 MB keyframe was in flight on loopback. A slow link's queue still delays them behind video (about 100 ms behind a 1 MB keyframe at 25 Mbit/s).
- Congestion control: `MediaCongestion` ([ADR 0005](adr/0005-media-congestion-control.md)). noq's BBR v3, Cubic and NewReno stay available for comparison.

**`h264_nvenc` settings**
- Preset p1, tune ull, `rc=cbr`, High profile.
- No B-frames, `delay=0`, `zerolatency=1`, `rc-lookahead=0`.
- `forced-idr=1` with an infinite GOP; `bufsize` = bitrate / fps (a one-frame VBV, which also keeps keyframes small).
- **Known cost.** FFmpeg 8.1's NVENC wrapper turns any bitrate change into an encoder reset with an IDR frame. That is why costly increases are batched. Calling NVENC directly (M9) reconfigures without one.

**Decoder:** single thread, `AV_CODEC_FLAG_LOW_DELAY`. After a decode error the viewer drops frames until the next keyframe and asks for one at most every 500 ms. It does the same when a frame waited more than 200 ms for the decoder: decoding can't keep up, and the host can't see that queue.

## Admission (M4a)

1. TLS handshake. iroh authenticates both endpoint ids.
2. The `after_handshake` hook rejects banned or rate-limited ids.
3. `Hello`.
4. Asymmetric SPAKE2. The dialer is A, the acceptor B. Identities are `"couch/v1" || room_id || endpoint_id`.
5. Key confirmation within 10 s.

**Derivations**
- Password input = `HKDF-SHA256(salt = room_secret, ikm = NFKC(password), info = "couch/pake-pw/v1" || room_id)`.
- Confirmation keys = `HKDF(spake2_key, salt = export_keying_material("couch-coapp pake v1", room_id))`. This binds them to the QUIC connection.
- Each side sends `HMAC(k_A or k_B, H(room_id || id_A || id_B || msg_A || msg_B))`. Compare in constant time.

**Rate limits**
- Per remote id: exponential backoff.
- Per room: 10 failures per hour pause new attempts for 10 minutes.

## Close and reset codes

- **Connection close:** `normal`, `admission_failed`, `banned`, `duplicate`, `version`, `rate_limited`, `room_full`.
- **Stream reset:** `SKIP_AHEAD`, `SESSION_END`, `VIEWER_REMOVED`.

## Viewer IPC

Framing is `u32 len (big-endian) | u8 kind | payload`. `len` counts the kind byte plus the payload and is capped at 16 MiB. The main process writes to the viewer's stdin and reads its stdout. The viewer must never print anything else to stdout.

| Kind | Direction | Meaning |
|---|---|---|
| `0x01` | main to viewer | Hello: IPC version and session parameters |
| `0x02` | main to viewer | Video frame |
| `0x03` | main to viewer | Audio packet |
| `0x04` | main to viewer | HUD state |
| `0x05` | main to viewer | Shutdown |
| `0x06` | main to viewer | Network stats for the HUD: `{rtt_us u32, relayed u8, receive_bps u32}`, 4 times a second |
| `0x07` | main to viewer | Seat state (postcard): `{holding, permissions}`. The viewer captures the mouse only while holding with mouse permission |
| `0x08` | main to viewer | Rumble (postcard): `{low u8, high u8}` |
| `0x81` | viewer to main | Local input (postcard `LocalInput`): `Key{usage, down}`, `MouseButton{button, down}`, `MouseMotion{dx, dy}`, `Wheel{dx, dy}`, `Pad(snapshot)` or `Alive` (every 100 ms). Deltas; the main process keeps the running totals |
| `0x82` | viewer to main | Frame timing: `{epoch, seq, capture_us, arrived_us, decoded_us, presented_us, barcode_us}` |
| `0x83` | viewer to main | User action (request control, leave) |
| `0x84` | viewer to main | Keyframe request (decoder error) |

`IPC_VERSION` (now 1) changes with any payload change. The viewer always ships with the app, so a mismatch means a broken install.

**Viewer hotkeys.** Keys pressed with Ctrl+Alt+Shift held stay in the viewer:
- H shows or hides the HUD; F3 does too, unless the keyboard is being forwarded.
- M frees the mouse; clicking the window captures it again.

Losing the focus releases every forwarded key and button. Gamepads keep working in the background.
