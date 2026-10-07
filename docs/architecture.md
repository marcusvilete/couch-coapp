# Architecture

Couch Co-App lets friends play a game "on the same couch" over the internet. One member of a password-protected room streams a game; the others watch, and the host hands control to one of them.

This document describes how the pieces fit together. The wire format is in [protocol.md](protocol.md); the reasons behind the big choices are in [adr/](adr/).

## Status

Windows first. Linux and macOS backends come later, behind the same traits.

| Milestone | Scope | State |
|---|---|---|
| M0 | Toolchain, workspace, probe, docs | done |
| M1 | Video over iroh between two processes; latency gates | done ([results](m1-results.md)) |
| M2 | Input: SDL3 capture, ViGEm pad, `SendInput` | done, except keyboard and mouse end to end, which needs a second machine ([results](m2-results.md)) |
| M3 | Audio: game process loopback, Opus, jitter buffer, SDL3 playback | done ([results](m3-results.md)) |
| M4a | Rooms in the engine: identity, invites, admission, the mesh | done ([results](m4a-results.md)) |
| M4b | Lobby UI: rooms, invites and deep links, members with their links, profiles side by side | done ([results](m4b-results.md)) |
| M5 | Sessions and control: M5a in the engine, M5b on screen | Done ([M5a](m5a-results.md), [M5b](m5b-results.md)) |
| M6 | Lifecycle and safety: liveness, the game's window and exit, watching that survives drops and crashes, keys released after a host crash, Alt+F4 blocked, the elevated-game warning | done ([results](m6-results.md)) |
| P1 | Seats and input modes: single player or multiplayer per game, a pad per controller seat, a keyboard seat, requests that name a seat; the "game not in front" notice | done ([results](p1-results.md)) |
| P2 | The host on a seat: the host's controller drives a seat's virtual pad, and HidHide hides the physical one, so any seat can be handed over | done; the two-PC run is to come ([results](p2-results.md)) |
| Polish | The rest of the polish backlog: the shortcut editor, choosing the relay (Settings → Network), the splash screen, the full-screen re-check | |
| M8 | Host overlay window | |
| M9 | Zero-copy GPU paths, HEVC, higher frame rates | |
| M7 | Own relay, the https invite page, the production relay setup | moved to just before M10: until then, iroh's public relays serve development and real-network tests |
| M10 | Packaging | |

## Processes

```
Host machine                                    Viewer machine
+-----------------------------------+           +-----------------------------------+
| Tauri app (main process)          |           | Tauri app (main process)          |
|   React UI (webview)              |           |   React UI (webview)              |
|   engine: rooms, sessions         |<- iroh -->|   engine: rooms, sessions         |
|   capture -> encode (per viewer)  |   QUIC    |          | stdin/stdout pipe      |
|   seat router -> ViGEm, SendInput |           | couch-viewer (child process)      |
+-----------------------------------+           |   decode -> render, audio out     |
                                                |   keyboard, mouse, gamepad, HUD   |
                                                +-----------------------------------+
```

- **Main process.** The Tauri app. Its webview shows the lobby and room UI; its Rust side links the engine, which owns the iroh endpoint, room and session state, and (on a host) capture, encoding and input injection.
- **Viewer process.** `couch-viewer`, started by the main process when its user joins a session. It owns a native SDL3 window, decodes and renders video, plays audio, captures local input and draws the in-stream HUD.
  - It does no networking. The main process forwards encoded frames over the viewer's stdin and reads input events and timing from its stdout (`couch_proto::ipc`).
  - Logs go to stderr. If the main process dies, the viewer sees end-of-file and exits.
- **Why the stream isn't in the webview.** Browser jitter buffers add about 100 ms. Webviews can't grab the keyboard or read gamepads reliably. Linux's WebKitGTK has no WebRTC. See [ADR 0002](adr/0002-separate-viewer-process.md).

## Crates

| Crate | Responsibility | May depend on |
|---|---|---|
| `couch-proto` | Wire and IPC types, framing, versioning, ids | nothing internal |
| `couch-media` | Frame types; capture, codec, transport and audio traits; rate controller; audio framing, jitter buffer and click track; test pattern; timestamp barcode | nothing internal |
| `couch-input` | Input model (HID usages, pad state), injection traits; the seat router, the viewer-side forwarder and the blocked-key filter, all sans-IO | `couch-proto` |
| `couch-room` | Room (membership, the owner's policy, the mesh), admission rate limits, and the host's session (viewers, requests, the control pause), all sans-IO; Ed25519 signatures on room facts | `couch-proto` |
| `couch-net` | iroh endpoint, its gate and congestion controller; the password check (SPAKE2); control, video, audio and input channels, datagram routing and the stream acceptor, netem test transport | `couch-proto`, `couch-media`, `couch-input` |
| `couch-codec-ffmpeg` | Everything that touches FFmpeg (video) | `couch-media` |
| `couch-codec-opus` | Opus encode, decode and loss concealment, through libopus built from source (the `opus` crate, needs CMake) | `couch-media`, `couch-proto` |
| `couch-platform-win` | Everything that calls Windows APIs, including WASAPI loopback capture, `SendInput`, ViGEm pads, HidHide and DPAPI | `couch-media`, `couch-input` |
| `couch-gamepad` | Physical gamepads through SDL3: a pad's state in XInput's terms (shared with the viewer), and the host's own controller, read on an SDL thread of its own | `couch-input`, `couch-proto` |
| `couch-engine` | UI-agnostic facade: `Command`s in, `Event`s out. Runs the rooms and their sessions over iroh; keeps the identity and remembered rooms (DPAPI-sealed) in the profile folder. Its `pipeline` (host and viewer side of a stream, behind a `Backend` trait) is shared with `couch-cli` | everything above |
| `couch-viewer` | The viewer binary | `couch-proto`, `couch-input`, media and codec crates |
| `couch-cli` | Developer harness on the engine's pipeline: `probe`, `host`, `view`, `latency-probe`, `gates`, `pad-test`, `input-sink`, `release-check`, `capture-test`, `screen-scan` | anything |
| `couch-testkit` | Fakes (recording injector and pad, test pattern helpers); dev-dependency only | trait crates |
| `xtask` | `fetch-deps`, `stage-dlls`, `doctor` | none |
| `apps/desktop/src-tauri` | Thin Tauri adapter over `couch-engine` | `couch-engine` |

Rules that keep this layering useful:

- **Sans-IO state machines.** `couch-room` transitions take `(state, input, now)` and return effects. No sockets, threads or clocks inside, so they are deterministic and property-testable.
- **One crate per native boundary.** Only `couch-codec-ffmpeg` links FFmpeg, only `couch-codec-opus` links libopus, and only `couch-platform-*` calls OS APIs. Logic crates therefore build and test anywhere, without LLVM, FFmpeg, a GPU or drivers.
- **The engine knows nothing about Tauri.** The UI talks to it through one Tauri command (`engine_command`, taking a `Command`) and one event (`engine-event`, carrying an `Event`). TypeScript types are generated by ts-rs into `apps/desktop/src/bindings/` (`cargo test -p couch-engine`).

## Data paths

**Host, per frame.**
1. A `Capturer` (Windows.Graphics.Capture) delivers a BGRA frame stamped with QPC time.
2. Each viewer's `VideoEncoder` (NVENC through FFmpeg, one encoder per viewer) produces an H.264 access unit.
3. That viewer's `VideoSender` writes it to the current epoch stream.
4. A rate controller per viewer adjusts its encoder's bitrate from the transport-neutral `FeedbackSnapshot`.

**Viewer, per frame.**
1. The main process receives the frame on the epoch stream.
2. It forwards the frame to `couch-viewer` over stdin.
3. The viewer decodes it (D3D11VA) and presents it immediately; there is no jitter buffer for video.
4. Decode and present times go back over stdout and become receiver reports.

**Audio, host to viewer** ([protocol.md](protocol.md#audio)).
1. WASAPI process loopback captures the game's process and its children (`couch_platform_win::audio`). For a monitor source it takes everything except our own processes. Packets come every 10 ms, stamped with QPC time.
2. `couch_media::audio::Framer` cuts them into 10 ms frames; Opus encodes each at 96 kbit/s (`couch-codec-opus`); each packet goes as one datagram.
3. The viewer's main process forwards packets over stdin. In `couch-viewer`, an adaptive jitter buffer (`couch_media::audio::JitterBuffer`, 10 to 40 ms) feeds SDL's audio thread, which decodes or conceals a 10 ms frame at a time.
4. SDL plays them on the default device. The buffer's playback ratio (at most 2% fast or slow) drives SDL's resampler, absorbing the drift between the two machines' audio clocks.

**Input, viewer to host** ([protocol.md](protocol.md#input)).
1. SDL3 captures local input in the viewer: keys as scancodes, which are HID usages; the mouse in relative mode; gamepads, also in the background.
2. It travels to the main process over stdout. There `couch_input::forward` adds the seat and its epoch, keeps the running mouse totals and sends the heartbeat.
3. Keys and buttons go over iroh on the input stream; pad state and mouse motion go as datagrams.
4. On the host, `couch_input::router` lets input through only from the seat's holder, with the current seat epoch and permissions, and keyboard and mouse only while the game is in front.
5. Accepted input drives the ViGEm pad or `SendInput`. Rumble goes back as a datagram.

The first versions copy frames through CPU memory on both ends. Zero-copy GPU paths arrive in M9 behind the same traits, which is why `EncoderInput` and `DecodedFrame` are non-exhaustive enums.

## Measuring latency

All timestamps are QPC microseconds, so on one machine the host's and the viewer's clocks are the same clock.

- **The latency clock.** `couch-viewer --clock X Y` opens a small window that paints the QPC time every frame:
  - as a barcode of 66 cells, 16 px each: a white and a black sync cell, then 48 bits of the value and a CRC-16/CCITT-FALSE;
  - and as digits, for people.
- **Clock to present.** The host captures that window. The viewer reads the barcode out of each decoded frame (`couch_media::barcode`) and compares it with the QPC time at present. This is the end-to-end number for gate G1. It includes the clock's own render and the compositor before capture, about 9 ms.
- **On screen.** `latency-probe --screen` captures the whole monitor, where the clock and the viewer's copy of it sit side by side, and reads both barcodes (gate G1b). Only values from the last 2 s count, which rejects false matches.
- **Capture to present** uses the capture timestamp Windows.Graphics.Capture attaches to each frame. That timestamp runs slightly ahead of when the frame is actually delivered, so this figure understates latency. Trust the barcode figure.
- **Impaired networks** come from `couch_net::netem`, a test-only iroh transport over loopback UDP. It adds delay, random loss, a rate limit with a drop-tail queue, and scheduled blackouts or rate changes.
  - iroh can't switch off its own multipath and NAT traversal, so a tool like clumsy would see traffic on paths it doesn't impair.
  - A blackout set on the host's side cuts both directions.
- **Rate-control traces.** `host --rc-trace file.csv` (or `latency-probe --rc-trace`) writes every controller tick. `view --timings-csv` writes every presented frame. Both use QPC time, so the two files line up. With `--netem`, the host's trace also records the time since the netem link started (`netem_us`), which places scheduled blackouts and rate changes exactly.
- **Gates.** `couch-cli gates` runs G1 and G3 to G5 through `latency-probe` and judges them from those traces. The `rate-control-gates` skill (`.claude/skills/`) has the procedure and how to read a failure.
- **Audio.** The clock window also clicks twice a second. `latency-probe --audio` loopback-captures the clock and the viewer and pairs up the clicks (click to playback). The viewer also reports, from packet timestamps, how long audio takes from host capture to being handed to the audio device; that is gate A1. See [m3-results.md](m3-results.md) for why the two differ by Windows' own playback path.

## Connectivity

- **One transport.** Every pair of room members shares one iroh (QUIC) connection. It carries control, video, audio and input ([ADR 0001](adr/0001-iroh-for-all-traffic.md)).
- **Connection setup.** iroh hole-punches a direct path when it can, which is about 9 pairs in 10, and falls back to a relay server otherwise.
- **Congestion control.** The per-viewer rate controller does it. It is delay-based, much like WebRTC's GCC. QUIC runs our `MediaCongestion` controller, which ignores isolated losses and probes quickly after an outage ([ADR 0005](adr/0005-media-congestion-control.md)). Stock Cubic and BBR v3 cut the bitrate to a tenth under 1% random loss.
- **Relays** (`couch_net::endpoint::RelayChoice`):
  - Development: iroh's public relays (`N0Public`), whose terms cover hobby use only.
  - Production: only our own `iroh-relay` on a small VPS, with no third-party address lookup (`Custom`).
  - Tests: no relays (`Disabled`).
- **Joining a room.** Only the room's owner admits newcomers. An invite link carries the room's owner and nonce (which make its id), the invite's own token and the owner's relay (or addresses on a LAN). The newcomer and the owner check the room password with SPAKE2; it never leaves either machine.

## Rooms

([protocol.md](protocol.md#rooms), [ADR 0006](adr/0006-rooms-without-a-server.md))

- **Identity.** Each profile has one Ed25519 key, which is both its iroh endpoint id and its member id. It lives in `identity.key` in the profile folder, sealed with DPAPI.
- **Who is in.** The owner (the creator) signs the roster. Only the owner admits newcomers, with an invite it made and the password, and only the owner removes anyone: a ban, a departure, or closing the room is a new roster.
- **The mesh.** Every member connects to every other member of a room. Members accept a connection only from an id on the owner's roster; the dialer shows its copy first, so a member who missed a join catches up. Members dial everyone they know; crossed dials settle on the one the lower id made. They reconnect after restarts without the owner.
- **State.** `couch_room::room::Room` holds one room as one member sees it: the owner's signed policy, signed member records, the connections, and, for the owner, its invites. It is sans-IO. `couch-engine`'s room actor runs one per room. It owns the connections, carries out the machine's effects (dial, send, close, save) and turns its status into the UI's `RoomView`.
- **Remembered rooms.** One file per room in the profile folder (`rooms/<id>.room`), sealed with DPAPI: the saved state and, for rooms we own, the invites and the password key (never the password). At start every remembered room dials all its members; a room with nobody online waits for others to dial in.
- **Gate and limits.** The endpoint's post-handshake hook refuses ids in backoff before any of our protocol runs. The owner checks the rest when a newcomer names its room and invite.

## Sessions and control

([protocol.md](protocol.md#sessions))

- **Where they live.** A session rides on its room's connections: the roster already decides who may watch. The host is the only writer of its session's facts and sends them straight to each member.
- **Ownership.** The host owns the session's state: viewers, requests, who holds each seat, the control pause (`couch_room::session`). Who holds a seat is decided by the seat router alone (`couch_input::router`); the session mirrors it.
- **Seats and input modes** (P1). The host's input setup for the game (`InputSetup`, remembered per program in `input-setups.json`) gives the seats:
  - single player: one seat, a controller, keyboard and mouse, or both;
  - multiplayer: 1 to 4 controller seats, each its own virtual pad, plus optionally one keyboard-and-mouse seat.

  The seat's kind sets its holder's permissions. One seat per viewer: a grant revokes that viewer's other seat first. Only one holder at a time has keyboard and mouse (the router refuses a second).
- **The host on a seat** (P2). In P1's co-op run, the game gave player 1 to the host's own DualSense, so a guest's virtual pad got no player. So the host can now play a seat too: Go live's "Your controller" choice.
  - **Reading it.** The engine reads that controller with SDL3 on a thread of its own (`couch_gamepad::host_pads`; the `sdl3` crate allows one SDL thread per process, and nothing else in the app uses SDL). Its states enter the input task and go through the seat router as the host's own holder, with the pad permission only.
  - **Hiding it.** HidHide hides the controller from every other program while the host is live (`couch_platform_win::hidhide`): every node of it, by container id, and our exe goes on its whitelist. HidHide refuses only new opens, so a game or Steam that had the controller open keeps it until it is reconnected; the app asks the host to do that once.
  - **Showing it again.** With HidHide 1.5.230 (the latest release) the entries go on its persistent blacklist, and a marker file records exactly what we changed: stopping undoes it after the pads are unplugged, and the next start of the app or of `couch-cli host` undoes what a killed host left. HidHide's newer session blacklist, which the driver clears when our process exits, is used when present.
  - **The seats.** The host starts on the first seat with a pad and holds one at a time. Their last seat comes back to them when it comes free (a *claim*, granted only if the seat is still free). The panic key doesn't pause them.
  - **Without it** (no HidHide, no controller, or the host's choice) the host plays outside the seats, as in P1: in single player while nobody holds the seat, in multiplayer beside them.
- **Handoff.** Passing a seat changes its holder and increments its epoch, releasing first. The virtual pads stay plugged in for the whole session, so the game never sees a controller disconnect.
- **Requests.** Viewers ask for a seat; the host accepts or declines, passes a seat without a request, or takes one or all back. The panic key takes everything back and pauses control.
- **The game's focus.** The host's input task notices the streamed window leaving the front and tells the viewers (`GameFocus`), whose stream window says why input may not work.
- **Disconnects.** If a viewer leaves, closes its window, loses its connection or is banned, everything it held is released at once.
- **In the engine.** The room actor runs the session protocol (`rooms::sessions`). The media and input run in `couch_engine::pipeline`: `HostStream` (one capture, an encoder per viewer, one sound capture, one input task) and `Watch` (the viewer process). **Input never goes through the actor**: it flows from the connection to the input task's seat router to the injectors.
- **On screen.** The app's stage shows a card per live session (Watch, ask, stop watching) and, for the host, a panel with the viewers, the requests and the pause; the taskbar flashes on a request. The viewer's HUD shows the session and takes clicks on its bar only while the mouse is free. While a host streams, the panic-key hook (`couch_platform_win::panic_key`, through `pipeline::Backend`) watches the physical keyboard. All shortcuts live in `couch_input::shortcuts`.

## Lifecycle

How sessions survive things going wrong, or end clearly (M6):

| What happens | What the app does | Where |
|---|---|---|
| The game swaps its window (display mode, leaving a launcher) | The host follows the new window within the 5 s grace | `pipeline::source`, `couch_platform_win::game` |
| The game exits | The session ends with "the game exited", at once | same |
| The window closes, the game runs on | "Source lost" after the grace | same |
| A peer's app is killed or hangs, or the path dies | `Alive` stops; after 5 s the connection is closed as `timed_out` | `couch_net::control` |
| The connection to the host drops while watching | The stream window stays ("Reconnecting…"); watching resumes on the next connection if the session is the same, within 30 s | `rooms::sessions`, `pipeline::watch` |
| The stream window crashes | It is started again (at most once a minute), and asks for a keyframe | `pipeline::watch` |
| The host app panics | A panic hook lets go of every key and button it holds | `couch_platform_win::held_input` |
| The host app is killed while a guest holds a key | A marker file records what is held; the next start of the app or of `couch-cli host` lets go of it | same |
| The host app is killed while the host's controller is hidden (P2) | A marker file records what was hidden; the next start of the app or of `couch-cli host` shows it again. Until then the controller doesn't work in games | `couch_platform_win::hidhide` |

`couch-cli lifecycle-check` stages the game's cases, a killed host, and a host killed holding a key, and times each.

## Safety invariants

These hold in every milestone. Tests guard them from M2 on.

- Guests get gamepad permission only. Keyboard and mouse are an explicit per-member grant.
- Keyboard and mouse are injected only while the captured game window is in the foreground.
- Remote members can't send the Windows key combinations, Alt+Tab, Alt+Esc, Ctrl+Esc, Ctrl+Shift+Esc, Alt+F4 (a guest quits through the game's menu), or the gamepad Guide button.
- A host panic key revokes all control; it ignores injected keystrokes.
- Every handoff, disconnect and teardown releases held keys and sends a neutral pad state first.
- Remote input reaches the injectors only through the seat router (`couch_input::router`), which enforces the rules above and the 300 ms dead-man release. So does the host's own controller when it plays a seat (P2).
- A controller the app hides from games is shown again when the session ends, and at the next start after a crash.
- Data from peers is size-limited and parsed defensively. Envelope frames are capped at 64 KiB and IPC frames at 16 MiB.

## Platform limits

- Virtual gamepads on Windows need the ViGEmBus 1.22.0 driver, which is archived but still the only redistributable option. It sits behind the `VirtualPad` trait so a user-mode driver can replace it.
- **The host on a seat** needs HidHide (MIT; 1.5.230, May 2024), a driver installed once with administrator rights and a restart; M10's installer can offer it. Our virtual pads report a real Xbox 360 controller's ids, so a real Xbox 360 controller on the host can't be told from them and can't play a seat (Xbox One and later, PlayStation and Switch Pro controllers can).
- **Injection limits.** Elevated game windows, UAC prompts, the lock screen and anti-cheat titles reject injected input. A virtual gamepad still reaches an elevated game, but the panic key's hook doesn't see keys while it is in front (measured in M6). Go live marks windows of programs running as administrator and warns when one is picked; Take back in the app still works.
- **Exclusive fullscreen.** True exclusive-fullscreen games may not capture through Windows.Graphics.Capture; the Desktop Duplication fallback lands in M9.
- **macOS.** Planned as a viewer and keyboard/mouse host only. System-wide virtual gamepads need an Apple-restricted entitlement.

## Native dependencies and licensing

- **FFmpeg.** The 8.1 LGPL shared build from BtbN, pinned in `xtask/src/deps.rs`. It is linked dynamically and its DLLs ship unmodified, so they stay replaceable. Its build config must not include `--enable-gpl` or `--enable-nonfree`, and `cargo xtask doctor` checks this.
- **FFmpeg bindings.** Its Rust bindings are generated once and committed (`cargo xtask gen-ffmpeg-bindings`, [ADR 0004](adr/0004-ffmpeg-via-rsmpeg.md)), so builds need no LLVM.
- **SDL3.** 3.4.16, Zlib license, prebuilt Visual C++ package.
- **libopus.** 1.6, BSD-3-Clause, compiled from source by the `opus` crate with CMake and linked statically. FFmpeg's build includes libopus too, but its wrapper can't ask for loss concealment.
- **Crates.** Permissive licenses only, enforced by `cargo deny check` (`deny.toml`). The app's own license is undecided, so nothing may narrow it.
- **GPL projects.** Sunshine, Moonlight and RustDesk are GPL/AGPL. Read them for ideas; never copy their code.
