# P2 results: the host on a seat

In your P1 co-op run, Fling to the Finish gave player 1 to your DualSense, which it read directly. So a guest's virtual pad on Controller 1 got no player, and the app said Controller 1 was "free" while two people played. P2 lets the host play through a seat too:
- **The app reads your controller** and feeds its state into the seat router as your own holder, like a guest's pad (`couch_gamepad`, SDL3 on a thread of its own).
- **HidHide hides the physical controller** from every other program while you're live (`couch_platform_win::hidhide`). Every player the game sees is then one of our virtual pads, and any seat, yours included, can be handed to anyone.
- **You start on Controller 1** (the first seat with a pad). "Play here" moves you to another seat. A guest may ask for yours; you decide.
- **Your seat comes back to you** when the guest you gave it to gives it back or leaves. **A seat you take back stays empty**, and so does your own when you leave it ("Leave"); "Play here" puts your controller on a seat again. The panic key removes the guests and leaves you playing (changed after your two-PC run; see below).

Your decisions:
- hide per session: at Go live, shown again at Stop;
- one controller, which you pick; others stay visible to games;
- without HidHide, play outside the seats as in P1.

**Wire changes** (in place; nothing is publicly released):
- `SessionStatus.host_seat`: the seat the host plays, if any. `holders` stays viewers only. Later `host_on_seats`: the host's controller plays only through the seats, so guests see an empty seat as empty.
- Viewer IPC v5: `HudSeat.host`; v6: `HudState.host_on_seats`.

Measured 2026-10-06 on the M1 PC (RTX 3060 Ti), HidHide 1.5.230 installed the same day, your DualSense on Bluetooth.

## What HidHide can and can't do

Found while planning, from its documentation and source (MIT; we only call its control device and copy no code):
- **It filters device creation** for HID devices and wired and wireless Xbox controllers. A program not on its whitelist can't *open* a blacklisted device. One that already had it open keeps it. Steam keeps controllers open, so after hiding, the controller must be reconnected once.
- **Its control device** (`\\.\HidHide`) needs no administrator rights, but only one process may hold it open at a time: while its configuration window is open, we can't hide (Go live says so).
- **A "session blacklist"** that the driver clears when our process exits, crash included, is in its source since April 2026 but in no release yet. We use it when the driver has it. With 1.5.230 the entries go on the persistent blacklist, and a marker file (`%LOCALAPPDATA%\CouchCoApp\hidden-controllers\`) records exactly what we changed, so we can undo it after a crash.
- **A controller is several devices** (yours: a Bluetooth node, a HID device, and another Bluetooth service node). We hide every one sharing its container id, the identity Windows gives a physical device. A controller built into the PC (a handheld) hides only its own devices, never the PC's container.

## Checks

| Check | What | Result |
|---|---|---|
| Session machine (`couch-room`) | The host starts on the first seat with a pad; passed, taken back, given back or left by a guest, the seat comes back to the host by a claim; "Play here" moves the host, and the old seat's release never claims it back (that would give them two); the panic key leaves the host their seat; the host never joins as a viewer. Property test (256 runs, also 4096 once), with and without the host: nobody holds two seats in the router, the host is granted the pad only, viewers are never granted while paused, only the host claims, every status is valid, and once reports settle the host is back on their seat unless a viewer has it | pass |
| Wire (`couch-proto`) | A status whose `host_seat` isn't a seat or is a viewer's is invalid; the largest status still fits a frame | pass |
| HidHide wrapper | Control codes match HidHide's; MULTI_SZ round trips; our entries come off the list and yours stay, also when another live host still hides one; entries you had already are never recorded as ours; our own markers count as live only while a guard owns them; a controller's devices found by container, by USB and Bluetooth ids; a built-in controller hides only itself; markers round trip; a pad with our ids is refused | pass |
| Session scenarios | 28 (3 new: the host's controller drives the seat, is passed, taken back, kept through the panic key, and shown again at Stop; "Play here" and rumble back to the host's controller; hiding that fails leaves the host outside the seats, with the reason shown). 4 runs in a row | pass |
| Workspace | All tests; clippy (all targets), fmt, `tsc`, the Vite build and `cargo deny` clean | pass |
| `release-check` | 276 / 52 / 144 / 355 / 0 ms (limits unchanged) | pass |
| `input-sink` | 118 keys, 5 buttons, 4 moves, 3 wheel turns; 6 blocked combinations held back | pass |
| `lifecycle-check` | swap 30 ms; exit 27 ms; source lost 5.2 s; stream window restarted 56 ms; host lost 4.7 s; a key left held released at the next start | pass |
| `pad-test --count 2` | Windows lists 0 controllers before, 2 with ours | pass |
| Our pads never read as the host's | `host-pad` while `pad-test --count 2` runs: both skipped ("it has our virtual pads' ids"), nothing listed | pass |
| **Your DualSense** | `host-pad`: listed as "DualSense Wireless Controller (054c:0ce6)". `host-pad --feed 1`: hid its 3 devices (persistent list, HidHide turned on by us); after you turned it off and on, `joy.cpl` (reopened) listed only "Xbox 360 Controller", and `xinput-watch` followed your DualSense, through the reconnect too. 2511 states in 2 min (21 a second: changes plus the 100 ms heartbeat). A game's test rumble reached it. At the end it was shown again, HidHide turned back off, the marker gone, and `joy.cpl` listed the DualSense again | pass |
| **Your DualSense again, after the review fixes** | `host-pad` lists it without opening it. `host-pad --feed 1 --secs 90`, now through a seat router: the same 3 devices hidden, the shared "turned-on" flag written; after a reconnect `joy.cpl` listed only "Xbox 360 Controller" and `xinput-watch` followed; 6861 states, 5592 changes. At the end: shown again, HidHide turned off, the marker and the flag gone, the DualSense back in `joy.cpl` | pass |
| **Killed while hidden** | The feed killed with `Stop-Process -Force`: the marker stayed and `joy.cpl` listed no controller at all (ours went with the process, yours stayed hidden). The next `couch-cli` start: "showed 1 controller(s) a crashed host had left hidden", marker gone, the DualSense back in `joy.cpl` | pass |

## Found and fixed while building

- **A move could give the host two seats.** Draft design: the host's last seat comes back whenever it frees. But "Play here" frees the old seat, and its claim would have reached the router after the grant of the new one. Now "Play here" makes the new seat the host's home at once, and the router also refuses a claim from anyone already holding a seat. The property test's router model doesn't have that second guard, so the session machine is tested on its own.
- **The reader logged each skipped pad twice:** once at start, once at SDL's "added" event. Now once.

**From the code review** (a subagent read the whole change; each finding was checked against the code before fixing; nothing of high severity in the input-safety core):
- **Stopping could remove your own HidHide entries.** We recorded every device of the controller, even those you had hidden yourself (DS4Windows users do), and took them all off at Stop. Now we record only what we add (`ours_to_add`). Test: `the_users_own_entries_are_never_recorded`.
- **A failure partway through hiding left the controller hidden.** The undo opened a second handle while the first was still open, and HidHide allows one. Now hiding uses one handle throughout and undoes through it on any failure.
- **An undo that failed at Stop was never retried** while the app ran, and later sessions took its marker for another live host's. Now our own markers count as live only while a guard owns them, and every hide (and every start) undoes the others. Test: `our_own_markers_are_stale_unless_a_guard_owns_them`.
- **Two hosts on one PC could turn HidHide off under each other.** Now every hide writes a marker, and a shared "we turned it on" file lets the last of them turn it off.
- **Hiding gave up at once** if another call held HidHide's device for a moment; it now waits up to a second.
- **The reader kept every controller open** from the first Go live dialog on, which can change a DualSense's lights or report mode. Now listing opens nothing; only the followed controller is open, and only while followed. Idle, the reader wakes every 50 ms instead of 5.
- **Moving to a lower seat** let the old seat's "stop rumble" cut the new seat's rumble; the stop now goes only when the host holds no seat.
- **CLI:** `host --host-pad` claims seat 0 again whenever its hold ends; `--allow-kbm` applies to the first guest's seat; `host-pad --feed` goes through a seat router, as the app does.
- **Text:** a `subst` drive's path is refused for the whitelist; the host panel's "no seat" line says only the seat you last played comes back; a forced "none" in Go live is no longer remembered as your choice; `CLAUDE.md` named a command that doesn't exist.

**Your runs with the app (one PC, Fling to the Finish, Multiplayer with 4 controllers, your DualSense on a seat):**
- **One seat short.** Only 3 of the 4 seats got a player in the game; on the 4th, "Play here" did nothing in it. `xinput-watch` showed all four slots working, so our side delivered every seat. (The slots' order doesn't match the seats': Windows numbers XInput slots itself, as seen since M5b.)
- **A wrong turn.** We first suspected the game kept player 1 for the controller it saw at start, before Go live hid it, and I added a "Hide it now" button for hiding before starting the game. It didn't change anything, and was rolled back before committing.
- **The cause was Steam Input.** With it on for the game, Steam kept a player for the hidden DualSense. With it off, all 4 seats worked, in the original order too: the game running with the DualSense visible, then Go live hiding it, then one reconnect. Go live and the host panel now say to turn Steam Input off for the game.
- **"HidHide's configuration window is open" when it wasn't.** The Go live dialog's check tried HidHide's control device once, and any failure read as that window. Something else held the device for a moment, perhaps HidHide's own watchdog service or our app. The check now waits up to a second, like hiding does; an unexpected error no longer reads as busy; and the text says "busy" without guessing why. Gone on your next try.
- **Steam's global "PlayStation Controller Support" off works as well,** but then the DualSense works in games only through our app (while live). Turning Steam Input off for the one game is enough.

**Your two-PC run (2026-10-07):** everything worked. One finding: in co-op a seat could end up empty by chance (after "Play here", say), but never on purpose, because taking a seat back gave it to your controller. Your choice: **Take back always leaves the seat empty.** What changed:
- **Take back** leaves the seat empty, your own included: your row now has **Leave**. "Play here" is how your controller takes a seat again. A guest giving your seat back, or leaving, still returns it to you.
- **The panic key** frees only the guests' seats (`RevokeViewers`, which the input task applies, sparing the seat your controller plays) instead of freeing every seat and handing yours back. A seat you had passed on stays empty, and nothing comes back to you while control is paused; allowing control again returns your seat only if it went free on its own.
  - Found by the session scenario while building this: the hook frees the seats from its own thread, so the router's report could reach the session before the panic did, and the session handed the seat back. The input task now refuses claims while paused, and Allow control clears that pause before its claim.
  - Found by the property test: "take every seat back" used the session's mirror to spare your seat, which may not know of it yet, so it could take your seat. It now sends `RevokeViewers` too.
- **Guests' screens no longer say "Ana: own controller"** while your controller plays no seat: it is hidden from the game. `SessionStatus.host_on_seats` tells them; a free single-player seat then reads "Nobody is playing".
- **From the review** (a subagent; each finding checked against the code):
  - **A late router report could still refill a seat taken back.** Pass your seat, then Take back before the router's reports arrive: the guest's late "granted" report cleared the "left empty" flag, so its "revoked" report gave the seat to your controller. The panic key had the same flaw (it read the session's possibly stale view of your seat). Now only your own actions decide (`Home`: playing, lent to a guest, left empty); reports never change it. Test: `late_reports_dont_refill_a_seat_taken_back`. One simplification: a seat you lent that came back to you stays "lent", so taking everything back marks it "left empty" while you play it; that only matters if your controller's reader stops, since it reports neutral even while the controller is unplugged.
  - **The property test checked one direction only.** It now keeps its own model of what you did, from the ops, and checks both ways: no claim (including from late reports) for a seat you left empty, and your seat back unless a guest has it, you left it empty, or control is paused. A run of 50000 cases found one more case, from moving to a seat the mirror still showed you on; fixed, and its seed is saved in `proptest-regressions`.
  - The engine scenario now checks the seat stays empty for 200 ms after the panic key; the guest's note on losing a seat says "You no longer have Controller 1" instead of "went back to the host".
- Checks: session machine 74 tests (new: `a_seat_taken_back_stays_empty`, `late_reports_dont_refill_a_seat_taken_back`), the property test 50000 runs once; session scenarios 28, 4 runs in a row (the host's scenario now takes the seat back, sees it empty from the guests' side and the stream window, plays it again, and presses the panic key both ways); workspace, clippy, fmt, `tsc` and the Vite build clean.

## Things to know

- **Turn Steam Input off for the game** (Steam → the game's Properties → Controller) when you play on a seat: with it on, Steam keeps a player for your hidden controller and one seat gets none (Fling to the Finish).
- **Reconnect it once after going live.** HidHide refuses only new opens; until the controller is reconnected, a game or Steam that had it open still sees it beside our pad. The host panel says so.
- **Killed while live, your controller stays hidden** until the app (or `couch-cli host`) starts again. HidHide's own configuration client (Start menu) also shows and clears the list. HidHide's session blacklist would remove this, once released.
- **HidHide's "Enable device hiding"** is turned on while you're live if it was off, and off again afterwards (by the last of our hosts on this PC to stop). With it on, anything else you put on HidHide's list is hidden too, as you'd expect.
- **Our exe stays on HidHide's whitelist** (it only lets our own exe see hidden devices). In development that is the debug and release `couch-cli.exe` and the app's exe, by path.
- **An Xbox 360 controller on the host can't play a seat:** our pads report its ids, so the reader can't tell them apart and never reads one. Xbox One and later, PlayStation and Switch Pro controllers work.
- **On one PC**, the stream window is another program, so it can't read your hidden controller either.
- **Your keyboard and mouse** always reach the game directly; only the controller plays through a seat.
- **The PS (or Xbox) button does nothing while you're on a seat:** the seat router never passes the Guide button, yours included (Windows and Steam react to it). Steam's overlay is still on Shift+Tab.
- **Reconnect into the same USB port.** Hiding names the controller's devices, and a USB controller's ids can depend on the port: in another port it may show to games again, beside our pad. Bluetooth isn't affected.
- **Two identical controllers** (two DualSenses) are both hidden, and the app reads whichever Windows lists first. Choose by name only when they differ.
- **Your own HidHide setup stays.** If you hide the controller yourself (for DS4Windows, say), we never record those entries, so stopping leaves them hidden.

## Follow-ups

- **Your two-PC run:** done 2026-10-07; a short re-check of the seats after the Take back change (below).
- **M10:** offer HidHide in the installer, as for ViGEmBus.
- **Then the rest of the polish backlog:** the shortcut editor, choosing the relay (Settings → Network), the splash screen, the full-screen re-check.

## Your run (two PCs; a fresh `pack-guest.ps1` build on the guest)

Done 2026-10-07: everything worked. The re-check after the Take back change, done the same day on one PC (the default profile hosting with your DualSense, alice and bob watching, `xinput-watch` showing the host's input), all as described:
1. Co-op with 2 controllers, your DualSense on Controller 1. Pass Controller 1 to the guest, then Take back: Controller 1 stays empty (the guest's stage card and stream window say "free", not "own controller"), and your DualSense drives nothing. "Play here" on it: you play again.
2. "Leave" on your own row: empty again. "Play here" brings you back.
3. The guest on Controller 2, you on Controller 1: the panic key. The guest is out, you keep playing. Then pass Controller 1 to the guest and press it again: Controller 1 stays empty. Allow control.
4. Single player: pass the seat, give it back from the guest's side (it returns to you); pass it again, Take back ("Nobody is playing" on the guest's side).

## How to reproduce

```powershell
. .\scripts\dev.ps1
cargo test --workspace
cargo test -p couch-engine --test sessions
cargo build --release -p couch-cli -p couch-viewer
.\target\release\couch-cli.exe release-check
.\target\release\couch-cli.exe input-sink
.\target\release\couch-cli.exe lifecycle-check
.\target\release\couch-cli.exe host-pad                          # lists this PC's controllers and HidHide's state
.\target\release\couch-cli.exe host-pad --feed 1 --secs 300      # with joy.cpl and .\scripts\xinput-watch.ps1; reconnect once
cargo run -p couch-viewer -- --hud-preview watching              # Controller 1 shows the host
```
