# Protocol

The wire protocol between room members, and the pipe between the main process and `couch-viewer`.

**Status:**
- M1 implemented and validated the control envelope with `Hello` and `KeyframeRequest`, video epoch streams, receiver reports and the viewer IPC.
- M2 added input: the input stream, the gamepad, mouse and rumble datagrams, and the seat messages.
- M3 added audio: the audio datagram, and the audio packet and stats on the viewer IPC.
- M4a added rooms: admission, signed member records, the owner's policy, presence, invites and close codes.
- M5a added sessions: the session directory, watching, control requests, the per-connection stream acceptor, the control stream's priority, the upload share, and viewer IPC v3.
- M5b put sessions on screen without changing the wire: the viewer's HUD bar and shortcuts, the shortcuts table, and the panic-key hook.
- M6 added liveness (`Alive`, the `timed_out` close code), Alt+F4 to the blocked keys, following a game to a new window and telling that it exited, and watching that survives a dropped connection or a crashed viewer window.
- M12c added HEVC: the codecs a viewer decodes in `SessionJoin`, codec 2 in the epoch preamble, and viewer IPC v8 (the codec in each video frame).
- M19 made sessions start before streams: `SessionStatus.stream`, the `STREAM_END` reset code, and viewer IPC v9 (the stream's state in the HUD).
- M18 let one viewer hold several seats: `ViewerControllers`, `ControlCancel { what: Seat(SeatId?) }`, a viewer on several seats in `SessionStatus`, and viewer IPC v10 (controllers by slot, the seat list with the controller driving each, rumble per seat).
- P6 added the stream window's volume: viewer IPC v11 (`VOLUME`, `VOLUME_CHANGED`), with no wire change. P7's host stream level changed neither.
- P11 put the stream window's controls in the app: viewer IPC v12 (`WINDOW`, `WINDOW_STATE`, `VOLUME` mid-stream), with no wire change.
- M23 let the host play several seats, one per controller of theirs: `SessionStatus.host_seats` (wire revision 5).
- M13 let an owner hand a room over: a chain of signed handovers travels with every policy (`OwnedPolicy`), `HandoverOffer`, `HandoverAnswer`, `HandoverWithdraw`, and invites of version 2 naming the founder (wire revision 6).
- P12 put a guest's controllers in the host's patch bay: `ViewerControllers` names them and says which drives each seat, `SeatAssigned.controller`, `SeatController` (wire revision 7), and viewer IPC v13 (`CONTROLLER_NAMES`).
- P13 moves one controller at a time: `ControlRequest.controller` (wire revision 8), and viewer IPC v14 (`RequestControl { seat, controller }`).
- M24 lets a session have no seats, to watch together, and drops `SeatKind::Both` (wire revision 9).
- M25a adds remote play: a device of the user's own runs a home PC's sessions from afar, the `0x07xx` messages (wire revision 10).
- The rest is a draft for later milestones.

## Versioning

- ALPN `couch-coapp/0`. There is no public binary release ([ADR 0009](adr/0009-source-only-releases.md)), so it stays; incompatible changes are caught by the wire revision below.
- Both sides send `Hello { proto_minor, app_version, features }` first on the control stream. Optional behaviour is negotiated through `features` bits: send a message type only if the peer advertised it.
- **`proto_minor` is the wire revision** (`couch_proto::PROTOCOL_MINOR`, M10a; now 10, for remote play in M25a). Payloads change in place (for good, ADR 0009), so it is bumped with every change to what members exchange, and builds of different revisions don't talk: right after the two `Hello`s, the side that sees another revision closes with the `version` code, before admission or anything about a room. The acceptor still sends its `Hello` before refusing, and waits up to 2 s for the dialer to close, so both learn the other's `app_version` (kept to letters, digits, `.`, `+` and `-`, at most 32, before it is logged or shown: anyone can dial). Only a member of a room we share is remembered and shown; the app shows it ("needs update" in the member list, or the join error), with which side is newer. Builds before revision 1 sent 0 and didn't check. `features` stays reserved: different revisions never interoperate.
- **Payloads may change in place** (ADR 0009: no public binary release, so nothing is frozen), with the wire revision bumped. Decoders reject trailing bytes, so a change that forgets the bump fails loudly.

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
| `0x8002` | `Goodbye { reason: LeftRoom \| Shutdown }` | Before closing a connection on purpose |
| `0x0003` | `Alive {}` | Optional. Both ways, every second ([Liveness](#liveness)) |
| `0x8101` | `KeyframeRequest { epoch, reason: Start \| DecoderError }` | Viewer to host: decoding can't go on without a keyframe |
| `0x8201` | `SeatAssigned { seat u8, seat_epoch u32, permissions u8, controller u8? }` | Host to member: you hold this seat; tag input with this epoch. Permissions: bit 0 gamepad, 1 keyboard, 2 mouse. `controller`: which of the member's controllers the host chose for it, by slot (P12); followed while that controller is connected |
| `0x8202` | `SeatReleased { seat u8, seat_epoch u32 }` | Host to member: your hold on the seat (with that epoch) ended |
| `0x0206` | `SeatController { seat u8, controller u8 }` | Optional. Host to member: drive this seat you hold with your controller in that slot, swapping with the seat it drove (P12). Ignored for a seat not held or a controller not connected |
| `0x8401` | `JoinStart { room [16], invite [16], pake [33] }` | Newcomer to owner, first after `Hello` (see [Admission](#admission)) |
| `0x8402` | `MemberHello { room [16], policy }` | Member to member, first after `Hello`: the dialer's copy of the owner's policy, with its handovers |
| `0x8403` | `AdmitReply { pake [33], confirm [32] }` | Owner's SPAKE2 message and key confirmation |
| `0x8404` | `AdmitConfirm { confirm [32] }` | Newcomer's key confirmation |
| `0x8501` | `RoomSnapshot { policy, members }` | The owner's signed policy, with its handovers, and every member's record ([Rooms](#rooms)) |
| `0x8502` | `MemberUpdate { record }` | One signed member record |
| `0x8503` | `PolicyUpdate { policy }` | The owner's signed policy with its handovers; also the proof shown when refusing someone it no longer lists |
| `0x0504` | `Presence { connected: [member id] }` | Optional: who the sender is connected to |
| `0x8505` | `HandoverOffer { offer u64, version u32 }` | Owner to a member: would it take the room over? (M13, [Handing a room over](#handing-a-room-over)) |
| `0x8506` | `HandoverAnswer { offer, accept bool }` | The member's answer, on the same connection |
| `0x8507` | `HandoverWithdraw { offer }` | Owner to the member offered: the offer is gone |
| `0x8203` | `ControlRequest { session u64, seat u8, controller u8? }` | Viewer to host: may I play on this seat? ([Sessions](#sessions)). `controller`: which of its controllers asks, by slot (P13); granted, it plays the seat, leaving its own if the viewer has none to spare |
| `0x8204` | `ControlCancel { session, what: Request \| Seat(SeatId?) }` | Viewer to host: withdraw my request (my seats stay), or give back one seat (`Seat(Some)`, M18) or all of mine (`Seat(None)`) |
| `0x8205` | `ControlDeclined { session }` | Host to viewer: no (or not now: the panic key) |
| `0x0601` | `SessionAnnounce { session, title, seats [Controller \| KeyboardMouse], max_viewers u8 }` | Optional. Host to each connected member: a session is live, with these seats (none: to watch together, M24) |
| `0x0602` | `SessionStatus { session, viewers [member id], holders [member id?], host_seats [SeatId], host_on_seats bool, control_paused, stream: Waiting \| Live \| Stopped(EndReason) }` | Optional. Host to each connected member: who watches, which viewer holds each seat, the seats the host plays with their own controllers (M23: one per controller, in seat order), whether those controllers play only through the seats, and whether anything is streamed (M19) |
| `0x0603` | `SessionEnded { session, reason }` | Optional. Host to each connected member |
| `0x8604` | `SessionJoin { session, codecs u8 }` | Member to host: let me watch. `codecs` is what this member decodes in hardware: bit 0 H.264 (always set), bit 1 HEVC (M12c) |
| `0x8605` | `SessionJoinReply { session, result: Joined \| Full \| Ended \| Unavailable }` | Host to member |
| `0x8606` | `SessionLeave { session }` | Viewer to host: I stopped watching |
| `0x0607` | `GameFocus { session, in_front }` | Optional. Host to its viewers: whether the streamed window is in front (P1) |
| `0x0608` | `ViewerControllers { session, report: { controllers: [{ slot u8, name }], seats: [{ seat u8, controller u8? }] } }` | Optional. Viewer to host: its stream window's controllers, by slot and named, and which drives each pad seat it holds; after joining and on every change (M18, P12). At most 8 controllers on distinct slots below 8, at most 8 seats, each once, each driven by one of those or none; names of at most 40 characters, control characters replaced. The host drops a report that breaks this whole |

| `0x0701` | `RemoteOffer { wait_secs u32? }` | Optional. Home PC to a device it trusts, on every room connection while remote play is on ([Remote play](#remote-play)): it may unlock; `wait_secs` while wrong PINs make it wait |
| `0x8702` | `RemoteUnlock { pin }` | Device to home PC: the PIN, at most 64 bytes, to become its operator on this connection. Never logged |
| `0x8703` | `RemoteUnlockReply { result: Unlocked \| WrongPin { wait_secs u32 } \| Wait { secs u32 } \| Off }` | Home PC to the device that sent `RemoteUnlock` |
| `0x8704` | `RemoteCommand { id u32, op }` | Operator to home PC: one of the host's commands (`RemoteOp`: `ListSources`, `StartSession { title, max_viewers, controllers u8, keyboard_mouse }`, `StartStream { source: Window(i64) \| Screen(i64), name, fps, max_mbps, upload_mbps? }`, `StopStream`, `SetStreamLevel`, `EndSession`, `AnswerControl`, `PassControl`, `TakeBackControl`, `PlaySeat`, `AllowControl`, `StopWaitingForPads`); texts at most 200 bytes |
| `0x8705` | `RemoteReply { id u32, result: Done \| Sources(...) \| Failed(text) }` | Home PC to its operator: how that command went. Sources: at most 64 windows and 16 screens, texts at most 200 bytes |
| `0x8706` | `RemoteState { session? }` | Home PC to each operator, on unlocking and on every change: what its host console shows that members don't otherwise hear (requests, each guest's controllers report, the stream's name and rate, the sound level, pads setting up or missing, its own controllers and their seats, whether it has a panic key). Lists within the room's size; each guest's report checked as a host checks one |
| `0x8707` | `RemoteLock { reason: ByOperator \| Panic \| Removed \| PinChanged \| Off }` | Either way: the operator locks itself, or the home PC locks it out |

Message families still to come, all on this stream: **media control** (`VideoConfig`, `BitrateHint`).

Families: `0x00xx` session basics, `0x01xx` media control, `0x02xx` seats, `0x03xx` the input stream, `0x04xx` admission, `0x05xx` rooms, `0x06xx` sessions, `0x07xx` remote play.

### Liveness

- **Each side sends `Alive` every second** on the control stream (`couch_proto::control::ALIVE_EVERY`), whatever else it sends.
- **A peer that has sent `Alive` and then says nothing for 5 s** (`SILENT_LIMIT`) is taken for gone: its connection is closed with `timed_out` (12). Its sessions end as for any closed connection, and the room redials.
- **A peer that never sent `Alive`** (a build from before M6) is never timed out this way; QUIC's 15 s idle timeout still applies.
- **Why it exists.** A killed or hung host app, or a dead path, otherwise shows only after QUIC's idle timeout (15 s), while the viewer stares at a frozen picture. `Alive` comes from each connection's writer task: it stops when the peer's process or its async runtime hangs, but not when only its room actor does.

## Channels

Each pair of room members shares one iroh connection.

| Channel | QUIC primitive | Direction | Content |
|---|---|---|---|
| Control | Bidirectional stream, opened first by the dialer | both | Envelope messages above; priority 50, above video, below input |
| Video | One unidirectional stream per epoch | host to viewer | Preamble `{kind 0x01, version u8, session u64, epoch u32, codec u8 (1 = H.264, 2 = HEVC; another is refused), width u16, height u16, fps u16}`, then frames `{seq u32, flags u8 (bit 0 = keyframe), capture_us u64, encode_us u32, len u32 (at most 8 MiB), Annex-B access unit}` |
| Input | One unidirectional stream, priority 100 (video streams have 0) | viewer to host | Kind byte `0x02`, then messages framed like the control stream (see [Input](#input)) |
| `0x01` Audio | Datagram | host to viewer | `{seq u32, capture_us u64, packet}`: one Opus packet of 10 ms (see [Audio](#audio)) |
| `0x02` Gamepad | Datagram | viewer to host | `{seat u8, seat_epoch u32, seq u32, pad}`, on change and every 100 ms while holding a seat. `pad` is `{buttons u16 (XInput bits), left_trigger u8, right_trigger u8, left_x, left_y, right_x, right_y i16 (up positive)}` |
| `0x03` Mouse motion | Datagram | viewer to host | `{seat, seat_epoch, totals}`, on change and every 100 ms. `totals` is `{version u32, x, y, wheel_x, wheel_y i32}` |
| `0x04` Receiver report | Datagram | viewer to host | Every 50 ms, postcard: `{epoch, epoch_bytes_received, total_bytes_received, frames_received, delays: up to 32 × (seq, arrival − capture µs), all from frames of `epoch`, decode_errors}`. Audio has no fields here: the host doesn't adapt it |
| `0x05` Rumble | Datagram | host to viewer | `{seat u8, low u8, high u8}`, on change |

- **Epochs.** An epoch is one keyframe plus the frames that depend on it. With an infinite GOP, every frame depends on the one before, so frames must be decoded in order anyway. Resetting a whole epoch is the only useful discard. See [ADR 0003](adr/0003-epoch-streams-for-video.md).
- **Datagram payloads** are postcard after the tag byte; trailing bytes are rejected. One reader per connection hands each datagram to the subscriber for its tag (`couch_net::datagrams`).

## Audio

Code: `couch_platform_win::audio` and `couch_engine::pipeline::host` (one capture and encoder for every viewer) on the host, `couch_net::audio` between, `couch-viewer`'s `audio` module with `couch_media::audio::JitterBuffer` on the viewer.

**Format.** Opus (RFC 6716), 48 kHz stereo, one 10 ms frame per packet, 96 kbit/s constant bitrate, low-delay (CELT-only) mode: 120-byte packets, 2.5 ms of algorithmic delay. Fixed by the protocol version; a different format would get a new datagram tag.

**Datagram** `0x01`, postcard `{seq u32, capture_us u64, packet: bytes}`.
- `seq` grows by one per packet and wraps. A gap is a lost packet.
- `capture_us` is the packet's first sample on the host's capture clock, like a video frame's.
- `packet` is at most 1000 bytes, so the datagram fits QUIC's initial 1200-byte packets (Opus allows 1275, but 10 ms at its 510 kbit/s maximum is 638). Longer is rejected, as are trailing bytes. The viewer also rejects a packet that doesn't decode to exactly 10 ms.
- Process loopback keeps delivering silence while the game plays nothing, so the stream normally never pauses. If the capture does stall, the host resumes with the next `seq`, and the viewer must not mistake the pause for network jitter (it doesn't: see the target below).

**Host**
- The source is the captured window's process and its children (WASAPI process loopback). For a monitor, everything except our own processes. For the test pattern, test clicks. `host --audio` overrides.
- Packets are cut from the capture in 10 ms frames. A gap of more than 20 ms in the capture's timestamps finishes the current frame with silence rather than joining audio across the gap.

**Viewer jitter buffer** (`couch_media::audio::JitterBuffer`)
- **Target:** the spread of transit times (arrival on the viewer's clock minus `capture_us`; the offset between the clocks cancels) over the last 2 s, 95th percentile minus minimum, plus one frame, between 10 and 40 ms. Late packets count towards it; a host that paused its sending doesn't. Playback starts, and restarts after running dry, once the target's worth is buffered.
- **A missing packet with newer ones present** is lost: concealed by Opus and skipped. If it arrives later, it is dropped as late.
- **A missing packet with nothing newer** is probably late: one frame is concealed and playback waits for it. After three such frames playback stops and waits to refill: that is an underrun (a network stall, or a stall of the host's capture). Packets that already missed their turn stay rejected while refilling.
- **A hole of more than three frames** before audio that has arrived is skipped after one concealed frame.
- **Too much buffered** (more than target + 60 ms): the oldest packets are dropped down to the target.
- **Playback ratio:** while the average level over the last 50 frames is more than 2.5 ms off target, playback runs faster or slower by error ÷ 2 s, at most 2%. This trims the delay that waiting for late packets added, and absorbs the drift between the two machines' audio clocks.
- **Restart:** a packet more than 50 frames behind or ahead of the newest means the host started over, or everything in between is lost; the buffer resets. This also bounds what a broken or hostile sequence can do.

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
- **The pad datagram is the heartbeat.** It goes every 100 ms for each seat held, whatever its permissions (M18: a viewer on several seats sends one per seat; a seat no controller drives sends neutral). It stands for the viewer process, not the main process: the viewer sends `Alive` over IPC every 100 ms from its window loop, and after 250 ms without anything from the viewer the main process stops the heartbeat. A hung viewer therefore trips the host's dead-man timer.
- **Rumble** goes only to the seat's holder. A running rumble is repeated every 500 ms, and the viewer runs each one for 1 s, so a lost "stop" ends within a second. Losing the seat stops it.

**What the host accepts** (the seat router). Input reaches the game only if all hold:
- it comes from the seat's holder, tagged with the seat's current epoch;
- the holder has the permission for that kind of input;
- for keyboard and mouse, the captured game window (not just its process) is in the foreground. The host checks right before each input, not only on its 10 ms tick;
- for a mouse button press, the pointer is over the game window too. Otherwise a click could reach the taskbar or another app. Motion and releases still go through;
- it isn't blocked: the Windows keys always; Tab while Alt is held; Esc while Alt or Ctrl is held; F4 while Alt is held (M6). Held modifiers count across every seat, so two holders can't combine into Alt+Tab. The pad's Guide bit is cleared.

**The host's own controllers** (P2; M23: several, one seat each), when they play seats, go through the same router as the host's holder, never around it. The host app reads them (SDL3, on a thread of its own: `couch_gamepad`) and turns each controller's state into a pad datagram for that controller's own seat (as the session says, never guessed), with that seat's current epoch and an increasing sequence; each reports every change and every 100 ms, neutral while it is unplugged, so the dead-man timer treats it like a viewer's pad and a controller that goes keeps its seat. A controller whose seat the host doesn't hold sends nothing. The reader reads physical controllers, and since M22 another program's virtual pad (a remapper's) proven so, never one of ours (P9). Nothing of this is on the wire: a remapper's pad plays its seat like any controller of the host's. Rumble on a host seat goes back to the controller driving it, and stops when the seat leaves them or another controller takes it over.

**What the host releases**
- Releases (key up, button up) only ever let go of something the router pressed, so keys held when a seat is granted stay ignored until released.
- `Held` only releases: anything pressed that isn't in it goes up. It never presses.
- **Dead-man:** nothing from the holder for 300 ms suspends the seat. Every key and button goes up and the pad goes neutral. Input resumes it. The first input after a grant arms the timer.
- **Lapses:** suspended for 10 s, or granted and never heard from in 10 s, the seat returns to the host. So does a third suspension within 5 minutes; the member needs a new grant.
- Every grant, revocation, disconnect and loss of foreground releases first. So does stopping the host (Ctrl+C, Ctrl+Break, closing its console): Windows keeps injected keys down after the process that pressed them exits.

**Injection on Windows** (`couch-platform-win`)
- Keys go in as set-1 scancodes through `SendInput`, so the host's layout decides what they type. Pause and Num Lock go by virtual-key code. Media and power keys are not sent.
- The mouse moves relatively; the wheel goes as `MOUSEEVENTF_WHEEL` and `HWHEEL`.
- The pads are ViGEmBus Xbox 360 controllers, one per seat with a pad. The app gives them a real Xbox 360 controller's ids, which games and Steam need; `couch-cli` uses USB ids 1209:0001 (pid.codes' test range) unless given `--real-pad-ids`. A viewer on the same machine skips the pads of any host running there, so they aren't captured and sent back: each host's marker names its pads' ViGEm serials (`couch_platform_win::virtual_pads`), and the viewer finds each gamepad's device in Windows' device tree (P9, `couch_platform_win::pad_origin`). One whose origin can't be told is skipped if it has Xbox 360 ids, as every Xbox 360 pad was before P9. Nothing of this is on the wire.

## Bitrate control (per viewer, 20 times a second)

The rate controller is the congestion control. QUIC's own controller only keeps the sender within twice what the path recently delivered ([ADR 0005](adr/0005-media-congestion-control.md)). Code: `couch_media::ratecontrol`.

**Inputs** (`FeedbackTracker`)
- From receiver reports:
  - **Delivery rate:** growth of `total_bytes_received` over the last 300 ms.
  - **Outstanding bytes:** sent in the current epoch minus that epoch's `epoch_bytes_received`.
  - **One-way delay trend:** the smallest network delay of the last 400 ms, minus the smallest of the last 10 s. The clock offset cancels out. Taking the minimum ignores frames held up behind a retransmission, since those delay only some frames while a queue delays all of them.
  - **Network delay** is what the viewer reports (arrival minus capture) minus the frame's time on the host, from capture until it was handed to the network, which the host records per frame (`SendDelays`). A host slowed by load (an encoder shared by several viewers, a busy GPU) would otherwise read as a filling queue, and cutting the bitrate can't speed an encoder up: before this, seven viewers on one PC all fell to the minimum bitrate.
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
5. **Clamp** between the minimum (1.5 Mbit/s) and the cap. The cap is the user maximum (20 Mbit/s), or the relay maximum (2 Mbit/s) on a relayed path. With an upload cap, the cap is also this viewer's share of it ([Sessions](#sessions)).
6. **Apply.**
   - Decreases apply at once.
   - Increases of more than 3% apply too, except where every bitrate change costs a keyframe (FFmpeg's NVENC, below). There, increases go in steps of at least 15%, at most once a second.

**QUIC settings**
- Timeouts: idle timeout 15 s, keep-alive 2 s. A silent peer is noticed sooner, after 5 s, by the missing `Alive` ([Liveness](#liveness)).
- Streams and windows: 64 concurrent unidirectional streams; stream window 8 MB, connection window 16 MB, send window 16 MB.
- Datagrams: 64 KB send buffer, so stale audio and input drop rather than queue. noq puts datagrams in each packet ahead of stream data. In M1's test, audio-sized datagrams gained under 1 ms while a 1 MB keyframe was in flight on loopback. A slow link's queue still delays them behind video (about 100 ms behind a 1 MB keyframe at 25 Mbit/s).
- Congestion control: `MediaCongestion` ([ADR 0005](adr/0005-media-congestion-control.md)). noq's BBR v3, Cubic and NewReno stay available for comparison.

**`h264_nvenc` settings**
- Preset p1, tune ull, `rc=cbr`, High profile.
- No B-frames, `delay=0`, `zerolatency=1`, `rc-lookahead=0`.
- `forced-idr=1` with an infinite GOP; `bufsize` = bitrate / fps (a one-frame VBV, which also keeps keyframes small).
- **Known cost.** FFmpeg 8.1's NVENC wrapper turns any bitrate change into an encoder reset with an IDR frame. That is why costly increases are batched. Calling NVENC directly (M9) reconfigures without one.

**`hevc_nvenc` settings** (M12c): the same as `h264_nvenc`'s, with the Main profile (8-bit). Only an NVIDIA host taking GPU frames sends HEVC.

**Which codec a viewer gets** (M12c, `pipeline::host::choose_codec`). Chosen when the viewer joins, and kept for its stream:
- HEVC if the viewer's `SessionJoin` says it decodes HEVC, the host's capture gives GPU frames to NVENC, and the host isn't forced to H.264 (`COUCH_HOST_CODEC=h264`);
- otherwise H.264.
- If `hevc_nvenc` fails to open or to encode, that viewer falls back to H.264 (a new epoch with a keyframe), and the other viewers keep theirs.
- A viewer reports HEVC when its GPU lists the HEVC Main decoder profile (`couch_platform_win::video_caps`), unless `COUCH_VIEWER_HEVC=0`.

**Decoder:** single thread, `AV_CODEC_FLAG_LOW_DELAY`. After a decode error the viewer drops frames until the next keyframe and asks for one at most every 500 ms. It does the same when a frame waited more than 200 ms for the decoder: decoding can't keep up, and the host can't see that queue.

## Rooms

Code: `couch_room::room` (the state machine), `couch-engine`'s `rooms` (runs it over iroh), `couch_net::admission` (admission). The design and its reasons: [ADR 0006](adr/0006-rooms-without-a-server.md).

**The owner decides who is in.**
- The room's founder is bound into its id: room id = the first 16 bytes of SHA-256(`"couch/room-id/v1"` || founder id || nonce [16]). Invites carry the founder (version 2, M13; the owner itself in version 1) and the nonce, so nobody can pass a room off as founded by someone else.
- **The owner now** is the founder, or, once the room has changed hands (M13), the last `to` of its chain of handovers ([below](#handing-a-room-over)), which travels with every policy.
- **Only the owner admits newcomers**, with an invite it made and the room password, and puts them on the roster.
- **Members accept connections only from ids on the roster** (iroh has already authenticated the ids). Members never run the password check among themselves.
- **Taking someone out is a new roster.** That covers a ban, a member leaving, and the owner closing the room. Every connection to someone no longer on it is closed, after showing them the policy as proof.

**Who writes what.** Each fact has one writer, who signs it, and any member relays it.
- **Policy** `{room, version u32, name, roster (at most 64), banned (at most 64)}`, signed by the owner. A higher `version` replaces a lower. It holds nothing secret.
  - Each roster entry is `{id, since}`: `since` is the policy version that put the member on the roster. It marks the member's *stay*: leaving and being let in again starts a new one.
  - The owner saves each policy before sending it, so a crash never makes it sign a different policy under a version already out.
- **Member record** `{room, id, since u32, version u64, name, addr {relay?, ips (at most 8)}, left}`, signed by that member. A higher `version` replaces a lower, within one stay.
  - A record counts only if its `since` matches the member's roster entry. Records of anyone else, or of an earlier stay, are dropped unverified, so a replayed `left` record from before a member came back can't remove it again.
  - A member that sees a record of itself, for its current stay, at least as new as its own (left over from before a crash) publishes a higher version.
- **Handover** `{room, from, to, after u32}` (M13), signed by `from`, the owner handing over: the room goes to `to`, after the policy at version `after`. At most 32 per room.
- **A policy travels with its chain** (`OwnedPolicy { policy, handovers }`) wherever it goes: `RoomSnapshot`, `PolicyUpdate`, `MemberHello`, and the proofs shown when refusing.
- **Signatures** are Ed25519 by the writer's endpoint key, over `domain || postcard(body)`. The domains are `couch/member-record/v1`, `couch/room-policy/v1` and `couch/room-handover/v1`. Receivers re-encode the body they decoded, so a non-canonical encoding fails the check.
- **Names** (display and room) are 1 to 96 bytes with no control characters, direction overrides or zero-width characters (the zero-width joiner stays, for emoji), so one name can't pass for another.

**Connections.** One iroh connection per pair of members *per room*. Members open it with `MemberHello` (see [Admission](#admission)); the acceptor then sends `RoomSnapshot`, and both send `Presence`. When two connections join the same pair in the same room:
- **dialed by different sides:** both sides keep the one the lower member id dialed (bytewise) and close the other with `duplicate`;
- **dialed by the same side:** the newer replaces the older, because the dialer only redials when it thinks the old one is dead.

**Gossip.**
- A record or policy that is new to a member goes on to every other member it is connected to.
- Every 30 s, each member sends a full snapshot to every peer, as repair. A snapshot with everything at its largest fits a 64 KiB frame.
- Signatures are checked only for news: a record or policy no newer than what we hold is dropped unverified.
- Messages from a peer no longer on the roster are ignored while its connection closes.

**Presence** (optional) is sent whenever a member's connections change. A member shows anyone a peer reports as "online", and dials them now rather than at the end of a backoff.

**Dialing.**
- Every member with a record is dialed at start and as soon as its record arrives. A member without one yet (it just joined) dials us instead.
- After a drop or a failed dial: after 1 s, doubling up to 60 s.
- A member that sent `Goodbye { Shutdown }` is dialed again after 60 s. It normally dials back first, when it starts.

**Leaving and removal.**
- **A member leaving** sends a `RoomSnapshot` (holding its `left` record) and `Goodbye { LeftRoom }` to everyone it is connected to, flushes, and closes with `left_room`. The others pass the record on, and the owner takes the member off the roster when it sees it. Coming back takes a new invite.
  - **Leaving takes delivery.** If no member was connected, the leaver keeps the room, hidden from the UI and across restarts, and keeps dialing the others until one of them has the record. A member that refuses it with the owner's policy already knows; one that answers `unknown_room` isn't in the room any more and is crossed off.
- **The owner leaving closes the room:** it signs a policy with an empty roster and sends it to everyone connected; they forget the room.
  - **Every former member must hear it.** The owner keeps the closed room, hidden and across restarts, until each one has. It dials those it hasn't told, its `MemberHello` carrying the closing policy, and shows that policy to any former member that dials it.
- **While leaving**, a member ignores anything about the room it receives and takes no part in it; it only tries to get the word out.
- **A ban** (owner only) takes the member off the roster, adds it to `banned`, and revokes every outstanding invite, since the banned member may hold one. The banned member learns of it from the owner's signature, never from a peer's say-so. If it was offline, every member it dials refuses it with that same proof.

**Size.** The owner admits nobody new once the roster has 12 members (`room_full`). The UI stops at 8.

### Handing a room over

M13; [ADR 0006](adr/0006-rooms-without-a-server.md).

**The chain.** A policy with its handovers (`OwnedPolicy`) checks out when:
- the chain runs from the founder: the first handover is `from` the founder, each next one `from` the previous `to`, never `to` itself, with `after` strictly increasing, all in this room, each signed by its `from`;
- and the policy is signed by the owner the chain names (its last `to`, the founder with none) at a version above the last `after`, or by the last `from` at exactly that `after` (the policy it handed over).

So an earlier owner's policies can't pass under a later chain, and a key that owns the room again (F→N→F) can't replay its first tenure's.

**Which policy wins.** One whose chain has ours as its start (handover by handover, by body), and then a longer chain, or the same chain with a higher version. A chain that parts from ours is never taken. This comes before any signature is checked, and only the handovers we don't hold yet are verified.
- So whatever a former owner signs after handing over (its chain shorter) loses to the new owner's policies, whatever its version.
- A former owner's key could still sign a second, diverging handover after the same version. Members holding the real chain never take it. A member that missed the real handover could, and then it never takes the real chain either: it stays on the fork until it leaves and is invited again. The app never signs such a handover.

**The handover**, both online, in two steps:
1. **The owner offers:** `HandoverOffer { offer, version }`, on its connection to a member on the roster.
   - `offer` is random; the offer lives in memory, on that connection only.
   - Any new policy voids it (the owner sends `HandoverWithdraw` on the connection), as does the connection closing (the owner sends `HandoverWithdraw` when the member connects again).
   - No offer while the owner is catching up after a restore, or has stopped waiting and hasn't heard every member yet, nor once the chain holds 32 handovers.
2. **The member answers:** `HandoverAnswer { offer, accept }`, on the same connection.
   - Accepting takes a new password, chosen there, for newcomers. Nothing secret travels.
   - **Accepting is for good.** The owner may sign at once, so there is no taking it back, and the member can't leave until it knows: until the room is handed to it, or the owner withdraws (on any connection), or the owner signs any newer policy (the offer is then void). Leaving before could leave a room nobody owns.
   - A room catching up declines any offer at once.
3. **The owner signs**, if the offer still stands (same id, connection, policy version): `Handover { from: it, to, after: its policy's version }`. Its policy with the extended chain is the last it signs.
   - It drops its password key and invites, saves, and sends the policy to everyone.
   - Every member relays it, so the new owner gets it even if the old one never comes back.
4. **The new owner, on taking it, signs a policy at once**: the same roster and bans, the next version (ahead, after a restore).
   - If it restarted in between and lost the password it chose, it owns the room without one, and sets one before inviting.

**Afterwards.**
- The former owner is a member: the new owner can ban it, and its leaving is a member's.
- An invite it made earlier reaches it and is refused with `invite_expired`.
- A newcomer admitted mid-check while the room changed hands is refused the same way.
- Invites the new owner makes are version 2 ([Invites](#invites)).
- A joiner's snapshot must come from the owner its invite names, by a chain from the founder.
- A stranger showing a policy whose chain isn't ours, or the start of it, is shown nothing: only a chain from our own room's owners proves someone a former member. Under the start of ours, only a policy from before the next handover we hold counts, so a former owner can't sign one afterwards that names a stranger.
- A relayed policy keeps the copies of the handovers we hold (verified): only the new ones are taken from it.

**With backups** (M13 part B):
- A restored former owner catches up and finds itself a member.
- A restored member that finds itself the owner by the chain catches up as an owner (waiting for everyone) and signs ahead.
- One that merges a newer policy it signed itself and doesn't hold has lost state: it catches up again before signing (as an owner, if the chain makes it one).
- **What this can't prevent:** an owner restored from a backup made before it handed the room over, whose new owner is away and unknown to every member it hears (let in after the backup), can hand the room over a second time. The two chains part, and neither side ever takes the other's. A member let in after the backup is how it happens; one who knows the new owner prevents it. Leaving and inviting again mends it.

## Admission

What a connection between room members exchanges after `Hello`, before it carries anything else.

**Member to member: `MemberHello { room, policy }`.** The dialer sends its copy of the owner's policy.
- The acceptor first adopts it if it is newer and the owner signed it. That way a member who missed a change (someone joined meanwhile) can decide anyway.
- **If the dialer is on the roster** and not leaving, the acceptor answers with its `RoomSnapshot`.
- **Otherwise it refuses**, closing with `banned` or `not_member`.
  - **A former member is shown why first:** `PolicyUpdate` with the policy, if the dialer's own copy, signed by the owner, lists it. A dialer that finds itself banned or dropped by the owner's signature forgets the room.
  - **A stranger is shown nothing**, so it learns nobody's id or name from knocking.
  - If the acceptor isn't in the room at all, it closes with `unknown_room`.

**Newcomer to owner: the password check.**
1. **Newcomer → `JoinStart { room, invite, pake }`.** `invite` names the invite without revealing its token: the first 16 bytes of SHA-256(`"couch/invite-tag/v1"` || token). The owner closes the connection, with the code shown, if:
   - the newcomer's id is in backoff (`rate_limited`; the endpoint's gate already refuses those right after the TLS handshake);
   - it isn't the owner of that room: a member answers `invite_expired` (the invite is one it made before handing the room over, M13), and it closes with `unknown_room` if it isn't in the room at all;
   - the newcomer is banned (`banned`), after sending it `PolicyUpdate` as proof;
   - the invite is unknown, expired, revoked or worn out (`invite_expired`);
   - the room has 12 members and the newcomer isn't one (`room_full`);
   - the rate limits say no (`rate_limited`, below).
2. **Owner → `AdmitReply`.**
3. **The newcomer checks the owner's confirmation.** A mismatch closes with `admission_failed` (wrong password). Otherwise it sends `AdmitConfirm`.
4. **The owner checks the newcomer's**, closing with `admission_failed` on a mismatch.
5. **The owner checks the rest again**, since it may have changed during the check: a ban (`banned`), the invite revoked or expired (`invite_expired`), the room filled (`room_full`). Then the newcomer goes on the roster.
6. **The owner sends `RoomSnapshot`**, whose policy now lists the newcomer. The newcomer builds its room from it, publishes its record, and dials the other members, showing that policy.

Each exchange must finish within 10 s. At most 32 incoming connections are being admitted at once; more are refused.

**Keys** (`couch_net::admission`)
- **Password key** = HKDF-SHA256(salt = room id, ikm = NFKC(trimmed password), info = `"couch/pw/v1"`). Only the owner keeps it; nobody keeps the password.
- **SPAKE2 password** = HKDF-SHA256(salt = the invite's token, ikm = password key, info = `"couch/pake-pw/v2"` || room id). A guess is worthless without the invite.
- **SPAKE2**, asymmetric, Ed25519 group. The newcomer is A and the owner B. The identities are `"couch/v1"` || room id || endpoint id. Messages are 33 bytes: a side byte and a point.
- **Exported** = 32 bytes from the TLS exporter (RFC 5705), label `"couch-coapp pake v1"`, context the room id. It binds the confirmations to this QUIC connection.
- **(k_A, k_B)** = HKDF-SHA256(salt = exported, ikm = the SPAKE2 key, info = `"couch/confirm/v1"`): 64 bytes, split in half.
- **Transcript** = SHA-256(`"couch/join/v1"` || room id || invite tag || id_A || id_B || msg_A || msg_B).
- **confirm_B** = HMAC-SHA256(k_B, transcript); **confirm_A** = HMAC-SHA256(k_A, transcript). Both are compared in constant time.

**Limits** (on the owner). Any check that started and didn't end in admission counts as a failure. That includes a newcomer that disconnects after reading the owner's confirmation, which lets it test one guess offline.
- **Per invite:** 5 failures and the invite stops working. Checks still running with it count too, so at most 5 run at once, fewer as failures add up.
- **Per remote id:** refused for 1 s after a failure, doubling up to 10 minutes; forgotten an hour after the last failure. The endpoint's post-handshake hook enforces it. One check at a time per id and room.
- **Per room:** 10 failures by non-members within an hour pause joining for 10 minutes, and the UI is told (`JoinsPaused`).
- **Running checks count:** checks still running count as failures against that threshold, so a burst of parallel guesses can't get past it before the first result lands. After a pause, while the hour still holds 10 failures, newcomers get one check at a time, and a failure pauses again.

**This composition is ours.** SPAKE2 comes from RustCrypto's `spake2` 0.5.0-pre.0; the way it is put together here should get an outside review before strangers are encouraged to rely on the app (optional since ADR 0009).

## Invites

`couchco://join#<payload>`, or, in a build with an invite page (M7), `https://<page>/j/#<payload>`: chat apps make https links clickable. The page (`deploy/invite-page`) reads the payload in the browser and offers the `couchco://` link and a copy button. The payload after `#` never reaches the page's server. The payload is unpadded lowercase base32 of:
- the magic bytes `CC`, which keep other base32 text from passing as an invite;
- a version byte: 1, or 2 for a room handed over (M13);
- postcard `{owner, nonce [16], name, token [32], expires u64, addr {relay?, ips}}`, then in version 2 `founder` [32]. `owner` is who admits now; the room id comes from the founder (the owner in version 1). A version 2 invite whose founder is its owner is refused, so each invite has one way to be written.

The layouts are pinned by `couch_proto::invite`'s `the_layout_is_pinned` and `the_handed_over_layout_is_pinned` tests. The invite page reads the room's name from either, at the same offset (`node deploy/invite-page/check.js`).

**Rules**
- **Only the owner makes invites**, and each has its own random token, which the owner keeps. Handing the room over (M13) drops them all: the new owner makes its own.
- **Lifetime.** An invite works for 7 days (`expires`, seconds since 1970, also shown to the joiner) and for anyone who has it and the password. It stops working on expiry, on the owner's "revoke invites", on a ban, or after 5 wrong passwords. At most 32 are kept; making another drops the one closest to expiring.
- **The address is the owner's own.** With relays configured that is its relay alone, never an IP address, so an invite posted in a chat publishes nobody's address. If the relay isn't known yet (just after startup), making the invite waits up to 10 s for it, then fails with "not connected to the relay yet". Without relays (a LAN, the tests), direct addresses are all there is.
- **Each member names its own relay.** Settings → Network (P3, M7) chooses one of these, per profile, from the next start:
  - the build's own relay, which is the default;
  - iroh's public relays;
  - one relay by URL: https, or http on this PC; a development or test build also takes http at a private LAN address.

  Invites and member records carry each member's own relay, and iroh dials a relay outside its own list (iroh's own `endpoint_relay_map_change` test covers that). With any choice, iroh tries a direct path first; the relay starts the connection and carries it when no direct path is found.
- **Our relay asks for a token** (M7): the relay URL and token are built into release builds. iroh sends the token only to relays in its own list, and **invites and member records never carry it**. A member whose setting is another relay can still dial ours, but ours refuses them. Without a direct path they don't connect, which is why Settings asks everyone in a room to use the same choice.
- **Parsing** accepts the bare payload or anything ending in `#<payload>`, in any case, with whitespace ignored, up to 4096 characters.
- **Joining needs the owner online.** Nobody else admits newcomers, so a join while the owner's app is closed fails with "the room's owner isn't online".
- **Opening a link only fills in the join screen.** Any web page can open a `couchco://` link, so the app previews the invite (the room's name, which only the invite claims, and whether it expired) and sends nothing until the user types the password and clicks Join. Links open in the default profile, since Windows runs one command per link scheme.
- **Changing the password** (the owner) replaces the owner's password key. Members stay; invites already sent keep working with the new password.

## Sessions

A session is a member sharing games with others in the same room, over the room's own connections. Code: `couch_room::session` (the host's state machine), `couch-engine`'s `rooms::sessions` (runs it), `couch_engine::pipeline` (video, sound and input).

**Sessions before streams** (M19). A session starts with nothing streamed: its seats, their virtual pads and the host's hidden controller come first, before any game runs. Streams then start and stop inside it, each a window or a screen with its own frame rate and quality; viewers and seats stay through them. `SessionStatus.stream` says which: `Waiting` (nothing yet), `Live`, or `Stopped` with why the last stream ended (the host stopped it, the game exited, its window or screen went, an error). While nothing is streamed, viewers' input is held off on the host (the seat router's hold): they keep their seats, but can't see what they would do. The host's own controller keeps playing.

**The host is the only writer.** It sends its session's facts straight to each member it is connected to; nobody relays them, so they carry no signature and never name the host.
- A member acts on a session message only from the session's own host: the connection's authenticated peer, on the current connection with it, while it is on the roster.
- To a host, `SessionJoin`, `SessionLeave`, `ControlRequest`, `ControlCancel` and `ViewerControllers` count only for its live session and from a member on the current connection; `ViewerControllers` only from a viewer, on the connection it joined on. A join for a session that isn't (or is no longer) live is answered `Ended`.

**The directory** (optional messages: a peer that doesn't know sessions only misses them).
- Starting a session, the host sends `SessionAnnounce` and `SessionStatus` to every member it is connected to, and again to every member whose connection to it becomes current. Titles follow the name rules.
- `SessionStatus` follows every change of viewers, controller, the control pause or the stream. It may list as many viewers as the room has members; the host's own limit is its business.
- `SessionEnded` ends it, with the reason: the host ended it, left the room or shut down. Since M19 the game exiting and its window or screen going end only the stream (`SessionStatus.stream`).
- A member forgets a session when its connection to the host goes; the next connection brings the announcement again.

**Watching.**
- A profile hosts at most one session and watches at most one, never both. A host takes 1 to 7 viewers (its choice when going live; 4 by default).
- The viewer subscribes to the connection's video epochs, sound and rumble *before* sending `SessionJoin`, so no epoch arrives unclaimed. Epochs naming another session are refused (`SESSION_END`).
- The host answers `SessionJoinReply`: `Joined` (video follows, starting with a keyframe, once something is streamed), `Full`, `Ended`, or `Unavailable` (no encoder could be opened). `Unavailable` can also come later, dropping a viewer whose stream failed.
- **Between streams** (M19), each viewer's epoch ends with `STREAM_END`, and the viewer keeps watching. The next stream is a new epoch on the same connection, numbered after the last (the viewer drops older ones), starting with a keyframe, in a codec chosen again for it.
- `SessionLeave`, the viewer's connection closing, a ban, or the viewer leaving the room ends watching. A ban or a departure counts the moment the room applies it, not when the connection finally closes. The host's seat router releases what that viewer held at once (`SeatRouter::disconnected`), without waiting for the dead-man timer.
- **A dropped connection, on the viewer's side** (M6). The viewer keeps its window open ("Reconnecting to …") while the room redials. When the host's new connection announces the same session, the viewer subscribes on it and sends `SessionJoin` again; to the host it is a new viewer, and the seat it held went with the old connection. Watching ends if the session ended or changed meanwhile, the host left the roster, the join is refused, or 30 s pass.
- **A join on a new connection, on the host's side.** If a viewer joins again on a connection other than the one it watched on (it reconnected before the host saw the old one close), the host drops the old viewer first, releasing what it held, then takes the join.
- **The viewer window** is the viewer's own business: if it exits without the user leaving (a crash), it is started again, at most once a minute, and the viewer asks for a keyframe. Nothing about it goes on the wire.

**Seats** (P1; one setup since M19). The announcement lists the session's seats, by `SeatId` (their index), fixed for the session:
- 0 to 4 `Controller` seats, and keyboard and mouse as one `KeyboardMouse` seat after them, or not at all. One player on pad and keyboard holds both seats (M18); the single seat with both, `Both`, went in M24 (wire revision 9).
- **Limits** (`session::valid_seats`): 0 to 5 seats, at most 4 with a pad (XInput's slots), at most one with keyboard and mouse (Windows has one keyboard and one pointer).
- **No seats** (M24): a session to watch together. Nobody plays: the host plugs no pad, hides none of their controllers and sets up no hook for its keys (no panic key, no answer keys); a `ControlRequest` names a seat that doesn't exist and is ignored, and `SessionStatus` has empty `holders` and `host_seats`. The stream, sound and viewers work as in any session.
- **What a seat gives.** Its kind decides what its holder may send: `Controller` the gamepad, `KeyboardMouse` keyboard and mouse. `SeatAssigned` carries those permissions, and the host approves every holder, so handing someone a keyboard seat is the trust decision.
- **Pads.** Each seat with a pad has its own virtual Xbox 360 pad, plugged in at the session's start in seat order (before any game runs, M19) and kept for the session, so neither a handoff nor a new stream unplugs one.
- **Keyboard and mouse need a window source.** A session with a keyboard seat streams only windows.
- **Where the host plays.** Either outside the seats, with whatever devices the game sees directly (while nobody holds a single seat, or beside the seats), or **on seats** (P2; M23: several), when the host chose their own controllers at the session's start:
  - the host app reads each controller (`couch_gamepad`) and feeds its state into the seat router as the host's own holder, on that controller's seat, with the pad permission only (the host's keyboard and mouse reach the game directly anyway);
  - the controllers are hidden from games meanwhile (HidHide; see the architecture), so every player the game sees is one of the seats' virtual pads, and any seat, the host's included, can be handed over;
  - each controller has its own seat: they start on the seats with a pad, in order, one each. One may move to another seat (a viewer on it loses it; the controller's old seat goes free), and moved onto a seat another of the host's controllers plays, the two swap. When a viewer lets go of a controller's seat (gives it back, leaves, goes quiet), it comes back to that controller: the host's seat router grants such a *claim* only if the seat is still free, it is one of the host's controllers' seats and control isn't paused, so it never takes a seat from a viewer it was just passed to. The session tells the router which controller drives which seat before any claim;
  - a seat the host takes back, or leaves (one of their own), stays empty: that controller takes a seat again only when the host moves it to one;
  - a controller that disconnects keeps its seat, its pad neutral, until it reconnects or the host leaves the seat (M23: the host is at the PC, unlike a guest);
  - viewers may ask for the host's seats; the host decides, as for any seat.
- `SessionStatus.holders` has one entry per seat and names viewers only; `host_seats` names the seats the host plays, in seat order, none of them held by a viewer; `host_on_seats` says the host's controllers play only through the seats, so a seat nobody holds is empty and the host may play none (otherwise the host plays outside the seats, and a free single-player seat is the host's). A viewer may hold several seats (M18), and so may the host (M23). A receiver ignores a status whose length differs from the announcement's seats, where a holder isn't a viewer, or whose `host_seats` name a seat twice or out of order, name a seat that isn't one or a viewer holds, or come without `host_on_seats`.

**Control.**
- A viewer asks for a seat with `ControlRequest`: a free one, one another viewer holds, or another than its own (added to its seats, or a move: see below). The host accepts (a `SeatAssigned` follows) or declines (`ControlDeclined`); a declined viewer's next request within 5 s is declined at once. Each viewer has one request pending at most; asking for another seat replaces it. A seat that doesn't exist is ignored.
- The host can also hand a seat to a viewer who didn't ask, and take one seat or every seat back. A viewer withdraws its request with `ControlCancel { what: Request }`, which never touches the seats it holds (a viewer asking for another seat keeps its own, even when the withdraw crosses a decline), and gives back one seat with `ControlCancel { what: Seat(Some(seat)) }`, or every seat it holds with `Seat(None)`.
- **Requests and grants that cross.** A grant answers a request only for the seat granted. A request for another seat stays pending, on the host and on the viewer alike, however a pass and a request cross on the wire.
- **Seats per viewer: added or moved** (M18). A viewer may hold one seat per controller it has, as it said in `ViewerControllers` (0 until it says, capped at 4), plus the keyboard-and-mouse seat. The host decides, for each grant:
  - a seat without a pad (`KeyboardMouse`) needs no controller: it is added to what the viewer holds;
  - a seat with a pad is added while the viewer holds (or has on their way) fewer seats with a pad than it has controllers;
  - otherwise one of its controllers moves (P13; before it, the viewer moved, losing every other pad seat): before granting, the host revokes that viewer from only as many pad seats as keep it within its controllers (normally one), on the same ordered channel to its seat router, so it never holds more than its controllers allow, even with a grant still on its way. The seat freed first is the one the moving controller drives, as the viewer last said (`ViewerControllers`): the controller its request names, or the one the host chose. With nothing naming one and more than one seat to choose from, the host's own pass is refused (the host picks a controller) and an accepted request frees from the last seat down. A keyboard-and-mouse seat it holds stays. The viewer hears `SeatReleased` for the freed seat, then `SeatAssigned` naming the controller for the new one.
  - **A named controller that plays one of the viewer's seats always moves** (0.19.1): its seat is freed even while the viewer has a controller to spare, so moving a controller never adds a seat. A named controller that plays none, or none named, adds the seat while one is spare.

  The count is the viewer's own word and decides only add or move, when the host grants; the host approves every grant. The host frees no seat when the count later drops (a seat held, or being added, stays); the viewer itself gives back the seat whose controller went, as below.
- **Which controller drives which seat** is the viewer's business (`couch_input::forward`): with one pad seat, the controller last touched; with several, each seat is pinned to one controller, a new seat taking the lowest one free, and the pins stay while their controllers are connected. **The host may choose** (P12): a grant naming a controller (`SeatAssigned.controller`), or `SeatController` for a seat held, pins that controller to the seat while it is connected, a seat on its own included, and a seat it drove takes the other's controller in exchange; a controller not connected is no choice. The viewer tells the host what it decided in `ViewerControllers`, which the host only shows: who holds a seat is still the router's alone. A controller that disconnects while its member holds several pad seats gives its seat back (`ControlCancel { what: Seat(Some) }`, sent by the viewer as if the user had); the others stay put. With one pad seat, nothing is given back: another controller, or the same one reconnected, carries on. Each seat's pad datagrams carry that seat's state; keys and mouse go to the seat that allows them.
- Every change goes through the host's seat router ([Input](#input)), the only judge of who holds each seat. It also refuses a grant with keyboard or mouse while another seat's holder has them. Seat epochs start at a random value per session, so input from an earlier session on the same connection can't carry a current one.
- **Rumble** from a seat's pad goes only to that seat's holder.

**The game's focus** (P1). Keyboard and mouse reach the game only while the streamed window is in front, and many games ignore controllers in the background. So the host tells its viewers with `GameFocus`:
- when it changes, coalesced to at most one message per 250 ms (the latest state);
- and to each new viewer, once known.
- Nothing is sent for a screen. A viewer assumes the game is in front until told otherwise, and shows a lasting line in the stream window while it isn't.

The session messages changed in place in P1 (seats in `SessionAnnounce`, `SessionStatus` and `ControlRequest`), P2 (`SessionStatus.host_seat` and `host_on_seats`) and M23 (`host_seat` became `host_seats`): nothing had been publicly released, and builds of different wire revisions refuse each other.
- **The panic key** (Ctrl+Alt+Shift+Backspace on the host's own keyboard) releases every seat every viewer holds at once, declines pending requests and pauses control (`control_paused` in the status) until the host allows it again. A grant to a viewer already on its way to the seat router when the key is pressed is refused too. The host's own controllers are not paused: they keep the seats they play, and may still move between seats. A seat the host had passed on stays empty; nothing comes back to the host while control is paused. The stream goes on.
  - It is a low-level keyboard hook (`couch_platform_win::panic_key`), installed while the host streams. Only physical key events count: injected ones (`LLKHF_INJECTED`, which is what a guest's keys become) are passed on and never change which modifiers it believes are held, so a guest can neither press it nor complete it. A modifier also counts only while Windows still has it down, so a key-up lost to the secure desktop doesn't leave it held; but once a guest's injected event has touched that modifier, only the physical state counts, because Windows' down bit is shared and a guest's key-up would clear it while the host holds the key. A guest can't block the key either.
  - Auto-repeat is told from a new press by timing (a repeat follows within Windows' longest repeat delay, 1 s). A Backspace already held before the chord is completed stays the game's, repeats and release included, so it never sticks down.
  - The chord's key-down, repeats and key-up are swallowed; the game never sees them. The hook releases straight through the input task, from its own thread, then tells the room actor.

**Ending.** When the host ends a session it releases all input first, then sends `SessionEnded` and resets the epoch streams with `SESSION_END`, then stops encoding, capture and sound, and unplugs the virtual pads last.

**Stopping a stream** (M19), the session going on: viewers' input is held off first (what they held is released at once), then each viewer's epoch is reset with `STREAM_END` and its encoder closed, then capture and sound stop; `SessionStatus.stream` says it stopped. Pads, seats and hiding stay.

**The game's window** (M6, `couch_engine::pipeline::source`). The host watches the process behind the streamed window. When the window goes:
- the process has exited: the stream ends with `GameExited`, at once (before M19, the session did);
- otherwise, for 5 s (`REPLACE_GRACE`), the host looks every 250 ms for a *new* window of that process or its descendants (visible, titled, not minimized, at least 200 pixels each way, largest first) and, if one shows, captures it instead, and watches its process from then on. Viewers see a short freeze and then a keyframe; nothing goes on the wire.
  - **Only a new window is followed.** The game's other windows (another browser window, Explorer's desktop) are never streamed in its place: the host notes the process tree's windows once a second while the captured one is there, and a window seen on two looks in a row is never followed. One that appeared within about a second of the old one closing is.
  - A process that exited still counts while one of its children shows a new window (a launcher handing over to the game).
- if none shows within the grace, the stream ends with `SourceLost`.

While nobody watches (no capture runs), the host checks once a second.

**The host's streams.** One capture feeds a latest-frame mailbox per viewer; each viewer has its own encoder, rate controller and epoch streams, so a slow viewer never holds up another (gate G7). One sound capture and Opus encoder serve every viewer. Capture and sound run only while someone watches.

**Upload share.** With an upload cap, once a second each viewer's ceiling is recomputed (`couch_media::ratecontrol::UploadShare`): a viewer held back by its own network (aiming below an equal share) gets its target plus 25% to grow into; the others split what is left equally. Shares move in 0.5 Mbit/s steps, rise at once, but come down only when the cut exceeds 10% (or keeping them would overrun the cap by more than 10%), since every bitrate change costs an NVENC keyframe.

**One stream acceptor per connection.** QUIC has one accept queue for unidirectional streams, so each room connection has one acceptor (`couch_net::streams`) that reads a stream's kind byte and hands it to whoever subscribed to that kind, as `couch_net::datagrams` does for datagram tags. Video and input can then flow both ways on one connection.

## Remote play

M25a ([ADR 0010](adr/0010-playing-your-own-pc-from-afar.md)): a device of the user's own (the *operator*) runs a home PC's sessions from afar, as if at it. Code: `couch_engine::remote` (the settings, the PIN), `rooms::remote` (the messages).

**Trust is set at the home PC only.** Its Settings → Remote play holds:
- whether remote play is on;
- the devices it trusts, each a member id picked from a room it shares, with a code (`SHA-256("couch/device-code/v1" || id)`, the first 4 bytes in hex, "AB12 CD34") compared on both screens;
- the PIN's Argon2id hash (19 MiB, 2 passes; the PIN trimmed and NFKC-normalized, at least 6 characters);
- the wrong-PIN count, and the time before which no PIN is tried.

It is kept in `remote-play.json`, never in a backup. No message changes any of it.

**A device accepts its home PC too.** An offer only asks the user whether that member is their own PC, showing its code. The device keeps the PCs it accepted in `remote-hosts.json`, and sends a PIN to no other: a room member offering remote play can't collect one.

**Unlocking.**
- A trusted device gets `RemoteOffer` on every current room connection while remote play is on.
- It sends `RemoteUnlock` on that connection, which the home PC checks off its actor thread. A device gets one check at a time, whichever of its connections asks, until that check ends; a connection of the device asking meanwhile hears that check's answer (a reconnect may replace the first connection). The try is counted before the check runs.
- After 5 wrong PINs, the next try waits 1 minute, doubling with each wrong one up to an hour, kept across restarts; a right PIN clears the count.
- A check started before the PIN changed, the panic key was pressed, the device was removed or remote play was turned off opens nothing.
- A right PIN makes the device the home PC's operator **on that connection**, until it closes.
- The device keeps the PIN in memory for the app's run, and unlocks again by itself after a reconnect.

**What an operator may do** is `RemoteOp`: the host's console, nothing else.
- The home PC turns each op into its own window's command and runs it the same way, with the same checks:
  - `StartSession` goes in the room of the operator's connection, without the home PC's own controllers;
  - the other ops act on its session only if that session is in that room.
- Rooms, invites, bans, handovers, settings, backups and remote play itself can't be asked for.
- The home PC stays the session's host and the only writer of its facts; members see its session as any other.

**Messages to the home PC count only** from a trusted id, on the current connection with it, while it is on the room's roster and remote play is on; operator messages only on the unlocked connection. **Messages to a device** count only on the connection its home PC's offer came on. None are relayed.

**The console's state goes only to operators in the session's room:** an operator in another room hears no session, not its guests or its window's title. Every op, `ListSources` included, refuses while the session (started or starting) is in another room.

**Locking.** The home PC locks operators out with `RemoteLock`:
- `Panic`: the panic key, pressed at it;
- `PinChanged`: a new PIN;
- `Removed`: the device removed;
- `Off`: remote play turned off.

After `Panic`, `PinChanged`, `Removed` or `Off`, the device forgets the PIN. After `Removed` or `Off`, it also forgets the offer. An operator locks itself with `RemoteLock { ByOperator }`; any other reason from a peer is a home PC locking us out (two PCs may trust each other). After the panic key, an operator who knows the PIN can unlock again: to keep a device out, the home PC turns remote play off or removes it.

## Close and reset codes

**Connection close** (`couch_proto::control::close_code`):

| Code | Name | Meaning |
|---|---|---|
| 0 | `normal` | Closed on purpose, nothing wrong |
| 1 | `admission_failed` | Wrong password, or the check didn't finish |
| 2 | `banned` | The owner banned this member |
| 3 | `duplicate` | Another connection between the same pair and room is kept |
| 4 | `version` | An unknown must-understand message, or another wire revision in `Hello` (M10a) |
| 5 | `rate_limited` | Too many failed checks; try later |
| 6 | `room_full` | The room has 12 members |
| 7 | `left_room` | The sender left the room |
| 8 | `invite_expired` | The invite expired, was revoked, or took too many wrong passwords |
| 9 | `unknown_room` | The acceptor isn't in that room (any more) |
| 10 | `protocol` | A malformed or unexpected message |
| 11 | `not_member` | Not on the owner's member list (any more) |
| 12 | `timed_out` | Nothing heard for 5 s from a peer that sends `Alive` |

**Stream reset** (an epoch stream, by the host; or stopped by the viewer):
- `SKIP_AHEAD` (1): a newer epoch replaces this one.
- `SESSION_END` (2): the session is over; also a viewer's answer to an epoch that names another session.
- `VIEWER_REMOVED` (3): this viewer left or was dropped.
- `STREAM_END` (4): the host stopped streaming; the session goes on, and the next stream is a new epoch (M19).

## Viewer IPC

Framing is `u32 len (big-endian) | u8 kind | payload`. `len` counts the kind byte plus the payload and is capped at 16 MiB. The main process writes to the viewer's stdin and reads its stdout. The viewer must never print anything else to stdout.

| Kind | Direction | Meaning |
|---|---|---|
| `0x01` | main to viewer | Hello: `IPC_VERSION` as a big-endian `u16`. A viewer that speaks another version stops |
| `0x02` | main to viewer | Video frame: `{epoch u32, seq u32, keyframe u8, codec u8 (as in the preamble), capture_us u64, encode_us u32, arrived_us u64}`, then the access unit. The viewer reopens its decoder when a keyframe brings another codec |
| `0x03` | main to viewer | Audio packet (postcard): `{seq, capture_us, arrived_us, packet}`, the datagram plus its arrival time on this machine |
| `0x04` | main to viewer | HUD state (postcard): `{title, host, role: Watching \| Asking \| Controlling, seats [{label, pad, keyboard_mouse, holder?, host, mine, asked}], host_on_seats, viewers u8, control_paused, game_in_front, stream, note}`, on every change. `host` and `holder` are display names; a seat's `host` says the host plays it with their own controller; `host_on_seats` that the host's controller plays only through the seats (a free seat is empty); `stream` is `SessionStatus.stream` (M19): while it isn't `Live`, the window shows black and says why |
| `0x05` | main to viewer | Shutdown |
| `0x06` | main to viewer | Network stats for the HUD: `{rtt_us u32, relayed u8, receive_bps u32}`, 4 times a second |
| `0x07` | main to viewer | Seat state (postcard): `{seats: [{seat, permissions, controller?}]}`, every seat held (M18), with the controller slot the main process chose to drive each (the window rumbles and shows by it). The viewer captures the mouse only while a held seat has mouse permission |
| `0x08` | main to viewer | Rumble (postcard): `{seat u8, low u8, high u8}`, for the controller driving that seat |
| `0x09` | main to viewer | Shortcut keys (postcard): `{keys: map of row id to HID usage}`, the rows the user moved off their defaults, sent right after Hello. The viewer checks them as the app does (`Shortcuts::with_keys`); rows that don't fit keep their defaults, which also apply until this frame arrives (P3) |
| `0x0A` | main to viewer | Stream volume (postcard): `{percent u8, muted}`, the profile's (`audio.json`), sent right after the shortcut keys, before any sound (P6). The pipe reader applies it to the sound at once, as a squared gain (50% is -12 dB) and never above 100%; full volume until it arrives. Sent again whenever the app's own volume control changes it (P11); the viewer doesn't echo it back as `0x86` |
| `0x0B` | main to viewer | Window request (postcard `WindowRequest`, P11): `Fullscreen(bool)`, `ShowBar(bool)` (the session bar alone: the centre card and the stats stay; the window's own shortcut, which hides everything over the picture, is apart and not reported), `ShowStats(bool)` or `Raise` (restored if minimized, then brought to the front; the app calls `AllowSetForegroundWindow` on the viewer's process first, since Windows refuses a background process the foreground otherwise) |
| `0x81` | viewer to main | Local input (postcard `LocalInput`): `Key{usage, down}`, `MouseButton{button, down}`, `MouseMotion{dx, dy}`, `Wheel{dx, dy}`, `Pad{controller, pad}` (a controller's state by its slot, kept while connected, M18), `ControllerSlots(bits)` (the slots connected, after every change) or `Alive` (every 100 ms). Deltas; the main process keeps the running totals |
| `0x82` | viewer to main | Frame timing: `{epoch, seq, capture_us, arrived_us, decoded_us, presented_us, barcode_us}` |
| `0x83` | viewer to main | User action (postcard): `RequestControl { seat u8, controller u8? }` (the controller its bar's choice named, with two or more, P13), `CancelControl` (withdraw, or give every seat back), `GiveBack(seat u8)` (one seat, M18; also sent by the main process's forwarder when the controller driving one of several seats goes), `Leave`. The viewer also sends `Leave` when its window is closed (M6): a viewer that exits without it crashed, and the app starts it again, at most once a minute |
| `0x84` | viewer to main | Keyframe request (decoder error) |
| `0x85` | viewer to main | Audio stats (postcard), every second: `{played, concealed, late, dropped, underruns}` since start, `buffered_us`, `target_us`, and capture to handing samples to the device (`capture_to_queue_us` median of the last second, `capture_to_queue_p95_us` over the session; meaningful only with host and viewer on one machine) |
| `0x86` | viewer to main | Stream volume changed (postcard, as `0x0A`), to save for the profile (P6): a shortcut or Mute at once, the slider once it rests for 0.5 s, and before `Leave` |
| `0x87` | viewer to main | Window state (postcard `WindowState`, P11): `{fullscreen, bar, stats}`, at once and whenever any changes, however (its shortcuts, its bar, a `0x0B`). The main process keeps the bar and stats for a window started again after a crash |
| `0x88` | viewer to main | Controller names (postcard `ControllerNames`, P12): `{controllers: [{slot, name}]}`, every connected controller, just before each `ControllerSlots` input, so a new one reaches the host by name at once. The main process's forwarder keeps each name until its controller goes and tells the host (`ViewerControllers`) |

`IPC_VERSION` (now 14: a request's controller, P13; 13 was P12's controllers' names) changes with any payload change. The viewer always ships with the app, so a mismatch means a broken install.

**Shortcuts.** Every shortcut is in one table, `couch_input::shortcuts`, which the viewer and the host-keys hook read and the app lists (`GetShortcuts`). Shortcut keys are never forwarded. The defaults, in the viewer with Ctrl+Alt+Shift held:
- C asks for a seat (the first nobody holds, controllers first), withdraws the request, or gives the seat back (a `USER_ACTION`); the HUD's bar has an Ask button per seat;
- F toggles full screen;
- H shows or hides the HUD; F3 alone does too, unless the keyboard is being forwarded;
- M frees the mouse; clicking the window captures it again;
- `-` and `=` turn the stream down and up by 10%, `0` mutes or unmutes it (P6); the HUD's bar has Mute and a slider.

On the host, while streaming, with Ctrl+Alt+Shift held (`couch_platform_win::host_keys`, one low-level hook for all three; physical keys only, so a guest's injected keys never count and the game never sees them):
- Backspace is the panic key;
- Y accepts and N declines the oldest request for a seat (M8), but only once it has been pending for 1 s, so a key meant for one request never answers another that just arrived in its place. The host overlay shows the requests and these keys over the game.

The user may move each row to another key in Settings (P3), never its modifiers: chord rows keep Ctrl+Alt+Shift, and the row that works alone takes only F-keys. Only keys from `shortcuts::KEYS` are allowed (letters, digits, F-keys but F4, Backspace, punctuation): never Esc, Tab, Delete, the gray navigation keys (keyboards wrap them in fake Shift events the panic-key hook can't tell from Shift), Enter, Space, the modifiers, the Windows keys, Pause, Print Screen or the numpad. No two rows share a key, the panic key included, and the panic key can be moved but not removed. The keys are saved per profile in `shortcuts.json`; the host's keys apply at the next session's start, the viewer's in the next stream window (the `0x09` frame).

**The HUD's bar.** The session bar (title, host, who plays, this viewer's state, a note, and buttons for the same actions plus Leave) takes clicks only while the mouse is free. A click on it goes to the HUD alone: it is never forwarded and never captures the mouse.

Losing the focus releases every forwarded key and button. Gamepads keep working in the background.
