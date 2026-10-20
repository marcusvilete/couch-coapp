---
name: release-installer
description: Build and hand out a Couch Co-App Windows installer with `scripts\build-installer.ps1`: when a version bump is needed (the wire revision, the three version files), building from a merged, clean, pushed main, release versus test builds, checking which relay and invite page it carries without printing the relay's token, serving the files, and what to tell the people who install it and record afterwards. Use when asked for an installer, a new version to hand out, a test build for another PC, or before a run with other people.
---

# A release installer

There are no public binaries (ADR 0009): installers are built from source and handed to people the builder knows, unsigned. A release installer carries the builder's relay URL and token and the invite page's address, baked in at build time from the private `release.toml` (`%USERPROFILE%\.couch-coapp\release.toml`, never in the repository; `cargo xtask release-config` creates it, `deploy/release.example.toml` shows its shape). `docs/self-hosting.md` is the human guide to the same steps.

## 1. Is a new version needed?

- **Everyone in a session must run the same wire revision** (`couch_proto::PROTOCOL_MINOR`). Builds of different revisions refuse each other right after `Hello`, and the app says who needs to update. So a `PROTOCOL_MINOR` change since the installer people have means everyone needs the new one.
- **Bump the version for every installer handed out,** whatever changed: people tell builds apart by it (Settings → About, and the "needs update" line). Bump it the same in all three: `Cargo.toml` (`[workspace.package] version`), `apps/desktop/src-tauri/tauri.conf.json` and `apps/desktop/package.json`; the script refuses mismatched ones. `Cargo.lock` follows on the next build. `apps/desktop/package-lock.json`'s own version isn't checked; leave it.
- A minor bump (0.11.0 → 0.12.0) when the wire revision changes or a milestone lands; a patch (0.11.0 → 0.11.1) for fixes alone. Record each in `docs/architecture.md`'s version line (under "Installing").
- The bump is a commit like any other, through the project's usual branch and review.

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
  - It asks to install for just them or for everyone. Afterwards it offers **ViGEmBus** (yes if they'll host games, so guests' controllers reach them) and **HidHide** (optional: lets the host's own controller play through a seat; needs a restart).
  - Everyone in a session must update: an older build is refused.
  - If something goes wrong, the log is `%LOCALAPPDATA%\CouchCoApp\profiles\default\desktop.log`.
- The host's own log names each viewer's encoder (`encoding WxH with hevc_nvenc …`), which answers questions like "which codec did they get?" afterwards.

## 6. Afterwards

- Note which version people now have, and what the next run should try: the milestones' "still to come" lines in their results pages.
- After the run, record what was seen in those results pages, in a section of its own, through the usual branch.
