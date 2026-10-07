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
| Desktop app | `cd apps/desktop`, then `npm run tauri dev` (builds `couch-viewer` first, which watching needs) |
| The viewer's HUD by eye, without a session | `cargo run -p couch-viewer -- --hud-preview watching` (or `asking`, `controlling`) |
| Front-end type check | `cd apps/desktop`, then `npx tsc --noEmit` |
| Regenerate TypeScript bindings | `cargo test -p couch-engine` (writes `apps/desktop/src/bindings/`) |
| Regenerate FFmpeg FFI bindings (after an FFmpeg bump; needs LLVM) | `cargo xtask gen-ffmpeg-bindings` |

To run several instances with separate identities on one machine, set `COUCH_PROFILE=alice` before launching (letters, digits, `-`, `_`; lowercased, as Windows folder names ignore case). Each profile's data lives in `%LOCALAPPDATA%\CouchCoApp\profiles\<name>`: `identity.key` and `rooms\*.room` (sealed with DPAPI), `settings.json`, and `webview\` (the UI's own storage, such as the room order).

The desktop app with several profiles: start `npm run tauri dev` (the default profile), then `.\scripts\profiles.ps1` starts alice, bob and carol from the same debug build, logging to `desktop.log` in their profile folders. Each profile runs once: a second launch hands its arguments to the running one and exits before going online, and `Engine::start` also holds `engine.lock` in the profile folder (never run two engines on one identity). `couchco://` invite links open in the default profile. Debug builds register the scheme for the current Windows user, pointing at the debug exe; the installer will do it in M10. The `app-manual-run` skill covers a manual run: where each profile's logs are, what one PC can't show (games and keyboard and mouse), and what to ask the user to try.

**Rooms.**

| Task | Command |
|---|---|
| Rooms end to end: engines over loopback (join through the owner, wrong passwords and rate limits, ban, owner offline, catching up, simultaneous restart, leave and close, including with the others offline, invite expiry) | `cargo test -p couch-engine --test rooms` (~20 s) |
| Sessions end to end: three engines with a fake backend (fake capture, encoder, pad and viewer window): going live, two viewers, a full session, asking, declining, passing and giving back control, the panic key, a viewer closing its window, a ban, the host leaving, the window closing, stopping, shutting down mid-game; the game swapping its window or exiting, a crashed stream window started again, watching resuming after a dropped connection (`Engine::drop_connection`); seats: two guests on two pads, moving seats, a keyboard seat, the game leaving the front, the setup remembered per program; the host on a seat (a fake controller and hiding): passed, taken back, kept through the panic key, moved, its rumble, hiding that fails | `cargo test -p couch-engine --test sessions` (~4 s) |
| Join through iroh's public relay (needs internet) | `cargo test -p couch-engine --test rooms -- --ignored` |
| The room state machine, including the simulated network and its property test | `cargo test -p couch-room` |

**Streaming and measurement.** Build both binaries in release first: `cargo build --release -p couch-cli -p couch-viewer`. `couch-cli` finds `couch-viewer` next to its own exe.

| Task | Command |
|---|---|
| Stream a window to viewers | `couch-cli host --source window --title "<substring>"` prints `TICKET ...`; then `couch-cli view <ticket>`, up to `--max-viewers` (4) at once, each with its own encoder. The first viewer gets the seat. `--upload-mbps` caps the total |
| Stream the test pattern | `couch-cli host --source testpattern --pattern-size 1280 720 --fps 30` |
| End-to-end latency on one machine (G1; `--screen` adds G1b) | `couch-cli latency-probe --secs 30 --relay none` |
| Rate-control gates G1, G3–G5 and G7 (one viewer's bad path leaves another's stream alone), run and judged | `couch-cli gates --quick` (6 min) or `couch-cli gates` (14 min). `latency-probe --viewers 2 --impair-first "rate=5mbit"` runs G7's setup by hand. The `rate-control-gates` skill covers traces and reading a failure. |
| Many viewers of one host on this PC (fps, latency, bitrate and encode time per viewer) | `.\scripts\viewers.ps1` (4) or `.\scripts\viewers.ps1 -Viewers 7`, after the release build |
| The app for a second PC (release build, stream window and DLLs zipped into `target\guest`) | `.\scripts\pack-guest.ps1`, then `npx --yes http-server target\guest -p 8000` |
| What an XInput game sees on the host (each slot's buttons and sticks, live): tells a stream problem from a game choosing another controller | `.\scripts\xinput-watch.ps1` |
| Relay only (G6) | add `--relay n0 --relay-only` to both `host` and `view` |
| Input: viewer may use keyboard and mouse | `host --allow-kbm` (the default is gamepad only); kbm reaches the game only while the captured window is in front, so testing it end to end needs a second machine. `host --pads N` (up to 4) plugs a pad per seat and gives each viewer the next free seat as it connects (only the first gets kbm) |
| Virtual pad (needs ViGEmBus) | `couch-cli pad-test` (`--count N` for several: pad N presses its buttons N steps ahead, and it prints how many game controllers Windows lists), then watch it in `joy.cpl`. Without `--real-pad-ids` it uses made-up ids (1209:0001), and games and Steam treat it as a generic joystick (only the D-pad works); the app always uses the real Xbox 360 ids. While a host on this PC has a pad plugged in, viewers here skip every Xbox 360 pad (`couch_platform_win::virtual_pads`) |
| The host's own controller on a seat (P2; hiding needs HidHide installed) | `couch-cli host-pad` lists this PC's controllers (never ours) and HidHide's state; `host-pad --feed N` hides controller N and drives a virtual pad from it (`--no-hide` to skip hiding). `couch-cli host --host-pad N` plays seat 0 with it (`--show-host-pad` to skip hiding). After hiding, reconnect the controller once: programs that had it open keep it until then |
| Keyboard and mouse injection through the seat router, checked on one machine (blocked combinations included) | `couch-cli input-sink` |
| Input released when a session ends badly (killed, frozen, closed, host stopped) | `couch-cli release-check` (about 1 minute). The `input-safety-checks` skill says when to run it and what to ask the user to try |
| A game swapping its window, exiting or closing it; the stream window killed; a host killed, and killed holding a key | `couch-cli lifecycle-check` (about 1 minute; `--only kill-host,...` for some). `couch-cli fake-game` is the stand-in game it uses (`swap`, `close`, `exit` on stdin) |
| Audio datagrams behind a keyframe (timing test) | `cargo test --release -p couch-net --features netem --test datagrams -- --ignored --nocapture` |
| Audio latency (gate A1) and click to playback | `couch-cli latency-probe --secs 30 --relay none --audio` (it clicks through the speakers). Add `--netem "delay=10ms,loss=1%"` for the loss check. The `audio-checks` skill covers reading the results and what to ask the user to listen for |
| Which sound `host` streams | `--audio auto` (default: the game's process tree for a window, everything but us for the monitor, clicks for the test pattern), or `game`, `system`, `tone`, `off` |

`--netem "<profile>"` (on `host`, `view` and `latency-probe`) replaces IP and relays with a loopback test transport that impairs the sending direction, e.g. `delay=20ms,loss=1%` or `rate=25mbit,at10s=rate:5mbit`. Without `--netem-viewer`, both sides get the same profile.

## Architecture in brief

**Main process.** The Tauri app. Its Rust side links `couch-engine`, which owns:
- the iroh endpoint;
- room and session state;
- room sessions (`rooms::sessions`): the one we stream, the one we watch;
- the streaming pipelines, `couch_engine::pipeline` (shared with `couch-cli`): on a host, one capture, an encoder and transport per viewer, one sound capture, and the input task with the seat router and the injectors; on a viewer, `couch-viewer` fed over its pipe. What they need from the machine comes through `pipeline::Backend`, faked in tests.

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
- `couch-gamepad`: physical gamepads through SDL3, for the viewer and the host's own controller. With `couch-viewer`, the only crates that use SDL3. The `sdl3` crate takes the first thread to start SDL as its main thread, so in the engine's process SDL lives only on `couch_gamepad`'s reader thread; never start SDL anywhere else there.
- `couch-engine`: knows nothing about Tauri. `apps/desktop/src-tauri` stays a thin adapter. Its room actor (`rooms.rs`) is the only owner of room state; other tasks report to it over a channel. **Input never goes through the actor**: it runs from the connection through the pipeline's input task to the injectors.
- `couch-net`: each connection has one datagram reader (`datagrams`) and one unidirectional-stream acceptor (`streams`). Subscribe to them by tag or kind; never call `accept_uni` or `read_datagram` yourself, or two readers steal each other's traffic.
- `couch-testkit`: fakes; dev-dependency only.

## Rules that aren't obvious from the code

**Protocol changes**
- Released control payloads are frozen. Add a new message type instead of changing fields; decoders reject trailing bytes.
- Type ids with bit `0x8000` set are must-understand.
- Update `docs/protocol.md` with every wire change. The `protocol-change` skill has the full checklist.

**Safety invariants.** Remote input is remote control of someone's PC. Never weaken these:
- Guests default to gamepad only (`Permissions::GUEST_DEFAULT`).
- Keyboard and mouse are injected only while the game window is in the foreground, and only one seat's holder at a time has them (a seat's kind sets its permissions; `SeatRouter::grant` refuses a second).
- The Windows key and its combinations, Alt+Tab, Alt+Esc, Ctrl+Esc, Ctrl+Shift+Esc, Alt+F4 and the gamepad Guide button are never forwarded.
- Every handoff, disconnect and teardown releases held keys and sends `PadState::NEUTRAL`.
- Input with a stale `SeatEpoch` is dropped.
- Remote input reaches `KbmInjector` and `VirtualPad` only through `couch_input::router::SeatRouter`, which enforces all of the above. Its tests are the guard; don't weaken them. After touching input or teardown code, follow the `input-safety-checks` skill.
- Peer data is size-limited: 64 KiB control frames, 16 MiB IPC frames.

**Rooms** ([ADR 0006](docs/adr/0006-rooms-without-a-server.md), `docs/protocol.md` Rooms and Admission)
- **Only the owner lets people in.** The owner signs the roster; members accept connections only from ids on it (iroh authenticates the ids), and only the owner admits newcomers, with an invite it made and the password. Never add a way for a non-owner to admit, or to put anyone on the roster.
- **One writer per fact, signed.** A member signs its own record; only the owner signs the policy (roster, bans). Room facts from peers enter only through `couch_room::room`, which checks signatures (`couch_room::signing`). Never accept or act on an unsigned room fact, nor on a peer's claim about someone else (a `banned` close code from a peer proves nothing; the owner's signed policy does). A record counts only within its member's current stay (its `since` matches the roster entry).
- **The roster is shown only to those it concerns.** A refusal carries the owner's policy only for a former member; a stranger gets a bare close. The engine saves room state before sending anything that came from it, so a signed version is never reused after a crash.
- **The admission composition is ours and security-critical** (`couch_net::admission`): SPAKE2, HKDF, HMAC and the TLS exporter. Changing any label, input or order changes what is proven, so update `docs/protocol.md` and ADR 0006 with it. It needs an outside review before any public release.
- **An opened link never joins by itself.** Any web page can open a `couchco://` link. It only fills in the join screen (through `Engine::open_link`); joining still needs the password and a click.
- **Secrets never reach logs.** `InviteToken`, `PasswordKey`, the passwords and invites in `Command`, and the links in `Response` print as `..`; keep it so for anything new. Only the owner stores the password key, never the password.
- After touching rooms, admission, the engine's room actor or `couch_net::control`, follow the `room-security-checks` skill.

**Sessions** (`docs/protocol.md` Sessions)
- **The host is the only writer** of its session's facts, sent straight to each member, never relayed. A member acts on a session message only from that session's host, on the current connection, while the host is on the roster. Never let one member speak for another's session.
- **The seat router alone decides who plays.** `couch_room::session` asks it and mirrors its `Granted`/`Revoked` per seat; never inject or track a holder elsewhere.
- **One seat per viewer.** A grant is always preceded by revoking that viewer from every other seat, on the same ordered channel to the router (`HostSession::grant`); keep that order.
- **The host on a seat** (P2). The host's own controller reaches a pad only through the seat router, as the host's holder with the pad permission only (the input task's local channel, `Inputs::local_input`; `couch-cli host-pad --feed` goes through a router too); never feed it to a pad directly. Only the host's last seat comes back to them, and only through a claim, which the router grants only if the seat is free and the host holds none: a plain grant there could take a seat a viewer was just passed. The panic key doesn't pause the host's own controller.
- **A hidden controller is always shown again** (`couch_platform_win::hidhide`): the guard drops after the pads are unplugged, and `restore_stale` (in `Backend::release_left_input`) undoes what a killed host left. Keep both.
- **Ending releases first:** input, then `SessionEnded` and `SESSION_END`, then encoders and capture, then the pad. A viewer that goes (leaves, closes its window, loses the connection, is banned) is released at once through `SeatRouter::disconnected`.
- **A seat never survives a connection.** A viewer that reconnects joins again as a new viewer and must be given control again; nothing restores a seat without the host.
- **Every key or button `SendInputInjector` presses is recorded** (`couch_platform_win::held_input`) so a crash can't leave it held. Inject keyboard and mouse only through it, and keep `release_left_input` at the start of anything that hosts.
- **Shortcuts live in one table**, `couch_input::shortcuts`: the viewer, the panic-key hook and the app's list all read it. Add or change a shortcut there, never as a key constant elsewhere (the user wants them editable later). The panic-key hook must keep ignoring injected keys.
- After touching sessions or the pipelines, follow both the `room-security-checks` and the `input-safety-checks` skills; after touching the host pipeline's video path, the `rate-control-gates` skill too.

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
