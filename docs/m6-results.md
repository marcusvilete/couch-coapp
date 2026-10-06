# M6 results: lifecycle and safety

M6 makes sessions survive what goes wrong, or end them clearly:
- **The game's window.** The host watches the game's process. When the captured window goes, it follows a new window of the same game within 5 s (games that change display mode or leave a launcher), ends with "the game closed" when the process exited, and with "source lost" only when nothing replaces the window.
- **Liveness.** Each side sends `Alive` every second on the control stream; a peer silent for 5 s after that is closed as `timed_out`. A killed or hung host shows as lost after about 5 s instead of QUIC's 15.
- **Watching survives.**
  - A dropped connection keeps the stream window open ("Reconnecting…") while the room redials, and watching resumes on the new connection if the session is the same, within 30 s. The seat never survives: the guest asks again.
  - A crashed stream window is started again (at most once a minute) and asks for a keyframe; a second crash ends watching.
- **Keys left held.** Every key and button the host's injector presses is recorded in a marker file (`couch_platform_win::held_input`). The next start of the app or of `couch-cli host` lets go of what a killed host left down; a panic lets go at once.
- **Safety rules.**
  - Alt+F4 joins the keys a guest can never send.
  - Go live marks programs running as administrator and warns when one is picked: guests' keyboard and mouse can't reach them, and the panic key may not work while one is in front.
  - `input-sink` now plays a guest through the seat router, and checks that the blocked combinations don't arrive.

Your decisions: no game launching (Steam relaunches games through itself); per-member keyboard and mouse permissions wait for the seats and input modes work; Alt+F4 blocked; one milestone.

**Wire changes:** `Alive` (`0x0003`, optional) and close code `timed_out` (12). No viewer IPC version change: the viewer now also sends `Leave` when its window is closed.

Measured 2026-10-06 on the M1 PC (RTX 3060 Ti).

## Checks

| Check | What | Result |
|---|---|---|
| Source watch (`pipeline::source`) | An exited game ends at once; a new window is followed and the grace starts again if it goes too; a running game without a window is lost after the grace; nothing to watch is lost at once; repeated reports keep the first time | pass |
| Game process (`couch_platform_win::game`) | Our own process runs and lists its tree; an exited one doesn't; elevation of our own process and of a missing one is "no" | pass |
| Held input (`held_input`, `markers`) | Markers round-trip; only stale markers (process gone, or id reused) are released and removed, doubt keeps them; garbage is ignored | pass |
| Liveness over loopback (`couch-net` `liveness`) | A peer that goes silent after `Alive` is timed out after the limit; one that keeps sending isn't, and `Alive` is never handed up; one that never sent it is never timed out | pass |
| Alt+F4 | Blocked with either Alt held, allowed alone or with Ctrl | pass |
| Session scenarios | 20, 15 runs in a row after the review fixes (8 new, the last two from the review: the game's other window is never followed; watching during the grace waits for the new window. The others: the window closing ends the session only after the grace; a swapped window keeps streaming, then the game exiting ends it; the game exiting ends it at once; a swap while nobody watches is followed; a crashed stream window comes back once, holding its seat, and a second crash ends watching with the seat back to the host; watching resumes after a dropped connection with the same window and no seat; a session that ended meanwhile isn't resumed) | pass |
| Room scenarios | 20, with `Alive` on every connection | pass |
| Workspace | All tests; clippy (all targets), fmt, `tsc` clean | pass |
| `release-check` | Viewer's main process killed: suspended after 241 ms. Window killed: gone, 64 ms. Closed: gone, 164 ms (the window now says `Leave` first). Frozen: suspended, 367 ms, active again after thawing. Host stopped: 0 ms | pass |
| `input-sink` | 118 keys, 5 buttons, 4 moves, 3 wheel turns through the seat router, in order; the Windows key, Alt+Tab, Alt+Esc, Ctrl+Esc, Ctrl+Shift+Esc and Alt+F4 held back | pass |
| `lifecycle-check` | Swap: followed after 21 to 25 ms. Exit: game exited after 18 to 273 ms. Window closed: source lost after 5.2 s. Stream window killed (added after your run): started again after 50 ms, the seat never suspended. Host killed: viewer lost it after 4.2 to 4.7 s. Host killed holding F24: F24 stayed down, then was released as the next host started. Again after the review fixes: 28 ms, 31 ms, 5.2 s, 44 ms, 4.7 s, released at start; and `release-check` 247 / 66 / 168 / 358 / 9 ms | pass |
| `gates --quick` | G1 p50/p95 13.6/14.6 ms. G3 p95 82.2 ms (limit 118), 19.2 Mbit/s under loss. G4 back in 112 ms, 1 keyframe. G5 settled in 0.33 s. G7 second viewer unaffected (14.5 ms, 20 Mbit/s) | pass |
| **Your run** (two PCs) | Quitting the game: the notice. Switching the game between windowed and full screen, also from the guest: the stream went on, and the guest's controller worked in both modes (in M5b's two-PC test it didn't in full screen; see below). Turning the guest's network off while it played: the picture froze, "Reconnecting…", the stream came back, and control was gone. Killing the host app: the guest got the "stream ended" notice. Notepad as administrator: marked, with the warning; its keyboard and mouse can't be controlled, as Windows rules, and **the panic key doesn't work while it is in front** (the open question from M5b, now measured; the warning says so). Alt+F4 on the guest closed its own stream window, then, after the fix below, no longer did. The basics (watch, ask, pass, the panic key, stop) work | pass |
| Stream window killed on the guest | From Task Manager's Processes tab it looked like stopping watching: that End task asks the window to close, which counts as leaving. From the Details tab, which kills outright, the window came back by itself and the guest kept the controller | pass |

## Found and fixed while checking

- **`couch-cli view` could exit without saying the connection was lost**: its `Alive` send failed once the connection closed and ended the run first, in about one run of three. The send's error is now ignored; the watch reports the loss.
- **Alt+F4 on the guest closed its own stream window** (your run). Windows closes the focused window on Alt+F4 before anything is forwarded; the host blocked it, but the guest lost the stream. The stream window now ignores Alt+F4 while its keyboard goes to the game (holding the seat with keyboard permission, through SDL's `SDL_WINDOWS_CLOSE_ON_ALT_F4` hint); while only watching, Alt+F4 still closes it.

From the code review (a subagent read the whole change; each finding was checked against the code before fixing):
- **A restarted stream window brought back what the crashed one held.** The forwarder kept the dead window's pad, keys and buttons, and its heartbeat kept sending them: a trigger or key held at the crash stayed held on the host until the guest touched it again. A restart now sends a neutral pad and an empty held set at once (`Forwarder::viewer_replaced`); the crash scenario holds A across the crash and checks the host's pad goes neutral.
- **Following the game could pick a window the host never chose**: another window of the same browser, or Explorer's desktop. Only a window that appeared around the time the old one closed is followed now; windows that were there before (seen on two looks in a row, a second apart, while the captured one was up) never are. New scenario: the game's other window is never streamed.
- **Watching during the grace ended the session as an error**: the capture tried the dead window. It now looks for the new window first. The fake backend now fails to capture a window that is gone, as Windows does, which is how the tests missed it; new scenario.
- **A launcher handing over to the game ended the session as "game exited"**: the exited process's children are now searched too, and the followed window's process becomes the game.
- **Reconnect and join timers could fire for a later attempt**, cutting a second wait short. They now carry the attempt they were set for, and a drop during a resumed join goes back to waiting within the same 30 s.
- **A window the user closed could be taken for a crash** if its `Leave` was still in the pipe. A window that went now gets half a second to finish what it said.
- **A late "connection lost" about an earlier connection** could end a resumed watch. Connections are numbered; a stale report is ignored, and a new connection always starts without a seat.
- Smaller: a listing kept while waiting to reconnect is dropped when watching ends any other way; the held-input marker is rewritten after a failed write, its panic hook retries the lock briefly, and half-written markers of dead processes are cleaned; listing windows runs off the async workers; a stopped capture no longer reopens; protocol.md no longer claims `Alive` stops when only the room actor hangs.

**The full-screen problem from M5b's two-PC test** (a game in full screen got no input from the guest) didn't show this time: the controller worked in full screen and windowed. If that game opens a new window when it switches, M6's following the game is the likely fix; the host's log says so with "the game opened a new window; streaming that one". Kept as an observation: at the polishing stage, check first whether it reproduces at all.

## Things to know

- **A reconnecting guest is a new viewer.** The seat goes with the old connection, as every safety rule wants; the guest asks again.
- **Only a session with the same id resumes.** If the host stopped and went live again while the guest was away, watching ends ("the stream ended") and the new session is listed.
- **The grace for a new window is 5 s.** A game that takes longer to show its new window ends the session as "source lost".
- **The held-input marker** is in `%LOCALAPPDATA%\CouchCoApp\held-input\`, one file per process while it holds anything.
- **An elevated program** is told by its token; a process whose rights we can't read at all counts as elevated. While one is in front, the panic key's hook sees nothing (Windows doesn't run a normal program's hooks for an administrator's window); Take back in the app works.
- **Closing the stream window counts as leaving**, however it is asked to close: its X, Alt+F4 while only watching, the HUD's Leave, Task Manager's End task in the Processes tab. Only an exit without that (a crash, a kill) restarts it.

## Follow-ups

- **Seats and input modes** (very important), with per-member permissions.
- **Full screen: an observation, not a known bug.** It failed once in M5b's two-PC test and worked in M6's run; at the polishing stage, first check whether it reproduces at all.
- **Tell players when the host's game isn't in front** (your observation, for polishing): the stream window should say so, since keyboard and mouse only go in while it is, and many games ignore a controller in the background. The host already checks the foreground before every input; the viewer would need a new (optional) message to hear it.
- **Editing shortcuts.**
- **M7:** our own relay and real-network tests.

## How to reproduce

```powershell
. .\scripts\dev.ps1
cargo test --workspace
cargo test -p couch-engine --test sessions
cargo build --release -p couch-cli -p couch-viewer
.\target\release\couch-cli.exe release-check
.\target\release\couch-cli.exe input-sink
.\target\release\couch-cli.exe lifecycle-check      # or --only swap,exit,close,kill-host,stale-key
.\target\release\couch-cli.exe gates --quick
```
