# Developing Couch Co-App

How to set up a Windows PC to build, run and test the app, and Linux to build, test and watch with it. If you only want to play, install a [release](https://github.com/marcusvilete/couch-coapp/releases/latest); [self-hosting.md](self-hosting.md) covers that and running a relay of your own. [CLAUDE.md](../CLAUDE.md) has the full command list and the rules that aren't obvious from the code; [architecture.md](architecture.md) explains how the pieces fit.

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

## Setup (Linux)

Linux builds, passes the tests, and watches and plays a Windows host's sessions (M16a and M16b, [ADR 0011](adr/0011-linux.md)); since M16c-1 it hosts with controller seats on a KDE or GNOME desktop, since M16c-2 a device of yours controls its desktop from afar and guests get the keyboard seat on KDE, and on GNOME with our extension ([ADR 0012](adr/0012-linux-host.md)). Ubuntu 24.04, also in WSL:

1. **The system packages,** once: `sudo bash scripts/linux-deps.sh`.
2. **Rust:** `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`, then the repository's `rust-toolchain.toml` picks the version. **Node.js 24** (Ubuntu's own is too old for Vite), e.g. from NodeSource.
3. **In WSL, keep the repository on Linux's own disk** (`~/couch-coapp`), not under `/mnt/c`: builds across the two are slow. If Windows' clock is off, WSL's wall clock jumps back and forth by the difference every few seconds (`journald` says "Time jumped backwards"): WSL pushes Windows' time in, Ubuntu's own sync sets the right one. That can fail the invite-expiry scenario. Sync Windows' clock (Settings → Time & language → Date & time → Sync now; `w32tm /stripchart /computer:time.windows.com /samples:3` shows how far off it is). Its disk image grows on C: and doesn't shrink by itself.
4. **Load the environment in every new shell,** from the repository's root. It chooses Linux's FFmpeg bindings:
   ```bash
   . scripts/dev.sh
   ```
5. **Fetch and stage FFmpeg and the stream window's icon font** (SDL3 is built from source on Linux, at the same version), and install the front end's packages:
   ```bash
   cargo xtask fetch-deps
   cargo xtask stage-dlls
   (cd apps/desktop && npm install)
   ```
6. **Build and test** with the same commands as on Windows (below). Only `probe`, `host`, `view`, `pad-test` and `front-probe` exist in `couch-cli` there.

A workflow (`.github/workflows/linux.yml`) runs clippy, the tests and the front end's checks on Linux for every pull request. The AppImage for other Linux PCs: [Building installers](#building-installers) below.

**A Linux host on a real desktop** (the portals' dialogs, the host's keys, notifications, a real sound device; WSL has none of them): a VirtualBox VM with GNOME and KDE, made with `scripts/vm/create.ps1`, and the AppImage copied in. A development build (`npm run tauri dev`) on Linux installs no menu entry, so the desktop's portals don't know the app and refuse its global shortcuts: the host's keys work only from an AppImage.

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
A development build uses iroh's public relays until a relay is added in Settings → Network (a relay code, or an address; `scripts\relay.ps1`'s relay too, below).

**The UI without the engine** (M21): a preview of the app on a fake engine, with a scenario for every state (an empty room, a full session, requests waiting, a handover…), so screens can be seen and changed without starting sessions:
```powershell
cd apps\desktop
npx vite                   # then http://localhost:1420/preview.html (the panel bottom left picks a scenario)
npm run shots              # every scenario in dark, light and High Contrast at three sizes, into target\ui-shots\latest
```
`npm run shots` uses the Microsoft Edge already on Windows (nothing to download) and also fails on text spilling out of its box, a menu that doesn't open, or a region that moves as you go between rooms, Settings pages or a dialog's tabs. A development build can be looked at from a script too: start it with `$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS='--remote-debugging-port=9222'` in its own shell (never a shared one: the port lets any program on the PC drive the app; release builds ignore the variable), then attach with Playwright's `connectOverCDP`.

**The app's icon** is drawn in `apps\desktop\src-tauri\icons\source`: `icon.svg`, and drawings for 32, 24 and 16 px (the same purple tile since 0.27.2), which the taskbar and the notification area show; earlier icons are in `alternatives`. After changing one, `npm run icons` (in `apps\desktop`) renders every icon file from them, also with Edge, and `cargo clean -p couch-desktop` makes the next build take the new icon. The README shows `docs\images\icon.svg`, a copy of `icon.svg`.

**The stream window and the developer CLI:** `couch-viewer` is the window that shows a stream. Its bar and cards can be seen without a session: `cargo run -p couch-viewer -- --hud-preview watching` (eighteen states, `drawing` among them), or all of them as screenshots with `cargo build -p couch-viewer` then `.\scripts\viewer-shots.ps1` (into `target\ui-shots\viewer`). `couch-cli` runs hosts, viewers and the measurements without the app.
```powershell
cargo build --release -p couch-cli -p couch-viewer
.\target\release\couch-cli.exe latency-probe --secs 30 --relay none   # end-to-end latency on this PC
```

**The update notice:** builds that name their releases (public builds) look at GitHub's latest release once a day. To try it in a development build, serve the copy of GitHub's reply in the repository, `npx --yes http-server crates/couch-engine/tests/fixtures -p 8000`, and set `$env:COUCH_RELEASES_URL = 'http://localhost:8000/github-latest-release.json'` before `npm run tauri dev`: ten seconds after start, the strip at the bottom says 99.0.0 is out. Release and public builds ignore the variable.

**A relay on this PC:** run `cargo xtask relay-tool` once, then `.\scripts\relay.ps1`. It serves `http://localhost:3340` with the token from your `release.toml` (`-Lan` opens it to your network, `-Open` drops the token). Point a development build at it by setting `COUCH_BUILD_RELAY_URL` and `COUCH_BUILD_RELAY_TOKEN` before `npm run tauri dev`.

**The invite page** (optional): a static page that makes invite links clickable in chat apps. A workflow publishes it on the repository's GitHub Pages, and `build-installer.ps1 -InvitePage <address>` points one build at a page. [deploy/invite-page/README.md](../deploy/invite-page/README.md) covers trying and hosting it.

## Building installers

The repository's releases come from its release workflow (`.github/workflows/release.yml`): on a tag `v<version>` it builds the Windows installer and the AppImage as **public builds** (no relay and no token: iroh's public relays until the user adds one) and makes a draft GitHub Release of them. Run by hand from the Actions tab, it builds them from any branch and keeps them as the run's artifacts, with no release. The same on this PC, from a clean, committed tree: `.\scripts\build-installer.ps1 -Public`, or `bash scripts/build-appimage.sh --public` on Linux.

**A test installer** (iroh's public relays; for trying it on another PC, from any tree):
```powershell
.\scripts\build-installer.ps1 -TestRelay      # target\installer\...-test-setup.exe
npx --yes http-server target\installer -p 8000
```

**A build carrying your relay,** for friends who'd rather not paste a code: it lists the relay as "this app's", which can't be removed. You need the setup above, a relay of your own ([deploy/relay/README.md](../deploy/relay/README.md)), and a new installer from you for each version.
1. **Your private settings,** once: `cargo xtask release-config` writes `%USERPROFILE%\.couch-coapp\release.toml` with a fresh token for the relay: give it to `install.sh` on a new server, and keep it in a password manager. **If your relay already runs,** replace `relay_token` with its token, or the build carries one the relay refuses. The file is outside the repository on purpose, so it never ends up in git; the build refuses one inside it.
2. **Point it at your relay:** in `release.toml`, set `relay_url = "https://<your relay's name>"`. It can also name an invite page (`invite_page`, such as the repository's own, `https://<user>.github.io/<repository>/j/`: [deploy/invite-page/README.md](../deploy/invite-page/README.md); whoever runs a page could see the invites opened with it, though the room's password still guards the room), and whose releases the app checks for new versions (`releases = "<owner>/<name>"`; none by default, so its users hear of nothing). `-InvitePage <address>` and `-Releases <owner>/<name>` set them for one build, `-NoInvitePage` builds with plain `couchco://` links.
3. **Build:** `.\scripts\build-installer.ps1`. It needs the code exactly as committed; it takes several minutes the first time.
4. **Hand out everything in `target\installer\`:** the setup file, FFmpeg's two source archives and `SOURCES.txt`. The licences require that whoever gets the app can also get its source and FFmpeg's. To serve the folder on your network: `npx --yes http-server target\installer -p 8000`.

**A version bump** comes with each installer handed out to people who have an older one: `cargo xtask bump patch` (or `minor`, after changes older builds can't talk to) sets the same new version in every file that names it.

**If you change the code,** the installer's notices must point your friends at what they run: the `repository` address in `Cargo.toml`, at the exact commit it was built from. Fork the repository, set `repository` to your fork's address, commit and push, then build. The build script refuses uncommitted changes, and warns when the commit isn't pushed yet.

**The AppImage, for Linux,** is built on Ubuntu 24.04. WSL on your Windows PC works: `wsl --install -d Ubuntu-24.04`, then everything below inside it.
1. **Prepare it, once:**
   - **a clone of the code, on Linux's own disk** (in WSL never under `/mnt/c`, and not the copy you develop in): `git clone <this repository's address> ~/couch-release`, then `cd ~/couch-release`;
   - the system packages, Rust and Node.js 24, as in the Linux setup above, and cargo-about: `cargo install --locked cargo-about --features cli`;
   - the front end's packages: `cd apps/desktop && npm ci && cd ../..` (`npm ci` leaves the tree clean, which a release build needs).
2. **Your release settings:** the same `release.toml`, at `~/.couch-coapp/release.toml`; in WSL, point at Windows' own instead of copying it: `export COUCH_RELEASE_CONFIG=/mnt/c/Users/<you>/.couch-coapp/release.toml`.
3. **Build**, from the clone, at the same commit as your Windows installer (`git pull`): `bash scripts/build-appimage.sh`. It takes the same options as `build-installer.ps1`, spelled the Linux way: `--test-relay`, `--public`, `--config <file>`, `--invite-page <address>`, `--no-invite-page`, `--releases <owner>/<name>`.
4. **Hand out everything in `target/appimage/`:** the AppImage, `THIRD-PARTY-NOTICES.txt`, FFmpeg's two source archives, `SOURCES.txt`, and the system libraries' sources (`…-system-sources.tar`): the notices name the source of every system library the AppImage carries, as the licences ask.

What your friends do with either package is in [self-hosting.md](self-hosting.md#1-install-the-app). When a build stops with an error, `cargo xtask doctor` (on Linux, after `. scripts/dev.sh`) checks every tool and dependency.

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
