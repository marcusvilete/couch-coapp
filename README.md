# Couch Co-App

Remote "same couch" gaming. Create a password-protected room, invite friends with a link, stream your game to them, and pass them the controller. Connections are peer-to-peer (iroh), with a self-hosted relay as fallback.

**Status:** early development, Windows first. See [docs/architecture.md](docs/architecture.md) for the design and milestone plan.

## Setup (Windows 11)

1. Install Rust: `winget install --id Rustlang.Rustup -e`. Also install Node.js and the Visual Studio C++ build tools.
2. Fetch the pinned native dependencies (FFmpeg LGPL build, SDL3) and put their DLLs next to the build output:
   ```powershell
   cargo xtask fetch-deps
   cargo xtask stage-dlls
   ```
3. Install the front-end packages: `cd apps/desktop; npm install`.
4. Check everything: `cargo xtask doctor`.
5. From M2 on, virtual gamepads need ViGEmBus 1.22.0: `winget install --id ViGEm.ViGEmBus -e --version 1.22.0` (admin).

In a new terminal, run `. .\scripts\dev.ps1` first. It puts cargo and the native DLLs on PATH.

## Everyday commands

```powershell
cargo test --workspace                       # all tests; no GPU or drivers needed
cargo run -p couch-cli -- probe              # what this machine can do (build couch-viewer first)
cd apps/desktop; npm run tauri dev           # the desktop app

# Streaming spike (M1): build both binaries, then measure end-to-end latency on this machine
cargo build --release -p couch-cli -p couch-viewer
.\target\release\couch-cli.exe latency-probe --secs 30 --relay none
```

More are in [CLAUDE.md](CLAUDE.md#commands).

## Documentation

- [Architecture](docs/architecture.md): processes, crates, data paths, safety rules
- [Protocol](docs/protocol.md): wire format and viewer IPC
- [M1 results](docs/m1-results.md): the streaming latency and network gates
- [Decision records](docs/adr/)
