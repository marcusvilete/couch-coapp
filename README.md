# Couch Co-App

Remote "same couch" gaming. Create a password-protected room, invite friends with a link, stream your game to them, and pass them the controller.

Connections are peer-to-peer (iroh QUIC). A relay starts each connection and carries it when no direct path exists. That relay is our own `iroh-relay` in production, and iroh's public relays in development.

**Status:** in development, Windows only for now. [docs/architecture.md](docs/architecture.md) has the design and the milestone table.

## Setup (Windows 11)

1. **Install the tools:**
   - Rust: `winget install --id Rustlang.Rustup -e` (the version is pinned in `rust-toolchain.toml`);
   - the Visual Studio C++ build tools;
   - Node.js;
   - CMake (`winget install Kitware.CMake`; libopus builds with it);
   - `cargo install cargo-deny --locked`.

   LLVM is needed only to regenerate the FFmpeg bindings.
2. **Open a terminal in the repository** and load the environment. Do this in every new terminal: it puts cargo and the native DLLs on PATH.
   ```powershell
   . .\scripts\dev.ps1
   ```
3. **Fetch the pinned native dependencies** (FFmpeg's LGPL build, SDL3, and the two drivers' installers), and stage them next to the build output:
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

**The installer** (for your other PC, or friends):
```powershell
.\scripts\build-installer.ps1 -TestRelay      # iroh's public relays; target\installer\...-test-setup.exe
npx --yes http-server target\installer -p 8000
```
- **A release installer** (no `-TestRelay`) uses our relay. Its address and token come from a private `release.toml` that never goes in git:
  - `cargo xtask release-config` creates it in `%USERPROFILE%\.couch-coapp\`;
  - [deploy/release.example.toml](deploy/release.example.toml) explains it.
- **The version** must be the same in `Cargo.toml`, `apps/desktop/src-tauri/tauri.conf.json` and `apps/desktop/package.json`; bump it for each installer you hand out.

**Our relay:**
- **On this PC, for testing:**
  ```powershell
  cargo xtask relay-tool        # once: builds the official iroh-relay into target\tools
  .\scripts\relay.ps1           # http://localhost:3340, with the token from release.toml (-Lan, -Open)
  ```
- **On a server:** [deploy/relay/README.md](deploy/relay/README.md) covers choosing one, the first run, and the token.

**The invite page:** a static page that makes invite links clickable in chat apps. To try it on this PC, run `npx --yes http-server deploy/invite-page -p 8080`. [deploy/invite-page/README.md](deploy/invite-page/README.md) covers the rest.

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
- FFmpeg: LGPL, dynamically linked;
- SDL3: zlib;
- ViGEmBus: BSD-3-Clause;
- HidHide: MIT;
- the Rust crates: mostly MIT and Apache-2.0, checked by `cargo deny`.
