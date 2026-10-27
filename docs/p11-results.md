# P11 results: the stream window's controls in the app

## Why

Many watch with the stream window on one monitor and the app on the other (your note, 2026-10-26). The app's card for a session we watch could ask for seats, give them back and stop watching, but the stream window's own controls (volume, full screen, the stats, the bar) and two of its lines (the game out of front on the host's PC, the host's notes) were only on the window's bar. You also asked for a way to turn the bar off.

Between the app and its own window only: viewer IPC v12, **no wire change** (still revision 6, so 0.17.5 talks to 0.16.0 and later).

## What changed

- **A "Stream window" panel** under the card we watch (`sessions/StreamWindowPanel.tsx`), shown only while we watch, so the card never moves:
  - the volume: mute, a slider 0 to 100%, in step both ways with the window's own (saved to `audio.json` as before);
  - **Full screen** (or Back to a window) and **Show window**, which brings the window up from behind another window or from minimized;
  - **Bar** and **Stats** switches. Bar hides the session bar alone: the centre card ("Bia isn't streaming anything yet") and the stats still show (your first try found it hid everything, as the window's own shortcut does; split since);
  - a line above them: "The game isn't in front on Ana's PC: your input may not reach it" (amber, while playing or asking), else the latest note ("The host said no, for now"), else nothing; "Opening the stream window…" while it opens, the controls waiting in their places.
- **The card's right-click menu** has the same window actions.
- **Settings → Stream window**: "Show the session bar over the picture" (`stream-window.json`, in backups). New windows open without it when off (`couch-viewer --no-bar`); an open one follows at once. The window's own "Show or hide the overlay" shortcut still hides everything over the picture, the bar included.
- **Underneath:**
  - IPC `WINDOW` (main to viewer: full screen, bar, stats, raise) and `WINDOW_STATE` (viewer to main: its full screen, bar and stats, at once and on every change, however it changed); `VOLUME` is also sent mid-stream now. A window started again after a crash keeps its bar and stats.
  - **Bringing a window to the front from another process:** Windows refuses a background process the foreground unless the process in front allows it. The app is in front when its button is clicked, so it calls `AllowSetForegroundWindow` on the stream window's process before asking it to raise (`couch_platform_win::foreground::allow_foreground`).
  - `SessionView` gains, for a session we watch, `streamWindow` (volume, muted, open, full screen, bar, stats), `gameInFront` and `note`. A change of the game's focus now refreshes the app's view too: it only reached the window's bar before.
  - Commands `SetStreamVolume`, `StreamWindow`, `GetStreamWindow`, `SetStreamWindow`.

## Checked

- Session scenarios (two new, with the fake stream window speaking the new IPC): the app's volume reaches the window and the window's the app; full screen and Raise reach the window and its state comes back; the window's own changes reach the app; the game leaving the front reaches the app; a crashed window starts again with its bar off; the setting off starts a window without its bar, and turning it on reaches an open one. 64 pass.
- The IPC payloads roundtrip, and trailing bytes are refused.
- `npm run shots`: the new scenarios `watching-window`, `playing-away` and `watching-opening`, Settings → Stream window, and a no-jump check ("watch card") that toggles full screen and the bar and finds neither the card nor the panel moving.
- The input-safety checks: `release-check`, `input-sink`, `lifecycle-check`.

## The review

A fresh review agent found nothing serious, and checked Show window can't be reached from a peer, that nothing echoes in a loop, and that restarts and reconnects keep the right state. Fixed:
- **With the bar off, a player had no guidance left in the window**: no "game isn't in front", and nothing saying which key frees a captured mouse (one monitor, full screen, the app out of reach). The bar now still comes up for those two, whatever the setting (`hud.rs`, `bar_up`'s `needed`); the rest stays off.
- **The app's slider could stay out of step** when the window's slider moved while one of the app's changes was on its way: it now catches up once all are answered, and window requests no longer count as volume changes.
- A change still waiting when the panel went could undo a mute (it kept the first render's mute); a second quick click on Mute sent the same value. Both now go by what was last asked.
- The note stayed in the panel for the rest of the watch: it now goes after 5 s, as on the window's bar.
- The scenario now checks a restarted window keeps its stats too, and that the foreground is allowed to the window's own process before it raises.
- The slider waits, disabled, while the window opens; the menu's bar and stats entries have icons; a doc comment said the state waits for `HELLO` (it's sent at once).

## Your run (2026-10-27, this PC)

- Turning the bar off first hid the centre card too ("isn't streaming anything yet"): the switch was the window's whole overlay. Split so it hides the bar alone; now "it works the intended way".
- Full screen and back to a window, and Show window, both work.

## Your later run (0.19.1, 2026-10-27)

The stream window's controls from the app work.

## Still to try

- With friends.
