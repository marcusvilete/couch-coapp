# M25b results: desktop control and Bring to front

## Why

**The author's two-PC run of M25a (0.21.0)** ran the home PC's sessions from the laptop, but three things stopped short of playing:
- a screen couldn't be streamed while the session had a keyboard-and-mouse seat;
- the streamed game window couldn't be brought to the front from afar, so keyboard and mouse never reached it, and many games ignore a controller in the background;
- games couldn't be launched: nothing of the laptop's reached the desktop.

M25b builds [ADR 0010](adr/0010-playing-your-own-pc-from-afar.md)'s "Desktop control (your devices only)", plus **Bring to front** for the streamed window. Wire revision 11, version 0.22.0, which doesn't talk to 0.21.0 or earlier.

**The author's choices (2026-10-27):**
- **The mouse stays relative, as in games.** The laptop captures its pointer, and the home PC's cursor shows in the stream. On a streamed screen the home PC keeps the cursor on that screen. An absolute pointer, the laptop's own over the picture, is left for later.
- **Trust changes wait while the desktop is controlled** (below).

## Two checks first

**Bring to front from a background process** (a probe in the scratchpad, `SetForegroundWindow` from a console not in front, VS Code and Notepad taking turns):

| Way | Result |
|---|---|
| `SetForegroundWindow` alone | works once per new process (Windows' free activation for a new process), then refused every time |
| after an F24 tap through `SendInput` | worked every time, a minimized window included (after `SW_RESTORE`) |
| after an Alt tap, or with `AttachThreadInput` | worked, but only after the F24 taps had already made the process the last to give input, so not told apart |

So the home PC taps F24, down and up in one `SendInput` call (so nothing can stay held), then calls `SetForegroundWindow`.

**SDL3's keyboard grab in the stream window.** `couch-viewer --input-sink FILE --grab` (new), with the mouse in relative mode as in a stream. The author pressed the keys:
- the Windows key, Alt+Tab, Alt+Esc, Ctrl+Esc, Ctrl+Shift+Esc and Alt+F4 stayed in the window: no Start, no switcher, no Task Manager;
- Ctrl+Alt+Shift+M gave them back, and the Windows key then opened Start;
- Ctrl+Alt+Del stayed the PC's;
- on the first try Alt+F4 closed the window: that was SDL's own `SDL_WINDOWS_CLOSE_ON_ALT_F4`, which the real stream window already turns off while its keyboard goes to the host (the test window now does too).

No hook of our own was needed: SDL reads the keyboard without raw input.

## What changed

**On the laptop, in the home PC's console** (a panel at the bottom, "Ana's PC's desktop"):
- **Control the desktop:** enabled while it watches the session, the session has a keyboard-and-mouse seat, something is streamed and control isn't paused; otherwise it says which is missing. **Stop controlling** ends it.
- **Then:** click into the picture, and the laptop's keyboard and mouse reach everything on the home PC, the Windows key and Alt+Tab included. The bar says "Ctrl+Alt+Shift+M frees the keyboard and mouse". The free key, or leaving the stream window, gives the keys back to the laptop.
- **The start dialog from afar** offers the keyboard seat by default and explains it. The stream picker lets a screen go with it.

**At the home PC, while a device controls its desktop:**
- **A banner in the host overlay:** "Ana's laptop is controlling this PC", with the panic key that ends it. It shows whatever the overlay setting is, and over the app too, and is kept out of the capture like the overlay always is. The console says the same.
- **Settings → Remote play:**
  - adding a device and changing the PIN are refused, and so are accepting, unlocking or running another PC of the user's from this one (which would stretch this PC's PIN to it);
  - removing a device and turning remote play off still work;
  - this is a speed bump: desktop control is the whole PC, files included.
- **The pointer is kept on the streamed screen:** each move goes to the cursor's position, clamped to the whole screen (taskbar included), as an absolute move, without acceleration. On a window, moves stay relative.

**Bring to front**, in the console's header (while the streamed window isn't in front) and its menu, here and from afar. Only the streamed window, never a screen and never a window named in a command. The stream line says "not in front" when it isn't.

**Underneath:**
- **The wire:**
  - `Permissions::DESKTOP` (bit 3);
  - `RemoteOp::Desktop { on }` and `BringToFront`;
  - the console state's desktop holder and window focus, and the stream's `screen` flag.
- **The seat router lifts three rules for a `DESKTOP` holder alone:** no window in front needed, no pointer over it, no key blocked. Its keys survive the front moving, since Alt+Tab moves it. Guide, the hold, epochs, suspension, every release and one keyboard holder at a time stay.
- **`HostSession::set_desktop`:** only the keyboard seat's grant to the member named carries the bit, never a pad seat, another member or the host. It ends with the viewer leaving, giving the seat back, a pass to someone else, a take-back, the panic key and the end.
- **The engine:**
  - gives desktop control only to the unlocked operator in the session's room, watching it;
  - ends it whenever that device stops being an operator: every path that removes one ends in `announce_hosting`, which checks;
  - takes back a desktop grant the session no longer gives;
  - a member drops `DESKTOP` from any host it doesn't run, unlocked.
- **`Permissions::all()` included `DESKTOP` once it existed.** `couch-cli host --allow-kbm` and `input-sink`'s check used it: left alone, every viewer of the command-line host would have had desktop control, and `input-sink` would have let the Windows key through. Both use the new `Permissions::ALL_INPUT` now, and CLAUDE.md says never to grant `all()`.

## Checked

- **couch-proto:** the new ops and fields roundtrip; the fullest state fits a control frame.
- **The router:** a desktop holder passes the Windows key, Alt+Tab and a click with nothing in front; the same holder without the bit stays blocked; its Alt survives the front moving and a revoke releases it; the hold still drops it, and Guide is still cleared.
- **The session machine:**
  - desktop control rides the keyboard seat of the member named only, and ends on each event listed above;
  - the property test gained `Op::Desktop`, and checks that no grant carries `DESKTOP` but the one given, on the keyboard seat. A temporary failing assert showed it reaches a desktop grant at once.
- **Session scenarios** (72, three runs in a row):
  - **`a_trusted_device_controls_the_home_desktop`:**
    - a friend can't ask for it, and holding the keyboard seat on a screen gets nothing in;
    - the laptop's desktop control takes the seat, its window is told, the home view names it, the pointer is kept on the screen's area;
    - the Windows key and Alt+Tab reach the home PC with nothing in front;
    - the trust guard refuses a new PIN, a new device, accepting another PC and running one;
    - stopping lets the seat and the pointer go;
    - on a window, the friend's Windows key stays blocked.
  - **`desktop_control_ends_with_the_operator`:** the panic key (Alt released), the laptop's own lock, removing it, remote play off, and the laptop closing its stream window each end it.
  - **`the_streamed_window_is_brought_to_front`:** refused with nothing streamed; from the home console and from the laptop, the window comes forward.
- **The room-security checks:**
  - proto, room and net;
  - rooms (27);
  - relay;
  - sessions three times.
- **The input-safety checks:**
  - `release-check`: 245 / 105 / 197 / 356 / 0 ms;
  - `input-sink`: everything arrived in order, and the 6 blocked combinations were held back;
  - `lifecycle-check`: all six passed.
- **Front end:**
  - vitest (49), with `desktopBlocked`, `desktopCommand`, `bringToFront` through `viaRemote`, and the banner;
  - `npx tsc --noEmit`;
  - `npm run shots`, with the scenarios `remote-desktop`, `remote-desktop-ready`, `remote-home-desktop` and `bring-to-front`, and the shot `settings-remote-desktop`.

## The review

A fresh review agent, as a security reviewer, found that only the unlocked operator in the session's room can get `DESKTOP`. It confirmed that every way an operator goes ends desktop control, that the router still releases a desktop holder's keys on every path but a front change, and that Bring to front touches only the streamed window.

Fixed:
- **A desktop grant given back before the router reported it** landed untracked:
  - no banner, no trust guard;
  - removing the device no longer ended it.

  A patched laptop could do this on purpose. Giving the keyboard seat back now revokes a desktop grant still on its way. The engine also takes back any desktop grant the session no longer gives, erring towards taking back a later plain grant to the same member.
- **The pointer couldn't reach the taskbar:** the bounds were the screen's work area. They are the whole screen now, and a screen that's gone gives no bounds (moves stay relative) instead of the primary screen's.
- **The first move after a stream switch** used the old bounds: they are refreshed before each input now.
- **"Trust is frozen" was overstated:**
  - `remote-play.json` is plain JSON, which someone with desktop control could edit;
  - running another PC of the user's from the home PC's own window would stretch its PIN to that PC.

  The guard now refuses accepting, unlocking and running other PCs too. The ADR, CLAUDE.md and the protocol call it a speed bump, not a wall.

**Left:**
- the bounds aren't refreshed if the screens are rearranged mid-stream;
- the absolute moves assume the app is per-monitor DPI aware, which the desktop app is (Tauri), and `couch-cli` isn't checked.

## Still to try

- **The author's two-PC run** (the `app-manual-run` skill's "Desktop control and Bring to front" row):
  - **on the laptop:** a session with the keyboard seat, the screen streamed, Control the desktop; launch a game from Steam; Alt+Tab, the Windows key, the free key; then the game's window streamed and Bring to front;
  - **at home:** the banner over the game, Settings refusing a new PIN, and the panic key.
- **M25c**, the unattended PC: start at sign-in, the notification area, staying awake, reporting a locked PC.
