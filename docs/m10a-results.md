# M10a results: an installer for testing with friends

Until M10a, the app reached another PC as a zip built by `pack-guest.ps1`:
- it was unblocked and extracted by hand, and started with `run.cmd`;
- `couchco://` links didn't open it;
- the drivers were installed separately. Your other PC had no ViGEmBus, and nothing in the app said so.

M10 is now split:
- **M10a, this:** an installer good enough for friends.
- **M10b, at the end** (after M9 and M7): the public release, with code signing, licence notices and the source offer, the fallback encoder, and our own relay.

M11, the updater, is very low priority, so new versions are installed by hand.

Your decisions:
- **the drivers are optional:** a Yes/No question for each missing one, saying what won't work without it;
- **each driver runs its own official setup window**, license and progress shown, with Windows' administrator prompt;
- **the installer asks: just me, or everyone on the PC** (first it installed for the current user only; on your first try you asked for the choice).

## What it does

**The installer** (`scripts\build-installer.ps1`: `Couch-Co-App_<version>_<commit>-setup.exe` in `target\installer\`, about 69 MB)
- **The bundle:** Tauri's NSIS installer in its "both" mode, with a Start menu entry and an uninstaller. A page asks:
  - **just me:** `%LOCALAPPDATA%\Programs\Couch Co-App\`;
  - **everyone on the PC:** `Program Files\Couch Co-App\`.

  On an administrator account (most home PCs), Windows asks for administrator rights as the installer opens, before the choice; that's how NSIS's multi-user support works. On a standard account there's no prompt, and only "just me" is offered.
- **What's in it:** the app, with its stream window (`couch-viewer.exe`) and the FFmpeg and SDL3 DLLs beside it, where the engine already looks; and `drivers\`, with the two drivers' installers and their licence texts.
- **`couchco://` links open it.** The installer registers them for whoever it installs for.
- **Its settings are kept apart.** The installer's own settings live in `tauri.installer.conf.json`, merged in only by the build script. In `tauri.conf.json` they made Tauri copy the bundled files next to the debug app too: the first try overwrote the debug stream window with the release one (it was rebuilt).
- **The build script:**
  - fetches and stages the pinned dependencies and driver installers;
  - checks the version is the same in `Cargo.toml`, `tauri.conf.json` and `apps/desktop/package.json` (0.8.0 now; bump it with each installer handed out);
  - builds the installer.

  It replaces `pack-guest.ps1`.
- **Unsigned until M10b.** Windows SmartScreen says "Windows protected your PC": install with More info, then Run anyway.

**The drivers, offered after install** (`installer-hooks.nsh`)
- **When:** a driver is offered only if missing (its service isn't registered), and nothing is asked in a silent install.
- **ViGEmBus:** "Without it, guests' controllers can't reach games you host. Watching, and a keyboard-and-mouse seat, still work."
- **HidHide:** "It hides your own controller from games while you stream, so it can play through a seat and never takes a guest's place in co-op games. Without it you play outside the seats." It says HidHide needs a restart.
- **A Yes** runs the driver's official installer with `ExecShellWait "runas"`. The installer may run as the user ("just me", or a standard account), and a plain `ExecWait` can't start a program that needs administrator rights; when it is already elevated, the driver's setup simply runs.
- **Not asked again** in an update, a passive or a silent run.
- **Uninstalling leaves the drivers** (other programs use them). The planned note saying so is gone after the review: an upgrade runs the old uninstaller in a way that can't be told from a real uninstall, so the note would have popped up mid-upgrade.
- **The pins.** Both are pinned by SHA-256 in `xtask/src/deps.rs` (`DRIVERS`):
  - ViGEmBus 1.22.0: BSD-3-Clause, so its notice ships beside it;
  - HidHide 1.5.230: MIT.

  `fetch-deps` downloads them; `stage-dlls` copies them, under fixed names and with their licences, to `target\<profile>\drivers\`.

**Settings → Drivers** (`couch_platform_win::drivers`)
- **Each driver's state:**
  - Installed;
  - "Installed: restart Windows to finish" (registered, but not answering yet);
  - "Installed (its configuration window is open)" (HidHide only);
  - Not installed, with what won't work, and an Install button.
- **Install** runs the same bundled installer with Windows' administrator prompt (`ShellExecuteExW`, verb `runas`) and waits for it. Declining the prompt, or cancelling the setup, says so.
- **Development builds** find the installers after `cargo xtask stage-dlls`.

**Go live warns** when ViGEmBus is missing and a seat has a pad: "Guests' controllers won't reach the game…", pointing at Settings → Drivers (`SourceList.virtual_pads`). The HidHide hint points there too.

**"Update needed"** (`couch_proto::PROTOCOL_MINOR`, now 1)
- **What the number means now:** until the first public release, payloads change in place. So `Hello`'s `proto_minor` became the wire revision, bumped with every change to what members exchange (now in the `protocol-change` skill). Builds of different revisions don't talk.
- **Where it's checked:** right after the two `Hello`s (`couch_net::control`). The side that sees another revision closes with the `version` code, before admission or anything about a room.
- **Both sides learn the other's version.** The acceptor sends its `Hello` anyway and waits up to 2 s for the dialer to close.
- **The member list** shows "Needs update" (theirs is older) or "Update yours" (theirs is newer) instead of the link, with the versions in its tooltip. It clears once a connection works again.
- **A join refused this way says so**, with both versions.
- **Builds from before** sent 0 and didn't check. They are refused by this one, which shows them as needing an update.

**A log file.** An installed (release) app has no console, so it logs to `desktop.log` in the profile folder, beside `viewer.log`, at `couch=info,iroh=warn` unless `RUST_LOG` is set. The old file is set aside as `desktop.old.log` past 10 MB. Development builds log to the console as before.

## Review

A review subagent read the change (no high-severity findings). Each finding was checked against the code:
1. **Strangers could grow memory** (medium): anyone with our address could dial with another revision, fresh keys each time, and a version text up to 64 KiB; each attempt left an entry that stayed. Now only a member of a room we share is remembered, and a peer's version is cleaned and capped at 32 characters where it's read (`control::shown_version`).
2. **…and the log** (medium): that path logged the stranger's text at info level, now in `desktop.log`, with newlines that could forge lines. Now debug, cleaned and short.
3. **The installer's hooks ran during upgrades** (medium): an upgrade runs the old uninstaller without `/S`, so the uninstall note would show mid-upgrade, and passive or update runs asked about drivers again. The driver questions now skip update, passive and silent runs; the uninstall note is removed.
4. **A refusal before reading the owner's `Hello` said "older"** (low): it can't know; now "one of you needs to install the newer one".
5. **A second launch could set aside the running app's log** (low): a clicked link with the log past 10 MB. The release log now starts after the single-instance check.
6. **Two driver setups could run at once** (low): from a reopened Settings. The engine now refuses a second while one runs.
7. **Go live read HidHide twice** (low): up to 1 s more when it's busy. It now asks ViGEmBus alone.
8. **Comments** (low): "the installer runs as the user" (not always, in "both" mode), and the protocol's wording on who sends `Hello` first.
9. **No engine-side tests of the version check** (low): a new rooms scenario plays an old install of a member (its own key, another revision): the owner shows it as needing an update until the member's real app connects; a stranger doing the same leaves nothing.

## Checks

| Check | What | Result |
|---|---|---|
| Installer | Builds (69 MB); the generated script asks just me or everyone, puts the stream window, DLLs and `drivers\` beside the app, registers `couchco://`, and includes the driver hooks (NSIS compiled them) | pass |
| Version check (`couch-net`, new `versions` tests) | An acceptor refuses a dialer of another revision, still sends its own `Hello`, and stops waiting once the dialer closes; a dialer refuses an acceptor of another revision; both report the other's `app_version` | pass |
| Drivers (`couch-platform-win`) | States read (nothing installed); an unknown service isn't registered, a known one is | pass |
| Engine | `GetDrivers` lists ViGEmBus and HidHide; everything else unchanged | pass |
| Session and room scenarios | 31 and 21 (1 new: another version shown for a member until it updates, nothing kept for a stranger), 3 runs each | pass |
| Workspace | All tests; clippy (all targets), fmt, `tsc`, the Vite build clean | pass |
| Your run | This PC: installed (both drivers present, so no questions) and it works. The other PC (Windows 10): a fresh install with both drivers declined; Settings → Drivers shows them not installed, with Install buttons; hosting there works now (the Windows 10 capture fix, confirmed). Settings' Install button, tried on this PC with HidHide uninstalled (ViGEmBus can't be tried here: Meta's Oculus bus answers in its place): HidHide's setup ran with the administrator prompt, then "restart Windows to finish", then installed after the restart | pass |

## Your run

**On this PC:**
1. Install `target\installer\Couch-Co-App_0.8.0_…-setup.exe`. SmartScreen: More info, then Run anyway. Windows asks for administrator rights as it opens, then the installer asks: just me or everyone. Both drivers are installed here, so no driver questions.
2. Start it from the Start menu: the splash, then your rooms. It uses the same default profile as `npm run tauri dev`, so close the dev app first: two can't run on one profile.
3. Settings → Drivers: both "Installed".
4. Open an invite link from the browser: the installed app opens its join screen.
5. Uninstall it from Windows' Settings → Apps: the app goes, with the note that the drivers stay.

**On your other PC** (no ViGEmBus):
1. Install, and answer Yes to ViGEmBus: its own setup opens with Windows' administrator prompt. Answer No to HidHide.
2. Settings → Drivers: ViGEmBus installed, HidHide "Not installed" with its reason.
3. Go live there with a controller seat (no ViGEmBus warning now). Watch from this PC, ask for the seat, and play: your controller reaches its game. This is also the Windows 10 capture fix's real test, and the overlay on that PC.
4. Its logs are in `%LOCALAPPDATA%\CouchCoApp\profiles\default\desktop.log`.

**"Update needed",** optional: run an older build on one side, such as the old guest package. The member list says "Needs update" on the new side, and nothing joins.
