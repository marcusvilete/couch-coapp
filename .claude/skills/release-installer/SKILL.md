---
name: release-installer
description: Build and hand out a Couch Co-App Windows installer with `scripts\build-installer.ps1`, and the Linux AppImage with `scripts/build-appimage.sh` in WSL: when a version bump is needed (the wire revision, the three version files), building from a merged, clean, pushed main, release versus test builds, checking which relay and invite page it carries without printing the relay's token, serving the files, and what to tell the people who install it and record afterwards. Use when asked for an installer or an AppImage, a new version to hand out, a test build for another PC, or before a run with other people.
---

# A release installer

There are no public binaries (ADR 0009): installers are built from source and handed to people the builder knows, unsigned. A release installer carries the builder's relay URL and token and the invite page's address, baked in at build time from the private `release.toml` (`%USERPROFILE%\.couch-coapp\release.toml`, never in the repository; `cargo xtask release-config` creates it, `deploy/release.example.toml` shows its shape). `docs/self-hosting.md` is the human guide to the same steps.

## 1. Is a new version needed?

- **Everyone in a session must run the same wire revision** (`couch_proto::PROTOCOL_MINOR`). Builds of different revisions refuse each other right after `Hello`, and the app says who needs to update. So a `PROTOCOL_MINOR` change since the installer people have means everyone needs the new one.
- **Every installer handed out has a version not handed out before:** people tell builds apart by it (Settings → About, and the "needs update" line).
- **Normally there's nothing to do:** every PR that changes the app bumps the version itself (CLAUDE.md, "Versions": `cargo xtask bump patch` or `minor`), so a merged `main` already carries a new one. Check it against the last installer handed out (`docs/architecture.md`'s version line, under "Installing").
- **If it was handed out already** (a change merged without its bump): bump in a PR of its own first, `cargo xtask bump patch` (or `minor` after a new wire revision), and say in that version line what it carries. The command sets the same version in `Cargo.toml`, `tauri.conf.json`, `apps/desktop/package.json` and our crates in `Cargo.lock`; the build script refuses mismatched files anyway. `apps/desktop/package-lock.json`'s own version isn't checked; leave it.
- A new wire revision always comes with a minor version (`couch_proto::REVISIONS`, held by a test).

## 2. Build from main, merged and clean

- **A release installer needs a clean, committed tree** (the script refuses otherwise): its notices link the app's source at that commit. Build from the merged `main` (`git pull --ff-only`), with `git status --short` empty.
- **That commit must be on the remote** (the script warns otherwise), or people can't reach the source the notices link to.
- **Nothing of ours running:** `Get-Process couch-*, "Couch Co-App" -ErrorAction SilentlyContinue`. A running `couch-cli`, stream window or app holds files the build replaces. Ask the user to close them rather than stopping them.
- `release.toml` exists: `Test-Path "$env:USERPROFILE\.couch-coapp\release.toml"`. Never print or open it in the conversation: it holds the relay's token.

## 3. Run it

From the repository's root, in PowerShell:

```powershell
. .\scripts\dev.ps1
& (Join-Path (git rev-parse --show-toplevel) 'scripts\build-installer.ps1')
```

- **Call it by its full path** (built as above, so it works from any clone), **and never redirect its output** (no `2>&1`, no piping the live run): it prints progress and warnings on stderr, and Windows PowerShell turns redirected stderr into errors. Run it in the background and read its output once it ends; it takes a few minutes (fetching and staging the pinned dependencies and drivers, the notices, a release build, NSIS).
- Options:
  - `-TestRelay`: a `-test` installer on iroh's public relays, from any tree (a dirty one warns that its notices ask people to request the source). For trying a branch on another PC, not for regular use.
  - `-InvitePage <https://…/j/>` sets the invite page for this build (test builds included); `-NoInvitePage` gives `couchco://` links.
  - `-Config <file>`: another release configuration (a LAN test one, say).

## 4. Check what it carries

From its output, leaving out any line containing "token":
- `Our relay: <relay_url from release.toml>`, or the test line for `-TestRelay`;
- `Invites link to <the invite page>`, or that invites are `couchco://` links;
- `Built Couch-Co-App_<version>_<commit>-setup.exe (… MB) in …\target\installer, with FFmpeg's sources and SOURCES.txt.`

`target\installer\` then holds the setup file, FFmpeg's pinned source and build scripts, and `SOURCES.txt`. They go out together: the LGPL asks for FFmpeg's source beside the binary.

## 5. Hand it out

- Serve the folder: `npx --yes http-server target\installer -p 8000`, and give people `http://<this PC>:8000/`, or send all the files another way.
- **What to tell the people installing it:**
  - Windows SmartScreen warns, since it's unsigned: "More info", then "Run anyway".
  - It asks to install for just them or for everyone. Afterwards it offers **ViGEmBus** (yes if they'll host games, so guests' controllers reach them) and **HidHide** (optional: lets the host's own controller play through a seat; needs a restart). A driver already installed isn't offered again.
  - The first session they host on a PC can take a while: Windows sets up each virtual controller the first time (P10). The host console says so, with a timer and Stop waiting; the controllers come up by themselves, and later sessions start at once.
  - Everyone in a session must update: an older build is refused.
  - If something goes wrong, the log is `%LOCALAPPDATA%\CouchCoApp\profiles\default\desktop.log`.
- The host's own log names each viewer's encoder (`encoding WxH with hevc_nvenc …`), which answers questions like "which codec did they get?" afterwards.

## The AppImage, for friends on Linux (M16b)

A Linux PC or a Steam Deck watches and plays, and is a remote-play device; since M16c-1 a Linux PC also hosts with controller seats, on the system's own PipeWire (the build fails if `libpipewire` ends up inside). So the AppImage needs `libpipewire-0.3` on the system to start at all, even to watch: tell friends on a minimal system. `cargo xtask appimage` (run by `scripts/build-appimage.sh`) is `build-installer.ps1`'s twin, with the same checks and options spelled `--test-relay`, `--config <file>`, `--invite-page <address>`, `--no-invite-page`.

- **Same commit as the installer:** build both from the same merged main, so one version goes out.
- **In WSL, from its own git clone, `~/couch-release`** (`git clone https://github.com/marcusvilete/couch-coapp ~/couch-release`, then `git -C ~/couch-release pull --ff-only`). Never the `sync.sh` copy, which has no `.git`, and never sync into the clone for a release: Windows' CRLF files make every file differ, so the tree is dirty. The `linux-checks` skill has the WSL basics; first time in the clone, `(cd apps/desktop && npm ci)` and `cargo install --locked cargo-about --features cli`.
- **Run it from a command file with `wsl.exe -d Ubuntu-24.04 -- bash <file>`** (not `run.sh`, which goes to the copy), in the background, with a `timeout`:
  ```bash
  cd ~/couch-release
  export COUCH_RELEASE_CONFIG=/mnt/c/Users/<user>/.couch-coapp/release.toml
  timeout 3500 bash scripts/build-appimage.sh > ~/runs/build-appimage.log 2>&1; echo "exit $?"
  ```
  Never print or open that file. It builds, bundles once, writes the notices from what the bundle carries (the AppImage's runtime and AppRun, and every system file traced to its Ubuntu package with `dpkg -S`), and bundles again with them inside.
- **Check its output** as for the installer (leave out lines with "token"): `Our relay: …`, `Invites link to …`, `The AppImage carries N system file(s) from M Ubuntu source package(s)`, and `Built Couch-Co-App_<version>_<commit>.AppImage (… MB) in …/target/appimage`.
- **It fails rather than leave anything out:** a file in the AppImage no package installed, or a runtime that names other parts than the texts pinned in `xtask/licences/appimage/` (then fetch and pin the new ones).
- **Try it once in WSLg** before handing it out: `COUCH_PROFILE=appimagecheck target/appimage/Couch-Co-App_*.AppImage`; Settings → About shows the version, the commit and the notices.
- **Hand out all of `target/appimage/`** (the AppImage, the notices, FFmpeg's archives, `SOURCES.txt`): copy it to Windows (`rm -rf /mnt/c/dev/couch-coapp/target/appimage && cp -r ~/couch-release/target/appimage /mnt/c/dev/couch-coapp/target/`) and serve it like the installer.
- **What to tell them:** make it executable (Properties, or `chmod +x`), then open it; without FUSE, `--appimage-extract-and-run`; on a Steam Deck, Desktop Mode, and for Game Mode add it as a non-Steam game; it needs glibc 2.39 or newer; it adds itself to the app menu at start (the panic key needs that entry); invite links work once it has started once; the log is `~/.local/share/CouchCoApp/profiles/default/desktop.log`.

## 6. Afterwards

- Note which version people now have, and what the next run should try: the milestones' "still to come" lines in their results pages.
- After the run, record what was seen in those results pages, in a section of its own, through the usual branch.
