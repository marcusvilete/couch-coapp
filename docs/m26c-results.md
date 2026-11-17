# M26c results: public releases

## Why

Until now only the builder could make a release: a release build carries its builder's relay and token (`release.toml`), so every update was an installer built by hand on the maintainer's PC. [ADR 0014](adr/0014-public-builds-and-registered-relays.md) settled public builds made by GitHub Actions, with no secret in them, since M26a lets anyone register a relay in the app.

**Your choices (2026-11-17,** in the ADR's "Settled for M26c"):
- three parts: M26c-1 the public build kind and the release workflow; M26c-2 the update notice and its switch, the version mismatch's link and Report a problem; M26c-3 the invite page's download and relay note, and the docs. The first public tag comes after M26c-2, so its users hear of the next version;
- the workflow tried by hand, into the run's artifacts, with no release; only a tag `v<version>` makes the draft; no pre-release tags;
- no antivirus submission, no virus scan and no signing for now; no SmartScreen text in the release notes until your README rewrite;
- the update notice in the shell's bottom strip, after network trouble;
- public builds check for new versions; friends' builds only with `releases` in `release.toml`; test and development builds never;
- the AppImage's system libraries' sources attached beside it, rather than a written offer (the review's finding, below).

## M26c-1: the public build kind and the release workflow (0.38.1)

**What changed**
- **The build kind** (`couch_engine::build_config`): `COUCH_BUILD_PUBLIC=1` makes `public_build()`, never with a test build, and `relay()` is none in it whatever its environment holds. `COUCH_BUILD_RELEASES` (`owner/name`, checked as GitHub allows) is `releases()`, the repository the update notice will check (M26c-2), never in a development or test build. Copy diagnostics says "public build" (`kind()`).
- **The scripts:** `build-installer.ps1 -Public` and `build-appimage.sh --public` (`cargo xtask appimage`) need no `release.toml`, refuse a dirty tree as a release does, and name the packages by their version alone (`Couch-Co-App_<version>_x64-setup.exe`, `Couch-Co-App_<version>_amd64.AppImage`). A public build checks the workspace `repository`'s releases (a repository not on GitHub is refused); `-Releases <owner>/<name>` (`--releases`) sets another for one build, `''` none; `release.toml` takes an optional `releases`. The two scripts follow the same rules, and the Windows build now refuses a tree the build changed, as the AppImage's did.
- **`cargo xtask release-notes`** refuses a tag that isn't `v` and the version in `Cargo.toml`, `tauri.conf.json` and `package.json`, and writes the notes: first `<!-- couch-wire-revision: N -->`, which the update notice will read, then the downloads, how to check them against `SHA256SUMS`, and the source at the tag and the licences.
- **`.github/workflows/release.yml`:** a `check` job (the workspace `repository` is the repository running it, so a fork's links aren't upstream's; on a tag, the tag is on `main`; the notes), then the Windows installer (`windows-2025`) and the AppImage (`ubuntu-24.04`) built as by hand, then on a tag only a `draft` job, the only one that may write: both packages, `SHA256SUMS`, each package's notices and `SOURCES.txt` named by system, FFmpeg's archives once (the two packages' compared), and the system libraries' sources, into a **draft** release, refused if one exists for that tag. No secret. Run by hand, it builds the same into the run's artifacts.
- **The AppImage's system libraries' sources** (`xtask/src/system_sources.rs`): every AppImage build fetches each Ubuntu source package its notices name, at that exact version, from Launchpad (which keeps every version, where Ubuntu's archive drops superseded ones), checks each file against its `.dsc`'s SHA-256, and packs them with a README into `…-system-sources.tar` beside the AppImage. About 490 MB; cached in `third_party/_downloads`.
- **Elsewhere:** `scripts/dev.ps1` keeps a runner's PATH (the registry doesn't have what the job's setup steps added); `.gitattributes` checks tauri's generated permission files out with LF (below); ADR 0014's amendment; CLAUDE.md, `docs/development.md` and the `release-installer` skill.

**Checked**
- `cargo fmt --check`, `cargo clippy --workspace --all-targets` and `cargo test --workspace` on Windows; `scripts/check-docs.sh`; Linux's CI on the PR.
- The scripts' refusals by hand (two kinds at once, a releases address for a test build, a URL in place of `owner/name`, a trailing line break).
- `release-notes` against `v0.38.0` (written) and `v0.39.0` (refused).
- A public installer built here: the exe holds "public build", `marcusvilete/couch-coapp` and the invite page, and nothing of our relay's name.
- **The workflow, on the branch** (a temporary push trigger, since GitHub runs a workflow by hand only once it is on `main`; removed before the PR):
  - run 1: both packages built (Windows 20.5 min, Linux 19 min, nothing cached);
  - run 2, with the review's fixes and the sources: the AppImage's sources fetched and packed (the Linux artifact 645 MB, from 156); **the Windows build refused its tree**;
  - run 3 named the files: tauri-build rewrites `permissions/autogenerated/*.toml` with LF on every build, and GitHub's Windows runners check out with CRLF (`core.autocrlf`), so git took them for changed. Reproduced in a clone; this PC never showed it, its copies being LF since they were first written. `.gitattributes` now checks them out with LF;
  - run 4: both packages built and kept as artifacts (Windows 10.6 min with the cache warm), the notes beside them. Downloaded and read: the notes (the wire marker first, the source at the commit), both `SOURCES` files and notices naming that commit and its source link, FFmpeg's two archives identical in both packages and matching their pins, and the sources tar: 98 source packages (323 files, 515 MB, the largest gcc-14's and webkit2gtk's), exactly the 98 the notices name.

**The review** (a fresh agent over the first commit): nothing critical or high. Fixed: a fork's notes, notices and releases address would have pointed at upstream (the `check` job now refuses it); a tag not on `main` would have released unreviewed code; a run again would have made a second draft; a public build's relay rested on the scripts alone; the Windows build didn't check that it left its tree as committed (which then found the line endings above); the scripts disagreed on `-Releases ''` and a repository not on GitHub; tag builds warned of no remote branch; the Linux notices beside a public AppImage are named for the release page; the docs spoke of the update notice as built; ADR 0014's SmartScreen lines now point at your README decision. **Raised for you:** the AppImage's LGPL system libraries were named but not offered from the same place; you chose to attach their sources (above). Left: the two copies of the repository check (Rust's and the script's) are kept alike by their tests, not shared.

**Still to come in M26c**
- M26c-2: the update notice and its switch (Settings → App), the version mismatch's link to the release, Report a problem.
- M26c-3: the invite page's download and relay note; `self-hosting.md` rewritten, the README below your note.
- Install the workflow's own installer once (Settings → About, Copy diagnostics: "public build", the public relays) and run its AppImage in WSLg.
