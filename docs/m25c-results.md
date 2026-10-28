# M25c results: the unattended PC

## Why

M25a and M25b let the author's laptop run and control the home PC from afar, as long as someone left Couch Co-App running there, the PC awake and unlocked. M25c is [ADR 0010](adr/0010-playing-your-own-pc-from-afar.md)'s "Unattended PC":
- start at sign-in, to the notification area;
- keep running when the window closes while remote play is on;
- stay awake;
- tell the device when Windows is locked at home.

Wire revision 12, version 0.23.0, which doesn't talk to 0.22.0 or earlier.

**The author's choice (2026-10-27):** **Start with Windows is a switch of its own** in Settings, off until turned on, not tied to remote play.

## What changed

**At the home PC, Settings → Remote play → This PC while you're away:**
- **Start Couch Co-App when I sign in to Windows:** it starts in the notification area, without opening its window.
  - It writes the current user's Run value (`"<exe>" --background`). Turning it off removes the value only if it names this exe.
  - Only an installed app offers it, for its main profile. A development build would start the debug exe at sign-in, which needs the dev server, and a Run entry can't name another profile. Elsewhere the switch shows, disabled, with why.
  - While a device controls the desktop (M25b), it can't be turned on.
- **Notes below the switch:**
  - while remote play is on, closing the window keeps the app running in the notification area, and the PC doesn't fall asleep on its own;
  - with remote play on and the switch off: after a restart, start the app yourself.

**Keep Couch Co-App in the notification area:** a second switch, off by default (the author, after the run of 0.23.0: the icon had come and gone with remote play, which was confusing).
- **On:** an icon shows, whose click opens the window and whose menu has Open Couch Co-App and Quit. Closing the window hides it there.
- **Off:** no icon, and closing quits.
- **The switch takes effect at once.**
- **Started at sign-in** (`--background`): with the icon on, the window stays hidden until first opened, which puts it where it was; with it off, the window opens. A second start at sign-in (signing out and in with the app running) shows nothing.

**Awake:**
- **While remote play is on,** the home PC holds a power request that keeps the system from sleeping on its own (`powercfg /requests` names it: "Couch Co-App: remote play is on").
- **While a device is unlocked,** the display stays on too: a display that turns off stops the capture, and Windows may lock then.
- **A power request is a handle,** cleared when closed or when the process ends, so a crash can't leave the PC kept awake.

**Locked:**
- **While a device is unlocked,** the home PC looks every 2 s whether Windows shows the user's own desktop. The lock screen or a UAC prompt doesn't count as the user's; the device can't get past either.
- **The device hears it** with the unlock and on every change (`RemoteState.locked`). Its panel then says "Ana's PC is locked: Windows there lets nothing in until someone unlocks it at the PC, so what you see may be black or frozen".

**Underneath:**
- `couch_platform_win`:
  - `startup` (the Run value);
  - `power::Awake` (`PowerCreateRequest`, `PowerSetRequest`);
  - `session_lock::locked` (the input desktop's name, failing closed).
- **The engine:**
  - `rooms::remote::sync_awake`, called wherever remote play or the operators change;
  - `remote_tick`, the lock poll;
  - `Engine::remote_play_on`, which the shell reads without waiting on the actor;
  - `EngineOptions::start_at_sign_in`.
- **The installer's uninstall** removes the Run value, but never during an update, which would turn the switch off.

## Checked

- **Session scenarios** (75, three runs in a row):
  - **`the_home_pc_stays_awake_while_remote_play_is_on`:** nothing before remote play; the system on; the display too once the laptop unlocks; let go when it locks itself; again when it unlocks; nothing once remote play is off. Never two requests at once.
  - **`a_locked_home_pc_is_reported`:** locked already at the unlock, then unlocked, then locked again, each reaching the laptop; the friend hears nothing.
  - **`starting_at_sign_in_is_the_users_choice`:** off until turned on, independent of remote play, and refused where it isn't offered.
- **couch-platform-win:**
  - the Run value set, read and removed, under a scratch key of the tests' own (never the real Run key), and another program's value left alone;
  - a power request made and dropped;
  - `locked()` false on this unlocked PC (ignored by default: it needs an interactive desktop).
- **By hand on this PC,** the development app under a throwaway profile, driven through its debugging port:
  - remote play on: the log says "keeping this PC awake"; closing the window hid it, and the process kept running;
  - remote play off: "this PC may sleep again"; closing the hidden window quit the app;
  - Start with Windows: not offered by a development build, and refused;
  - started with `--background`: running, its window hidden; a second `--background` start exited and showed nothing; a plain second start showed the window.
- **The room-security checks:**
  - proto, room and net;
  - rooms (27);
  - relay;
  - sessions three times.
- **Front end:**
  - vitest (50);
  - `npx tsc --noEmit`;
  - `npm run shots`, with the scenarios `remote-locked-pc` and `remote-away`, and the shot `settings-remote-away`.
- **Not run: the input-safety checks.** Nothing of input or teardown changed.

## Found while checking

- **Opening the app showed its window even with no invite link:** the link handler showed the window for any start that asked for one, a link or not. It shows the window only for a link now. My first check of the background start counted the single-instance plugin's hidden helper window as the app's, which hid this for a run.

## The review

A fresh review agent found no security defect. It checked that:
- nothing remote changes the power request or the startup value;
- a device takes `locked` only from its own unlocked PC on that PC's connection;
- every operator change goes through `sync_awake`, and only one request is ever held;
- the Run value is quoted and removed only when ours;
- the tray keeps the app reachable;
- with remote play off, closing still quits.

Fixed:
- **Quit from the tray before the window was ever opened** saved the hidden window's default place over the user's real one. Nothing is saved before the window was placed.
- **Turning remote play off when the setting couldn't be saved** left the PC awake and the devices unlocked. It now acts at once, and still reports the failed save.
- **Uninstalling left the Run value behind.** The uninstaller removes it now, but not during an update.
- **The startup tests wrote to the developer's real Run key:** a killed test run could have left an entry starting at sign-in. They use a scratch key now.
- **Turning Start with Windows on** is refused while a device controls the desktop.
- **"Doesn't go to sleep" overstated it:** choosing Sleep or closing a lid still works.

## The author's run of 0.23.0 (2026-10-28, two PCs)

- **Worked:**
  - desktop control from the laptop: the Windows key, Alt+Tab, launching a game;
  - Bring to front.
- **The tray icon came only with remote play on:** as built, closing hid the window only while remote play was on, so with it off the app quit and its icon went. The author asked for a switch of its own (above), which starting with Windows follows.
- **Closing the laptop's stream window:** the session's card still offered Watch, further down the stage, which brought it back. A scenario now checks watching again after closing the window, desktop control included (`an_operator_watches_again_after_closing_its_window`).

**Found while building the switch:** Tauri runs `setup` (which makes the window and the icon) only once the app runs, so code right after `build` found neither. The icon was shown and the window opened there, so with the tray on the icon would have stayed hidden. Both moved to the app's Ready event. Checked by hand in a throwaway profile:
- started with `--background`: the tray on kept the window hidden, the tray off opened it;
- closing: the tray on hid the window and the app kept running, the tray off quit it.

## Still to try

- **The author's two-PC run of M25a, M25b and M25c together, with an installed build:**
  - Start with Windows on, sign out and in: the app is in the notification area;
  - close the window with remote play on: it keeps running;
  - leave the PC: it doesn't sleep, and its screen stays on while the laptop is unlocked;
  - lock it with Win+L: the laptop says so.
- **The host overlay's banner (M25b) while the window is hidden.** The banner is driven from the main window's page, and WebView2 may slow a hidden window's timers. To be seen in that run.
