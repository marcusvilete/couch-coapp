# M10b results: the public source release

M10b was planned as a public binary release. Planning its first part, the licence notices, led you to a new direction ([ADR 0009](adr/0009-source-only-releases.md)):
- **no public installers, ever.** Installers go to friends only.
- **Anyone else builds from source** and hosts their own relay.

So M10b became the public source release:
- the repository public on GitHub;
- the notices the licences require, in friends' installers;
- docs for a stranger to build the app and run a relay.

**Dropped:** M11 (the updater), code signing, the wire-format freeze, a relay for the public.
**Optional:** the fallback encoder, an outside review of the admission composition.

The licence came first: **GPL-3.0-only** ([ADR 0008](adr/0008-gpl-3.0.md)).

## What it does

**The roadmap and rules**
- **ADR 0009** records the new direction.
- **The milestone table:** M10b is renamed, and M11 is marked dropped.
- **The wire format is never frozen.** `PROTOCOL_MINOR` stays the wire revision for good. Payloads may change in place with a bump; builds of different revisions refuse each other with "update needed", and the ALPN stays `couch-coapp/0`. The rules changed with it: CLAUDE.md, `protocol.md`, the `protocol-change` skill and couch-proto's comments.

**The notices** (`cargo xtask notices` → `THIRD-PARTY-NOTICES.txt`, about 520 KB)
- **The Rust crates** linked into the two binaries, `couch-desktop` and `couch-viewer`. They come from [cargo-about](https://github.com/EmbarkStudios/cargo-about) 0.9.2 (`about.toml`), on the Windows target, without build or dev dependencies.
  - Today that's 462 crates under 268 licence texts, most used first. libopus comes in through `opusic-sys`; egui's fonts bring the OFL and the Ubuntu font licence.
  - It fails if any crate cargo-about lists has no licence text.
- **The front end's runtime npm packages** (`npm ls --omit=dev`), each with its own licence file: `@tauri-apps/api`, React, React DOM, and `scheduler`.
- **What isn't a crate or a package:**
  - FFmpeg: its LGPL licence (v3; the build uses `--enable-version3`), its full configure line, which names the libraries inside the DLLs, and a note that the DLLs may be replaced;
  - SDL3: zlib;
  - the two drivers.
- **The header:**
  - the version and commit;
  - the GPL notice ("version 3 only", no warranty);
  - where the app's source is: the repository at that commit, once `Cargo.toml`'s `repository` is set; otherwise "ask whoever gave you this installer";
  - where FFmpeg's source is.

**The source offer**
- **FFmpeg's source and build scripts are pinned** in `deps::SOURCES` (`fetch-deps` downloads them to `third_party\sources\`):
  - FFmpeg at the build's commit, `29e619e767cde904…` (16 MB);
  - BtbN's FFmpeg-Builds at the build's tag (0.1 MB): the scripts that pin and build every library inside the DLLs.
- **`build-installer.ps1` puts them beside the setup file,** with `SOURCES.txt`, which explains each file, with its SHA-256, and gives the app's source address.
- **A release build refuses uncommitted changes:** the linked commit must match the installer exactly. A test build only warns.
- **Either kind warns when the commit isn't pushed yet.**

**In the app**
- **Settings → About** shows:
  - the version and commit ("a development build" without one);
  - the GPL in plain words;
  - where the source is, selectable for copying;
  - the notices in a scrollable box.
- **Behind it:** `Command::GetNotices` reads the file next to the exe. A missing file says how to make it. `EngineInfo` gains `commit` (from `COUCH_BUILD_COMMIT`) and `repository`.
- **The installer** puts `LICENSE` and `THIRD-PARTY-NOTICES.txt` next to the app, and its uninstaller removes them.

**The guide for strangers**
- **README:**
  - "There are no official downloads", with the three steps;
  - cargo-about among the tools;
  - a new "Your own relay" section: `release-config`, the server, `relay_url`, the release installer, and what to hand out.
- **`deploy/relay/README.md`:** "Your own relay", for anyone. A worked example for Oracle Cloud's free tier covers:
  - the VCN wizard, because the instance form won't give a public IP;
  - the security list, with each port's description;
  - the Micro shape and its long build;
  - `tmux`;
  - Oracle's iptables rules;
  - reclaiming;
  - the log's scanner noise.

## Checks

| Check | What | Result |
|---|---|---|
| `cargo xtask notices` | 462 crates under 268 texts, 4 npm packages, FFmpeg, SDL3 and the drivers; the completeness check passes; the fonts' and libopus's licences are present | pass |
| FFmpeg sources | Resolved from the build's name; downloaded twice, byte-identical; pinned; `fetch-deps` checks them | pass |
| Test installer build | `target\installer\` has the setup file, both archives and `SOURCES.txt`; the NSIS script installs `LICENSE` and the notices and uninstalls them; the dirty-tree and not-pushed warnings show | pass |
| Release build from a dirty tree | Refused before building, with the reason | pass |
| Engine | `the_notices_are_read_and_a_missing_file_says_how_to_make_it`; the bindings carry `getNotices`, `notices`, `commit` and `repository` | pass |
| Workspace | All tests, clippy (all targets), fmt, `cargo deny`, `tsc` | pass |
| Your run | Settings → About in a development build: the version, the GPL lines and the notices box all work. The test installer installs and works; its Network tab defaulted to iroh's public relays, as a test build should, but said "a development build has no relay of its own": now "this build has no relay of its own" | pass |

## Your run

1. **A development build:** `npm run tauri dev`, then Settings → About.
   - It shows "Couch Co-App 0.9.0 (a development build)" and the GPL lines.
   - The notices box starts with the header, then FFmpeg; scroll to the crates. They come from `target\debug`, where `stage-dlls` put them.
2. **Optional: the test installer,** `target\installer\…-test-setup.exe`. After installing, the install folder has `LICENSE` and `THIRD-PARTY-NOTICES.txt`, and About shows "(commit 696d7a2c69-dirty)".

## Left in M10b

- **Going public:**
  - create the GitHub repository;
  - set `repository` in `Cargo.toml`, so the notices and About link to it;
  - the secrets scan again;
  - push;
  - build friends' installers from the pushed commit.
- **Optional:** the fallback encoder, the crypto review package, the invite page online.
