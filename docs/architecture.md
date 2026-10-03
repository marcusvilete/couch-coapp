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
| M4a / M4b | Rooms in the engine / lobby UI | |
| M5 | Sessions and control | |
| M6 | Lifecycle and safety | |
| M7 | Own relay, real-network tests | |
| M8 | Host overlay window | |
| M9 | Zero-copy GPU paths, HEVC, higher frame rates | |
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
| `couch-room` | Room, session and seat state machines, sans-IO | `couch-proto` |
| `couch-net` | iroh endpoint and congestion controller, control, video, audio and input channels, datagram routing, netem test transport; admission and mesh from M4a | `couch-proto`, `couch-media`, `couch-input` |
| `couch-codec-ffmpeg` | Everything that touches FFmpeg (video) | `couch-media` |
| `couch-codec-opus` | Opus encode, decode and loss concealment, through libopus built from source (the `opus` crate, needs CMake) | `couch-media`, `couch-proto` |
| `couch-platform-win` | Everything that calls Windows APIs, including WASAPI loopback capture, `SendInput` and ViGEm pads | `couch-media`, `couch-input` |
| `couch-engine` | UI-agnostic facade: `Command`s in, `Event`s out; session orchestration | everything above |
| `couch-viewer` | The viewer binary | `couch-proto`, `couch-input`, media and codec crates |
| `couch-cli` | Developer harness: `probe`, `host`, `view`, `latency-probe`, `gates`, `pad-test`, `input-sink`, `release-check`, `capture-test`, `screen-scan` | anything |
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
- **Joining a room.** An invite link carries the room id, a random room secret and members' addresses. The room password is checked peer-to-peer with SPAKE2 and never published.

## Sessions and control

- **Ownership.** The session host owns the session's state: viewers, seats and grants. Single-player mode has one seat; co-op later adds seats.
- **"The host has control."** The host's own devices drive the game directly.
- **Handoff.** Passing control changes the seat's holder and increments its epoch. The virtual pad stays plugged in for the whole session, so the game never sees a controller disconnect.
- **Requests.** Members request control and the host accepts or declines.
- **Disconnects.** If the controlling member drops, everything they held is released and the seat returns to the host.

## Safety invariants

These hold in every milestone. Tests guard them from M2 on.

- Guests get gamepad permission only. Keyboard and mouse are an explicit per-member grant.
- Keyboard and mouse are injected only while the captured game window is in the foreground.
- Remote members can't send the Windows key combinations, Alt+Tab, Alt+Esc, Ctrl+Esc, Ctrl+Shift+Esc, or the gamepad Guide button.
- A host panic key revokes all control; it ignores injected keystrokes.
- Every handoff, disconnect and teardown releases held keys and sends a neutral pad state first.
- Remote input reaches the injectors only through the seat router (`couch_input::router`), which enforces the rules above and the 300 ms dead-man release.
- Data from peers is size-limited and parsed defensively. Envelope frames are capped at 64 KiB and IPC frames at 16 MiB.

## Platform limits

- Virtual gamepads on Windows need the ViGEmBus 1.22.0 driver, which is archived but still the only redistributable option. It sits behind the `VirtualPad` trait so a user-mode driver can replace it.
- **Injection limits.** Elevated game windows, UAC prompts, the lock screen and anti-cheat titles reject injected input.
- **Exclusive fullscreen.** True exclusive-fullscreen games may not capture through Windows.Graphics.Capture; the Desktop Duplication fallback lands in M9.
- **macOS.** Planned as a viewer and keyboard/mouse host only. System-wide virtual gamepads need an Apple-restricted entitlement.

## Native dependencies and licensing

- **FFmpeg.** The 8.1 LGPL shared build from BtbN, pinned in `xtask/src/deps.rs`. It is linked dynamically and its DLLs ship unmodified, so they stay replaceable. Its build config must not include `--enable-gpl` or `--enable-nonfree`, and `cargo xtask doctor` checks this.
- **FFmpeg bindings.** Its Rust bindings are generated once and committed (`cargo xtask gen-ffmpeg-bindings`, [ADR 0004](adr/0004-ffmpeg-via-rsmpeg.md)), so builds need no LLVM.
- **SDL3.** 3.4.16, Zlib license, prebuilt Visual C++ package.
- **libopus.** 1.6, BSD-3-Clause, compiled from source by the `opus` crate with CMake and linked statically. FFmpeg's build includes libopus too, but its wrapper can't ask for loss concealment.
- **Crates.** Permissive licenses only, enforced by `cargo deny check` (`deny.toml`). The app's own license is undecided, so nothing may narrow it.
- **GPL projects.** Sunshine, Moonlight and RustDesk are GPL/AGPL. Read them for ideas; never copy their code.
