# P17 results: the small loose ends

## Why

Four small things left by M27e and P16, in one step (**your choice, 2026-11-12**), in this order:
1. `pictures_go_through_the_host_with_their_bytes` failed once in six runs of the session scenarios under their parallel load (P16's results, "Found while checking");
2. the host read and decoded a member's whole picture before the cheap refusals (pictures off, the member stopped, too fast), so a misbehaving member cost a read and a decode per picture (M27e's results, "Left for later");
3. a late `PictureAnswer` could open a member's gate early: an answer arriving after the 75 s wait, for a picture of the same element put again since, let the batches behind the newer upload go first, to be refused (M27e's results, "Left for later");
4. a cancelled grant's late report took a guest's new request for the same seat: a guest whose grant the panic key overtook, or who left and came back, and who asked again for that seat while the old report was in flight, was told it had the seat, then that it was released, and its request was gone (P16's review, older than P16).

Part 3 changes the wire: **version 0.33.0, wire revision 19**, which doesn't talk to 0.32.x or earlier.

## What changed

- **The flake was a real race, in two scenarios.** Turning drawing on sends each viewer a snapshot in a new generation of its own; the host sent `BoardState` ("drawing is on") *before* it. A window, or the test, drawing as soon as it heard "on" drew in the old generation, and the host dropped the batch as stale, saying nothing. The picture scenario then waited 20 s for a refusal that never came. The own-window scenario (`the_hosts_own_stream_window`) had the same shape after drawing *off*, whose clear also starts a new generation; it failed in the first run of this step. Now:
  - `HostSession::set_drawing` sends the board first, then the switch: on, each viewer's snapshot (and the host's windows' board) before `BoardState`; off, the clear before it. A viewer starting to watch gets its snapshot before `BoardState` too (`board_for`, after the review). A real window could hit this (a stroke begun within milliseconds of drawing turning on vanished);
  - the own-window scenario waits for the window to hear the clear before drawing after drawing off.
- **The host asks before it reads** (`couch_net::pictures::read_upload_header`, `UploadBytes::read` and `refuse`). The reader takes the header, then asks the actor (`Input::PictureComing`) whether the session would refuse it without the bytes (`HostSession::picture_coming`, `Board::picture_coming`): the rates, the generation, the keyboard seat or no live stream, drawing off, the member stopped, a malformed element, pictures off. Refused, the member is answered at once (with a resync, as for any refusal but a stale one) and the stream is stopped unread. Otherwise it reads the bytes and decodes them as before; the board's own checks still run in full when it takes the picture; bytes that don't come whole are answered as no picture (`BadPicture`), so the member's gate never waits out its 75 s for them. The cheap checks live in one place, `Board::admit`, which `take` runs too, so the two can't drift. Every upload counts toward the batch rate once: refused at its header, there (any refusal, as any refused batch counts); let through, when the board takes or refuses it; and one whose bytes weren't a picture counts too (`Board::count_refused`), so a member sending junk fast is soon refused at the header, unread.
- **A number per upload** (`PictureUpload.upload`, echoed in `PictureAnswer.upload`). The member's gate waits on its upload's number, not the element's id. The numbers come from one count for the whole process, never restarting with a new connection, a new watch or another window, so an answer to an upload given up on lets nothing go. The host's own windows' gates (its board over the game and its own stream window, checked in the engine) number their uploads the same way, though nothing of theirs is on the wire.
- **A cancelled grant's report answers no request.** When a grant the panic key, a departure or a later grant cancelled is reported while the guest has asked again for that seat, `HostSession::seat_changed` keeps the request and the engine sends no `SeatAssigned` for it (`HostSession::report_keeps_request`), so both sides keep the request. Without such a request, the guest is told of the seat and then its release, as before: it may still show the request the host accepted, and that settles it (the review's finding). Every cancellation already puts a release behind the grant on the same ordered channel to the seat router, so the guest never holds the seat for long, and nothing of the router changes.

## Checked

- **The race, both ways, deterministically.** With the fake window's handling of each snapshot delayed by 300 ms (a temporary edit), `pictures_go_through_the_host_with_their_bytes` and `drawings_go_through_the_host_to_every_window` both fail under the old order, at the waits the cause predicts ("refused while pictures are off", "one element on the board"), and both pass under the new.
- **Under load,** each run being all 95 session scenarios in parallel: `main`'s build failed 1 run of 20 (the own-window scenario, at "refused, to it"), and the first run of this branch before the fix failed the same way; with the fixes, 20 runs of 20 passed, then every later run (below).
- **New tests:**
  - `couch_room` session: `a_picture_is_refused_before_its_bytes_where_it_can_be` (pictures off, stale, stopped and too fast refused with the upload's number; a hundred let through spend nothing, and the picture is then taken), `refused_pictures_count_toward_the_rate`, `a_cancelled_grants_late_report_leaves_a_new_request_pending` (also: not asked again, the guest is told as before) and `a_grant_from_a_former_stay_answers_no_request_of_the_next`. Both of the last two fail without the fix and pass with it;
  - the engine: `a_late_answer_never_opens_a_newer_uploads_gate` (across a reset and another gate too);
  - `a_drawing_goes_to_the_other_viewers_with_its_author` now checks the snapshot comes before the switch.
- **The session property test** asks about each viewer's picture before taking it, as the engine does, and checks each answer names its upload; a temporary failing assertion showed it still reaches a viewer's picture taken.
- **The wire:** couch-proto's roundtrips at the fields' extremes; `a_new_wire_revision_comes_with_a_minor_version` passes with revision 19 and 0.33.0.
- **Windows:** fmt and clippy clean; couch-proto, couch-net, couch-room, couch-input and couch-engine all pass; rooms, sessions and relay three rounds, all passing; the whole suite clean (44 test binaries).
- **Linux (WSL):** clippy and the whole suite clean; rooms and sessions eight rounds, all passing.
- **Input safety:** part 4 changes which request a report answers and whether a guest hears of a cancelled grant, nothing of the router, the injectors or teardown; couch-input, couch-net and the session machine and scenarios pass. The live checks (your go-ahead, 2026-11-12; release build, after the review's fixes):
  - `release-check`, all pass: main process killed, suspended after 225 ms (limit 350); window process killed, gone after 74 ms; window closed, 187 ms; window frozen, suspended after 374 ms (limit 600) and active again after thawing; host stopped, 1 ms (limit 500);
  - `input-sink`: 118 keys, 5 mouse buttons, 4 moves and 3 wheel turns through the router, all in order, 13 blocked combinations held back;
  - `lifecycle-check`, all pass: window swap followed in 37 ms, game exit in 106 ms, window closed 5.2 s (5 to 6 s), stream window restarted in 54 ms (its seat suspended meanwhile, active again after 428 ms), host killed lost in 4.6 s (limit 5.5), a key left down by a killed host released at once by the next.
- `check-docs.sh`: nothing stale.

## Found while checking

- **The own stream window's scenario had the same race** (above), found by the first loop of this step; `main` shows it too.
- My first version of part 4 also kept the mirror out of a cancelled grant's report, returning before the session's other cases; the session property test caught it at once (a member who had left shown holding a seat). Only the request handling changed in the end; the rest of `seat_changed` is as before.

## The review

A fresh review agent went over the whole diff. **Nothing high.** It checked as sound: every picture path answered or not exactly as before (but one, below), the reply to the reader dropped only when the session or the viewer is gone, `admit` keeping `take`'s order, an honest upload counted once, the gate's numbers (wrapping, reset, an overdue upload's late answer opening nothing), every window and the watch pipeline taking board news whatever drawing's state, a release behind every cancelled grant, and the wire and security rules. What it found, each checked against the code, all fixed:
- **Medium: a guest could be left showing a request forever.** The first version of part 4 told the guest nothing of *any* cancelled grant's report. But the host takes a request off its list when it accepts it, so a guest whose accepted request the panic key overtook (or whose seat a later pass freed) was never told of the seat nor, then, of its release, and kept showing "asking" while the host showed nothing; asking again for that seat did nothing. Now the guest is told nothing only when it has asked again for that seat (`report_keeps_request`), as the case the step was for; otherwise as before.
- **Low: refused picture headers didn't count toward the rate**, breaking the board's rule that every batch counts, refused or not (one refused for its rate excepted); and older, bytes that weren't a picture never counted either. Both count now (above).
- **Low: an upload whose bytes didn't come whole went unanswered,** leaving the member's gate waiting 75 s, against what `docs/protocol.md` now says. Answered as no picture now.
- **Low: the upload numbers restarted with each watch,** so an old watch's upload still queued at the host, answered late, could open a new watch's first gate. One count for the process now.
- **Low: a newcomer still heard drawing was on before its snapshot**, the same race at a join. The snapshot goes first there too.
- **Low: removing my temporary edit had joined two lines of the scenario file into one.** Split again.

After the fixes: clippy clean; couch-proto, couch-net, couch-room and couch-engine all pass; rooms and sessions three rounds, all passing; all 95 session scenarios 15 runs of 15 under load (a Linux build alongside). Linux (WSL): fmt, clippy and the whole suite clean, rooms and sessions eight rounds, all passing.

## Still to come

- Nothing for you to try by hand: each of the four needs timing no person can make. The `app-manual-run` skill's drawing checks still apply to 0.33.0 as a whole, if you want a run.
