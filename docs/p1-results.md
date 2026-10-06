# P1 results: seats and input modes

The first polishing milestone. It makes passing controllers around fit the game being played:
- **The host sets up the game's input when going live**, remembered per program:
  - **Single player:** one seat, which takes a controller, keyboard and mouse, or both.
  - **Multiplayer:** 1 to 4 controller seats, each its own virtual Xbox 360 pad, plus optionally one keyboard-and-mouse seat.
- **A seat decides what its holder may send.** The host approves every holder, so per-member permissions aren't needed: handing someone the keyboard seat is the trust decision.
- **Requests name a seat.** A guest asks for a free seat, one another guest holds, or a move to another seat.
  - The host accepts or declines, passes a chosen seat, and takes one seat or all of them back.
  - **One seat per guest:** a grant first revokes that guest from every other seat, on the same ordered channel to the seat router.
  - **Only one holder at a time has keyboard and mouse:** the router refuses a second.
- **"The game isn't in front."** The host tells its viewers when the streamed window leaves the front (`GameFocus`). The guest's stream window then says input may not reach the game, and stays visible for a player until the game is back.
- **The host's own controllers.** Go live warns in multiplayer when a game controller is plugged into the host.
  - Counted from Windows' HID game controllers: `couch_platform_win::controllers`.
  - Whether to hide them (HidHide) waits on the co-op investigation in your two-PC run.

Your decisions:
- the seat kinds above;
- one seat per guest;
- investigate the host's controllers before hiding them;
- the "not in front" notice in this milestone, the shortcut editor and the full-screen re-check later;
- the session messages changed in place, since nothing is publicly released.

You also moved M7 (our own relay) to just before packaging, and asked for a Settings → Network relay choice at a later polishing stage (in the backlog).

**Wire changes:**
- `SessionAnnounce.seats` (replacing `mode` and `keyboard_mouse`), `SessionStatus.holders` (replacing `controller`) and `ControlRequest.seat`, all changed in place.
- New optional `0x0607 GameFocus`.
- Viewer IPC v4: the HUD's seats and `game_in_front`; `RequestControl(seat)`.

Measured 2026-10-06 on the M1 PC (RTX 3060 Ti).

## Checks

| Check | What | Result |
|---|---|---|
| Seats on the wire (`couch-proto`) | Seat limits (1 to 5, at most 4 pads, at most one keyboard); a status whose holder isn't a viewer, or with a viewer on two seats, is invalid; labels ("Controller 2", "Keyboard and mouse"); the largest status fits a control frame; `GameFocus` round-trips and is optional | pass |
| Router (`couch-input`) | A second keyboard or mouse holder is refused; a pad-only seat beside it isn't; the keyboard seat can change hands | pass |
| Session machine (`couch-room`) | Grants carry the seat's permissions; a move revokes the old seat first, even before the router reported the first grant; asking for another seat replaces the request; one seat taken back leaves the other. Property test (256 runs, two pads and a keyboard seat, router reports late): nobody ever holds two seats in the router, every grant carries its seat's permissions, holders are viewers, one request per viewer, the mirror ends matching the router | pass |
| Session scenarios | 25 (5 new: two guests on two pads at once, each guest only on its own pad, one seat taken back, the panic key taking both; moving to another seat, the old pad neutral before the new one hears the guest, then the host moving it back; a keyboard seat typing while a controller seat plays, neither crossing over, its key released when taken back; the viewers hearing the game leave the front, a new viewer too, and come back; the setup remembered for the program, a bad one refused). 7 runs in a row | pass |
| Workspace | All tests; clippy (all targets), fmt, `tsc`, the Vite build and `cargo deny` clean | pass |
| HUD by eye | `--hud-preview` now shows a multiplayer session: watching (an Ask button per seat), asking (the asked seat greyed, Withdraw), playing (its seat named, the "game isn't in front" line) | pass |
| `release-check` | 266 / 63 / 175 / 358 / 10 ms (limits unchanged) | pass |
| `input-sink` | 118 keys, 5 buttons, 4 moves, 3 wheel turns through the seat router; 6 blocked combinations held back | pass |
| `lifecycle-check` | Swap followed after 30 ms; game exited after 28 ms; source lost after 5.2 s; stream window restarted after 37 ms; host lost after 4.7 s; a key left held released at the next start | pass |
| Two real pads | `pad-test --count 2 --real-pad-ids`: XInput slots 0 and 1, each moving on its own; Windows' game-controller count 0 before, 2 with ours (after the fix below) | pass |
| **Your run** (two PCs) | Single player: a controller seat, a keyboard-and-mouse seat, and both each drove the game as set; the host's own keyboard and mouse kept working too (by design: the host can always step in). Co-op (Fling to the Finish) without a controller on the host: the guest played as Controller 2, the host joined on keyboard and mouse. Co-op with the host's DualSense: the game made the DualSense player 1 on its own. The guest on Controller 2 (XInput slot 1) played; on Controller 1 (slot 0) the input arrived but the game ignored it, because the game had given no player to that pad. The app showed Controller 1 as "free" while two people played. See "The host's own controller" below | pass, with the finding below |

## Found and fixed while checking

- **The controller count missed Xbox controllers.** It looked for the HID game-controller usage in the devices' compatible ids. An XInput pad's "HID-compliant game controller" has none: the usage is in its hardware ids. Both lists are read now.
- **"Keyboard and mouse: Alice"** in the multiplayer bar: a free seat was shown as the host's. In multiplayer the host plays beside the seats, so a free seat now reads "free" (stream window and app).

## The host's own controller (your co-op run)

- **What the game saw:** three controllers. Your DualSense (read directly, not through XInput: `xinput-watch` showed only our two pads, in slots 0 and 1) and our two virtual pads. It gave player 1 to the DualSense and player 2 to whichever virtual pad pressed a button first. The other virtual pad had no player, so input on it did nothing in the game, although it reached the pad.
- **So our "Controller 1" isn't the game's "player 1".** A game numbers players by its own rules (usually who joins first), and the host's own controller is one of the controllers it counts. Our seats are only the virtual pads.
- **Why the app said "free":** the host isn't on a seat. The host's own devices drive the game directly, outside the seats, so the app can't show or hand them over.
- **What would make it seamless:** the host plays through a seat too, from the start. The app reads the host's controller and feeds a virtual pad, and the host's physical controller is hidden from games (HidHide), so every player in the game is a virtual pad and any of them can be handed over. That is the "host on a seat" work; see the follow-ups.

**From the code review** (a subagent read the whole change; each finding was checked against the code before fixing; nothing of high severity):
- **A withdraw could cost a guest its seat.** `ControlCancel` meant "withdraw my request" or, with none pending, "give my seat back". A guest asking to move whose withdraw crossed the host's decline, or arrived during the decline cooldown, lost the seat it was playing on. `ControlCancel` now says which (`what: Request | Seat`); a withdraw never touches the seat. New tests: in the session machine (the race), the property test (a withdraw never revokes), and the move scenario (withdraw, keep playing).
- **A request and a pass that crossed** left the host with a request the guest couldn't see. A grant now answers only a request for the seat granted; one for another seat stays pending on both sides, as a move.
- **Asking for the seat you hold while asking to move** desynced the two sides; it now withdraws the move.
- **While asking to move, the stream window's bar stayed pinned** over the game and said "Waiting…". It now says "You're playing as Controller 1 · asked for Controller 2" and hides as usual; the stage card says the same.
- **Go live carried one program's remembered setup over to the next window picked** (even a keyboard seat). A window with nothing remembered now gets the default, one controller.
- **The controller count gave up on a long id list**; it now retries at the size Windows asks for.
- Docs: several pads unplugged at the end; the app's real pad ids; `ControlCancel`'s two meanings.

**Fixed after your run (your choice: a small fix now, the host seat next).**
- Wherever seats are listed (your panel, the stage card, the stream window's bar), the host's own controller, keyboard and mouse now show beside them ("Ana: own controller").
- The host panel says that games number players themselves, and that a guest whose seat doesn't respond should be moved to another seat.

## Follow-ups

- **P2, the host on a seat** (next, your decision). You start on Controller 1 with your own controller: the app reads it and drives that seat's virtual pad, and HidHide hides the physical one from games. Every player in the game is then one of our pads, and any seat, yours included, can be handed to anyone. Needs HidHide installed once (administrator), and the controller must be shown to games again whatever way the app ends.
- **Then the rest of the polish backlog:** the shortcut editor, choosing the relay (Settings → Network), the splash screen, and the full-screen re-check.

## Things to know

- **All pads are plugged in at go-live**, in seat order, and stay for the session. A game that makes a player for every controller it sees will show idle players for free seats: set fewer controllers.
- **The host's own controllers aren't hidden.** They take XInput slots before our pads (they were plugged in first), and some games give them a player. Go live warns; your run's investigation decides what comes next.
- **The keyboard seat has no pad.** Its holder's gamepad goes nowhere, by design.
- **Moving seats** shows the guest a short "your seat went back to the host" before "You have Controller 2": the old seat is released first, on purpose.
- **Ctrl+Alt+Shift+C** asks for the first seat nobody holds (controllers first); the bar has a button per seat.
- **The setup file** is `input-setups.json` in the profile folder, by the program's file name, lowercased. Only the engine writes it, never the room actor (which writes `settings.json`).
- **The "not in front" notice is coalesced** to at most one message per 250 ms; nothing is sent for a screen.

## Your run (to do, two PCs; a fresh `pack-guest.ps1` build on the guest)

1. A single-player game with a controller seat, then with a keyboard-and-mouse seat: the guest drives it with exactly that.
2. Fling to the Finish in multiplayer with 2 controllers, **no** controller on the host: the guest asks for Controller 2 and plays player 2.
3. **The investigation:** the same with the host's controller plugged in, and `.\scripts\xinput-watch.ps1` on the host. Note which slot each pad takes and whether the game ignores the guest's pad.
4. Asking for a specific seat, passing a seat, moving to another seat.
5. With keyboard and mouse on, switch the host to another window: the guest's stream window says the game isn't in front, and the line goes when it is back.
6. The basics: watch, the panic key, stop.

## How to reproduce

```powershell
. .\scripts\dev.ps1
cargo test --workspace
cargo test -p couch-engine --test sessions
cargo build --release -p couch-cli -p couch-viewer
.\target\release\couch-cli.exe release-check
.\target\release\couch-cli.exe input-sink
.\target\release\couch-cli.exe lifecycle-check
.\target\release\couch-cli.exe pad-test --count 2 --real-pad-ids --secs 10   # with .\scripts\xinput-watch.ps1
cargo run -p couch-viewer -- --hud-preview controlling                         # or watching, asking
```
