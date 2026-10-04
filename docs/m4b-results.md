# M4b results: the lobby UI

M4b puts M4a's rooms on screen. The desktop app now has:
- a **room rail**: one round icon per room, in the order you drag them into, with **＋** to create or join a room;
- a **room column**: the members with presence dots and how each is connected (direct, relayed, online, offline), the owner's actions (invite, ban, change the password, revoke invites, close the room) and leaving for members;
- your **display name**, which you can edit, and the profile you run as;
- **dialogs** to create a room, join with a link (previewed before the password is asked), and get an invite link to copy;
- **notices** when a room is closed, you are banned, or wrong passwords pause joining;
- **`couchco://` links** that open the app on the join screen;
- **profiles side by side**: each one is its own single instance, with its own webview data.

The look follows Discord's layout with the colours of VS Code's "Default High Contrast" theme, as you asked; you confirmed it on seeing it.

**It works.** Every check below passes, measured 2026-10-04 on the M1 PC. Sessions (streaming inside a room) are M5.

## Checks

| Check | What | Result |
|---|---|---|
| **Three profiles form a room** (you, by hand) | Default, alice, bob and carol: create, invite, join, all four connected directly | pass |
| | A second invite says "you're already in this room" | pass |
| | A member closing its app shows as offline to the others | pass |
| | Banning a member while another is offline; the offline one, back, no longer sees the banned one | pass |
| | Leaving; closing the room while a member is offline, who on return sees it gone with a notice | pass |
| | An invite link opened from the browser lands on the default profile's join screen | pass |
| Engine scenarios (20, real iroh on loopback) | M4a's 18, plus: **an opened link** is kept once for the UI (and one too long to be an invite is dropped), and **a preview** gives the room's name, its expiry, and whether it is ours, one we are in, or expired, while garbage is refused; **the owner changes the password**: a member can't, a blank one is refused, it lasts across a restart, the old password is refused and the new one admits | pass, three runs in a row, about 18 s each |
| Through iroh's public relay (ignored test, needs internet) | B joins through A's relay, then reaches A directly | pass |
| Secrets out of logs | `Debug` of the commands and replies that carry a password, an invite or a link shows `..` (a new test) | pass |
| Single instance | A second launch of the same profile, with a link, exits inside Tauri's `build`, before the engine starts; the first keeps running | pass |
| One engine per profile | While an engine runs, another on the same profile (in any letter case) is refused after a short wait; once the first stops, it starts | pass |
| Workspace | 281 tests; clippy (all targets) and fmt clean; `cargo deny` ok with the two new plugins (MIT or Apache-2.0); the front end type-checks and builds | pass |

## Decisions made along the way

- **An opened link only fills in the join screen.** Any web page can open a `couchco://` link, so the engine keeps it (`Engine::open_link`) until the UI takes it, the UI previews it, and nothing is sent until you type the password and click Join.
- **Links open in the default profile.** Windows runs one command per link scheme, without our environment. Other profiles paste the link.
- **Debug builds register `couchco://`** for your Windows user (HKCU), pointing at the debug exe. The installer takes over in M10.
- **One instance per profile, in two layers.**
  - The single-instance plugin's lock is named after the app's identifier, so other profiles get `com.couchcoapp.desktop.<name>`. A second launch hands its link to the running window and exits before the engine starts.
  - That lock alone leaves gaps: Tauri releases it before our shutdown finishes, and its name isn't the folder. So the engine also locks `engine.lock` in the profile folder for as long as it runs, and a newcomer waits up to 5 s for it, then gives up.
  - Profile names are lowercased, since Windows folder names ignore case.
- **The owner can change the password** (a new engine command). Members stay; invites already sent keep working with the new password. The password itself still isn't saved anywhere, so the app can't show it.
- **The room order lives in the profile's webview storage**, as a convenience: losing it only loses the order.
- **The extra profiles log to files** (`desktop.log` in the profile folder) instead of a console window each.

## Found and fixed

From your test run:
- **Banning showed "The engine answered room, not done"**, though the ban worked. The engine answers a ban with the room as it is now; the UI now expects that and shows it.
- **The password-change notice had an empty room name.** An editing slip of mine.
- **A console window opened for every instance.** Debug builds are console programs; `scripts\profiles.ps1` now starts each in a console of its own without a window (my first fix shared the terminal's console instead, see below), and the app writes plain text (no colour codes) when not on a terminal.

From the code review (a subagent read the whole change; each finding was checked against the code, Tauri's and the plugins' source before fixing):
- **An opened link could slip under a typed password.** A link opened while the join dialog was up replaced the invite and kept the password typed for the first room, so a web page could get it sent to another "owner" (one online guess, but still). Now a password belongs to the invite it was typed for and is cleared when the invite changes; an open join dialog only takes a new link while nothing is typed, and otherwise asks ("Use it instead"); other dialogs keep their contents and the link waits until they close.
- **Two engines could run one profile**: with the name in another case (`Alice` and `alice`), in the 1.5 s while a closing app finishes its shutdown, or if two launches race. Fixed by the engine's own lock and lowercased names (above).
- **A password change reported success even if it wasn't saved**, which would have brought the old password back at the next start. It now fails and keeps the old one.
- **Dropping a room in a gap or above the first one sent it to the end.** The drop position now comes from the pointer against every room.
- **`profiles.ps1` dropped names after the first, and its instances shared the terminal's console**, so closing the terminal or Ctrl+C would have killed them without a goodbye. Both fixed, and checked by probing the console a child gets.
- **Catching up when the window regains focus could bring back a room just removed**, if an event landed while the list was in flight. The list is now asked again in that case.
- Small ones: an owner closing a room was told "You left"; the opened link stayed in the UI's state after use.

While fixing: the new preview scenario failed once under the full workspace's load. It joined with an invite that lasts one second, and expiry counts whole seconds. The test now joins with a normal invite and checks expiry with a second owner.

From your first look:
- **The show-password button vanished after the field lost focus.** That is WebView2's own button, by design; it is turned off, and every password field has our own show/hide button.
- **Many rooms pushed ＋ out of sight.** The room list scrolls on its own now, with ＋ fixed below it.
- **＋ wasn't centred**, being a text character; it is an icon now.
- **Scroll bars** are thin and in the palette.
- **Dragging rooms** needed Tauri's file-drop handling turned off for our window (`dragDropEnabled: false`), which otherwise swallows drag events on Windows. We take no dropped files.

## Things to know

- **A previewed room name is only what the invite claims.** Anyone can make up an invite; the owner's admission is what proves it. Showing a short fingerprint of the owner's id would let a careful user tell two "Friday games" apart; not done yet.
- **A profile already running** (or still closing after 5 s) stops a new launch with "profile … is already running". Release builds have no console, so that message isn't seen yet; a dialog for it can come with the installer (M10).
- **The registered link points at whichever debug exe registered last**, and stays registered after development. To remove it, delete `HKEY_CURRENT_USER\Software\Classes\couchco`.
- **A link opened while the dev server is down** starts a debug build with nothing to show. Only a development concern.
- **The UI has no automated tests.** The engine's scenarios cover what it calls; its own logic was checked by hand.
- **Still from M4a:** a room you closed keeps dialing, hidden, until every former member has heard. There is no "forget anyway" button yet.

## Follow-ups

- **M5**: sessions on the stage: going live, watching, and handing control.
- **M7**: our relay. Production invites will name it.
- **M10**: the installer registers `couchco://`; the `https` invite page, for chat apps that don't make custom schemes clickable.
- **Later:** a cap or "forget anyway" for closed rooms that never reach a member; an owner fingerprint in the invite preview; showing an invite's expiry date in the invite dialog; listing and revoking single invites.

## How to reproduce

```powershell
. .\scripts\dev.ps1
cargo test -p couch-engine                                  # unit tests and the 20 scenarios, ~20 s
cargo test -p couch-engine --test rooms -- --ignored        # join through iroh's public relay (internet)
cd apps\desktop; npx tsc --noEmit; npm run tauri dev        # the default profile
.\scripts\profiles.ps1                                      # alice, bob and carol, from the repo root
```
