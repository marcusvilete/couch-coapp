---
name: linux-checks
description: Build, test and run Couch Co-App on Linux from the Windows PC through WSL (Ubuntu 24.04) and WSLg — syncing the tree into WSL, running commands there without wsl.exe mangling them, clippy and the tests, watching a Windows host from the Linux stream window, running the Linux app, capturing its windows — and the pitfalls already found (dual-stack ports, a fast Windows clock, the relay). Use after changing anything that builds differently on Linux (cfg(windows)/cfg(unix) code, couch-platform-linux, the decoder, couch-viewer, xtask's deps, the shell), before a PR whose Linux CI must pass, when CI's Linux job fails, and for M16 work.
---

# Linux checks (WSL)

M16 ([ADR 0011](../../../docs/adr/0011-linux.md)) builds Couch Co-App on Linux. `.github/workflows/linux.yml` runs clippy, the tests and the front end's checks on every pull request; this is the same on the author's PC, faster to iterate, and the only way to *see* the Linux app and stream window here. It isn't the real thing: WSL has no hardware Vulkan, and a real Linux run is a VM, bare metal or the Steam Deck.

Writing the Linux platform code itself (D-Bus, the portals, PipeWire, `uinput`, the KWin script and GNOME extension): the `linux-platform-pitfalls` skill. A real desktop: `linux-host-vm-run`.

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
- **WSL's wall clock jumps** by however much Windows' clock is off ("Time jumped backwards" in `journalctl`): WSL pushes Windows' time in, Ubuntu's own sync sets the right one. It failed the two 1-second invite scenarios until they waited for expiry rather than checking once (`rooms.rs`'s `until_expired`, after M27b): any test of something timed by the wall clock must wait for it, never check once after a sleep. `w32tm /stripchart /computer:time.windows.com /samples:3` shows Windows' offset; syncing Windows' clock fixes it, `wsl --shutdown` doesn't.
- **Disk.** WSL's `ext4.vhdx` (under `%LOCALAPPDATA%\wsl`) grows with every build and doesn't shrink; with `target\debug` on Windows it once filled C:. Check free space before long runs.
- **Virtual controllers (`uinput`) in WSL:** the kernel has `uinput` and `evdev` as modules, not loaded, and no udev gives the user access. As root, `modprobe uinput evdev`; then run the test binary as root (build it as the user with `cargo test -p couch-platform-linux --no-run`, then the printed executable `--include-ignored`), or `couch-cli pad-test` as root, and watch with `evtest` (installed). Root-only, for checks: never leave `/dev/uinput` opened to everyone. The portal (global shortcuts, screen sharing) isn't in WSL at all: that's the VM's.
- **The "in front" helpers in WSL** (M16c-2 part 2): WSLg runs neither KWin nor GNOME Shell, so `front-probe` finds no helper there. The engine's side of the D-Bus path runs against a stand-in KWin on a private session bus: `dbus-run-session -- cargo test -p couch-platform-linux front -- --include-ignored` (its first portal request activates `xdg-desktop-portal`, whose warnings are noise). The real helpers need the VM (`linux-host-vm-run`).
- **logind in WSL** (part 5's home PC): processes started through `wsl.exe` are in no logind session. So `user/self` and `session/auto` are unknown objects (the code finds the user by id, `GetUser`), and polkit refuses a sleep inhibitor (`allow_any` is no; `systemd-inhibit` itself says "Access denied"). Check the inhibitor as root: `--include-ignored awake` with the test binary, as for `uinput` above. `--include-ignored session_lock` reads `LockedHint` as the user. The inhibit portal isn't in WSL: the VM's.
- **PipeWire in WSL** (M16c): installed for checks only (`pipewire pipewire-bin wireplumber gstreamer1.0-pipewire gstreamer1.0-tools gstreamer1.0-plugins-base`, as root), started as the user's service (`systemctl --user start pipewire wireplumber`; WSL runs systemd). For the capture tests: a null output (`pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=couch-test-sink media.class=Audio/Sink object.linger=true audio.position=[FL FR] }'`), a sine into it (`gst-launch-1.0 audiotestsrc is-live=true ! audio/x-raw,rate=48000,channels=2 ! pipewiresink`) and a red test source (`gst-launch-1.0 videotestsrc pattern=red is-live=true ! video/x-raw,format=RGBx,width=640,height=480,framerate=30/1 ! pipewiresink mode=provide stream-properties="props,node.name=couch-test-video,media.class=Video/Source"`), each in the background; its node id from `pw-cli ls Node` goes in `COUCH_TEST_VIDEO_NODE`; then the test binary `--ignored screencast::tests sound::tests::what_plays`. With nothing else playing (stop that sine first), `sound::tests::ours_is_left_out_and_one_program_is_taken` (M16c-1 part 4) plays sines of its own through `pw-play` (GStreamer's `pipewiresink` stalled under the churn): one as our child, which must not be heard, one of another program's, heard and picked by its name; run it alone, never beside another sound test. **PulseAudio** (the chime's path): `pipewire-pulse` and `pulseaudio-utils` are installed in WSL for the check; WSLg points `PULSE_SERVER` at its own server, so set `PULSE_SERVER=unix:$XDG_RUNTIME_DIR/pulse/native`, then `--ignored sound::tests::a_pulse_client_of_ours_is_left_out`. `pw-cli info <id>` shows an object's own props, which the registry's snapshot (`pw-cli ls`) may lack. Our links go with the capture: `pw-link -l` shows none of ours afterwards. **Notifications** (part 4): `dunst` is installed in WSL for the check (`setsid dunst &` in the WSLg session; `busctl --user list | grep Notif` shows it), then `--ignored notices::tests`. `couch-cli host --source testpattern --audio system` streams the sine to a Windows viewer. Never a Linux stream window here while that runs: its sound would go into the null output and back. The portal isn't here: the VM's.
- **The AppImage builds from its own clone, `~/couch-release`,** never this copy (no `.git`); the `release-installer` skill has the procedure. To try a branch there, sync into it (`COUCH_WSL_COPY=~/couch-release` before `sync.sh`): Windows' CRLF files make the tree dirty, so only `--test-relay` builds work that way.
- **`couch-media`'s test binary sometimes dies at its start in the whole workspace's run** (`std`'s `sys/random/linux.rs` panicking, "failed to fill whole buffer", before any test runs; seen in P18's and the session before it), and `cargo test` then stops there, leaving the crates after it unrun. Run with `--no-fail-fast`, then that crate alone (`cargo test -p couch-media --lib`: its 91 pass). WSL's, not the code's; CI on real Ubuntu doesn't show it.
- **A branch's test AppImage** builds from `~/couch-release` with the branch as a patch: `git format-patch main --stdout` (committed) or `git diff --binary` (uncommitted) on Windows, `git apply` (or `git -c user.name=… -c user.email=… am`: the clone has no identity) in the clone on main, then `build-appimage.sh --test-relay`; afterwards `git checkout -- .` there. Copy it into a running VM as a new name and `mv` it over (writing over the file a running app comes from can crash it).
- **Clippy differs per system:** code only Windows uses shows as dead on Linux. Prefer a `#[cfg(windows)]` on the item; `#[cfg_attr(not(windows), allow(dead_code, reason = "..."))]` where the item must exist on both.

## After

- Report what ran and the numbers; a CI failure's log comes from the author (they paste the failing step).
- Results go in the milestone's results page (`docs/m16*-results.md`).
