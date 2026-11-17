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

## M26c-2: the update notice, the mismatch's link, Report a problem (0.39.0)

**Your choice (2026-11-17):** trying the notice before there are two public releases (GitHub's "latest" ignores drafts and pre-releases) through `COUCH_RELEASES_URL`, honoured by development and test builds alone, and a copy of GitHub's reply in the repository.

**What changed**
- **The look** (`couch_engine::updates`): a build that names its releases (`build_config::release_source`: a public build, or a friends' build with `releases`) reads GitHub's latest-release API 3 minutes after start, at most once a day (an hour after a failure), never while hosting or watching (the room actor's word, `Input::Busy`), and never with the switch off. The reply is read up to 1 MiB within 30 s: its tag (`v` and a version, never a draft or a pre-release) and its notes' first line, the wire revision (`couch_proto::RELEASE_WIRE_MARKER`, now shared with `cargo xtask release-notes`). What it found, when, and the version dismissed are kept in `updates.json`, never backed up.
- **The request** (`couch_net::web`): the app's only HTTP of its own, through the reqwest iroh already brings (no new crate), on the TLS roots and crypto iroh's relays use; https only (plain http only for the stand-in), three redirects at most, the user agent `couch-coapp`, nothing of the user's.
- **Opening a page** (`Command::OpenPage`, `Backend::open_page`): the app had no way to open the browser. The webview names a page (`WebPage::NewVersion`, `Releases`, `ReportProblem`), never an address: the engine makes each from the build's repository (`<repo>/releases/tag/v<version>`, `/releases/latest`, `/issues/new?template=problem.yml`), taking of GitHub's reply only the version its tag names once it parsed as numbers and dots. Windows opens it through the shell (`couch_platform_win::web`, https addresses only), Linux through the OpenURI portal (`couch_platform_linux::web`), like Open the log folder.
- **The notice:** in the shell's bottom strip when nothing is wrong with the network (trouble keeps the strip), "Couch Co-App X is out.", with "Everyone in your rooms needs it to play together." when the release's wire revision isn't ours; See the release, and a dismiss that holds until a newer version (`Command::DismissUpdate`, `Event::Updates`).
- **Settings → About** (there is no App section: the tray's switch lives in Remote play): "Tell me about new versions", on by default, saying it asks GitHub, which sees the PC's IP address; and Report a problem, opening the issue form `.github/ISSUE_TEMPLATE/problem.yml`, which asks for Copy diagnostics (from Settings → Network) and warns that the log may name rooms and people. Neither shows in a build without releases.
- **The version mismatch:** a member on a newer version than ours ("Update yours") has "Get the new version" in their menu, opening the latest release, where the build names its releases; it needs no look and shows with the switch off.
- **Trying it:** `COUCH_RELEASES_URL` (development and test builds only, read at run time) stands in for GitHub's address: looked at 10 s after start, every start; the pages are then the workspace repository's. `crates/couch-engine/tests/fixtures/github-latest-release.json` is a copy of GitHub's reply for 0.99.0 on wire revision 99, read by the unit tests, served by `npx --yes http-server crates/couch-engine/tests/fixtures -p 8000`.
- **Elsewhere:** `app.json` gained `updates` (a file from before reads as on, and the tray's switch no longer rewrites it away); the preview's scenarios `update-notice`, `update-wire`, `update-and-trouble`, `update-none`, and shots of About and the member's menu with and without releases; `build_config`'s module comment, garbled in M26c-1, mended.

**Checked**
- Windows: `cargo fmt --check`, `cargo clippy --workspace --all-targets`, `cargo test --workspace` (1034 passed, 0 failed, 12 ignored, 44 binaries).
- Linux (WSL, Ubuntu 24.04): fmt, clippy, `cargo test --workspace` (991 passed, 0 failed, 16 ignored), the front end's `tsc` and `vitest` (118).
- **GitHub itself:** `reads_githubs_real_reply` (ignored by default, needs internet) read a real latest release over TLS through `couch_net::web`: the reqwest iroh brings works with iroh's TLS config. Ours has no release yet, so it reads the bundled ViGEmBus driver's repository.
- **The whole look** (`a_look_waits_while_busy_then_tells_the_ui`): a loopback server serving the fixture, an actor busy for its first three answers: nothing asked of the server until it isn't, one request, then `Event::Updates` naming 0.99.0 on another wire revision; nothing written to the profile.
- The request (`couch_net::web`'s tests): a reply over the limit refused, a server's status said, plain http refused unless allowed, a redirect to another host not followed. Windows' `web::open` refuses anything but an https address (a file, a program name, quotes, spaces).
- The front end: `npx tsc --noEmit`, `npx vitest run` (118, `newVersionText` among them), `npm run shots` over every scenario in three looks and three sizes. Looked at: the strip in each look (the notice; with "Everyone in your rooms needs it"; trouble first while offline), About with and without releases, and the member's menu with and without Get the new version. Two failures in the full run came from editing files while it ran (Vite reloading mid-shot); both, and every update shot and the strip's no-jump check, passed run again alone.
- `bash scripts/check-docs.sh`: nothing stale.
- **By hand (you, 2026-11-17):** the app against the stand-in, "seems to work"; the `problem` label made on GitHub.

**The review**
A fresh agent over the diff (it also ran the update tests, GitHub's real reply included): nothing critical or high. Fixed, each checked against the code first:
- **The stand-in wrote into the profile the installed app shares** (medium): a development build's 0.99.0 would have shown in the installed public build, pointing at a missing page, and kept it from looking for a day. The stand-in now keeps what it finds in memory (`Updates::persist`; the end-to-end test checks no `updates.json` is written).
- **Two views of the notice could land out of order** (an older command reply over a newer event, or a quick off and on in About): `UpdatesView::revision`, the lobby keeping the newer, and About's switch waiting for its answer.
- **Redirects to any host** would have told another server of the PC, with a referrer: now the same host only (a renamed repository stays on `api.github.com`), three at most, with no referrer (`never_follows_a_redirect_to_another_host`).
- **A reply we refuse** (a tag that isn't a version, marked latest) was asked again every hour: it now counts as a look, not again for a day; an hour's wait stays for no answer. The stand-in waits a minute after no answer, not 10 s for the whole run.
- **A dismiss took any version**, and a notice came back if GitHub's latest went back below one dismissed: a dismiss is taken only for the version shown, and nothing older than the version dismissed is shown.
- `Event::Network`'s comment had slid onto `Updates`; the docs said the release page's address takes nothing from GitHub's reply, where it takes the version its tag names once it parsed as numbers and dots.

Left, as the review judged it: a session starting during a look already under way (one small request, 30 s at most) isn't stopped; an older build (0.38 or before) on the same profile rewriting `app.json` for its tray switch drops `updates: false`, so the switch comes back on there; environment proxies (`HTTPS_PROXY`) apply, Windows' own proxy setting doesn't.

**Still to come in M26c**
- If not tried yet: See the release and Report a problem through the portal on Linux (a test AppImage in the VM); Report a problem showing the form once this is on `main`.
- The first public tag (0.39.0 or later), and with the second, the notice seen for real.
- M26c-3: the invite page's download and relay note; `self-hosting.md` rewritten, the README below your note.
- Install the workflow's own installer once (Settings → About, Copy diagnostics: "public build", the public relays) and run its AppImage in WSLg.

## M26c-3: the invite page's download and relay note, the guides

No app change, so no version of its own: the first public tag can be 0.39.0.

**What it does**
- **The invite page offers the download** to someone without the app: a button for the visitor's system (Windows, or Linux with the AppImage) to the repository's latest release, naming the file to pick there, and a link for the other system. A Mac is told the app isn't on macOS yet, a phone to open the link on a PC; a Chromebook and anything unknown get Windows first. The links open the release page rather than a file (your choice: file names carry the version, and the page has the notes and the sums). The repository is written into the page by `invite-page.yml` as it publishes it (`github.repository` in a `couch-releases` meta tag), so a fork's page links to the fork's releases; served from the repository as it is, the page asks for the installer as before.
- **The relay note:** an invite naming a relay that isn't one of iroh's public ones, and carrying no code (version 1 or 2: only version 3 carries one), says "Whoever invited you uses a relay of their own, relay.example.com, and this link doesn't carry its code: if the app asks for one, ask them for it." The page reads the invite up to the relay's address and never reads the code; a newer version than 3 gets no note. The page still makes no request but its own files, and sends nothing.
- **The guides:** `self-hosting.md` is now "install a release, make a room, add a relay of your own and your rooms get it", with the Linux package's details, checking a download, new versions, what goes wrong and what to do; SmartScreen's two clicks stay there alone (your choice). Building installers (a build carrying your relay, the AppImage, a public build) moved to `development.md`. The relay guide takes a token of its own (`openssl rand -hex 32`, as `install.sh` now says) and puts the code first. The README, below your note: the status line, "Use it with your friends" and "How it connects" name the releases and the public relays.

**Checked**
- `node deploy/invite-page/check.js`: the page against five invites the app made, pinned in `couch_proto::invite` (`the_invites_for_the_relay_note_are_pinned` adds version 3 with its code, one on iroh's public relay, one on a LAN), a version 4, damaged ones, seven browsers' user agents, the repository's shape, and the placeholder the workflow fills.
- `couch_net::endpoint`'s `the_invite_page_knows_the_public_relays`: every one of iroh's public relays matches the page's pattern, so an iroh update that moves them fails there.
- The page in Edge as published (the workflow's substitution run by hand), as Windows, Linux, macOS and an iPhone, dark and light, at 480 and 390 px: no error, no request but its own files.
- Windows: fmt, clippy and the whole workspace's tests (1,036, and the doctests: rustdoc once couldn't find `couch_engine` for `couch_desktop_lib`'s, a stale build file; they pass run again); `scripts/check-docs.sh`. Linux: the two new tests are the same on both, left to CI.

**The review**
A fresh agent over the diff: the page held (the parser matches the Rust layout for versions 1 to 3, every value from the invite goes in as text, nothing can leave the page, the workflow's substitution is safe), and nothing high. Fixed, each checked first:
- **The relay's scripts weren't executable in git** (`100644`), and the rewrite had dropped the old `chmod +x`: both are executable now, and the guides call them with `bash`.
- **Someone with only a release had no way to the relay kit:** the guides now clone the repository at the app's version on the server, which keeps the relay on that version's iroh.
- **A builder whose relay already runs** would have built with `release-config`'s fresh token, which the relay refuses: `development.md` says to put the relay's own in.
- **The AppImage's steps no longer installed cargo-about** (the Linux setup never did): put back.
- **A crafted five-byte length could wrap round 32 bits** into a small one, showing a name for a link the app refuses: lengths are read from three bytes at most (`check.js` has the case, which fails without it).
- Words: the relay guide's token is now kept off by its code, not by builds; Copy diagnostics names relays by their server; "others reach you through it" is the home relay's alone; the AppImage's update; the invite page's caveat for builders, Plasma 5.27 untried, a relay open to everyone added by its address, all put back; `install.sh`'s "untested" line gone (it runs).

Left, as judged: a Mac or a phone still sees the download buttons under its note (to send the link on, or pick it later); an ARM PC gets the x86-64 package first (there is no other); ADR 0009's line on `self-hosting.md` listing the toolchain stays as history.

**Still to come**
- Once merged, the page goes out by itself (`invite-page.yml`): until the first release is published, its download opens an empty releases page. Tag and publish soon after.
