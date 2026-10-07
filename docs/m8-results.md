# M8 results: the host overlay

Until M8, a viewer's request for a seat reached the host only in the app's window: a notice, and the taskbar icon flashing. A host in the middle of a game had to leave it to answer. M8 shows requests over the game and lets the host answer without leaving it.

Your decisions:
- **answer with keys:** Ctrl+Alt+Shift+Y accepts and Ctrl+Alt+Shift+N declines, both editable in Settings. Clicks pass through the overlay;
- **the overlay also shows session notes:** watching, leaving, who plays, a request no longer pending, control paused;
- **a chime** on each request.

No wire or IPC change.

## How it works

**The keys** (`couch_platform_win::host_keys`, formerly `panic_key`)
- **One hook, several chords.** The panic key's low-level hook now watches Ctrl+Alt+Shift plus any of the host's keys: the panic key, Accept and Decline.
  - The modifier tracking is shared and unchanged: physical keys only, and a guest's injected key-ups can't block a chord.
  - Each chord key keeps its own state, so a key held for the game stays the game's.
- **Why the hook.** It ignores injected keys, so a guest holding the keyboard seat can't accept their own request by typing the chord, any more than they can press the panic key. `RegisterHotKey` or the webview would take injected keys too.
- **The shortcuts table** gains two host rows, `acceptRequest` (Y) and `declineRequest` (N). Everything from P3 applies: they're editable in Settings, no two rows share a key, and the same keys are allowed.
- **What a press does.** Accept and Decline go to the room actor, which answers the oldest request through the same `HostSession::answer` as the app's buttons.
  - **Only a request that has been the oldest for at least 1 s** (`ANSWER_AFTER`). A key pressed for one request then never answers another that took its place: if bob withdraws just as you press Y, carol's request, however long it has waited, isn't accepted by mistake, because it has only just moved to the front.
  - A press with nothing pending, or too soon, does nothing (logged).

**The overlay window** (`apps/desktop/src/useOverlay.ts`, `overlay.tsx`, a second page in the Vite build)
- **The main window decides.** It already follows the rooms, so it works out what to show:
  - the pending requests, oldest first;
  - notes made by comparing each view of our session with the previous one.

  When there's something to show and the app's own window isn't in front, it opens the overlay and sends it the list. When the list empties, it closes it.
- **It never takes the focus.** Guests' keyboard and mouse reach the game only while it's in front, and some games pause without it. So:
  - the window is created unfocused and unfocusable (tao gives `WS_EX_NOACTIVATE` and `SW_SHOWNOACTIVATE`);
  - it's closed when empty rather than hidden, because tao shows a hidden window again with a plain `SW_SHOW`, which may activate it;
  - moving it uses `SWP_NOACTIVATE`.

  The plan had a spike to try hiding and showing first. I skipped it, because tao's code already answered it; your run checks the result.
- **No white flash.** In your first run a white box blinked where the overlay was about to appear: a new window shows the system's white until its page has loaded. Now each overlay is created off every screen, and moved into its corner once its page says it has loaded (or after 3 s). Moving is `SWP_NOACTIVATE`, so the focus rule holds. Its background colour is transparent too.
- **Clicks go through it** (`setIgnoreCursorEvents`), and **it's left out of captures** (`content_protected`, i.e. `WDA_EXCLUDEFROMCAPTURE`), so a stream of the whole screen never shows it.
- **Where it sits:** top right of the work area of the monitor showing the streamed window, or of the streamed screen. The engine gives the area (`SessionView.overlay_area`, from `couch_platform_win::monitor`), and the primary monitor is the fallback.
- **Its permissions:** the overlay page may only hear events and say it has loaded (`capabilities/overlay.json`). The app's own commands are now listed in `build.rs`, so only the main window, whose capability grants them, may call them; the overlay can't reach the engine. The main window gained the right to create, move and close the overlay and make it click-through.
- **One step at a time.** Opening and closing go through one queue that always settles on the latest wish, waiting for a closing window to be gone before a new one is made, so an open still loading and a close still going can't leave the wrong state.
- **Closing the app's window ends the app** even while the overlay is open (the adapter exits when the main window is destroyed).

**The taskbar stops flashing** once no request is left, whether answered with the keys, withdrawn or ended (your request after the first run). It used to flash until you focused the app's window, though there was nothing left to answer.

**The chime** (`chime.ts`): two short tones made with Web Audio, so no sound file ships. The audio context is created by the Go live click, which the webview's autoplay rule needs.

**Settings → Overlay:** "Show requests and events over the game" and "Play a sound when someone asks for a seat", both on by default, kept in the profile's web storage.

**Limits**
- **True exclusive full screen** draws over the overlay; the chime and the keys still work there.
- **A game running as administrator** in front keeps our hook from seeing keys, as for the panic key (measured in M6). The overlay still shows; answer in the app.

## Also in this change: your internet test

You tested two PCs over the internet on 2026-10-07: the link said "direct", and hosting from this PC worked. Hosting from the other PC failed: every stream ended as soon as this PC started watching.
- **The cause.** The capture library refuses to start a capture that asks for an option this Windows lacks. We asked to hide the yellow capture border, which needs Windows 11; the other PC's log said "Toggling the capture border is not supported…". The minimum frame interval (newer Windows 11) and the cursor choice (Windows 10 2004) would have failed the same way.
- **The fix** (`couch_platform_win::capture::settings`): ask for each option only where Windows supports it, otherwise keep Windows' default. On older Windows that means the border shows around the window on the host's screen (not in the stream), frames come as often as the screen changes, and the cursor is captured. Nothing changes on this PC.
- **The guest package's empty `desktop.log`.** `run.cmd` sent only stderr to the file, while the app logs to stdout. Now both go there.
- **Full screen, re-checked** (the last polish item): with this PC hosting, the guest's input reached the game in full screen, and the guest could switch the game to full screen remotely. The M5b observation (a full-screen game ignoring the guest) didn't come back; it's closed.
- **Also seen:** the other PC has no ViGEmBus, so guests' controllers can't reach games it hosts until it's installed.
- **Still to do:** hosting from the other PC again, with the new package.

## Review

A review subagent read the change (no high-severity findings). Each finding was checked against the code:
1. **The 1 s rule timed the wrong thing** (medium): it counted from a request's arrival, so a request that moved to the front when the one before it went (withdrawn, left, asked for another seat) could be answered at once by a press meant for the other. Now it counts from when the request became the oldest, kept after every change to the session. The scenario checks a press right after one moves up does nothing.
2. **Opening and closing could race** (medium-low): a close waiting on a slow open, or an open finding a window still closing, could leave no overlay while a request waited. Fixed with one queue (above).
3. **Notes** (low): a declined request read "bob is no longer asking", as if bob had withdrawn; now "bob's request ended". Someone who lost a seat straight to another viewer got no note; now they do (not when they only moved seats). Someone who left the room is still named.
4. **The taskbar kept flashing** when the stream ended with a request pending (low): it stops then too.
5. **The overlay page could call the app's commands** (low): Tauri lets a local page call any command of an app that doesn't list them. Now listed and granted to the main window only.
6. **The focus rule held partly by luck** (low): after creation, tao applies any style change with a plain `ShowWindow(SW_SHOW)`, harmless only because the overlay is already visible (off screen). Commented in the code, so nobody hides it or restyles it hidden.
7. **One bad answer key would cost the panic key** (low; can't happen today, as every allowed key has a plain scancode and no two rows share one): if the three keys can't be hooked, the panic key alone is. New scenario.
8. **A key moved before M8 may be reset** (low): a viewer shortcut you had moved to Ctrl+Alt+Shift+Y or N now clashes with the new defaults, and that row goes back to its default (logged). Move Accept or Decline first if you want it back.
9. **The chime held the sound device** (low): it now lets go between chimes.

Also noted: on Windows 10 before version 2004, `couch-cli host` with the cursor off would capture it anyway (the app always captures it).

## Checks

| Check | What | Result |
|---|---|---|
| Hook (`couch-platform-win`) | The old chord tests, now on several keys; several keys each press their own and report their index; one key held for the game while another is pressed as a chord; injected Y and N never count, with injected or real modifiers; install refuses a key without a plain scancode or two keys alike; every allowed shortcut key has a plain scancode | pass |
| Monitor areas | The primary monitor has one; a handle that isn't a window or monitor falls back to it | pass |
| Shortcuts (`couch-input`) | The host's rows are chords, scope Host, never the viewer's; defaults Y and N | pass |
| Session scenarios | 31 (2 new: the hook gets every host key, a moved Accept included; the host view names the answer keys; a press with nothing pending does nothing; a press right after a request, or right after one moves to the front, does nothing; once seen, Decline answers the oldest and Accept the next, which gets the seat. And if the answer keys can't be hooked, the panic key still is). 3 runs in a row | pass |
| Workspace | All tests; clippy (all targets), fmt, `tsc` and the Vite build (two pages) clean | pass |
| `input-sink` | 118 keys, 5 buttons, 4 moves, 3 wheel turns through the seat router; 6 blocked combinations held back | pass |
| `release-check` | 268 / 65 / 166 / 366 / 0 ms (limits unchanged) | pass |
| `lifecycle-check` | swap 25 ms; exit 28 ms; source lost 5.2 s; stream window restarted 46 ms; host lost 4.7 s; a key left held released at the next start | pass |
| Your run | Below. The overlay shows over the game and the keys answer; after two fixes (a white flash before each toast, and the taskbar still flashing after an answer) everything works; re-checked after the review's fixes (the commands granted to the main window only, the overlay's queue, the 1 s rule) | pass |

## Your run (one PC)

The default profile hosts a game window; alice and bob watch. Keep the game in front, with the stream windows behind it.
1. alice asks for the seat: the overlay appears top right over the game with a chime, and the game keeps the focus. Ctrl+Alt+Shift+Y gives her the seat, and a note says so.
2. bob asks, and you press Ctrl+Alt+Shift+N: declined.
3. Notes: carol starts watching, alice gives the seat back, the panic key; each shows and fades.
4. Click where the overlay is: the click reaches the game.
5. Go live with a screen as the source: the overlay isn't in alice's stream.
6. Move Accept to another key in Settings; it works after the next Go live.
7. Settings → Overlay: turn the sound off, then the overlay off.
