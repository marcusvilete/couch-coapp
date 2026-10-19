# M18 results: several seats for one guest

## Why

A friend asked to hand more than one controller to a guest who has several: local co-op on the guest's side, two people on one remote PC. Until now a guest held one seat at most. Asking for another seat, or being passed one, moved them, and their stream window sent the state of one controller (the last touched).

## What changed

**The rule** (`couch_room::session`). A guest may hold one seat with a pad per controller they have, plus the keyboard-and-mouse seat:
- **Their stream window counts their controllers** and tells the host (`ViewerControllers`, after joining and on every change; capped at 4, 0 until told). The host panel shows it: "bob · 2 controllers".
- **Added or moved, decided by the host** (`HostSession::keeps`, your choice: by their controllers). A seat without a pad is always added. A seat with a pad is added while they hold fewer pad seats than they have controllers, counting grants still on their way; otherwise they move, their other pad seats revoked first, as before; a keyboard-and-mouse seat they hold stays.
- **The host still approves every grant.** The count is the guest's own word and only chooses add or move. A count that drops later frees no seat: that seat's pad goes neutral.
- **Giving back one seat or all** (`ControlCancel { what: Seat(Option<SeatId>) }`).

**The guest's side** (`couch_input::forward`, `couch-viewer`):
- **The forwarder keeps a hold per seat:** epoch, permissions, pad sequence, heartbeat. Each controller's state arrives by its slot (`LocalInput::Pad { controller, pad }`), and which slots are connected with `LocalInput::ControllerSlots`. The stream window gives a controller the lowest free slot and keeps it there while it is connected, so nobody is renumbered when one goes.
- **The forwarder alone decides which controller drives which seat,** and tells the window (`HeldSeat::controller`), which rumbles and shows by it:
  - with one pad seat, the controller last touched drives it, as before;
  - with several (your choice, refined after the review): each seat is pinned to a controller. A new seat takes the lowest free one; when a second seat comes, the first keeps the controller driving it. Pins stay while their controllers are connected, even once one seat is left, so a controller coming back never takes the seat another plays.
- **A controller that disconnects** (your choice, after the review): with several pad seats, its seat goes neutral at once and is given back, as if the guest had; the guest's other controllers stay on their seats. With one pad seat, nothing is given back (a controller sleeping or a Bluetooth blip doesn't cost the guest their only seat): another controller, or the same one reconnected, carries on. A restarted stream window gives nothing back.
- **Keys and mouse** go to the seat that allows them, beside the pad seats.
- **The stream window** rumbles the controller driving the seat the game shakes, and shows "pad A → seat 1, pad B → seat 2" in the stats panel. Its bar says "You're playing Controller 1 and Controller 2", and has "Give back Controller N" per seat and "Give all back".
- **A dropped connection** clears the seats but keeps the controllers, so the count reaches the host again after the next join.

**The app.** A watcher's card lists every seat they play, lets them ask for any other, and give back one or all. The host panel lists each watcher's seats and controllers, and "Pass another seat…" marks each seat "— added" or "— moves them". The host overlay's notes tell a guest giving back one of several seats.

**The wire** (`PROTOCOL_MINOR` 4): `ViewerControllers` (optional, `0x0608`); `ControlCancel`'s `Seat` names one seat or none; `SessionStatus` may show a viewer on several seats. Viewer IPC v10: controllers by slot and the slots connected; the seat list with permissions and the controller driving each; rumble names its seat; `UserAction::GiveBack(seat)`.

## Tests

- **Session machine** (`couch-room`): seats added for spare controllers and moved otherwise; grants on their way count toward a viewer's seats. **The property test** now models controller counts: the router never sees a viewer on more pad seats than the most controllers it said (one if none), nor two seats without a pad, nor the host on two, even with reports arriving late.
  - **It wasn't reaching the new rule at first.** With every op equally likely, `End` came after about 19 ops, too soon for a viewer to join, say it has controllers and be passed two seats: a temporary assertion that failed on any viewer with two seats never failed. Its ops are now weighted; the same probe then failed at once, for two seats and for two pad seats, and the test passes five runs in a row. The probe's two smallest cases stay in `proptest-regressions/session/tests.txt` (a pad seat plus the keyboard seat; two pad seats after saying 2 controllers), so every run replays them first.
- **Forwarder** (`couch-input`): two seats driven by two controllers, the first keeping its controller when the second comes and after it goes; a controller that goes leaves its seat (neutral, named once to give back) and the other stays, even when the first comes back; one seat stays when its controller goes; a controller connecting moves nobody; a restarted window gives no seat back; keys go to the keyboard seat beside a pad seat; one member drives two pads end to end through a real router.
- **Session scenarios** (`tests/sessions.rs`, 52, several runs in a row):
  - a guest with two controllers asks for two pad seats and keeps both, each controller on its own pad; the keyboard seat is added too and types; giving back one seat keeps the others, each with its controller; the panic key releases all three seats, the pad and the key;
  - a seat passed to a one-controller guest moves them; after a second controller is plugged in, a pass adds, the first seat keeping its controller; that second controller going gives its seat back and the first stays; the last controller going with one seat left frees nothing; closing the window releases the seat;
  - a guest on two seats keeps them through a stream stop, both pads let go; the session's end ends watching with both pads neutral.
- **Stream window** (`couch-viewer`): several seats named together on the bar.
- `cargo test --workspace`, clippy, fmt, `tsc`.

## Code review

A review confirmed the router's rules (one keyboard-and-mouse holder, stale epochs, every release path covering every seat), `ViewerControllers` accepted only from a viewer on the connection it joined on, the forwarder's per-seat heartbeats and the watcher's state machine. Fixed from it:
- **A guest could briefly get one pad seat too many.** Grants on their way were kept by seat alone, so when a seat was passed to A and then to B, the router's report of A's grant cleared B's too, and a third grant to B was added instead of moving B. Pending grants are now kept in the order sent, and a report answers only its own member's grant (and earlier ones for that seat, which it overtook). The router's guard revoked the extra seat once every report landed, and keyboard and mouse were never affected. Unit test: `a_report_for_one_viewer_leaves_anothers_grant_pending`.
- **A move took the keyboard-and-mouse seat too.** It now revokes only the other pad seats, as "always added" says (`a_move_keeps_the_keyboard_seat`).
- **A count dropping while a seat was being added freed a seat already held.** The router's guard now revokes only for grants that moved the guest (`a_count_dropping_during_an_add_frees_nothing`).
- **Plugging in a controller took over a single seat,** and **unplugging the first controller moved the others up** (connection order), possibly leaving its press on the seat for a moment. Both came from numbering controllers by position and mapping them in two places. Your decision: a controller that disconnects frees its seat and the others don't move. So controllers now keep their slots, the forwarder pins seats to them and is the only one to map, and the window follows what it says (above).
- **A rare flake, not from M18:** `viewers_hear_when_the_game_leaves_the_front` failed once in a full run (its first HUD said the game wasn't in front), then passed 4 full runs and 20 runs alone. It's on the game-focus path, which M18 doesn't touch; noted for later.

## This PC (2026-10-16, release build, idle)

| Check | Result |
|---|---|
| `release-check` | all 5 PASS: viewer's main process killed, suspended after 252 ms; its window killed, gone after 79 ms; closed, 172 ms; frozen, suspended after 357 ms and active again after thawing; host stopped, 0 ms |
| `lifecycle-check` | all 6 PASS: a swapped window followed after 36 ms; the game exiting, 26 ms; the window closing, source lost after 5.2 s; the stream window killed, restarted after 40 ms, the seat never suspended; the host killed, lost after 4.6 s; killed holding a key, released 0 ms after the next start |
| `input-sink` | PASS: 118 keys, 5 mouse buttons, 4 moves, 3 wheel turns in order; 6 blocked combinations held back |

The same as M19's runs: the per-seat forwarder changes nothing for one seat.

**After the review's fixes and pinned controllers** (the same day, release build): `release-check` all 5 PASS (266, 72, 165, 358 ms, 11 ms); `input-sink` PASS; `lifecycle-check` all 6 PASS (followed 45 ms, game exited 11 ms, source lost 5.3 s, restarted 40 ms with the seat never suspended, lost 4.7 s, the stale key released 0 ms after the next start).

## Your run (2026-10-16, this PC, the app with alice)

A controller seat and the keyboard-and-mouse seat for one guest worked: alice held both, the keyboard seat added beside her pad seat.

**Two controllers couldn't be tried on one PC:** the second controller here is a generic pad that reports Xbox 360 ids, and a stream window on the same PC as a host skips every such pad (it can't tell them from the host's virtual pads; `couch_platform_win::virtual_pads`). On a guest's own PC it works. Telling real 360 pads from ours by their parent device is noted as a candidate for later.

**With friends** (the 0.11.0 installer, 2026-10-17 and 18): one controller each, as before; nobody had two, so several seats for one guest weren't tried.

**Still to come:** two controllers on a guest's own PC (the other PC with a `-TestRelay` build of this branch, or a friend), ideally two players in a co-op game (Fling to the Finish).
