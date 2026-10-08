# Couch Co-App

Remote "same couch" gaming. Create a password-protected room, invite friends with a link, stream your game to them, and pass them the controller.

Connections are peer-to-peer (iroh QUIC). A relay starts each connection and carries it when no direct path exists: your own `iroh-relay` server, or iroh's public relays for testing.

**Status:** in development, Windows only for now. [docs/architecture.md](docs/architecture.md) has the design and the milestone table.

**There are no official downloads.** Couch Co-App is shared as source code ([ADR 0009](docs/adr/0009-source-only-releases.md)). To use it with your friends:
1. build it, following the steps below;
2. run your own relay, a small server ([Your own relay](#your-own-relay));
3. build an installer that uses your relay, and give it to your friends.

## Setup (Windows 11)

1. **Install the tools:**
   - Rust: `winget install --id Rustlang.Rustup -e` (the version is pinned in `rust-toolchain.toml`);
   - the Visual Studio C++ build tools;
   - Node.js;
   - CMake (`winget install Kitware.CMake`; libopus builds with it);
   - `cargo install cargo-deny --locked`;
   - `cargo install --locked cargo-about --features cli` (it gathers the licences that every installer must carry).

   LLVM is needed only to regenerate the FFmpeg bindings.
2. **Open a terminal in the repository** and load the environment. Do this in every new terminal: it puts cargo and the native DLLs on PATH.
   ```powershell
   . .\scripts\dev.ps1
   ```
3. **Fetch the pinned native dependencies** (FFmpeg's LGPL build and its source, SDL3, and the two drivers' installers), and stage them next to the build output:
   ```powershell
   cargo xtask fetch-deps
   cargo xtask stage-dlls
   ```
4. **Install the front end's packages:** `cd apps\desktop; npm install`.
5. **Check the machine:** `cargo xtask doctor`.

**Drivers are optional for development:**
- [ViGEmBus](https://github.com/nefarius/ViGEmBus) lets guests' controllers reach games you host;
- [HidHide](https://github.com/nefarius/HidHide) lets your own controller play through a seat.

The app's Settings → Drivers installs either.

Keep the repository at a short path such as `C:\dev\couch-coapp`: libopus's build fails past Windows' 260-character path limit.

## Building and running each part

**The desktop app** (Tauri 2: Rust engine, React UI):
```powershell
cd apps\desktop
npm run tauri dev          # builds the stream window first, then runs the app (the default profile)
```
To try rooms on one PC, start more profiles from that build (alice, bob and carol, each with its own identity), from the repository root:
```powershell
.\scripts\profiles.ps1
```

**The stream window and the developer CLI:** `couch-viewer` is the window that shows a stream. `couch-cli` runs hosts, viewers and the measurements without the app.
```powershell
cargo build --release -p couch-cli -p couch-viewer
.\target\release\couch-cli.exe latency-probe --secs 30 --relay none   # end-to-end latency on this PC
```

**A test installer** (iroh's public relays; for trying it on another PC):
```powershell
.\scripts\build-installer.ps1 -TestRelay      # target\installer\...-test-setup.exe
npx --yes http-server target\installer -p 8000
```

## Your own relay

iroh's public relays are for testing only. For regular use, run the same server we do, `iroh-relay`, on a small VPS. A free tier works: Oracle Cloud's is the worked example.

1. **Create your private release configuration:** `cargo xtask release-config`. It makes `%USERPROFILE%\.couch-coapp\release.toml` with a fresh access token. It's outside the repository and must stay out of git; [deploy/release.example.toml](deploy/release.example.toml) explains it.
2. **Set up the server:** follow [deploy/relay/README.md](deploy/relay/README.md):
   - a server and a name;
   - the firewall;
   - `install.sh`, which asks for your token.
3. **Put the relay's address in `release.toml`** (`relay_url = "https://your-relay.example.com"`).
4. **Build the installer for your friends**, now without `-TestRelay`: `.\scripts\build-installer.ps1`. It refuses a tree with uncommitted changes: the installer's notices link to the source of that exact commit, so commit and push first.
   - **Hand out everything in `target\installer\`:** the setup file, FFmpeg's source archives and `SOURCES.txt`. The licences require that whoever gets the app can get its source, and FFmpeg's.
   - **Bump the version** for each installer you hand out: it must be the same in `Cargo.toml`, `apps/desktop/src-tauri/tauri.conf.json` and `apps/desktop/package.json`.

Everyone in a room should use installers from the same build. Different builds refuse each other with "update needed", and other people's builds can't use your relay without your token.

To try the relay on your PC first: `cargo xtask relay-tool` (once), then `.\scripts\relay.ps1`, which serves `http://localhost:3340` with the token from `release.toml`.

**The invite page** (optional): a static page that makes invite links clickable in chat apps. [deploy/invite-page/README.md](deploy/invite-page/README.md) covers hosting it.

## Tests and checks

```powershell
cargo test --workspace                                  # no GPU, gamepad driver or internet needed
cargo clippy --workspace --all-targets -- -D warnings   # must be clean
cargo fmt --all
cargo deny check                                        # licences and advisories
cd apps\desktop; npx tsc --noEmit                       # the front end's types
```
The end-to-end scenarios run with the tests:
- several engines over real iroh on loopback (`couch-engine`'s `rooms`, `sessions` and `relay` tests);
- the streaming gates: `couch-cli gates --quick`, which takes 6 minutes and uses the screen.

## Documentation

- [CLAUDE.md](CLAUDE.md): the full command list, the crate boundaries and the rules that aren't obvious from the code (safety, protocol, secrets).
- [Architecture](docs/architecture.md): processes, crates, data paths, milestones.
- [Protocol](docs/protocol.md): the wire format and the stream window's IPC.
- [Decision records](docs/adr/).
- Each milestone's results: `docs/m*-results.md` and `docs/p*-results.md`, with what was measured and what was tried by hand.

## Licence

Couch Co-App is free software under the **GNU General Public License, version 3** (GPL-3.0-only): see [LICENSE](LICENSE). You may use, study, share and change it. If you distribute a modified version, it must stay under the same licence, with its source available.

Bundled components keep their own licences:
- FFmpeg: LGPL v3, dynamically linked;
- SDL3: zlib;
- ViGEmBus: BSD-3-Clause;
- HidHide: MIT;
- the Rust crates and the front end's packages: mostly MIT and Apache-2.0, checked by `cargo deny`.

`cargo xtask notices` gathers them all into `THIRD-PARTY-NOTICES.txt`. Every installer carries it, and the app shows it in Settings → About.
