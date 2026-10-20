# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Couch Co-App is a Windows-first desktop app for remote "same couch" gaming. A member of a password-protected room streams a game; the others watch, and the host hands control to one of them.

It is a Rust workspace with a Tauri 2 shell (React + TypeScript). Where to read more:
- `docs/architecture.md`: start here. It has the milestone table: what each milestone (M) and polish step (P) did, and what is planned.
- `docs/protocol.md`: the wire format.
- `docs/adr/`: the decisions behind the big choices.
- `docs/development.md` (setting up, running, testing) and `docs/self-hosting.md` (an installer and a relay for your friends): the human guides, which the README links. Keep them in step with the commands here. The README's top is the author's own note; leave it to them.

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
| Dependency licenses and advisories | `cargo deny check` (and `cargo xtask notices` for the licence texts) |
| Capability probe (codecs, iroh, SDL3, ViGEm) | `cargo build -p couch-viewer`, then `cargo run -p couch-cli -- probe` |
| Desktop app | `cd apps/desktop`, then `npm run tauri dev` (builds `couch-viewer` first, which watching needs) |
| The viewer's HUD by eye, without a session | `cargo run -p couch-viewer -- --hud-preview watching` (or `asking`, `controlling`) |
| The viewer's maximum frame latency, for measuring on another GPU (M9a kept 1; NVIDIA showed no difference) | `$env:COUCH_VIEWER_FRAME_LATENCY = 2` (1 to 3) before `latency-probe` or the app; the stats line shows the value |
| Front-end type check | `cd apps/desktop`, then `npx tsc --noEmit` |
| Regenerate TypeScript bindings | `cargo test -p couch-engine` (writes `apps/desktop/src/bindings/`) |
| Regenerate FFmpeg FFI bindings (after an FFmpeg bump; needs LLVM) | `cargo xtask gen-ffmpeg-bindings` |
| Bump the app's version (every PR that changes the app; "Versions" below) | `cargo xtask bump patch` or `cargo xtask bump minor` |

To run several instances with separate identities on one machine, set `COUCH_PROFILE=alice` before launching (letters, digits, `-`, `_`; lowercased, as Windows folder names ignore case). Each profile's data lives in `%LOCALAPPDATA%\CouchCoApp\profiles\<name>`: `identity.key` and `rooms\*.room` (sealed with DPAPI), `settings.json` (the name), `shortcuts.json`, `network.json`, `audio.json` (the stream window's volume, P6) and `host-audio.json` (the host's stream level, P7; written by the engine, not the room actor), and `webview\` (the UI's own storage, such as the room order and the last session setup).

The desktop app with several profiles: start `npm run tauri dev` (the default profile), then `.\scripts\profiles.ps1` starts alice, bob and carol from the same debug build, logging to `desktop.log` in their profile folders. Each profile runs once: a second launch hands its arguments to the running one and exits before going online, and `Engine::start` also holds `engine.lock` in the profile folder (never run two engines on one identity). `couchco://` invite links open in the default profile. The installer registers the scheme for the installed app (M10a). A debug build registers it for itself (for the current Windows user, which outranks an install for everyone) only with `COUCH_DEV_LINKS=1` set (P5). Without it, a start finding this user's registration pointing at itself removes that registration only (`couch_platform_win::links`; never Tauri's `unregister`, which goes for the installer's machine-wide key first), so an app installed for everyone gets them back. An app installed for just this user writes the same per-user key, so a `COUCH_DEV_LINKS=1` start overwrites it: reinstall that app afterwards. The `app-manual-run` skill covers a manual run: where each profile's logs are, what one PC can't show (games and keyboard and mouse), and what to ask the user to try.

**Rooms.**

| Task | Command |
|---|---|
| Rooms end to end: engines over loopback (join through the owner, wrong passwords and rate limits, ban, owner offline, catching up, simultaneous restart, leave and close, including with the others offline, invite expiry) | `cargo test -p couch-engine --test rooms` (~20 s) |
| Sessions end to end: three engines with a fake backend (fake capture, encoder, pad and viewer window): starting a session and a stream, two viewers, a full session, asking, declining, passing and giving back control, the panic key, answering with the host's keys (M8), a viewer closing its window, a ban, the host leaving, the window closing, ending, shutting down mid-game; the game swapping its window or exiting (the stream ends, the session goes on), a crashed stream window started again, watching resuming after a dropped connection (`Engine::drop_connection`); sessions before streams (M19): nothing streamed at first, a stream stopped and started again with the viewers staying, a guest's input released and held off between streams, the host's controller playing between them; seats: two guests on two pads, moving seats, a keyboard seat, the game leaving the front, the input setup's seats; the host on a seat (a fake controller and hiding): passed, taken back (left empty), played again, kept through the panic key, moved, its rumble, hiding that fails; HEVC per viewer and its fallbacks (M12c); several seats for one guest (M18): two controllers on two pads beside the keyboard seat, a seat added or moving by the guest's controllers, giving back one seat, an unplugged controller, and the panic key, a closed window, a stream stop and the session's end with several seats held | `cargo test -p couch-engine --test sessions` (~5 s) |
| Join through iroh's public relay (needs internet) | `cargo test -p couch-engine --test rooms -- --ignored` |
| The room state machine, including the simulated network and its property test | `cargo test -p couch-room` |

**Streaming and measurement.** Build both binaries in release first: `cargo build --release -p couch-cli -p couch-viewer`. `couch-cli` finds `couch-viewer` next to its own exe.

| Task | Command |
|---|---|
| Stream a window to viewers | `couch-cli host --source window --title "<substring>"` prints `TICKET ...`; then `couch-cli view <ticket>`, up to `--max-viewers` (4) at once, each with its own encoder. The first viewer gets the seat. `--upload-mbps` caps the total. A session with one stream in it, which ends the host when it ends; `--restart-stream-after <secs>` stops the stream and starts it again once (M19), the viewers staying |
| Stream the test pattern | `couch-cli host --source testpattern --pattern-size 1280 720 --fps 30` |
| End-to-end latency on one machine (G1; `--screen` adds G1b) | `couch-cli latency-probe --secs 30 --relay none`. `--cpu-load` adds busy threads on every core, a stand-in game, and reports their work (the game's share); `--cpus 16-19` confines the whole run to those CPUs, a weaker PC (M9d). Under load, the latency clock (the stand-in game) renders late too, which shows in clock to present. `--clock-fps 61` caps the clock like a game at that rate, against the stream's `--fps` |
| Rate-control gates G1, G3–G5 and G7 (one viewer's bad path leaves another's stream alone), run and judged | `couch-cli gates --quick` (6 min) or `couch-cli gates` (14 min). They run in H.264 (they set `COUCH_HOST_CODEC=h264` for their host): HEVC needs so little on the clock that the throttles never touch it (M12c). `latency-probe --viewers 2 --impair-first "rate=5mbit"` runs G7's setup by hand. The `rate-control-gates` skill covers traces and reading a failure. |
| Many viewers of one host on this PC (fps, latency, bitrate and encode time per viewer) | `.\scripts\viewers.ps1` (4) or `.\scripts\viewers.ps1 -Viewers 7`, after the release build. The test pattern is made in memory; `-Screen` captures the primary screen instead, so frames take the GPU path (M9b), and `-Screen -CopyThroughMemory` the path from before, to compare. `-CopyFrames` makes the viewers copy decoded frames through memory, as before M9c. `-Fps 120` sets the stream's rate (60 by default). With `-Screen` alone, only the stream windows change on the captured screen, so it changes only after the host sends: a loop no real host has (M9e). `-Clock` puts the latency clock below the windows, a busy screen independent of the stream (`-ClockFps 61` a game near its rate). `-Trace` also writes the host's pacing trace and each viewer's presented frames to the run's folder. It also prints the stream windows' CPU use |
| Frames through memory on the host, as before M9b (for measuring) | `$env:COUCH_HOST_ZERO_COPY = 0` before `host`, `latency-probe` or the app |
| The capture without pacing, as before M9d (Windows' one-frame interval, which loses frames when the screen's refresh isn't a multiple of the rate) | `$env:COUCH_HOST_PACING = 0` before `host`, `latency-probe` or the app |
| The capture's frame pool with one buffer, as before M9f (two let Windows write the next frame while the GPU still copies the last: under a GPU-bound game, a third more frames) | `$env:COUCH_CAPTURE_BUFFERS = 1` (1 to 4; 2 by default) before `host`, `latency-probe` or the app |
| Each captured frame's timings in the capture callback (M9f) | `$env:COUCH_CAPTURE_TRACE = '<csv>'` appends `entry_us,windows_us,lock_us,copy_us,flush_us,total_us,outcome` per GPU frame: how late Windows delivered it (`entry_us - windows_us`), and whether our copy waited on the device's lock |
| A GPU-bound stand-in game (M9f) | `latency-probe --gpu-load 130`: the clock draws that many blended rectangles into a 4K texture each frame, without vsync, about 110 fps on this PC; the summary shows `clock_fps`. It loads the GPU but didn't reproduce Witcher 3's capture loss, so a real game is still the test |
| What pacing did with each captured frame (M9e) | `couch-cli host --pacing-trace <csv>` (or `latency-probe --pacing-trace`): one row per frame (`at_us,captured_us,event`: `sent`, `held`, or `released` after its wait; a `held` row followed by another frame was replaced). Unpaced, every frame is `sent`, which still shows how often the capture delivers |
| H.264 for every viewer, as before M12c (HEVC goes automatically to viewers that decode it, from an NVIDIA host) | `$env:COUCH_HOST_CODEC = 'h264'` before `host`, `latency-probe`, `viewers.ps1` or the host's app; `$env:COUCH_VIEWER_HEVC = 0` on a watcher's app makes it ask for H.264 only (with `couch-cli host`, whose viewers don't say what they decode, set either on the host). With frames in memory there's no HEVC. `gates` sets H.264 itself. The stream window's decoder line names the codec |
| Picture quality, H.264 against HEVC at one bitrate (M12c) | `couch-cli capture-test --window "<title>" --codec hevc --bitrate-mbps 10 --psnr` (or `--monitor`; `--codec h264` to compare): the luma PSNR and SSIM of each decoded frame against the captured one. A moving test picture: `third_party\ffmpeg\bin\ffplay -window_title couch-psnr -f lavfi -i mandelbrot=s=1280x720:rate=60` |
| NVENC's quality settings, for measuring (P8 kept none: HEVC p4 broke 7 viewers, H.264 multipass cost each 2.5 ms) | `$env:COUCH_NVENC_PRESET = 'p4'` (p1 to p7), `$env:COUCH_NVENC_SPATIAL_AQ = 1` (with `COUCH_NVENC_AQ_STRENGTH`, 1 to 15), `$env:COUCH_NVENC_MULTIPASS = 1` (0 to 2) before `capture-test`, `host`, `latency-probe`, `viewers.ps1` or the app: laid over whichever option set opens, and named in the encoder's caps line ("measuring …") |
| The stream window's scaler, as before P8 (bicubic on luma by default, sharper for text at no measurable cost) | `$env:COUCH_VIEWER_SCALER = 'bilinear'` before `latency-probe`, `view` or the watcher's app; the stats panel's decoder line names it |
| One encoder by FFmpeg's name, taking frames in memory: another machine's path here, e.g. the software fallback | `$env:COUCH_HOST_ENCODER = 'libopenh264'` before `host`, `latency-probe` or the app |
| Decoded frames through memory on the viewer, as before M9c (for measuring) | `$env:COUCH_VIEWER_ZERO_COPY = 0` (`1` turns zero-copy on for a GPU other than NVIDIA's); `$env:WGPU_BACKEND = 'dx12'` (or `vulkan`) picks wgpu's backend alone. The stats panel's decoder and GPU lines show which ran. Set these for the **watcher's** app, which opens the stream window (`$env:COUCH_VIEWER_ZERO_COPY = '0'; .\scripts\profiles.ps1 alice`) |
| wgpu's validation in the stream window (the D3D12 debug layer on DX12); off by default, debug builds included, since it slows every frame | `$env:WGPU_VALIDATION = 1` before `latency-probe` or the watcher's app |
| D3D12VA's frames brought into wgpu against D3D11VA's copies (needs NVIDIA; opens no window) | `cargo test -p couch-viewer zero_copy -- --ignored --nocapture` |
| NVENC fed D3D11 textures against frames from memory (needs NVIDIA) | `cargo test -p couch-codec-ffmpeg --features gpu-tests --test gpu_input`; the texture pool alone: `cargo test -p couch-platform-win gpu_frames -- --ignored` |
| The installer, for this PC or another (M10a: the app, stream window, DLLs and the two drivers, offered after install; for just you or everyone, as the installer asks) | `.\scripts\build-installer.ps1` builds a release installer with the relay in the private `release.toml` (M7, below); it needs a clean, committed tree. `-TestRelay` builds a `-test` installer on iroh's public relays instead, from any tree. Copy invite links to the invite page in `release.toml`'s `invite_page` (ours: `https://marcusvilete.github.io/couch-coapp/j/`, published by `.github/workflows/invite-page.yml`); `-InvitePage <address>` sets it for one build, test builds included, and `-NoInvitePage` gives `couchco://` links; the build says which. Uninstalling with "Delete the application data" ticked also asks to delete `%LOCALAPPDATA%\CouchCoApp`; with a crash marker waiting there, it advises starting the app once first (`installer-hooks.nsh`). Installers go to friends only (ADR 0009). Run it without redirecting its stderr, then `npx --yes http-server target\installer -p 8000`. It fetches and stages the pinned dependencies and driver installers, checks the version is the same in `Cargo.toml`, `tauri.conf.json` and `apps/desktop/package.json`, and builds with `tauri.installer.conf.json` (the bundled files and the driver hooks, `installer-hooks.nsh`), kept out of `tauri.conf.json` so development builds aren't touched. Bump the version with each installer handed out. An installed app logs to `desktop.log` in the profile folder. The `release-installer` skill has the whole procedure: when to bump, building from a merged clean main, checking what it carries, handing it out |
| What an XInput game sees on the host (each slot's buttons and sticks, live): tells a stream problem from a game choosing another controller | `.\scripts\xinput-watch.ps1` |
| Relay only (G6) | add `--relay n0 --relay-only` to both `host` and `view`; through our relay, `--relay http://localhost:3340 --relay-token <token> --relay-only` |
| Our relay on this PC (M7: the official iroh-relay, production's limits, the token) | `cargo xtask relay-tool` once (builds it into `target\tools`, the version of our iroh pin), then `.\scripts\relay.ps1` (`http://localhost:3340`; `-Lan` for your other PC, `-Open` without the token). The deploy kit and its README are in `deploy/relay/` |
| The third-party notices every installer carries (M10b) | `cargo xtask notices` writes `target\THIRD-PARTY-NOTICES.txt`: the crates both binaries link (cargo-about, `about.toml`), the front end's npm packages, FFmpeg (with its configure line), SDL3 and the drivers. It fails if a crate has no licence text. `stage-dlls` copies it and `LICENSE` next to the build; Settings → About shows it (`Command::GetNotices`). `build-installer.ps1` runs it, refuses a dirty tree for release builds, and puts FFmpeg's pinned source and build scripts (`deps::SOURCES`) beside the setup file, with `SOURCES.txt` |
| The private release configuration (our relay's URL and token, the invite page) | `cargo xtask release-config` creates `%USERPROFILE%\.couch-coapp\release.toml` (never in the repository; `deploy/release.example.toml` shows its shape) |
| Our relay end to end, offline (in-process iroh-relay with a token: members meet through it alone, a client without the token is turned away) | `cargo test -p couch-engine --test relay`; the rooms and sessions scenarios each have one run through it too |
| The invite page's reading of an invite | `node deploy/invite-page/check.js`; try the page with `npx --yes http-server deploy/invite-page -p 8080` (`deploy/invite-page/README.md`) |
| Input: viewer may use keyboard and mouse | `host --allow-kbm` (the default is gamepad only); kbm reaches the game only while the captured window is in front, so testing it end to end needs a second machine. `host --pads N` (up to 4) plugs a pad per seat and gives each viewer the next free seat as it connects (only the first gets kbm) |
| Virtual pad (needs ViGEmBus) | `couch-cli pad-test` (`--count N` for several: pad N presses its buttons N steps ahead, and it prints how many game controllers Windows lists), then watch it in `joy.cpl`. Without `--real-pad-ids` it uses made-up ids (1209:0001), and games and Steam treat it as a generic joystick (only the D-pad works); the app always uses the real Xbox 360 ids. While a host on this PC has a pad plugged in, viewers here skip exactly its pads, found by their ViGEm serials (P9, `couch_platform_win::pad_origin`) |
| Where each gamepad comes from: physical, ours, another program's virtual pad (P9) | `couch-cli pad-origins`: each gamepad SDL finds, its device-tree chain and the origin the app gives it, and each Xbox driver interface with its player light (its XInput slot). `--ours N` plugs our pads first (marked as a host's), `--unmarked N` pads that leave no marker (another program's stand-in), `--hold <secs>` keeps them. Reads only |
| The host's own controller on a seat (P2; hiding needs HidHide installed) | `couch-cli host-pad` lists this PC's physical controllers (never a virtual pad, P9) and HidHide's state; `host-pad --feed N` hides controller N and drives a virtual pad from it (`--no-hide` to skip hiding). `couch-cli host --host-pad N` plays seat 0 with it (`--show-host-pad` to skip hiding). After hiding, reconnect the controller once: programs that had it open keep it until then |
| Keyboard and mouse injection through the seat router, checked on one machine (blocked combinations included) | `couch-cli input-sink` |
| Input released when a session ends badly (killed, frozen, closed, host stopped) | `couch-cli release-check` (about 1 minute). The `input-safety-checks` skill says when to run it and what to ask the user to try |
| A game swapping its window, exiting or closing it; the stream window killed; a host killed, and killed holding a key | `couch-cli lifecycle-check` (about 1 minute; `--only kill-host,...` for some). `couch-cli fake-game` is the stand-in game it uses (`swap`, `close`, `exit` on stdin) |
| Audio datagrams behind a keyframe (timing test) | `cargo test --release -p couch-net --features netem --test datagrams -- --ignored --nocapture` |
| Audio latency (gate A1) and click to playback | `couch-cli latency-probe --secs 30 --relay none --audio` (it clicks through the speakers). Add `--netem "delay=10ms,loss=1%"` for the loss check. The `audio-checks` skill covers reading the results and what to ask the user to listen for |
| Which sound `host` streams | `--audio auto` (default: the game's process tree for a window, everything but us for the monitor, clicks for the test pattern), or `game`, `system`, `tone`, `off`; `--audio-level <0-400>` sets how loud viewers hear it (P7; above 100 a boost, soft-limited within full scale). Windows' master volume doesn't reach the capture; the game's own volume and its mixer slider do |

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

**UI to engine.** One Tauri command, `engine_command(Command) -> Response`, plus one event, `engine-event`, carrying an `Event`. The TypeScript types are generated by ts-rs. A command's reply and the events reach the UI by different channels, in either order, so a room view carries `RoomView::revision` (one counter for every view the engine makes) and the UI keeps the newer of two views of a room (`lobby.ts`'s `putRoom` and `refreshRooms`); anything new that stores room views must do the same. The app's own commands (`engine_command`, `restart_app`) are listed in `src-tauri/build.rs` and granted to the main window only (`capabilities/default.json`): the host overlay's window may call none. A new command needs both.

**Crate boundaries.** Keep these intact:
- `couch-proto`: wire and IPC types. Depends on no other internal crate.
- `couch-media`, `couch-input`, `couch-room`: pure logic, with no OS APIs, FFmpeg or networking.
  - `couch-room` is sans-IO: `(state, input, now) -> effects`, with no clocks, threads or randomness inside (invite tokens and wall-clock seconds come in as arguments). Its Ed25519 and SHA-256 are pure Rust.
- `couch-codec-ffmpeg`: the only crate that links FFmpeg (video).
- `couch-codec-opus`: the only crate that links libopus. FFmpeg's libopus wrapper can't do loss concealment, so audio doesn't go through FFmpeg.
- `couch-platform-win`: the only crate that calls Windows APIs. It compiles to nothing elsewhere (`#![cfg(windows)]`). Before writing unsafe Windows, COM or SDL code, read the `windows-ffi-pitfalls` skill.
  - One exception: `couch-viewer`'s `zero_copy` (M9c) uses `windows`' Direct3D 12 types to hand COM pointers between FFmpeg and wgpu-hal, and reads a resource's description. Keep it to that.
- `couch-gamepad`: physical gamepads through SDL3, for the viewer and the host's own controller. With `couch-viewer`, the only crates that use SDL3. The `sdl3` crate takes the first thread to start SDL as its main thread, so in the engine's process SDL lives only on `couch_gamepad`'s reader thread; never start SDL anywhere else there.
- `couch-engine`: knows nothing about Tauri. `apps/desktop/src-tauri` stays a thin adapter. Its room actor (`rooms.rs`) is the only owner of room state; other tasks report to it over a channel. **Input never goes through the actor**: it runs from the connection through the pipeline's input task to the injectors.
- `couch-net`: each connection has one datagram reader (`datagrams`) and one unidirectional-stream acceptor (`streams`). Subscribe to them by tag or kind; never call `accept_uni` or `read_datagram` yourself, or two readers steal each other's traffic.
- `couch-testkit`: fakes; dev-dependency only.

## Rules that aren't obvious from the code

**Protocol changes**
- Payloads may change in place (nothing is frozen, ADR 0009); prefer a new message type when that's just as simple. Decoders reject trailing bytes.
- **Bump `couch_proto::PROTOCOL_MINOR` with every change to what members exchange** (M10a; permanent since [ADR 0009](docs/adr/0009-source-only-releases.md): there is no public binary release, so the wire format is never frozen). Builds of different revisions refuse each other right after `Hello` (close code `version`), and the app says who needs to update; without the bump, mismatched installs fail in confusing ways.
- **A new wire revision is a minor version** (`couch_proto::REVISIONS` names the version each one arrived in): add its row and `cargo xtask bump minor` in the same PR. The test `a_new_wire_revision_comes_with_a_minor_version` fails otherwise.
- Type ids with bit `0x8000` set are must-understand.

**Versions.** Every PR that changes the app bumps the version once, in that PR: `cargo xtask bump patch` for fixes and small features, `cargo xtask bump minor` for a milestone or a new wire revision. It sets the same version in `Cargo.toml`, `tauri.conf.json`, `apps/desktop/package.json` and our crates in `Cargo.lock`; then say what the version carries in `docs/architecture.md`'s version line ("Installing"). PRs of docs, skills, tests or tooling alone don't bump. If two open PRs both bump, the one merged second takes the next number. So `main` always carries a version not yet handed out, and an installer can be built from it with no PR of its own (the `release-installer` skill).
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
- **The admission composition is ours and security-critical** (`couch_net::admission`): SPAKE2, HKDF, HMAC and the TLS exporter. Changing any label, input or order changes what is proven, so update `docs/protocol.md` and ADR 0006 with it. An outside review is optional since ADR 0009, but worth having before encouraging strangers to rely on the app.
- **An opened link never joins by itself.** Any web page can open a `couchco://` link. It only fills in the join screen (through `Engine::open_link`); joining still needs the password and a click.
- **Secrets never reach logs.** `InviteToken`, `PasswordKey`, the passwords and invites in `Command`, and the links in `Response` print as `..`; keep it so for anything new. Only the owner stores the password key, never the password.
- After touching rooms, admission, the engine's room actor or `couch_net::control`, follow the `room-security-checks` skill.

**Sessions** (`docs/protocol.md` Sessions)
- **The host is the only writer** of its session's facts, sent straight to each member, never relayed. A member acts on a session message only from that session's host, on the current connection, while the host is on the roster. Never let one member speak for another's session.
- **The seat router alone decides who plays.** `couch_room::session` asks it and mirrors its `Granted`/`Revoked` per seat; never inject or track a holder elsewhere.
- **Seats per viewer: one per controller, decided by the host** (M18). A viewer may hold as many seats with a pad as its stream window has controllers (`ViewerControllers`, the viewer's own word, capped at 4), plus the keyboard-and-mouse seat; `HostSession::keeps` alone decides whether a grant adds a seat or moves the viewer, counting grants still on their way (`pending`, in the order sent: a router report answers only its own member's grant). A move revokes that viewer from every other pad seat first (its keyboard-and-mouse seat stays), on the same ordered channel to the router (`HostSession::grant`); keep that order. The count only chooses add or move: the host still approves every grant, and a count that drops later frees no seat (its pad goes neutral). The session property test must keep reaching a viewer on two pad seats (its ops are weighted for it).
- **Only the forwarder decides which of a guest's controllers drives which seat** (`couch_input::forward`): one pad seat, the controller last touched; several, each seat pinned to a controller slot while it is connected. The stream window keeps a controller's slot while it is connected (no renumbering) and rumbles and shows by the mapping the forwarder sends (`HeldSeat::controller`); never map them anywhere else. A controller that goes while its guest holds several pad seats gives that seat back (`Forwarder::take_left`, sent as the guest's own give-back); a restarted window (`viewer_replaced`) gives none back.
- **The host on a seat** (P2). The host's own controller reaches a pad only through the seat router, as the host's holder with the pad permission only (the input task's local channel, `Inputs::local_input`; `couch-cli host-pad --feed` goes through a router too); never feed it to a pad directly. Only the host's last seat comes back to them, when a viewer lets go of it, and only through a claim, which the router grants only if the seat is free, the host holds none and control isn't paused: a plain grant there could take a seat a viewer was just passed. A seat the host takes back or leaves stays empty (only Play here seats their controller again). The panic key frees only viewers' seats (`RevokeViewers`, which the router applies, never the session's mirror) and doesn't pause the host's own controller.
- **Which pads are ours is decided by where they come from** (P9, `couch_platform_win::pad_origin`), never by ids alone: our pads carry a real Xbox 360 controller's ids, as do remappers'. A gamepad's device is found by SDL's path, or for `XInput#N` by the one pad the Xbox driver shows slot N's light on (`xinput_slots`), and must carry SDL's ids; under ViGEmBus it is ours if its serial is in a running host's marker (`virtual_pads`), while a host is still plugging one, or if no serial can be read; under USB or Bluetooth it is physical. **Unknown counts as ours by ids** (the rule before P9: a pad with our pads' ids is skipped, never listed, never hidden). A host writes its pad's marker before plugging it and adds the serial after; keep that order. Only a physical controller, or one whose origin can't be told without our pads' ids, can be the host's controller or be hidden (`pad_origin::may_be_the_hosts`, for the reader and HidHide alike), and nothing under a virtual bus is ever hidden; a stream window on the same PC skips exactly the hosts' pads and reads the rest, other programs' virtual pads included. Any doubt in finding a pad's device (two pads on one light, a light that can't be read) makes it unknown.
- **A hidden controller is always shown again** (`couch_platform_win::hidhide`): the guard drops after the pads are unplugged, and `restore_stale` (in `Backend::release_left_input`) undoes what a killed host left. Keep both.
- **Ending releases first:** input, then `SessionEnded` and `SESSION_END`, then encoders and capture, then the pad. A viewer that goes (leaves, closes its window, loses the connection, is banned) is released at once through `SeatRouter::disconnected`.
- **Sessions before streams** (M19, `pipeline::host::HostPipeline`). A session owns the seats, pads, hiding, input task and viewers; streams start and stop inside it (`StreamControl`), and the game exiting or its window going ends only the stream. **While nothing is streamed, viewers' input is held off** (`SeatRouter::hold`, from the session's start): they keep their seats, but nothing of theirs reaches the game, since they can't see. The hold keeps counting their input as heard (no suspension) and keeps mouse totals current (no jump later); the host's own controller is its exception. **Stopping a stream releases first:** the hold (what viewers held is released at once), then each viewer's epoch ends with `STREAM_END` and its encoder closes, then capture and sound. A viewer's video sender lives across streams so its epochs keep counting: the viewer drops older ones. **Starts and stops run one at a time, in the order asked for:** `StreamControl` queues an operation when it is called (not when its future first runs) to the session's one operations task, so the actor must call it synchronously, then spawn the wait. Each start's answer names its request (`OurStream::Starting { request }`); an answer for an older one changes nothing. Stream events (`SourceClosed`, `GameExited`, `CaptureFailed`) carry their stream's number; one from a stream already replaced says nothing, and one arriving while a start is pending is kept until its answer (the starting stream may fail first). Seats are fixed for a session.
- **A seat never survives a connection.** A viewer that reconnects joins again as a new viewer and must be given control again; nothing restores a seat without the host.
- **Every key or button `SendInputInjector` presses is recorded** (`couch_platform_win::held_input`) so a crash can't leave it held. Inject keyboard and mouse only through it, and keep `release_left_input` at the start of anything that hosts.
- **Shortcuts live in one table**, `couch_input::shortcuts`: `SHORTCUTS` holds the defaults, and the user's keys (Settings, saved per profile in `shortcuts.json`) are laid over them by `Shortcuts`, which the viewer (IPC `SHORTCUTS`), the host-keys hook (read at a session's start) and the app all use. Add or change a shortcut there, never as a key constant elsewhere; a new row needs a stable `id`. The user moves keys, never modifiers: only keys in `shortcuts::KEYS` (each with a plain, non-extended scancode; never Esc, Tab, Delete, F4 or the gray navigation keys, which come with fake Shift events), no two rows on one key, and the panic key can't be removed. The host's keys (the panic key, Accept and Decline) go through one low-level hook, `couch_platform_win::host_keys`, which must keep ignoring injected keys: that is what stops a guest holding the keyboard from pressing the panic key or accepting their own request. Never answer requests through `RegisterHotKey` or the webview, which take injected keys too.
- **The host overlay never takes the focus** (M8; `apps/desktop/src/useOverlay.ts`). Keyboard and mouse reach the game only while it is in front, and some games pause without it. The overlay window is created unfocused and unfocusable, closed when empty rather than hidden (showing a hidden window again may activate it), click-through, and excluded from capture. The answer keys answer only a request pending for at least 1 s (`rooms::sessions::ANSWER_AFTER`), so a key meant for one request never answers another that just arrived.
- After touching sessions or the pipelines, follow both the `room-security-checks` and the `input-safety-checks` skills; after touching the host pipeline's video path, the `rate-control-gates` skill too.

**Licensing.** The app is **GPL-3.0-only** (`LICENSE`; [ADR 0008](docs/adr/0008-gpl-3.0.md)). Every crate inherits it (`license.workspace = true`).
- Dependencies stay permissive by policy (`deny.toml`). A copyleft crate needs a recorded exception, and must be GPL-3.0-compatible.
- FFmpeg must stay the LGPL build, dynamically linked.
- **Code from other projects** is allowed only under a GPL-3.0-compatible licence: permissive, LGPL, GPL-2.0-or-later, or GPL-3.0. Sunshine and Moonlight are GPL-3.0, so their code may be reused, never silently:
  - check that project's exact licence first;
  - keep its copyright notice;
  - name the source (project, file, commit) in a comment where it's used;
  - add it to the third-party notices.

  RustDesk is AGPL-3.0, and GPL-2.0-only code is incompatible: ask the user first.
- **Notices:** every installer, even one for friends, must carry every bundled component's notice, the app's source link at its commit, and FFmpeg's source beside it (M10b, `cargo xtask notices`). A new bundled component that isn't a crate or an npm package needs a section in `xtask/src/notices.rs`. A new FFmpeg pin needs new `deps::SOURCES` pins.

**Opening encoders** (M9a)
- **Never call rsmpeg's `AVCodecContext::open` with options.** When the open fails, rsmpeg 0.18 double-frees the dictionary and crashes the process. Open through `couch_codec_ffmpeg::options::OptionSet::open`, which calls `avcodec_open2` itself.
- **Each encoder's options are an ordered list of sets** (`options::option_sets`). The last one is always the settings that worked before; add new sets before it, never replace it. `tests/open.rs` (every encoder, twice, no crash) must keep passing on any machine.

**Frames on the GPU** (M9b on the host, M9c on the viewer, M9d's pacing, M9f's capture; `couch_platform_win::wgc` and `gpu_frames`, `couch_codec_ffmpeg::d3d11` and `d3d12`, `couch-viewer`'s `zero_copy`)
- **NVENC remembers every texture it registers, by address.** So:
  - a pool is replaced only under a new `GpuFrame::source`, and an encoder reading GPU frames reopens when `source` changes (the encode loop in `pipeline/host.rs`; `D3d11Frames::wrap` refuses another pool's frame);
  - an encoder keeps its pool's textures alive until it closes (`GpuTexture::keep_alive`, held by `D3d11Frames`; the codec context drops first);
  - never free, swap or resize a pool's textures in place.
- **When no encoder reads GPU frames** (one couldn't take them, or failed), the stream sets `gpu_input_off` and the capture starts again with frames in memory (`CaptureRequest::in_memory`): reading every frame back from the GPU costs more than capturing to memory.
- **The capture is ours** (M9f, `couch_platform_win::wgc`, on Windows.Graphics.Capture directly; `top_windows` and `monitor` list what it can take). Its rules:
  - **two buffers in the frame pool,** so Windows can write the next frame while the GPU still runs our copy of the last. Under Witcher 3 (about 100 fps) one buffer gave about 72 frames a second, two about 107;
  - **in the frame handler, the session's lock comes before `TryGetNextFrame`:** Windows calls it on several threads at once, and a call that waits while holding a frame takes a buffer from Windows;
  - **a closed window is noticed by checking `IsWindow`** every 100 ms: the item's `Closed` event doesn't reliably arrive on a free-threaded pool;
  - **frames under 160x64 are dropped** (a minimized window): one once made NVENC refuse to open, which turned GPU frames off for the whole stream;
  - **no GPU priority:** raising it (Sunshine's calls) delayed Windows' own capture work under a game and halved the gain; measured, dropped.
- **The capture runs faster than the stream only on the GPU** (M9d): a 4 ms interval, each extra frame a GPU copy into the pool, and the capture thread's pacer (`couch_media::pacing`) sends exactly the stream's rate. Windows can't change a running capture's interval, hence the restart above. Frames in memory keep the one-frame interval: a copy to memory per screen refresh would cost too much. A held (early) frame is dropped if a newer one comes first; how long it waits for one depends on how often frames come (M9e, the largest recent gap, hitches aside):
  - **near the stream's rate:** an interval past its due time. A shorter wait held up a game running a little faster than the stream (61 fps into 60) by up to an interval per frame, for seconds;
  - **a busy source** (over 1.5 times the rate): a gap and a quarter, at the earliest at its due time, so a moment without changes doesn't hold it an interval;
  - **a lone change** (its own gap over two intervals: the first change on a still picture): its due time. Only that frame's own gap counts, and the source's rate ignores such hitches: after one, a game runs as before, and releasing its frames at their due times would make the next ones early.

  The pacer's tests simulate each case; keep them passing. The wait runs on the clock frames arrive by, not their capture timestamps.
- **The capture's device is multithread-protected** (`TexturePool::new`): the capture thread and the encoder threads share it. Keep that for any new user of the device.
- **FFmpeg owns one device reference per encoder** (`GpuTexture::d3d11_device_owned`) and releases it; a wrapped frame owns a `GpuFrame` clone, freed by the buffer's callback. Never hand FFmpeg a borrowed COM pointer as its own. FFmpeg's D3D12VA context releases a device only if it created it, so `d3d12::device_context` installs the `free` callback that does.
- **Measure a performance change against itself turned off** (its switch, e.g. `COUCH_HOST_ZERO_COPY=0`), on the same build: the `performance-checks` skill has the procedure, the known artifacts and the reference numbers. Give each new performance change such a switch, and drop a change that shows no gain (M9d dropped thread and process priorities that way).
- **Only NVENC takes GPU frames** (`gpu_input_encoder(codec)`: `h264_nvenc`, or `hevc_nvenc`). Another encoder gets them from `GpuFrame::cpu()`, copied once per frame. Turning on AMD or Intel GPU input needs testing on their hardware first, and Quick Sync needs NV12 (a conversion shader).
- **HEVC only from an NVIDIA host taking GPU frames** (M12c, `pipeline::host::choose_codec`), to a viewer whose `SessionJoin` says it decodes HEVC. A failed `hevc_nvenc` falls back to H.264 for that viewer alone, never turning GPU frames off for the others. AMD and Intel HEVC needs their hardware first (M12a).
- **Colours follow the stream's declared matrix** (M12c): NVENC converts RGB with BT.601 whatever we ask, and says so; our converter for the other encoders writes BT.709. The decoder puts the declared matrix on every frame (`YuvMatrix`), and the stream window's shader and `capture-test --psnr` use it. Never assume one matrix.
- **HEVC on D3D12VA needs our own frame pool** (`decoder.rs`, `aligned_d3d12_frames`, in `get_format`): FFmpeg's (8.1 to 9.0) is the picture's exact size, and NVIDIA's driver fails to create it for many heights (720, 2160, 900). Ours is aligned to 128, as D3D11VA's; so the viewer's `FrameImporter` copies the picture out of a texture that may be larger. Keep both.
- **On the viewer, FFmpeg's D3D12VA decoder writes into wgpu's own DX12 device's resources** (`FrameImporter`). So:
  - every submit that reads a decoded frame first waits for FFmpeg's fence on the GPU (`add_wait_fence`);
  - the frame goes back to COMMON (`PRESENT` to wgpu) in that submit, the state FFmpeg's next decode expects;
  - a frame is dropped, back to FFmpeg's pool, only after that submit has finished (`on_submitted_work_done`), never sooner;
  - a new wgpu texture is made per frame: never cache one per FFmpeg resource, or wgpu's idea of its state goes stale.
- **Zero-copy decoding is on by default only on NVIDIA** (`COUCH_VIEWER_ZERO_COPY=1` tries other GPUs). Any failure falls back to D3D11VA with copies, logged. Turning it on by default elsewhere needs testing on that hardware.

**Native dependencies and pins**
- FFmpeg and SDL3 are pinned by URL and SHA-256 in `xtask/src/deps.rs`. So are the drivers' official installers the app bundles (`DRIVERS`: ViGEmBus 1.22.0, BSD-3; HidHide 1.5.230, MIT), with their licence texts in `apps/desktop/src-tauri/drivers/`; `stage-dlls` copies them to `target/<profile>/drivers/`, where Settings → Drivers finds them.
- libopus is compiled from source by the `opus` crate, which needs CMake (`doctor` checks). Its CMake build fails past Windows' 260-character path limit, so keep `target/` at a short path.
- The FFmpeg FFI bindings in `crates/couch-codec-ffmpeg/bindings/` are generated and committed. rusty_ffmpeg's build-time bindgen 0.71 produces fieldless structs with libclang 22 or later. Regenerate the bindings whenever the FFmpeg pin changes; `doctor` flags stale ones.
- Exact pins: `iroh` and `sdl3` (`=`). `windows` stays on 0.62 to match wgpu-hal's Direct3D 12 types (`couch-viewer`'s `zero_copy`).

**Relays** ([ADR 0007](docs/adr/0007-our-relay-and-release-configuration.md))
- **Which relays run where.**
  - Development: `RelayChoice::N0Public` (iroh's public relays). A release build runs them only when the user picks them in Settings ("for testing"); a saved setting the build can't take falls back to the build's own relay, never to them.
  - Release builds: only the builder's own iroh-relay (`RelayChoice::Custom { urls, token }`); the maintainer's runs at `couchrelay.duckdns.org` for their friends.
  - Tests: `RelayChoice::Disabled`, or `couch_testkit::relay::TestRelay`, an in-process iroh-relay with a token.
- **Our relay admits only clients with its token.** iroh sends it only to relays in our own list; it never goes in invites, member records or logs (`RelayToken` prints `..`).
- **A release build carries the relay's URL and token,** and the invite page's address (`couch_engine::build_config`, `COUCH_BUILD_*` at build time, set by `build-installer.ps1` from `release.toml`, or for the page `-InvitePage`).
- **Settings → Network** (`network.json`): the default, `builtin`, is that relay, or iroh's in a build without one; or `public`; or a URL. `EngineOptions::desktop` reads it, and it applies from the next start (`restart_app`). Plain http relays are allowed on this PC, and at private LAN addresses in development and test builds only.
- **Versions.** The relay server's version follows the iroh pin: bump `RELAY_VERSION` in `deploy/relay/install.sh` with it. `cargo xtask doctor` compares the local relay tool.

**Secrets and release values never go in git.**
- **Where they live:**
  - the relay's token and URL, and the invite page: `release.toml` outside the repository;
  - on the server: `/etc/iroh-relay/token.env`, root-only;
  - any signing key, should one ever be added: a private file or hardware.

  Committed files hold placeholders only (`deploy/release.example.toml`, `deploy/relay/iroh-relay.toml`).
- **Never write a real token, URL with credentials or key into the repository.** The build script refuses a `release.toml` inside it.
- **Builds get them through `COUCH_BUILD_*` for that build only.** The script unsets them afterwards.

**Congestion control lives in the rate controller** (`couch_media::ratecontrol`, ADR 0005). QUIC runs `couch_net::congestion::MediaCongestion`, which deliberately ignores isolated losses. A change to either can collapse the bitrate under random loss or stall recovery after an outage. Run `couch-cli gates` after touching them, or the video transport or encoder bitrate handling (the `rate-control-gates` skill has the procedure); `docs/m1-results.md` has the reference numbers.

**Tests.** `cargo test --workspace` must pass without a GPU, a gamepad driver or internet access. Tests that need those are opt-in (`#[ignore]` or a `gpu-tests` feature).
