# Developing Couch Co-App

How to set up a Windows PC to build, run and test the app. If you only want an installer and a relay for your friends, [self-hosting.md](self-hosting.md) is the shorter path. [CLAUDE.md](../CLAUDE.md) has the full command list and the rules that aren't obvious from the code; [architecture.md](architecture.md) explains how the pieces fit.

## Setup (Windows 11)

1. **Install the tools:**
   - Git;
   - Visual Studio Build Tools (or Visual Studio) with the **Desktop development with C++** workload;
   - Rust: `winget install --id Rustlang.Rustup -e` (the version is pinned in `rust-toolchain.toml`, and rustup fetches it);
   - Node.js;
   - CMake: `winget install Kitware.CMake` (libopus builds with it);
   - `cargo install cargo-deny --locked`;
   - `cargo install --locked cargo-about --features cli` (it gathers the licences every installer must carry).

   LLVM is needed only to regenerate the FFmpeg bindings.
2. **Clone the repository to a short path,** such as `C:\dev\couch-coapp`: libopus's build fails past Windows' 260-character path limit.
3. **Open a terminal in the repository** and load the environment. Do this in every new terminal: it puts cargo and the native DLLs on PATH.
   ```powershell
   . .\scripts\dev.ps1
   ```
4. **Fetch the pinned native dependencies** (FFmpeg's LGPL build and its source, SDL3, and the two drivers' installers), and stage them next to the build output:
   ```powershell
   cargo xtask fetch-deps
   cargo xtask stage-dlls
   ```
5. **Install the front end's packages:** `cd apps\desktop; npm install`.
6. **Check the machine:** `cargo xtask doctor`.

**Drivers are optional for development:**
- [ViGEmBus](https://github.com/nefarius/ViGEmBus) lets guests' controllers reach games you host;
- [HidHide](https://github.com/nefarius/HidHide) lets your own controllers play through seats, one seat each (a remapper's virtual pad, DS4Windows's say, included).

The app's Settings → Drivers installs either.

## Running each part

**The desktop app** (Tauri 2: Rust engine, React UI):
```powershell
cd apps\desktop
npm run tauri dev          # builds the stream window first, then runs the app (the default profile)
```
To try rooms on one PC, start more profiles from that build (alice, bob and carol, each with its own identity), from the repository root:
```powershell
.\scripts\profiles.ps1
```
A development build uses iroh's public relays.

**The UI without the engine** (M21): a preview of the app on a fake engine, with a scenario for every state (an empty room, a full session, requests waiting, a handover…), so screens can be seen and changed without starting sessions:
```powershell
cd apps\desktop
npx vite                   # then http://localhost:1420/preview.html (the panel bottom left picks a scenario)
npm run shots              # every scenario in dark, light and High Contrast at three sizes, into target\ui-shots\latest
```
`npm run shots` uses the Microsoft Edge already on Windows (nothing to download) and also fails on text spilling out of its box, a menu that doesn't open, or a region that moves as you go between rooms, Settings pages or a dialog's tabs. A development build can be looked at from a script too: start it with `$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS='--remote-debugging-port=9222'` in its own shell (never a shared one: the port lets any program on the PC drive the app; release builds ignore the variable), then attach with Playwright's `connectOverCDP`.

**The stream window and the developer CLI:** `couch-viewer` is the window that shows a stream. Its bar and cards can be seen without a session: `cargo run -p couch-viewer -- --hud-preview watching` (twelve states), or all of them as screenshots with `cargo build -p couch-viewer` then `.\scripts\viewer-shots.ps1` (into `target\ui-shots\viewer`). `couch-cli` runs hosts, viewers and the measurements without the app.
```powershell
cargo build --release -p couch-cli -p couch-viewer
.\target\release\couch-cli.exe latency-probe --secs 30 --relay none   # end-to-end latency on this PC
```

**A test installer** (iroh's public relays; for trying it on another PC, from any tree):
```powershell
.\scripts\build-installer.ps1 -TestRelay      # target\installer\...-test-setup.exe
npx --yes http-server target\installer -p 8000
```
Release installers, with your own relay, are in [self-hosting.md](self-hosting.md).

**A relay on this PC:** run `cargo xtask relay-tool` once, then `.\scripts\relay.ps1`. It serves `http://localhost:3340` with the token from your `release.toml` (`-Lan` opens it to your network, `-Open` drops the token). Point a development build at it by setting `COUCH_BUILD_RELAY_URL` and `COUCH_BUILD_RELAY_TOKEN` before `npm run tauri dev`.

**The invite page** (optional): a static page that makes invite links clickable in chat apps. A workflow publishes it on the repository's GitHub Pages, and `build-installer.ps1 -InvitePage <address>` points one build at a page. [deploy/invite-page/README.md](../deploy/invite-page/README.md) covers trying and hosting it.

## Tests and checks

```powershell
cargo test --workspace                                  # no GPU, gamepad driver or internet needed
cargo clippy --workspace --all-targets -- -D warnings   # must be clean
cargo fmt --all
cargo deny check                                        # licences and advisories
cd apps\desktop; npx tsc --noEmit                       # the front end's types
cd apps\desktop; npx vitest run                         # the front end's logic
cd apps\desktop; npm run shots                          # the screens (above)
```
The end-to-end scenarios run with the tests:
- several engines over real iroh on loopback (`couch-engine`'s `rooms`, `sessions` and `relay` tests);
- the streaming gates: `couch-cli gates --quick`, which takes 6 minutes and uses the screen.

## Where to read more

- [CLAUDE.md](../CLAUDE.md): every command, the crate boundaries, and the safety, protocol and secrets rules.
- [Architecture](architecture.md): processes, crates, data paths, milestones.
- [Protocol](protocol.md): the wire format and the stream window's IPC.
- [Decision records](adr/).
- Each milestone's results: `m*-results.md` and `p*-results.md` in this folder, with what was measured and what was tried by hand.
