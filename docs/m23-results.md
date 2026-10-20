# M23 results: several seats for the host

## Why

Since M18 a guest plays one pad seat per controller they have; the host still played one seat with one controller (P2). You wanted anyone to hold as many seats as they have controllers, the host included.

## What changed

**Each of the host's controllers has its own seat** (`couch_room::session`). The single "home" seat of P2 became one per controller, each with what the host last did with it (playing, lent to a guest, left empty). Your choices:
- each chosen controller **starts on its own seat with a pad, in seat order**;
- a host controller that **disconnects keeps its seat** (its pad neutral until it comes back; Leave frees it), unlike a guest's, since the host is at the PC;
- two controllers of **one model are told apart by the order** the reader finds them ("DualSense 1", "DualSense 2"); hiding goes by ids, so both are hidden either way.

**The rules, per controller:**
- **Which controller drives which seat is the session's word alone**: it tells the input task (`Effect::HostSeats`) before any claim or grant it concerns, on the same ordered channel. The input task routes each controller's states to its own seat, and only while the router has the host on it.
- **A seat comes back to its controller** when a guest lets go of it, only through a claim, which the router grants only if the seat is free, is one of the host's controllers' seats, and control isn't paused. A late claim for a seat a controller has left is refused.
- **Play here** moves one controller: its old seat is revoked first, whatever the session's mirror says (a claim of it may not be reported yet; the property test found that case), then the new seat granted. Onto a seat another of the host's controllers plays, **the two swap**: the router's holders don't change, only which controller drives which.
- **Take back and Leave** leave a seat empty; the panic key frees guests' seats only, and the host keeps all theirs.
- **Rumble** goes to the controller driving the seat now; after a swap, the one that drove it stops.

**The pipeline** follows each chosen controller and hides each model once, all or nothing: if one can't be hidden or followed, none plays through a seat, and those hidden are shown again.

**The wire:** the session status named one seat for the host; it now lists them (`SessionStatus.host_seats`, in seat order, each a seat no viewer holds). Wire revision 5, version 0.12.0: builds before it refuse it, so friends need the new installer.

**The app:**
- the session dialog's "Your controllers": one check per controller, up to the number of controller seats, each saying where it starts; the choice is remembered (the single choice from before still loads);
- the host panel names the controller on each of the host's seats, and with several controllers each pad seat has "Play here with…" (a move, or "they swap").

## Code review

A review found the safety rules intact: the host never on more seats than controllers, no controller reaching a seat it doesn't own or a viewer holds, claims never taking a seat just passed, and which controller drives which seat always reaching the pipeline before the claims and grants it concerns; a 0.11 peer is refused at `Hello`, before a status it would misread could flow. Fixed from it:
- **Ticking only "DualSense 2" played DualSense 1**: the reader takes the first free controller of a model. Choosing one now chooses the earlier ones of its model too (and leaving one out, the later ones), as listing order means.
- **"Play here with…" called a swap a move** when the other controller's own seat was lent or left empty: the view now carries each controller's own seat (`SessionView.host_homes`), and the labels follow it ("swaps seats with your GameSir", "leaves Controller 1", "back on its seat").
- **A swap cut a rumble off** for up to half a second: the second seat's turn stopped the controller the first had just started. A controller now stops only if it drives no other seat of the host's.
- **Play here right after passing that seat to a guest** skipped its grant (the mirror still showed the host on it), so the guest kept it: a viewer's grant on its way now counts.
- Smaller: the dialog stored choices beyond the seats and forgot a remembered controller unplugged at the time; stale texts in protocol.md, a skill and comments; a note on HidHide's session list (cleared as a whole, which is right while a host's guards drop together).

**Left as before M23:** with "keyboard and mouse with Controller 1", the host's first controller starts on that seat (pad only); guests get its keyboard and mouse once the host passes it.

## Tests

- `couch-proto`: the host on two seats valid; a seat twice, out of order, a viewer's or none at all invalid; the largest status still fits a frame.
- `couch-room`: each controller on its own seat at the start, capped at the seats with a pad; a seat passed on coming back to its controller alone; a move (old seat first, no claim for it, a late report of it sent back) and a swap; a swap onto a seat lent to a guest; Play here right after a pass still on its way; the panic key and Take back sparing the host's other seat. **The property test** runs with no host (half the cases, so guests still reach two pad seats, as M18 needs), one host controller and two, against a model of the host's moves; it checks the router never sees the host on more seats than controllers, claims only for a controller's own seat never left empty, the session's seats matching the model, and each controller back on its seat at the end unless lent, left empty or paused. Temporary assertions confirmed it reaches the host on two seats and a guest on two pad seats.
- `couch-engine`: `the_hosts_two_controllers_play_two_seats`, with a second fake controller: each on its pad, one passed to a guest (her pad drives it, the host's other keeps playing), taken back and played again, a swap, rumble to the controller driving the seat, the panic key keeping both, both shown again at the end. All 55 session scenarios, three runs in a row.
- `cargo test --workspace`, clippy, fmt, `tsc`.

## This PC (2026-10-20)

- `release-check`: viewer killed, suspended after 233 ms; its window process killed, gone after 79 ms; window closed, gone after 175 ms; frozen, suspended after 338 ms and active again after thawing; host stopped, 1 ms. All passed.
- `input-sink` and `lifecycle-check`: passed.
- `host-pad --feed 1`: the GameSir hidden (9 devices), a pad fed from it, shown again after.

## Your run (2026-10-20, this PC)

The DualSense and the GameSir both chosen, two controller seats: each drove its own pad; passing one to alice showed her on it; Take back, Play here, the swap and the panic key behaved; both controllers came back at the end. Alice's own input couldn't be tried on one PC (both of the host's controllers are hidden from her stream window); the session scenarios cover a guest's pad on a seat passed from the host.
