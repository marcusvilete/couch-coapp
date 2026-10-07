---
name: app-manual-run
description: Set up and read a manual test of the Couch Co-App desktop app, on one PC with several profiles (the default profile plus alice, bob and carol) or on two PCs with the guest package: starting them, where every log is, what one PC can and can't show (games grab the guest's physical controller; keyboard and mouse never reach the game), how to tell a stream problem from a game's choice with xinput-watch.ps1, checking the viewer's HUD without a session, two-PC setup, checklists per area and traps already met, and what to ask the user to try. Use when asking the user to try the app (going live, watching, passing control, the panic key, games, two PCs), when the user reports something from such a run, or when reading its logs.
---

# A manual run of the app on one PC

## Start it

From the repo root, in PowerShell:

```powershell
. .\scripts\dev.ps1
cd apps\desktop; npm run tauri dev        # the default profile; builds couch-viewer (debug) first
# a second terminal, from the repo root:
.\scripts\profiles.ps1                    # alice, bob and carol, from the same debug build
```

- **Restart everything after a code change.** Ask the user to close every app window (the default profile and each of the others) and to start both commands again. Never kill their processes yourself.
  - `tauri dev` rebuilds the app.
  - The stream window is started fresh for each Watch, so a rebuilt `couch-viewer` needs no restart. Running `cargo build -p couch-viewer` is enough.
- **A room** with the profiles in it usually exists from earlier runs. The owner is the one who can ban.

## Where the logs are

| Who | Where |
|---|---|
| alice, bob, carol (the app) | `%LOCALAPPDATA%\CouchCoApp\profiles\<name>\desktop.log` (and `desktop.err.log`). The size shown while the app runs can be stale; it is right once the app exits |
| Any profile's stream window | `...\profiles\<name>\viewer.log`, appended per window (ANSI colour codes included) |
| The default profile (the app) | only the `npm run tauri dev` terminal: ask the user to copy the lines, or repeat the test with alice hosting |

- **Log level.** `profiles.ps1` gives its instances `dev.ps1`'s default `RUST_LOG` (`couch=debug,iroh=info`); without it only errors are logged.
- **Useful lines:**
  - `went live`, `seat status=Granted/Active/Suspended/Host`, `a viewer asks for control`, `panic key: every seat back to the host`, `stream ended reason=...`;
  - in a viewer log, `gamepad "..."` and `skipping "...": a host on this PC has a virtual pad plugged in`.
- **A brief `Suspended` then `Active`** is the host's 300 ms dead-man timer catching a short silence from the guest; it is safe by design.

## What one PC can't show

- **Games pick the guest's physical controller.** The guest's controller (the user's DualSense) is plugged into the host's PC too, and games read it directly. Graveyard Keeper 2 (Unity) binds the player to it, and lets alice's virtual pad only move through menus with the D-pad. Steam settings don't change that.
  - So "the guest can't play the game" on one PC is not evidence of a bug.
  - Check our side with `xinput-watch.ps1` (below). A real game test needs a second PC as the guest. (HidHide hides only the *host's* controller, and only from programs other than ours; the stream window on the same PC is another program, so with the host's controller hidden it can't read it either.)
- **Keyboard and mouse never reach the game on one PC.** They go in only while the game window is in front, and then the stream window doesn't have the keyboard. That is by design.
- **Steam notifications.** Steam announces each virtual pad as it is plugged in ("Xbox 360 Controller"). Each go-live or `pad-test --real-pad-ids` shows one.
  - "XInput Controller #N" means made-up ids: the pad is a CLI test pad, or something regressed in `real_pad_ids`.

## Tell a stream problem from a game's choice

With alice live and carol holding the controller, on the same PC:

```powershell
.\scripts\xinput-watch.ps1        # 60 s; prints each XInput slot when it changes
```

Ask the user to press each button and move each stick on their controller. alice's virtual pad (usually slot 0) should show every input under the right name.
- **If it does,** the stream is right and the game chose another controller.
- **If inputs are missing or wrong there,** the problem is ours: the viewer's `read_pad`, `couch_input::forward`, the router, or `couch_platform_win::vigem`.
- `joy.cpl` → "Controller (XBOX 360 For Windows)" → Properties is the same check in Windows' own panel.

## The viewer's HUD without a session

```powershell
cargo run -p couch-viewer -- --hud-preview watching     # or asking, controlling
```

- **Check the debug build, not only release.** The app runs the debug `couch-viewer`, and egui has checks that panic only in debug builds (an undrained `TexturesDelta` crashed every stream window in M5b while release looked fine).
- **To look at it yourself:** start the preview with stdin redirected and kept open (a .NET `ProcessStartInfo` with `RedirectStandardInput`), wait 3 s, and screenshot the region with `System.Drawing` (`CopyFromScreen`). Then read the PNG.
- **A still picture repaints only when asked:** egui lays a new area out unseen on its first frame, so the HUD must honour egui's repaint requests (`Hud::needs_paint`).

## The panic key

- **Only a person can press it.** It ignores injected keys, so no script can.
- **Ask the user** to press Ctrl+Alt+Shift+Backspace with a guest playing, in each of these places:
  - with the game in front;
  - with the host's own app window in front;
  - with another program in front.
- **Expect** the guest's pad to go neutral, "control paused" in the host panel, no Backspace in the game, and `panic key:` in the host's log.
- **If it fails only in our own window:** something registered the process for raw keyboard input (see `windows-ffi-pitfalls`).

## Lifecycle (M6): what to expect

- **Closing the game** ends the stream with "The game closed". A game that swaps its window (some do when changing display mode) keeps streaming after a short freeze; host log: `the game opened a new window; streaming that one`.
- **A window that closes while the game runs** ends the stream after 5 s ("went away").
- **Killing the host app** (Task Manager): its viewers see the connection lost after about 5 s (log: `the peer went silent; closing`).
- **A dropped connection while watching**: the stream card says "Reconnecting…", the stream window stays, and the stream comes back by itself within 30 s. The guest must ask for control again. Log: `lost the host's connection; waiting for it to come back`, then `the host is back; joining its stream again`.
- **Killing `couch-viewer.exe`**: use Task Manager's **Details** tab (End task there kills). The window comes back by itself once; a second kill within a minute ends watching ("crashed twice"). Log: `the stream window crashed and was started again`. The Processes tab's End task asks the window to close instead, which counts as leaving: watching just stops.
- **A program run as administrator** is marked "admin" in Go live, with a warning when picked. Keyboard and mouse can't reach it, and the panic key doesn't work while it is in front (measured in M6); Take back does.
- On one PC, the network can't be dropped for one pair; two PCs (turn off the guest's network for about 10 s) or the `sessions` scenarios cover it.

## Seats (P1): what to expect

- **Go live's Input section.** Single player (a controller, keyboard and mouse, or both) or Multiplayer (1 to 4 controllers, plus optionally a keyboard-and-mouse seat). Keyboard choices are disabled for a screen. Picking a window brings back the setup last used with that program (`input-setups.json` in the profile folder).
- **A controller plugged into the host** shows a warning in Multiplayer: it may take a player's place.
- **On this PC with profiles:** alice goes live in Multiplayer with 2 controllers; bob and carol each ask for one. `xinput-watch.ps1` should show two virtual pads (after any physical ones), each moving only with its own guest. Steam announces each pad.
- **Moving.** A guest holding Controller 1 who asks for Controller 2 keeps the first until the host accepts; then the first goes neutral and the second follows them. Host log: `seat` lines for both.
- **The "game not in front" line.** With alice live and the game window behind another window, the guests' stream windows say "The game isn't in front on Ana's PC"; for a player the bar stays up until it comes back. On one PC the stream window itself takes the front, so expect the line while clicking in it: that is right.

## The host on a seat (P2): what to expect

Needs HidHide installed on the host (administrator, then a restart; installed on the user's PC on 2026-10-06) and a controller that isn't an Xbox 360 one (theirs is a DualSense, on Bluetooth).
- **Go live's "Your controller".** Lists the host's controllers (never our pads) when a seat has a pad; the choice is remembered in the profile's web storage. Its line says what HidHide allows: not installed (with the download page), its configuration window open (close it, refresh), or ready.
- **After going live** the host panel says the controller is hidden and asks to reconnect it once. Until then, a game or Steam that had it open still sees it. After it, `joy.cpl` (closed and reopened) lists only "Xbox 360 Controller", and `xinput-watch.ps1` shows the host's input in that pad's slot.
- **Steam Input off for the game** (its Properties → Controller), or one seat gets no player in it: Steam keeps a player for the hidden controller. Turning off Steam's global PlayStation support instead also works, but then the DualSense works in games only while live.
- **The seats list** shows "you (DualSense Wireless Controller)" on Controller 1 with Leave, Take back on guests' seats, and Play here on the others with a pad. Passing the host's seat to a guest leaves the host on none; the guest giving it back or leaving returns it. Take back and Leave leave the seat empty (guests see "free", or "Nobody is playing" in single player, never "own controller"); only Play here seats the host's controller again. The panic key removes guests but the host keeps the seat they play; a seat they had passed on stays empty.
- **Stopping** shows the controller again (log: `the host's controller is shown again`). If the app was killed, it stays hidden until the next start of the app or of `couch-cli host` (log: `showed 1 controller(s) hidden by a host that crashed`). HidHide's own configuration client (Start menu) also shows and clears the list.
- **If hiding fails**, the host panel says why and the host plays outside the seats, as in P1.

## Settings and the splash (P3): what to expect

- **The splash** ("Player 2 joins") plays at every start for about 3 s: two controllers slide in, land on a couch, the name types out, the picture holds a second, then fades. A click or a key skips it. It animates even with Windows' animation effects off (the user's choice; their PC has them off). It covers the lobby's first load only; the engine is already running.
- **⚙ Settings** in the user panel, two tabs:
  - **Shortcuts:** Change waits for a key (Esc cancels; Ctrl, Alt and Shift needn't be held, and are ignored). A refused key says why under its row ("Ctrl+Alt+Shift+P already does this: …", "that key can't be used", "only F1 to F12 (but F4)"). Default and Reset all put keys back. Saved in the profile's `shortcuts.json`. The panic key changes from the next Go live (the host panel's reminder names the key in use); the stream window's keys from the next window opened (its bar's hint names them).
  - **Network:** iroh's public relays (the default) or My relay with a URL (https; `http://localhost…` for a local `iroh-relay --dev`). Saved in `network.json`; "Applies when the app restarts" with Restart now (asks once more while streaming or watching). The line under it says which relay we're connected through. A typo in the URL is refused at Save. To try the production endpoint shape on one PC, give a profile iroh's own relay as "My relay" (the host shown in the status line, as `https://…`): it then runs without iroh's address lookup, and must still meet the others.
- **Keys are checked within one profile only.** On one PC, a viewer profile's chord set to the hosting profile's panic key is swallowed by the host's hook and presses the panic key. Not a bug (on two PCs they never meet); keep them apart when testing.
- **Restart now** goes through the normal exit (the rooms hear we go), then starts the same profile again; the new instance waits up to 5 s for the old one's profile lock. **Release builds only** (the guest package): `npm run tauri dev` stops with its app, and the Vite server with it, so in development the app says to close it and start it again (`import.meta.env.DEV`). Pressing it under `tauri dev` once ended both, leaving no app running.

## A full run to ask for

1. Watch, then stop watching.
2. Ask for the controller and get declined. Ask again after 5 s, get accepted, and play.
3. Pass control to another viewer, then take it back.
4. The panic key, then Allow control.
5. Close the game window: everyone should hear the stream ended.
6. Stop streaming.
7. Close the host's app while a guest plays.
8. Ban a viewer mid-stream.

## Two PCs

The user has a second PC on their home network. Suggest a two-PC run after any change one PC can't show:
- games reading the guest's pad;
- keyboard and mouse;
- network drops and reconnecting;
- killing a host;
- administrator windows;
- real networks and other GPUs.

### Setup

- **Roles.** This PC hosts: it has ViGEmBus, the games and the dev build (`npm run tauri dev`). The other PC is the guest and runs the release package.
- **The package.**
  - `.\scripts\pack-guest.ps1` builds the release app and zips it with the stream window and the DLLs into `target\guest\couch-coapp.zip`; `npx --yes http-server target\guest -p 8000` serves it.
  - Run the script without redirecting its stderr (`2>&1` or `*>&1` with `$ErrorActionPreference = 'Stop'` aborts on Tauri's info lines).
  - Rebuild it after every change the guest side runs (viewer, engine, UI). `BUILD.txt` names the commit; both sides must match.
- **On the guest PC** (the script's header has the list):
  - unblock the zip before extracting;
  - allow the app on private networks when the firewall asks;
  - start `run.cmd`;
  - paste the invite into the Join box (release builds don't register `couchco://`).
- **Controllers.** The guest's controller goes on the guest PC. On the host, for co-op games, either choose the host's controller in Go live (P2: hidden, it plays Controller 1, and the host reconnects it once) or keep none plugged in: otherwise the game sees it directly, gives it a player, and a guest's pad may not work (seen in Fling to the Finish in P1). Single-player games took the guest's pad either way.
- **The link.** The stream window's HUD should say "Direct". "Relay" means the firewall or the network blocked the direct path.

### Checklists

Pick what the change touched; add the basics (watch, ask, pass control, the panic key, stop) every time.

| Area | Ask the user to | Expect |
|---|---|---|
| Gamepad in games | play a single-player game, then a co-op one, as the guest | the guest drives the game; for co-op, no controller on the host |
| Keyboard and mouse | go live with keyboard and mouse on, the guest plays with them | input reaches the game only while its window is in front on the host; the Windows key, Alt+Tab, Alt+F4, Ctrl+Esc never do |
| Alt+F4 on the guest | press it while holding the controller with keyboard, then while only watching | holding: nothing happens; watching: the stream window closes (that counts as leaving) |
| Display mode | switch the game between windowed and full screen, also from the guest | the stream goes on; the guest's input works in both (the host's log says "the game opened a new window" if the game swapped windows) |
| Game exit | quit the game | everyone gets "The game closed" |
| Network drop | turn off the guest's network for about 10 s while it plays | frozen picture, "Reconnecting…", the stream comes back within 30 s, the guest has to ask for control again |
| Host killed | end the host app in Task Manager | the guest is told the stream ended within about 5 s |
| Stream window crash | on the guest, Task Manager's **Details** tab, End task on `couch-viewer.exe`; again within a minute | it comes back by itself, still holding the controller; the second time, watching ends |
| Administrator window | run Notepad as administrator, go live with it | Go live marks it "admin" with a warning; keyboard and mouse can't reach it; the panic key doesn't work while it is in front (Take back does) |
| Single-player seat kinds | go live with a controller seat, then the same game with a keyboard-and-mouse seat | the guest drives the game with exactly that, nothing else |
| Multiplayer | a co-op game (Fling to the Finish) with 2 controllers, no controller on the host; the guest asks for Controller 2 | the guest plays player 2; asking for and moving between seats works |
| The host on a seat (P2) | Go live in co-op with 2 controllers and "Your controller" chosen; reconnect it; start the game; the guest asks for Controller 2; then pass Controller 1 to the guest and Play here on Controller 2; then take it back | the host plays player 1 and the guest player 2; every seat can be handed over; `joy.cpl` on the host lists only our pads; after Stop the host's controller works in games again |
| Game not in front | with keyboard and mouse on, the host switches to another window | the guest's stream window says the game isn't in front; the line goes when the game is back |

### Reading the results

- **Logs.**
  - The guest's app log is `desktop.log` next to `run.cmd`; its stream window's is `viewer.log` in `%LOCALAPPDATA%\CouchCoApp\profiles\<profile>` on that PC.
  - The host's (default profile) is the `npm run tauri dev` terminal.
  - Ask for the lines around the moment, naming what to look for (`seat status=`, `lost the host's connection`, `the stream window crashed`, `the peer went silent`).
- **Traps already met** (not bugs):
  - Task Manager's **Processes** tab End task asks the window to close: that is leaving, so watching just stops. Only the Details tab kills.
  - An administrator window: Windows keeps a normal program's input and hooks from it. Expected; the warning says so.
  - Alt+F4 while only watching closes the guest's own window.
  - A host controller taking player 1 in co-op when it isn't on a seat: Go live warns; choosing it in "Your controller" (P2) hides it.
  - The host's controller still working in a game right after going live: it wasn't reconnected yet (HidHide refuses only new opens).
  - With the host's controller on a seat, one seat short in the game (3 of 4 work, `xinput-watch` shows all 4 slots moving): Steam Input is on for the game and keeps a player for the hidden controller. Turn it off (the game's Properties → Controller). Seen with Fling to the Finish.
  - Seat order isn't XInput slot order: Windows numbers the slots itself. Tell seats apart by moving one pad at a time in `xinput-watch`.
  - A full-screen game ignoring the guest once (M5b): an observation, not a known bug; note whether it happens again.
- **Record** each run in the milestone's results file ("Your run" row), and new observations the user wants kept in the polish backlog memory.
