# P16 results: seat reports matched to their grants

## Why

During M27b (2026-11-10), the session property test found a sequence on Linux after which the seat router briefly held one guest on two controller seats while that guest had only one controller. It was older than drawing: pinned to 0.28.0's test, it failed there too. **Your choice (2026-11-10):** fix it as a polish step of its own, after the whole of drawing (M27c to M27e).

**The cause.** The session (`couch_room::session::HostSession`) keeps the grants it sent to the seat router until the router reports them (`pending`). These grants count toward a guest's seats, so two quick grants never give it more than its controllers allow. A report was matched to its grant by seat and guest. The panic key dropped guests' grants still on their way, while their reports could still be in flight. Then:
1. you pass seat 0 to Ana; the router takes it, and its report is on its way;
2. you press the panic key, then Allow control, then pass seat 0 to Ana again; the router takes that too;
3. the first report arrives and answers the *second* grant; the panic key's release report follows; the session now believes seat 0 free, though the router has given it to Ana again;
4. you pass Ana seat 1: with one controller she should move, leaving seat 0, but the session sees nothing to free, so the router holds her on both until the next report revokes the extra one.

At most one report's delay, only after a panic racing reports in flight. No keyboard and no extra input reached anything: her one controller drove one seat.

**Why not just keep the forgotten grants?** The pipeline refuses grants that reach it after the panic key, and it used to say nothing about them. Those would have lingered in the session's books, counting toward the guest's seats and waiting to swallow a later report.

No wire change: **version 0.32.1**, which still talks to 0.32.0.

## What changed

- **Each grant has a ticket** (`GrantTicket`, numbered by the session, in `Effect::Grant`). The router's per-seat epoch, the row's first idea, can't serve: the session sends a grant before the router gives it an epoch. The ticket goes with the grant to the input task (`HostPipeline::grant`, `InputCmd::Grant`), and comes back on the router's report of it (`HostEvent::Granted`'s `ticket`). `HostSession::seat_changed` matches by ticket alone.
- **Every grant is answered exactly once.** The input task now reports each grant it refuses (`HostEvent::GrantRefused`): one the panic key overtook, one arriving while the session stops, one the router itself refuses (a second keyboard). `HostSession::grant_refused` takes it off the books. With no input task to send it to, the engine refuses it at once.
- **The panic key and a guest leaving keep grants on their way, cancelled** until their answer comes. A cancelled grant counts toward none of the guest's seats (as P13's grants freed by a later one already did), and its late report answers it alone and frees nothing more. Moving one of your controllers to its own seat still counts any guest's grant of it on its way, cancelled or not (the review's finding, below).
- **The controller the engine chose for a grant** (P12, sent with `SeatAssigned`) is kept by ticket as well. Keyed by guest and seat, an old report could have taken a newer grant's choice.
- `couch-cli host`, which runs no session, sends grants without tickets.

## Checked

- **The regression, both ways.** Three new session tests:
  - `a_report_from_before_the_panic_key_never_answers_a_later_grant`: the steps above, by hand;
  - `a_refused_grant_is_answered_by_its_refusal`;
  - `a_report_across_the_panic_key_answers_only_its_own_grant`: the property test's sequence kept, played through the property test's own checks.

  Under the old matching (reports by seat and guest, the panic key forgetting), all three fail, the last with the bug itself: one guest on seats 0 and 1 with one controller. With the fix they pass.
- **The property test's router is closer to the real one.** It used to apply every grant the moment the session made it; only the reports came late. Now the session's commands are on their way to it too (a new op, `Run`), with the pipeline's pause: set the moment the panic key is pressed, before anything queued runs, and cleared at Allow. It refuses as the input task does, and stops at the end behind whatever is queued. At the end it checks that every grant was answered once. Two of its checks had to learn what a queue allows:
  - a guest whose departure is still on its way may hold the seats it had before;
  - a guest may draw while its keyboard seat's release is on its way: the session gave the seat up already.

  After the review it also has the panic key's hook on its own (`HookPanic`): the hook pauses the pipeline and frees guests' seats from its own thread before the actor hears of it, so the session may still grant in between, and those grants are refused. The actor's panic follows one op later.

  With grants waiting on their way, a guest reached two pad seats far less often: measured over 4,000 cases, about 1 in 2,000 with the old weights. So there are more passes, controller counts and runs, sequences of up to 150 ops, and 1,024 cases (about one case in 200 now, the session tests taking 7.5 s instead of 1.8). Each of the five things it must reach (the host on two seats, a guest on two pad seats, a guest's and the host's drawing taken, a picture taken) was reached in three runs of three, each checked by a temporary failing assertion. 60,000 cases pass in all. Under the old behaviour, 20,000 random cases didn't find the breach: the race needs more steps to line up now. That is why the found sequence is a test of its own.
- **The pipeline:** `after_the_panic_key_no_grant_goes_through_until_allowed` now checks that the refused grant reports its ticket, and the next grant's report carries its own.
- **Windows:** fmt, clippy and every test clean. Before the review, the full suite passed four runs of five; the fifth failed once in the session scenarios (under "Found while checking"); rooms and sessions three more runs all passed. After the review's fixes: fmt, clippy and the full suite clean, rooms and sessions three runs all passing.
- **Linux (WSL):** before the review, fmt, clippy and the whole suite clean, rooms and sessions three runs all passing; after it, the same again.
- **The input-safety live checks** (your go-ahead, 2026-11-11; release build):
  - `release-check`, all pass: main process killed, suspended after 274 ms (limit 350); window process killed, gone after 89 ms; window closed, 186 ms; window frozen, suspended after 367 ms (limit 600) and active again after thawing; host stopped, 10 ms (limit 500);
  - `input-sink`: 118 keys, 5 mouse buttons, 4 moves and 3 wheel turns through the router, all in order, 13 blocked combinations held back;
  - `lifecycle-check`, all pass: window swap followed in 28 ms, game exit in 68 ms, window closed 5.1 s (5 to 6 s), stream window restarted in 42 ms with the seat never suspended, host killed lost in 4.6 s (limit 5.5), a key left down by a killed host released at once by the next.
- `check-docs.sh`: nothing stale.

## Found while checking

- **`pictures_go_through_the_host_with_their_bytes` (M27e) failed once** in six runs of the session scenarios, then passed ten runs of ten alone. It uses no seats, so this change can't reach it; it times out under the scenarios' parallel load. Left for its own fix.
- **The property test counted a legitimate drawing as desktop control's.** It remembered whom it gave desktop control and forgot only on the panic key, a departure, a take-back and the end. The session also ends it when the keyboard seat is passed to someone else or given back, after which that guest may draw. Older than P16; the new weights reached it in about one run of 20,000 cases. The test now follows the session's end of it (its check that desktop control never goes to anyone else stays), and the sequence is a test (`desktop_control_ended_by_a_pass_lets_its_member_draw`).

## The review

A fresh review agent went over the whole diff. **Nothing high or medium:** every grant with a ticket is answered exactly once on every path of the input task and the engine; no report answers the wrong grant; every way a grant is cancelled puts a release behind it on the same ordered channel, so the router never holds a seat the session doesn't count; the safety rules are unchanged (the panic key's refusals now only report). It also noted one case that is now better: a guest leaving and coming back could lose seats of its new stay to an old report's guard. What it found, each checked against the code:
- **Fixed:**
  - this page named a pipeline test that doesn't exist (now `after_the_panic_key_no_grant_goes_through_until_allowed`);
  - the engine kept the chosen controller of a grant it couldn't send; dropped now, as on a refusal;
  - the property test didn't model the hook's press before the actor's panic (`HookPanic`, above);
  - the test's keyboard-seat drawing check excused more than needed: a revoke of the seat, or its grant to another, leaves the session's mirror naming the holder, so the session refuses itself; only the panic key's release, a departure and the end are excused now;
  - moving your controller to its own seat while paused ignored a guest's cancelled grant the router may already have run, so the release behind it could leave the seat empty until Allow (as before P16, which dropped such grants). It counts every guest grant on its way again: your grant goes behind the release, so at worst it re-grants a seat you hold;
  - `HostPipeline::grant`'s comment now says a grant sent while the input task stops, after the session's end, may go unanswered.
- **Left, older than P16:** a cancelled grant's late report still counts as an answer to the guest's request for that seat. A guest that left and came back, or whose grant the panic key overtook before Allow, and that asked again for the same seat within that report's flight, would see its new request taken by the old report: told it has the seat, then that it's released. Fixing it needs the engine to skip `SeatAssigned` for such a grant as well as the session to keep the request, or the two would disagree about the request; it needs a network round trip to race a report on the same PC, so it's left as it was.

## Still to come

- Nothing for you to try by hand: the race needs reports in flight across the panic key, which no person can time.
- The review's one finding left (above), if it is ever seen.
- The picture scenario's flake under load (above).
