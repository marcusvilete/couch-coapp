---
name: app-manual-run
description: Set up and read a manual test of the Couch Co-App desktop app with several profiles on one PC (the default profile plus alice, bob and carol): starting them, where every log is, what one PC can and can't show (games grab the guest's physical controller; keyboard and mouse never reach the game), how to tell a stream problem from a game's choice with xinput-watch.ps1, checking the viewer's HUD without a session, and what to ask the user to try. Use when asking the user to try the app (going live, watching, passing control, the panic key, games), when the user reports something from such a run, or when reading its logs.
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
  - Check our side with `xinput-watch.ps1` (below). A real game test needs a second PC as the guest, or HidHide hiding the controller from games.
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

The host PC needs ViGEmBus and the app; the guest PC needs the app (built or installed).
- **Getting the app there:** `.\scripts\pack-guest.ps1` builds the release app and zips it with the stream window and the DLLs into `target\guest\couch-coapp.zip`; `npx --yes http-server target\guest -p 8000` serves it. The guest starts it with `run.cmd`, which logs to `desktop.log` beside it. Its header lists what the guest PC will ask (unblocking the zip, the firewall, pasting invites).
- **Both sides from the same commit:** `BUILD.txt` in the zip says which.
- **The guest's controller goes on the guest PC;** with none on the host, games see only the virtual pad.

Two PCs test what one can't:
- games reading the guest's pad;
- keyboard and mouse;
- real networks and relays;
- other GPUs.

If the host has their own controller connected, it stays controller 1, and single-player games may ignore the guest. That is a known limitation, not a bug: see `docs/m5b-results.md` and the "seats and input modes" requirement.
