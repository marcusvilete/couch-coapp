# M19 results: sessions before streams, and one input setup

## Why

Go live did everything at once: it picked the window, the seats and the host's controller, plugged in the virtual pads, hid the controller and started streaming. Stopping ended all of it. Two problems followed:

- **The controller was hidden too late.** A game started before going live had already opened it, and HidHide refuses only new opens, so the game kept it until the controller was reconnected (P2). Games that look for controllers only at launch could miss the seats' pads too.
- **Settings were lost.** Changing the window, the frame rate or the quality meant ending the session: the seats, who held them and who watched all went.

## What changed

**A session first, then streams inside it.**
- **Start a session** (the dialog that replaced Go live): a title, how many can watch, the seats and the host's controller. The pads are plugged in and the controller hidden at once, before any game runs; nothing is captured yet.
- **The host panel's Stream section:** the windows and screens, frame rate, quality and upload limit, then Start streaming. While live, Stop streaming, or pick something else and "Stream this instead". The choices stay between streams (your choice: in the panel, not a dialog).
- **Ending** happens only with End session (or leaving the room, shutting down). The game exiting, its window going away, or the capture failing ends only the stream.

**The engine** (`pipeline::host`):
- **Two levels.** `HostPipeline` (renamed from `HostStream`) owns the session: pads, the host's controller and its hiding, the input task and seat router, the viewers. A stream (its source, frame rate, rates, upload share, capture, sound, source watch, pacing and GPU-frames state) is swapped as a unit by `StreamControl::start_stream` and `stop_stream`.
- **Each viewer's encode thread and video sender live as long as it watches,** so its epochs keep counting across streams: the viewer drops older epochs (`couch_net::video`), and a sender started afresh would have begun at 1 again. A new stream is a new epoch with a keyframe, and the codec is chosen again for it (M12c).
- **Stream events carry their stream's number** (`SourceClosed`, `GameExited`, `CaptureFailed`, `SourceFollowed`): a late one from a stream already replaced is ignored.
- **The overlay** goes to the streamed source's monitor, or to the main screen with nothing streamed.

**Holding viewers' input off between streams** (`SeatRouter::hold`). While nothing is streamed, guests can't see what they would do:
- holding releases at once every key, button and pad a viewer held;
- their input is then dropped, but still counts as heard, so a seat is never suspended or returned while nothing is streamed;
- mouse totals stay current, so the pointer doesn't jump when the hold ends;
- the host's own controller is the exception: the host may start the game with it.

The input task holds from the session's start; `start_stream` lets go, `stop_stream` holds again (and waits for it) before ending the epochs.

**The wire** (`PROTOCOL_MINOR` 3): `SessionStatus.stream` (`Waiting`, `Live`, `Stopped` with why), and the reset code `STREAM_END` (4) for an epoch whose stream stopped. Viewer IPC v9: the HUD gets the stream's state.

**The stream window** clears the picture while nothing is streamed and says why in the middle: "Ana isn't streaming anything yet", "Ana stopped streaming", "The game closed", "The window or screen being streamed went away", "The stream failed", with "Your seat stays yours; the next stream shows here."

**One input setup** (the single-player/multiplayer choice is gone): 0 to 4 controller seats, and keyboard and mouse as a seat of their own or with Controller 1 (today's `Both` seat, single player's old "both", so M19 doesn't need M18). Fixed for the session (your choice). The last setup is remembered in the app's own storage; the per-program setups (`input-setups.json`) are gone.

**`couch-cli host`** starts a session and one stream at once, and still ends when that stream does, so `release-check`, `lifecycle-check`, `latency-probe`, `gates` and `viewers.ps1` behave as before. `--restart-stream-after <secs>` stops the stream and starts it again once, two seconds later.

## Tests

- **Router** (`couch-input`): holding releases a guest's key and pad and drops their input but keeps the seat; a held seat is never suspended while its holder is heard; motion while held moves nothing later; the host's own controller plays through a hold.
- **Room** (`couch-room`): streams start and end inside a session, broadcast, with viewers and seats kept.
- **Session scenarios** (`tests/sessions.rs`, 49 after the review, three runs in a row):
  - a session starts with nothing streamed (pads plugged, the hook in place, the viewer's HUD says so, no frames, nothing captured), then streaming starts from a keyframe;
  - a stream stops and starts again with the viewer staying, its seat kept, no frames between, the second stream from a keyframe, the codec chosen again;
  - a guest's held key and pad are released at Stop streaming, nothing they send gets in until the next stream, then it does;
  - the host's controller plays between streams; streaming needs a session;
  - the game exiting and the window closing end the stream, not the session, and the viewer stays and hears why;
  - the existing scenarios, adapted to start a session and then a stream;
  - the input setup's seats, and setups that can't be (5 controllers, no seats) refused.
- `cargo test --workspace`, clippy, fmt, `tsc`.

## Code review

A review found no input-safety hole on the paths the UI drives (the hold, its release, the order of a stop, epochs and codecs across streams, lock order, window titles out of `Command` logs). Fixed from it:
- **Stream starts and stops could race.** Each ran in a task of its own, unordered, and a start installed its stream over a running one without ending it. Reachable only by fast or out-of-order commands (not from today's UI), but it could leave a capture running that nothing stopped, and guests' input let through with nothing streamed. Now one task per session runs every start and stop **in the order they were asked for** (`StreamControl` queues at the call, not when its future first runs), and each start's answer names its request, so an old answer never promotes a newer start.
- **A stream failing as it started was missed** when its failure reached the engine before the start's answer (a capture that fails at once, with viewers watching): the session showed it live, with a dead capture, and guests' input let through. Such ends are now kept while a start is pending, and applied when its answer comes.
- **A viewer could show "game in front" when it wasn't**: the host's first focus report for a new stream can come before its status. The guess is now reset when a stream ends, not when the next one starts.
- **Window titles could reach the `Response` Debug** through the host's stream view: its Debug leaves the name out now, and the test checks it.
- **Smaller:** the pacing trace is numbered per stream (a restart no longer overwrites the first); sound packet numbers count on across streams.
- **Three more scenarios:** stopping while a stream starts leaves nothing streamed; a second start while one is pending is refused; a stream failing as it starts ends, and guests' input stays held.

## This PC (2026-10-16, release build, idle; before the review's fixes)

| Check | Result |
|---|---|
| `release-check` | all 5 PASS: viewer's main process killed, suspended after 240 ms; its window killed, gone after 83 ms; closed, 181 ms; frozen, suspended after 356 ms; host stopped, 10 ms |
| `lifecycle-check` | all 6 PASS: a swapped window followed after 46 ms; the game exiting, 97 ms; the window closing, source lost after 5.2 s; the stream window killed, restarted after 39 ms; the host killed, lost after 4.6 s; killed holding a key, released 0 ms after the next start |
| `input-sink` | PASS: every event arrived, in order; 6 blocked combinations held back |
| A stream stopped and started again (`host --restart-stream-after 10`, a `view` watching for 25 s) | stopped at 10.0 s, streaming again at 12.0 s; the viewer kept watching, 1353 frames, 0 decode errors, not ended early; its longest gap 2.0 s (the pause); the second stream ramped up from 10 Mbit/s again, from a keyframe |
| `gates --quick` (H.264) | all PASS: G1 p50/p95 10.8/11.7 ms; G3 p95 77.3 ms, 19.5 Mbit/s; G4 back after 79 ms, 1 keyframe; G5 settled in 0.35 s, latency back 0.50 s; G7 second viewer p95 11.9 ms (unimpaired 12.1) |
| `viewers.ps1 -Screen -Clock`, 4 viewers, 60 fps (HEVC) | 59.8–60.0 fps, capture to present p50 7.2–7.3 ms, p95 9.6–9.8 ms; the stream windows 27% of one core. M12c: 60.0–60.1 fps, p50 8.0–8.3, p95 10.8–11.0 |

The restructured pipeline costs nothing measurable: the gates and the four viewers match M12c's runs.

**After the review's fixes** (the same day, release build): `release-check` all 5 PASS, `lifecycle-check` all 6 PASS; the stream stopped and started again with a viewer watching, twice: 1353 and 1352 frames, 0 decode errors, the longest gap 2.0 s. That test's capture to present read p50 8.5 and 7.6 ms (5.0 before the fixes); `latency-probe` right after gave clock to present p50 11.0, p95 12.2 ms (G1 earlier that day: 10.8 / 11.7), so the frame path is unchanged and the difference is that test's own variation. The fixes touch only how starts and stops are ordered.

## Your run (2026-10-16, this PC, the app with alice)

Working as intended: the session started before the game, streams started, stopped and started again, the idle states showed in the stream window, and ending the session showed the controller again. Passing a real controller to a guest couldn't be tried on one PC (the guest's controller is plugged into this PC too); the session scenarios cover it with recording pads, including across a stream's stop and restart.

**Still to come:** a run with friends over the internet, a friend holding a seat while the stream stops and starts again.
