# Protocol

The wire protocol between room members, and the pipe between the main process and `couch-viewer`.

**Status:** M1 implemented and validated these: the control envelope with `Hello` and `KeyframeRequest`, video epoch streams, receiver reports and the viewer IPC. The rest is a draft for later milestones.

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

Message families still to come, all on this stream:
- **Admission:** `AdmitStart`, `AdmitReply`, `AdmitConfirm`, `AdmitResult`.
- **Room:** `RoomSnapshot`, `MemberJoined`/`MemberLeft`, `Presence`, `MemberAddrUpdate`, `RoomPolicy`.
- **Session directory and participation:** `SessionAnnounce`, `SessionUpdate`, `SessionEnded{reason}`, `SessionJoin`, `SessionWelcome`, `SessionLeave`.
- **Control:** `ControlRequest`, `ControlCancel`, `ControlDecision`, `SeatAssigned`, `SeatReleased`, `ControlRevoked`.
- **Media control:** `VideoConfig`, `KeyframeRequest`, `BitrateHint`.
- **Liveness:** `Ping`/`Pong` every second, `Goodbye{reason}`.

## Channels

Each pair of room members shares one iroh connection.

| Channel | QUIC primitive | Direction | Content |
|---|---|---|---|
| Control | Bidirectional stream, opened first by the dialer | both | Envelope messages above |
| Video | One unidirectional stream per epoch | host to viewer | Preamble `{kind 0x01, version u8, session u64, epoch u32, codec u8 (1 = H.264), width u16, height u16, fps u16}`, then frames `{seq u32, flags u8 (bit 0 = keyframe), capture_us u64, encode_us u32, len u32 (at most 8 MiB), Annex-B access unit}` |
| Input | Unidirectional stream, highest priority | viewer to host | `KeyEvent{seat_epoch, seq, hid_usage, down}`, `MouseButton{..., cum_dx, cum_dy}`, `HeldKeysSync` bitmap every second |
| `0x01` Audio | Datagram | host to viewer | `{seq, ts (48 kHz), Opus 10 ms}` |
| `0x02` Gamepad | Datagram | viewer to host | `{seat, seat_epoch, seq, buttons, triggers, sticks}` on change and every 100 ms |
| `0x03` Mouse motion | Datagram | viewer to host | `{seat, seat_epoch, seq, cumulative dx, dy, wheel}` |
| `0x04` Receiver report | Datagram | viewer to host | Every 50 ms, postcard: `{epoch, epoch_bytes_received, total_bytes_received, frames_received, delays: up to 32 × (seq, arrival − capture µs), decode_errors}`. Audio fields come in M3. |
| `0x05` Rumble | Datagram | host to viewer | `{seat, low, high}` |

- **Epochs.** An epoch is one keyframe plus the frames that depend on it. With an infinite GOP, every frame depends on the one before, so frames must be decoded in order anyway. Resetting a whole epoch is the only useful discard. See [ADR 0003](adr/0003-epoch-streams-for-video.md).
- **Mouse motion as running totals.** Each datagram carries the running totals, so a lost datagram's motion arrives with the next one. Button events carry the totals too, so the host applies pending motion before the click.
- **Gamepad as full snapshots.** A lost packet can never leave a button stuck.
- **Seat epochs.** Input tagged with anything but the seat's current epoch is dropped, which discards stragglers from a previous holder.

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
| `0x81` | viewer to main | Input event |
| `0x82` | viewer to main | Frame timing: `{epoch, seq, capture_us, arrived_us, decoded_us, presented_us, barcode_us}` |
| `0x83` | viewer to main | User action (request control, leave) |
| `0x84` | viewer to main | Keyframe request (decoder error) |

`IPC_VERSION` changes with any payload change. The viewer always ships with the app, so a mismatch means a broken install.
