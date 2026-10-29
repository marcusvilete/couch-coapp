---
name: linux-checks
description: Build, test and run Couch Co-App on Linux from the Windows PC through WSL (Ubuntu 24.04) and WSLg — syncing the tree into WSL, running commands there without wsl.exe mangling them, clippy and the tests, watching a Windows host from the Linux stream window, running the Linux app, capturing its windows — and the pitfalls already found (dual-stack ports, a fast Windows clock, the relay). Use after changing anything that builds differently on Linux (cfg(windows)/cfg(unix) code, couch-platform-linux, the decoder, couch-viewer, xtask's deps, the shell), before a PR whose Linux CI must pass, when CI's Linux job fails, and for M16 work.
---

# Linux checks (WSL)

M16 ([ADR 0011](../../../docs/adr/0011-linux.md)) builds Couch Co-App on Linux. `.github/workflows/linux.yml` runs clippy, the tests and the front end's checks on every pull request; this is the same on the author's PC, faster to iterate, and the only way to *see* the Linux app and stream window here. It isn't the real thing: WSL has no hardware Vulkan, and a real Linux run is a VM, bare metal or the Steam Deck.

## Once

- Ubuntu 24.04 in WSL (`wsl --install -d Ubuntu-24.04`, the author's to run).
- **System packages, as WSL's root** (no password asked): write `bash /mnt/c/dev/couch-coapp/scripts/linux-deps.sh` into a command file and run it with `wsl.exe -d Ubuntu-24.04 -u root -- bash <that file>`. Node 24 from NodeSource (Ubuntu's 18 is too old for Vite); rustup for the user (the repository's toolchain file picks the version).
- The copy lives on WSL's own disk (`~/couch-coapp`), never built under `/mnt/c`.

## Every time

1. **Sync** the Windows tree into the copy: `wsl.exe -d Ubuntu-24.04 -- bash /mnt/c/dev/couch-coapp/scripts/wsl/sync.sh`. It leaves WSL's own `target/`, `node_modules` and fetched `third_party` folders alone; `.git` isn't copied (edit and commit on Windows only).
2. **Run commands from a file**, never inline: `wsl.exe` splits a PowerShell command line at spaces, quotes and pipes. Write the commands (plain bash, one per line) to a file in the scratchpad, then `wsl.exe -d Ubuntu-24.04 -- bash /mnt/c/dev/couch-coapp/scripts/wsl/run.sh /mnt/c/Users/<user>/.../task.sh`. It runs in the copy with `scripts/dev.sh`'s environment (Linux's FFmpeg bindings).
3. **The checks**, in such a file:
   ```bash
   cargo xtask fetch-deps          # first time, or after a pin changed
   cargo fmt --all --check
   cargo clippy --workspace --all-targets --message-format=short -- -D warnings > /tmp/clippy.log 2>&1; echo "clippy exit $?"
   cargo xtask stage-dlls
   timeout 1800 cargo test --workspace --no-fail-fast > ~/runs/full.log 2>&1; echo "tests exit $?"; grep -E "FAILED|panicked" ~/runs/full.log
   ```
   Long runs go in the background (the Bash or PowerShell tool's own background mode), with a `timeout`: a hang must not run for half an hour (it once did).
4. **Generated files come back by hand.** Anything made in WSL that belongs in the repo (FFmpeg's Linux bindings after `gen-ffmpeg-bindings`) is copied to `/mnt/c/dev/couch-coapp/...` before the next sync, which would delete it.

## Seeing it work (WSLg)

- **The stream window against a Windows host:** WSL can't reach Windows directly (its network address translation and the firewall), so both go through iroh's public relay and then connect directly:
  - on Windows, `target\release\couch-cli.exe host --source testpattern --relay n0`, saving its `TICKET` to a file;
  - in WSL, `target/debug/couch-cli view "$(cat <ticket file>)" --relay n0 --measure-secs 15`.

  Read the `RESULT` line (fps, decode errors, arrival to present) and the stream window's log: "decoding with H.264, NVDEC (copied)" here, where NVIDIA's CUDA reaches WSL; drawing is `llvmpipe` (software Vulkan). Stop the Windows host afterwards.
- **The stream window's bar alone:** `target/debug/couch-viewer --hud-preview watching --size 1280 720` (Lucide's icons and the system's fonts).
- **The app:** `wsl.exe -d Ubuntu-24.04 -- bash /mnt/c/dev/couch-coapp/scripts/wsl/app.sh [profile]` (in the background; log in `~/runs/app.log`, "starting profile=..." when up). Stop it with `pkill -f target/debug/couch-desktop; pkill -f vite`.
- **Screenshots:** capture only WSLg's window: `Get-Process msrdc` names it ("Couch Co-App · linuxtest (Ubuntu-24.04)"), then `PrintWindow` on its handle into a bitmap (as `scripts/viewer-shots.ps1` does). **Never the whole screen while the author is at the PC**; titles can match VS Code too.

## Known pitfalls

- **Dual-stack sockets.** On Linux an IPv6 socket takes IPv4 too: a port probe holding IPv4 while binding IPv6 loops forever. Test ports come from `couch_testkit::net::free_udp_port`, and the scenarios wait out a port an engine restarting on it still holds.
- **Timing races Windows never shows.** Linux surfaced several in the session scenarios (M16a). Run `cargo test -p couch-engine --test rooms --test sessions` several times (8 to 15) after touching them; a failure once in ten is a real race to understand, not noise.
- **WSL's wall clock jumps** by however much Windows' clock is off ("Time jumped backwards" in `journalctl`): WSL pushes Windows' time in, Ubuntu's own sync sets the right one. It can fail the two 1-second invite scenarios. `w32tm /stripchart /computer:time.windows.com /samples:3` shows Windows' offset; syncing Windows' clock fixes it, `wsl --shutdown` doesn't.
- **Disk.** WSL's `ext4.vhdx` (under `%LOCALAPPDATA%\wsl`) grows with every build and doesn't shrink; with `target\debug` on Windows it once filled C:. Check free space before long runs.
- **Clippy differs per system:** code only Windows uses shows as dead on Linux. Prefer a `#[cfg(windows)]` on the item; `#[cfg_attr(not(windows), allow(dead_code, reason = "..."))]` where the item must exist on both.

## After

- Report what ran and the numbers; a CI failure's log comes from the author (they paste the failing step).
- Results go in the milestone's results page (`docs/m16*-results.md`).
