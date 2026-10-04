# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Couch Co-App is a Windows-first desktop app for remote "same couch" gaming. A member of a password-protected room streams a game; the others watch, and the host hands control to one of them.

It is a Rust workspace with a Tauri 2 shell (React + TypeScript). Where to read more:
- `docs/architecture.md`: start here. It has the milestone table (M0 to M10).
- `docs/protocol.md`: the wire format.
- `docs/adr/`: the decisions behind the big choices.

## Commands

Run from the repo root in PowerShell, after `. .\scripts\dev.ps1`. That script rebuilds PATH so cargo and the FFmpeg/SDL3 DLLs are found, even in a terminal opened before they were installed.

| Task | Command |
|---|---|
| Native deps, first time | `cargo xtask fetch-deps`, then `cargo xtask stage-dlls` |
| Check the machine | `cargo xtask doctor` |
| Build | `cargo build --workspace` |
| All tests | `cargo test --workspace` |
| One crate / one test | `cargo test -p couch-proto` / `cargo test -p couch-proto envelope::tests::roundtrip` |
| Lint (must be clean) | `cargo clippy --workspace --all-targets -- -D warnings` |
| Format | `cargo fmt --all` |
| Dependency licenses and advisories | `cargo deny check` |
| Capability probe (codecs, iroh, SDL3, ViGEm) | `cargo build -p couch-viewer`, then `cargo run -p couch-cli -- probe` |
| Desktop app | `cd apps/desktop`, then `npm run tauri dev` |
| Front-end type check | `cd apps/desktop`, then `npx tsc --noEmit` |
| Regenerate TypeScript bindings | `cargo test -p couch-engine` (writes `apps/desktop/src/bindings/`) |
| Regenerate FFmpeg FFI bindings (after an FFmpeg bump; needs LLVM) | `cargo xtask gen-ffmpeg-bindings` |

To run several instances with separate identities on one machine, set `COUCH_PROFILE=alice` before launching (letters, digits, `-`, `_`). Each profile's data lives in `%LOCALAPPDATA%\CouchCoApp\profiles\<name>`: `identity.key` and `rooms\*.room` (sealed with DPAPI), and `settings.json`.

**Rooms.**

| Task | Command |
|---|---|
| Rooms end to end: engines over loopback (join through the owner, wrong passwords and rate limits, ban, owner offline, catching up, simultaneous restart, leave and close, including with the others offline, invite expiry) | `cargo test -p couch-engine --test rooms` (~20 s) |
| Join through iroh's public relay (needs internet) | `cargo test -p couch-engine --test rooms -- --ignored` |
| The room state machine, including the simulated network and its property test | `cargo test -p couch-room` |

**Streaming and measurement.** Build both binaries in release first: `cargo build --release -p couch-cli -p couch-viewer`. `couch-cli` finds `couch-viewer` next to its own exe.

| Task | Command |
|---|---|
| Stream a window to a viewer | `couch-cli host --source window --title "<substring>"` prints `TICKET ...`; then `couch-cli view <ticket>` |
| Stream the test pattern | `couch-cli host --source testpattern --pattern-size 1280 720 --fps 30` |
| End-to-end latency on one machine (G1; `--screen` adds G1b) | `couch-cli latency-probe --secs 30 --relay none` |
| Rate-control gates G1 and G3–G5, run and judged | `couch-cli gates --quick` (4 min) or `couch-cli gates` (12 min). The `rate-control-gates` skill covers traces and reading a failure. |
| Relay only (G6) | add `--relay n0 --relay-only` to both `host` and `view` |
| Input: viewer may use keyboard and mouse | `host --allow-kbm` (the default is gamepad only); kbm reaches the game only while the captured window is in front, so testing it end to end needs a second machine |
| Virtual pad (needs ViGEmBus) | `couch-cli pad-test`, then watch it in `joy.cpl`. `--real-pad-ids` on `pad-test` or `host` gives it a real Xbox 360 controller's ids, which a viewer on this machine would capture and send back |
| Keyboard and mouse injection, checked on one machine | `couch-cli input-sink` |
| Input released when a session ends badly (killed, frozen, closed, host stopped) | `couch-cli release-check` (about 1 minute). The `input-safety-checks` skill says when to run it and what to ask the user to try |
| Audio datagrams behind a keyframe (timing test) | `cargo test --release -p couch-net --features netem --test datagrams -- --ignored --nocapture` |
| Audio latency (gate A1) and click to playback | `couch-cli latency-probe --secs 30 --relay none --audio` (it clicks through the speakers). Add `--netem "delay=10ms,loss=1%"` for the loss check. The `audio-checks` skill covers reading the results and what to ask the user to listen for |
| Which sound `host` streams | `--audio auto` (default: the game's process tree for a window, everything but us for the monitor, clicks for the test pattern), or `game`, `system`, `tone`, `off` |

`--netem "<profile>"` (on `host`, `view` and `latency-probe`) replaces IP and relays with a loopback test transport that impairs the sending direction, e.g. `delay=20ms,loss=1%` or `rate=25mbit,at10s=rate:5mbit`. Without `--netem-viewer`, both sides get the same profile.

## Architecture in brief

**Main process.** The Tauri app. Its Rust side links `couch-engine`, which owns:
- the iroh endpoint;
- room and session state;
- on a host: capture, a per-viewer encoder and transport, and input injection.

**Viewer.** `couch-viewer` is a separate child process: SDL3 window, decode, render, audio, input capture, HUD.
- It does no networking. The main process feeds it encoded frames over stdin and reads input and timing from its stdout (`couch_proto::ipc`).
- **Never write to stdout in `couch-viewer`**; `clippy::print_stdout` is denied there. Logs go to stderr.

**Transport.** One iroh QUIC connection per pair of room members, carrying:
- control on a bidirectional stream, framed as `u32 len | u16 type | postcard`;
- video on one unidirectional stream per *epoch* (a keyframe plus the frames that depend on it);
- audio, gamepad, mouse motion and receiver reports as datagrams;
- keyboard and mouse buttons on a reliable stream.

**UI to engine.** One Tauri command, `engine_command(Command) -> Response`, plus one event, `engine-event`, carrying an `Event`. The TypeScript types are generated by ts-rs.

**Crate boundaries.** Keep these intact:
- `couch-proto`: wire and IPC types. Depends on no other internal crate.
- `couch-media`, `couch-input`, `couch-room`: pure logic, with no OS APIs, FFmpeg or networking.
  - `couch-room` is sans-IO: `(state, input, now) -> effects`, with no clocks, threads or randomness inside (invite tokens and wall-clock seconds come in as arguments). Its Ed25519 and SHA-256 are pure Rust.
- `couch-codec-ffmpeg`: the only crate that links FFmpeg (video).
- `couch-codec-opus`: the only crate that links libopus. FFmpeg's libopus wrapper can't do loss concealment, so audio doesn't go through FFmpeg.
- `couch-platform-win`: the only crate that calls Windows APIs. It compiles to nothing elsewhere (`#![cfg(windows)]`). Before writing unsafe Windows, COM or SDL code, read the `windows-ffi-pitfalls` skill.
- `couch-engine`: knows nothing about Tauri. `apps/desktop/src-tauri` stays a thin adapter. Its room actor (`rooms.rs`) is the only owner of room state; other tasks report to it over a channel.
- `couch-testkit`: fakes; dev-dependency only.

## Rules that aren't obvious from the code

**Protocol changes**
- Released control payloads are frozen. Add a new message type instead of changing fields; decoders reject trailing bytes.
- Type ids with bit `0x8000` set are must-understand.
- Update `docs/protocol.md` with every wire change. The `protocol-change` skill has the full checklist.

**Safety invariants.** Remote input is remote control of someone's PC. Never weaken these:
- Guests default to gamepad only (`Permissions::GUEST_DEFAULT`).
- Keyboard and mouse are injected only while the game window is in the foreground.
- The Windows key and its combinations, Alt+Tab, Ctrl+Esc, Ctrl+Shift+Esc and the gamepad Guide button are never forwarded.
- Every handoff, disconnect and teardown releases held keys and sends `PadState::NEUTRAL`.
- Input with a stale `SeatEpoch` is dropped.
- Remote input reaches `KbmInjector` and `VirtualPad` only through `couch_input::router::SeatRouter`, which enforces all of the above. Its tests are the guard; don't weaken them. After touching input or teardown code, follow the `input-safety-checks` skill.
- Peer data is size-limited: 64 KiB control frames, 16 MiB IPC frames.

**Rooms** ([ADR 0006](docs/adr/0006-rooms-without-a-server.md), `docs/protocol.md` Rooms and Admission)
- **Only the owner lets people in.** The owner signs the roster; members accept connections only from ids on it (iroh authenticates the ids), and only the owner admits newcomers, with an invite it made and the password. Never add a way for a non-owner to admit, or to put anyone on the roster.
- **One writer per fact, signed.** A member signs its own record; only the owner signs the policy (roster, bans). Room facts from peers enter only through `couch_room::room`, which checks signatures (`couch_room::signing`). Never accept or act on an unsigned room fact, nor on a peer's claim about someone else (a `banned` close code from a peer proves nothing; the owner's signed policy does). A record counts only within its member's current stay (its `since` matches the roster entry).
- **The roster is shown only to those it concerns.** A refusal carries the owner's policy only for a former member; a stranger gets a bare close. The engine saves room state before sending anything that came from it, so a signed version is never reused after a crash.
- **The admission composition is ours and security-critical** (`couch_net::admission`): SPAKE2, HKDF, HMAC and the TLS exporter. Changing any label, input or order changes what is proven, so update `docs/protocol.md` and ADR 0006 with it. It needs an outside review before any public release.
- **Secrets never reach logs.** `InviteToken`, `PasswordKey`, and the password and invite in `Command` print as `..`; keep it so for anything new. Only the owner stores the password key, never the password.
- After touching rooms, admission, the engine's room actor or `couch_net::control`, follow the `room-security-checks` skill.

**Licensing.** The app's license is undecided.
- Only permissive crates are allowed (`deny.toml`).
- FFmpeg must stay the LGPL build, dynamically linked.
- Sunshine, Moonlight and RustDesk are GPL/AGPL: read them for ideas, never copy code.

**Native dependencies and pins**
- FFmpeg and SDL3 are pinned by URL and SHA-256 in `xtask/src/deps.rs`.
- libopus is compiled from source by the `opus` crate, which needs CMake (`doctor` checks). Its CMake build fails past Windows' 260-character path limit, so keep `target/` at a short path.
- The FFmpeg FFI bindings in `crates/couch-codec-ffmpeg/bindings/` are generated and committed. rusty_ffmpeg's build-time bindgen 0.71 produces fieldless structs with libclang 22 or later. Regenerate the bindings whenever the FFmpeg pin changes; `doctor` flags stale ones.
- Exact pins: `iroh` and `sdl3` (`=`). `windows` stays on 0.62 to match `windows-capture`'s COM types.

**Relays.** `RelayChoice::N0Public` (iroh's public relays) is for development only. Production uses only our own iroh-relay (`RelayChoice::Custom`). Tests use `RelayChoice::Disabled`.

**Congestion control lives in the rate controller** (`couch_media::ratecontrol`, ADR 0005). QUIC runs `couch_net::congestion::MediaCongestion`, which deliberately ignores isolated losses. A change to either can collapse the bitrate under random loss or stall recovery after an outage. Run `couch-cli gates` after touching them, or the video transport or encoder bitrate handling (the `rate-control-gates` skill has the procedure); `docs/m1-results.md` has the reference numbers.

**Tests.** `cargo test --workspace` must pass without a GPU, a gamepad driver or internet access. Tests that need those are opt-in (`#[ignore]` or a `gpu-tests` feature).
