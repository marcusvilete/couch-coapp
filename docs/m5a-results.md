# M5a results: sessions in the engine

M5a gives the engine sessions: a member streams a window or a screen to others in a room, and hands the controller around.
- **Going live** in a room, with a title, a frame rate, a bitrate per viewer, an optional upload cap, a viewer limit (4 by default, up to 7) and a keyboard-and-mouse switch for window sources.
- **Watching**: the engine starts `couch-viewer` and feeds it, as `couch-cli view` did.
- **Control**: a viewer asks; the host accepts or declines (a 5 s cooldown after a no), passes control without a request, or takes it back; a viewer gives it back.
- **The panic key**, as an engine call for now: every seat back to the host, pending requests declined, control paused until allowed again. The keyboard hook that calls it is M5b.
- **The streaming pipeline moved into the engine** (`couch_engine::pipeline`), serving several viewers, each with an encoder, a rate controller and epoch streams of its own. `couch-cli host` and `view` run on it, so the latency gates measure the app's own code.

Everything is reachable through engine commands; the screens are M5b.

**It works.** Every check below passes, measured 2026-10-05 on the M1 PC (RTX 3060 Ti).

## Checks

| Check | What | Result |
|---|---|---|
| Protocol | Session and control messages round trip; only the directory (announce, status, ended) is optional; a status with 64 viewers fits a control frame; titles follow the name rules; a status whose controller isn't a viewer is refused; IPC v3's HUD state and user actions round trip | pass |
| Session machine (14 unit tests) | Joining up to the limit, idempotent; a viewer that can't be streamed to is dropped and told; requests only from viewers, one each; accept, decline with a 5 s cooldown, pass, take back, give back, withdraw; the panic key releases first, declines what waits, pauses control, and a grant crossing it goes straight back; a controller who leaves loses the seat at once; after the end nothing moves; keyboard and mouse only with the host's switch | pass |
| Session machine, property test | 256 random runs of joins, leaves, requests, answers, passes, take-backs, panics, waits and ends against a seat router whose reports arrive late: no grant while paused or ended or to a non-viewer, no request inside a cooldown, the controller is always a viewer, and once every report has arrived the session's mirror matches the router | pass |
| Upload share | Equal shares when everyone wants more; a viewer held back by its network keeps a 25% margin and the rest is split; shares never add up to more than the cap (2000 random cases); small swings leave shares alone, a large cut is taken, a newcomer starts afresh; a viewer pushed to the minimum climbs back under a cap; a lower share brings the bitrate down at once and lifting it lets it grow back | pass |
| Host delay | The host's own delay is taken off each reported frame, across an epoch change; unknown frames are left alone; an encoder slowing down by 12 ms over two seconds leaves the delay trend at 0 | pass |
| Seat router | A session's epochs count on from a random start; input tagged with a fresh router's first epoch is dropped | pass |
| One stream acceptor per connection | Real endpoints: video and input flow both ways on one connection at once; epochs of another session are refused; watching again on the same connection works and the new sender gets the reports | pass |
| netem | `to=<port>` impairs only one peer's traffic | pass |
| **Three engines with sessions** (real iroh on loopback, a fake backend: gray frames, a fake encoder, a recording pad and injector, an in-process viewer window; 12 scenarios, three runs in a row, about 1.4 s each) | **Two viewers** each get a stream of their own, starting with a keyframe; everyone sees who watches; the HUD gets the title and host | pass |
| | **A full session** turns one away; one stream per profile, never while watching; keyboard and mouse refused for a screen | pass |
| | **Control**: asked for, accepted, played (the pad moves); a viewer without the seat moves nothing; passed to another, with the pad neutral between holders and the old holder's input dropped; given back from the viewer window; passed and taken back | pass |
| | **A decline**, then asking again at once is declined without reaching the host; the HUD says so | pass |
| | **The panic key** releases (pad neutral), pauses control (seen by viewers), declines a new request and refuses a pass; allowing control again lets a request through | pass |
| | **A viewer closing its window while playing** is released at once and leaves the session | pass |
| | **A ban while watching** releases the viewer and closes its window | pass |
| | **A host leaving the room** ends its stream; the viewer's window closes | pass |
| | **The window closing** ends the session: the host hears "source lost", the viewer "ended" | pass |
| | **Stopping and going live again** on the same connections; stopping watching | pass |
| | **Shutting down while a guest plays**: the pad is neutral before the engine is gone | pass |
| | **The panic key refuses a grant already on its way** to the seat router, until control is allowed again | pass |
| Room scenarios | M4b's 20, unchanged | pass |
| Workspace | 333 tests; clippy (all targets) and fmt clean; `cargo deny` ok; the front end type-checks with the new bindings | pass |
| **Gates** (release build, `gates --quick`) | Final run, after the review fixes: G1 clock to present p50 13.6, p95 15.1 ms. G3 (1% loss, 20 ms each way): p95 83.2 ms, no drift, 19.5 Mbit/s mean, no catch-ups. G4 (2 s blackout): back to baseline on the first frame, 85 ms after, with 1 keyframe. G5 (25 to 5 Mbit/s): settled in 0.35 s, latency back in 0.88 s, worst frame 339 ms. All inside M1's ranges. **G7** (two viewers, the first throttled to 5 Mbit/s): the second viewer's p95 15.6 ms against 15.1 unimpaired, its bitrate 19.9 against 20.0; the throttled one settled at 4.3 Mbit/s (the throttle applied). Earlier runs, before and after the rate-control fix, also passed; G5's latency came back in 0.64 to 1.02 s across them | pass |
| `input-sink` | 118 keys, 5 mouse buttons, 4 moves and 3 wheel turns injected and received, in order (your run, with the mouse left alone; two earlier runs failed only on mouse motion while the mouse was in use) | pass |
| `release-check` | Input released when the viewer's main process is killed (suspended after 263 ms), its window killed (gone, 66 ms), closed (gone, 156 ms), frozen (suspended, 373 ms, active again after thawing), and the host stopped (1 ms). Three earlier runs during M5a passed with similar times | pass |
| Viewers on this PC (a 1080p60 test pattern; host, viewers and their windows all on one PC, so the GPU also decodes every stream) | **Four:** 60 fps each, capture to present p50 22, p95 30 ms, 20 Mbit/s each, slowest encode 11 to 12 ms, no decode errors. **Seven:** this PC is the limit: about 36 fps each, p50 61, p95 73 ms, slowest encode 22 to 32 ms, a few decoder resyncs. Every rate controller stays at its 20 Mbit/s target (before the fix below, all fell to the 1.5 Mbit/s floor) | four: fine; seven: limited by this PC, not misread |

## Decisions made along the way

- **Sessions ride on the room's connections.** The roster already decides who may watch, and admission is done.
- **The host is the only writer**, sending straight to each member; nothing is relayed, so nothing needs a signature. A member acts on a session message only from that session's host, on the current connection, while it is on the roster.
- **One stream acceptor per connection** (`couch_net::streams`), like the datagram reader. Before, video and input each accepted streams themselves and would have taken each other's on a shared connection.
- **The viewer subscribes before asking to watch**, so no video epoch arrives unclaimed.
- **The seat router stays the only judge of who plays.** The session machine asks it and mirrors its reports; its tests are untouched apart from a constructor for a random first epoch.
- **Capture and sound run only while someone watches.** While nobody watches, the host checks once a second that the window still exists.
- **Ending releases first.** `HostStream::stop` releases all input, ends each viewer's stream with `SESSION_END`, stops the threads, then unplugs the pad. Dropping a stream releases too, explicitly, since a panic handle may keep its input channel open.
- **The control stream got priority 50** (video 0, input 100), so a seat change never waits behind a keyframe.
- **Window titles stay out of logs**: `GoLive` and the source list print without them.
- **A failed or closed viewer connection is not a failure.** An encoder thread that ends because the connection went reports nothing.

## Found and fixed

- **Rate control read a busy host as a congested network.** Seven viewers on this PC slowed encoding to about 25 ms a frame. Viewers measure each frame's delay from its capture, so the wait for the encoder looked like a filling queue, and every controller cut to the 1.5 Mbit/s floor, which can't speed an encoder up. You chose to fix it now: the host records each frame's time on the host (capture to handing it to the network) and takes it off the delay the viewer reports, so the controller sees the network's part only (`couch_media::ratecontrol::SendDelays`). No wire change. Seven viewers then stayed at their 20 Mbit/s target; the gates were run again (below).

From the code review (a subagent read the whole change; each finding was checked against the code before fixing):
- **A banned viewer kept the seat for up to 2 s.** A ban only queued closing the viewer's connection, which waits for its last messages to be acknowledged; the session dropped the viewer only once the connection closed, so a modified viewer withholding acknowledgements could keep injecting meanwhile. Now any room change that leaves someone no longer a member drops it from our stream in the same step, releasing what it held. The ban scenario checks it is gone the moment the ban returns, and released within 100 ms.
- **With an upload cap, a viewer pushed to the minimum bitrate stayed there.** Its share, its target plus 25%, rounded down to a 0.5 Mbit/s step, came out equal to the minimum, so it could never grow. The margin is now at least one step.
- **The panic key could be overtaken by a grant already on its way**, an accept or pass the app sent just before the key. The key now also sets a flag the input task checks before every grant, until the host allows control again.
- **Taking the seat back wasn't aimed:** a viewer giving control back while the host was passing it to another could end the other's hold. A revoke now names the holder it is meant for.
- **A delay sample from an abandoned epoch could be matched to the wrong frame.** The viewer now drops an abandoned epoch's samples, and the host matches on epoch and frame exactly.
- **Weak tests:** the window-closing scenario now requires the release within 150 ms (the dead-man timer can't fire before about 200 ms), and G7 checks that the throttle applied.
- **Stopping a stream didn't wait for removed viewers' encoder threads**; it does now.

## Things to know

- **A viewer whose connection to the host is replaced** (a redial) stops watching; its stream was bound to the old connection. Clicking Watch again works. M6's heartbeats can make this smoother.
- **Capture and encoding are CPU copies until M9**: every viewer's encoder uploads its own copy of each frame.
- **`couch-cli host` still hands the seat to the first viewer**, which `release-check` relies on.

## Follow-ups

- **M5b**: the screens (going live, the stage, the host's panel), the viewer's HUD and hotkeys, the panic key's keyboard hook, the shortcuts list, your run as alice, bob and carol.
- **Later**: editing shortcuts (your request); restarting a crashed viewer (M6); per-member permissions (M6).

## How to reproduce

```powershell
. .\scripts\dev.ps1
cargo test -p couch-proto -p couch-room -p couch-media -p couch-net    # messages, session machine, upload share, stream acceptor
cargo test -p couch-engine --test sessions                             # three engines with sessions, ~2 s
cargo build --release -p couch-cli -p couch-viewer
.\target\release\couch-cli.exe gates --quick                           # G1, G3-G5, G7
.\target\release\couch-cli.exe release-check
```
