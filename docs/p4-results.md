# P4 results: the invite page online, and two installer fixes

Before the next installer for friends: the invite page goes on GitHub Pages, each build says where its invites link to, uninstalling can delete our data, and Restart now no longer repeats the invite link. The version goes to 0.10.0, for M9 and P4.

## What it does

**The invite page on GitHub Pages** (`.github/workflows/invite-page.yml`, the repository's first workflow)
- **On every push to main that changes the page** (or by hand), it:
  1. checks it (`node deploy/invite-page/check.js`);
  2. publishes only `j/`;
  3. deploys with GitHub's Pages actions (checkout v7, configure-pages v6, upload-pages-artifact v5, deploy-pages v5).
- **The page is at `https://marcusvilete.github.io/couch-coapp/j/`.**
- **Switched on once,** in the repository's Settings → Pages → Source: "GitHub Actions".
- **It doesn't depend on the relay.** It reads the invite after `#` in the browser and builds the `couchco://` link, so any build can link to it. `deploy/invite-page/README.md` and `docs/self-hosting.md` say so, and also:
  - whoever runs a page could see the invites opened with it;
  - the password still guards the room;
  - a fork can host its own copy.

**Where a build's invites link to** (`scripts/build-installer.ps1`)
- **Release builds** take `release.toml`'s `invite_page`, as before.
- **`-InvitePage <address>`** sets it for one build, test builds included (they had none before).
- **`-NoInvitePage`** gives `couchco://` links.
- **The address is checked:** https only (http in a test build), in lower case as the app compares it, and ending in `/`. Passing both switches is refused.
- **Every build prints which it uses:** "Invites link to …" or "Invites are couchco:// links".
- **The comments** in `cargo xtask release-config`'s file and `deploy/release.example.toml` give the GitHub Pages form and the overrides.

**Uninstalling deletes our data when asked** (`installer-hooks.nsh`, `NSIS_HOOK_POSTUNINSTALL`)
- **When "Delete the application data" is ticked, and not during an update,** the uninstaller also handles `%LOCALAPPDATA%\CouchCoApp`, the profiles of the user running it. Tauri's box only deletes its own folder.
- **If a crash marker is waiting** (a `pid-*` file in `hidden-controllers` or `held-input`), the question changes. Only the app shows a hidden controller again or lets go of held keys, so:
  - **it advises No:** start the app once, then uninstall again. No is the default;
  - **Yes still deletes,** saying a hidden controller then needs HidHide's own app. The app can't always clear a marker, for example once HidHide is uninstalled, and a refusal would then repeat forever.
- **Otherwise it asks Yes/No first.** It says that every profile goes, and what that means for rooms, the wording chosen after your run:
  - rooms you own stay for their members, but no one can join them any more;
  - the rooms you're in still list you, until their owner bans you.

  The box can't be ticked in a silent or passive run.

**Restart now without the invite link** (`apps/desktop/src-tauri/src/lib.rs`)
- **`restart_app`** marks a restart and exits normally.
- **The exit handler,** after the engine's shutdown, starts the exe again with no arguments. The same environment keeps the same profile.
- **Why not Tauri's restart:** it passed on the original arguments, so an app opened from a `couchco://` link showed that invite's join screen again.
- **Why this is safe:**
  - the single-instance lock is already released on exit, before our handler runs;
  - the new engine waits for the old one's profile lock.

## Checks

| Check | Result |
|---|---|
| `cargo test --workspace`, clippy, fmt, `tsc` | pass |
| `node deploy/invite-page/check.js` | pass |
| `build-installer.ps1` refusals: both switches; an `ftp://` page; a page not ending in `/`; an http page in a release build; `HTTPS://` in capitals | each refused with its message, before anything is built |
| A test installer with `-TestRelay -InvitePage https://marcusvilete.github.io/couch-coapp/j/` | built (0.10.0); it said "Invites link to …"; the uninstall hook compiled |

## Code review

A review by a separate agent; each finding checked against the code before acting on it. No bugs in a normal run.

| Finding | Outcome |
|---|---|
| Any file in the marker folders blocked deleting, but the app keeps markers when HidHide can't be reached (uninstalled since), and a stray `turned-on` file is never removed: the uninstaller would refuse for good | **Fixed:** only `pid-*` markers count, and the question then advises No (the default) but lets Yes delete, saying what that means for a hidden controller |
| `HTTPS://` in capitals passed the script, but the app compares the scheme as written, so the build silently had no page | **Fixed:** refused, with the reason |
| The workflow's first run, on the merge, fails if Pages isn't switched on yet | **Documented:** switch it on before merging, or run the workflow again by hand |
| On an install for everyone, uninstalled with another account's administrator password, the question is about that account's data | **Left:** as for Tauri's own folder; the question shows the path |
| If Tauri can't exit through its event loop, it exits without the exit handler, so Restart now would close without starting again; the old window may linger a moment beside the new one | **Left:** a failure path not seen, and cosmetic |
| `-InvitePage ''` counts as not given | **Left:** `-NoInvitePage` is the way to ask for none |

## Your run

The test installer, on this PC:
- **Uninstalling:** the folder stayed with the box unticked. With it ticked, the question let you keep or delete the data.
- **The invite page, served locally:** worked from a copied invite.
- **Restart now** (after changing the relay setting, which is what shows the button; with alice and the default profile): the app came back without the join screen.
- **Not yet:** the page on GitHub Pages, which goes online after the merge, once Pages is switched on.

**Your question: what happens to rooms when someone deletes their data** (ADR 0006: the owner's identity alone signs who is in a room)
- **An owner:**
  - the room goes on for its members, who still meet, stream and watch;
  - nobody can join it any more, every invite included, and nobody can be banned or close it;
  - the owner shows as offline for good. Reinstalled, they're a new identity, which can't even rejoin.
  - Handing ownership over, or admins, is future work (ADR 0006).
- **A member:** uninstalling sends nothing, so the rooms keep listing them, offline, and they keep a place (of 8). Reinstalled, they need a fresh invite and appear twice. The owner can remove the old entry with Ban, which also cancels the invites still out.
