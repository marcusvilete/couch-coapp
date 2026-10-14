# M9e results: the held frame's wait

## Why

P5 found a regression from M9d's review fix ([m9d-results.md](m9d-results.md), "Found later"). With 4 viewers of a screen at 60 fps (`viewers.ps1 -Screen`), paced streams gave 55.5 to 57.3 fps and a p95 of 10 to 28 ms; unpaced ones, 59.5 to 59.9 fps and 10.2 to 10.6 ms. The suspected causes were a capture that stalls under a busy GPU, and delivery times bunching after a stall.

## What the trace showed

**A new tool: the pacing trace.**
- **`couch-cli host --pacing-trace <csv>`** writes a row for every captured frame: when it arrived, its capture time, and whether it was sent at once, held, or released after its wait. A held frame followed by another frame was replaced by it.
- **`viewers.ps1 -Trace`** passes it to the host, and `--timings-csv` to each viewer (each presented frame, with its `seq`).

**Four runs of the regression's case** (4 viewers at 60 fps; two paced, two unpaced):

| | Paced | Unpaced |
|---|---|---|
| Frames captured | 117 a second | 60 a second |
| Arrival minus capture | p50 0.1 ms, max 6.2 | p50 0.1 ms, max 4.4 |
| Bunched arrivals (under 1 ms apart) | none | none |
| Frames sent | 58.2 a second | 59.8 a second |
| Frames each viewer presented | all that were sent: none skipped | all |
| Held frames released after their wait | 31 in 18 s (the first 3 s, viewers joining, left out), each about 21 ms old | none |

**So neither suspected cause was there.**
- The capture didn't stall, and nothing bunched.
- The viewers showed every frame they got: the host sent fewer.

**The cause was the test itself.**
- The captured screen showed nothing but the 4 stream windows. So it changed only when they presented a frame: about 8 and 12 ms after each send, and at no other time.
- Such a change arrives before its due time, so it is held. M9d's pacer then waits a whole interval for a newer one, which can't come before the next send.
- So the held frame went out about 21 ms old, and that frame's slot was lost.
- **When:** mostly in the first 4 seconds, while the windows open: 10 to 14 releases a second over the whole run's first seconds, which the table above leaves out. Afterwards, 0 to 5 a second, with a p95 of about 10 ms.

**Why it still mattered.**
- No real host's screen shows its own stream.
- But the same wait held frames with no successor coming: on a busy screen, the last change before a still picture or a moment without changes; and the first change on a still picture (menus, the desktop). Each waited up to an interval past its due time.

## The fix (`couch_media::pacing`)

A held frame waits for a newer one only while the source's next frame is expected. That depends on how often frames come, measured by the largest of the last 8 gaps, hitches of over two intervals left out:

| Source | Gap | The held frame goes |
|---|---|---|
| **A lone change:** the first on a still picture | its own gap over two intervals | at its due time |
| **Busy:** over 1.5 times the stream's rate (a screen changing every refresh) | the largest recent one up to 2/3 of an interval | a gap and a quarter after it arrived, at the earliest at its due time |
| **Near the stream's rate:** a game a little faster or slower | the largest recent one up to two intervals | an interval past its due time, as in M9d |

The last change before a still picture follows its source's rule: a busy screen's goes about a refresh after it came; a game's near the stream's rate waits an interval past its due time, as in M9d.

**Why each:**
- **Busy:** a newer frame always comes before the next due time. So releasing the held frame early can't make later frames early.
- **Near the rate:** the next frame's time is uncertain by a refresh or more. Releasing the held frame before it makes every later frame early: M9d review's cascade, up to an interval of latency per frame for seconds.
- **Why hitches are left out, and a lone change judged by its own gap:** a game runs as before after a hitch. Judged by the hitch, its next frames would count as lone changes, go at their due times, and make the following ones early: the cascade again, for about 8 frames after each hitch (found by the code review, below).
- **Why the largest gap, not the median:** the median takes the shorter gap of a source alternating between two, such as 70 fps on a 240 Hz screen or 100 fps on 144 Hz. In simulation that brought back the cascade (199 and 477 frames held).

**Tried and dropped along the way** (simulation only):
- **The slack from the median gap:** P5's review suspected bunching collapses the slack, but the trace showed none.
- **A wait of a gap and a quarter for every source:** that brought back the cascade for games near the stream's rate.

**Not fixed: the test's loop.**
- It would need the held frame released at its due time even near the stream's rate. That is M9d review's cascade.
- No real host has the loop. `viewers.ps1 -Clock` measures a busy screen without it (below).

**The switch.**
- While measuring, `COUCH_HOST_PACING=m9d` selected M9d's wait.
- It was removed afterwards: M9d's wait was a known regression, and the new rules were as good or better everywhere measured.
- `COUCH_HOST_PACING=0` stays, for measuring against unpaced capture.

## Tests

Every earlier test of the pacer passes, including the game near the stream's rate (8 screens and rates, no frame held) and the rate test (6 screens × 4 rates).

**New simulations:**

| Test | With M9e | With M9d's wait |
|---|---|---|
| A busy 240 Hz screen whose changes stop for 20 ms three times a second | 60.0 fps at 60, longest wait 8.3 ms | 57.0 fps, longest wait 25 ms |
| A change every refresh for 90 ms, then a still picture | the last change waits at most 12.5 ms at 60 fps, 5.2 at 120 | 29 and 12.5 ms |
| Games with uneven frame times: 60 fps ±3 ms, 61 ±2, 55 ±4, on 240 Hz | no frame held, the rate exact | the same |
| Games that hitch: 66 and 70 fps with a 50 ms hitch each second; 61 fps ±2 ms with a 40 ms hitch every 300 ms | none held for the even games; at most one per hitch for the jittery one (2 in 20 s) | — |

The pause and burst tests also bound how many frames wait (at most one per pause or burst), not only the longest wait.

**Also updated:**
- a busy screen's last change before a still picture, and the arrival-clock test, now go at their due time;
- a new test of each kind of source's wait.

## Measurements (this PC, release build, alternating runs)

**`viewers.ps1`, 4 viewers** (each cell is two runs unless noted; "fps" is what the viewers presented). The "M9e, first version" column is the build before the code review's fix (below); "M9e, final" is one run each on the final build.

| Run | M9e, final | M9e, first version | M9d's wait | Unpaced |
|---|---|---|---|---|
| `-Screen` at 60 fps (the loop) | 56.9–57.3 fps; p95 21.6–24.7 ms | 57.6–57.8; 13.8–15.9 | 56.1–57.4; 11.6–26.8 | 59.5–59.9; 10.3–10.6 |
| `-Screen -Clock` at 60 (a busy screen) | 60.0–60.1; 10.1–10.2 | 60.0–60.1; 9.9–10.1 | 60.0–60.1; 9.9–10.1 | 59.9–60.0; 10.0–10.2 |
| `-Screen -Clock -ClockFps 61` (a game near the rate) | 59.0–59.2; 10.3 | 59.1–59.2; 10.3–10.5 | 59.0–59.4; 10.2–10.4 | — |
| `-Screen` at 120 fps | 113.4–113.9; 6.8–7.0 | 114.6–116.0; 6.8–7.0 | 115.1–116.5; 6.8–6.9 (one run) | 117.7–118.5; 6.7–6.8 (one run) |

**Reading it:**
- **On a busy screen, pacing now costs nothing:** 60 fps and a p95 of 10 ms, as unpaced.
- **A game near the rate** is as with M9d's wait.
- **The loop is about where M9d left it.**
  - The first version halved its worst p95, but only because a long gap anywhere in the last 8 frames counted the next held frames as lone changes.
  - That is what brought back the cascade for games after each hitch (the code review, below).
  - From frame times alone, the loop's frames look like a game near the stream's rate. A game that hitches is real and the loop isn't, so the fix stays.
- **At 120 fps,** the loop costs a few fps either way; the final build's single run was 113 to 114.

**`latency-probe`** (one viewer, 20 s each):

| Run | M9e, first version | M9d's wait |
|---|---|---|
| 60 fps | 60.0 fps; clock to present p50/p95 11.3/12.0 ms | 60.1; 11.4/12.1 |
| 100 fps | 100.1; 10.9/11.5 | 100.0; 10.9/11.5 |
| 60 fps, the clock at 61 fps | 59.4; 11.7/14.4 (final: 59.4; 11.7/14.8) | 59.5; 11.6/14.4 |
| 60 fps, the clock at 66 fps | 58.6; 11.7/14.3 | 58.4; 11.6/14.4 |

The same with both: pacing for one viewer, and for games near the rate, is unchanged.

## Checks

| Check | Result |
|---|---|
| `cargo test --workspace`, clippy, fmt | pass |
| `couch-cli gates --quick` (the first version; the review's fix changes only what follows a hitch) | all pass: G1 11.4 / 12.2 ms; G3 p95 80.0 ms, drift +0.0, 19.5 Mbit/s; G4 back in 53 ms with exactly 1 keyframe; G5 settled in 0.35 s, latency back in 0.33 s; G7 the second viewer 12.6 ms (unimpaired 12.8), 20.0 Mbit/s |

**Version 0.10.1:** for a friends' installer without the regression, which 0.10.0 has.

## Code review

A review by a separate agent; each finding checked against the code before acting on it.

| Finding | Outcome |
|---|---|
| **A hitch** (one gap over two intervals) made the next 8 held frames count as lone changes, released at their due times: the near-rate cascade again, briefly. Simulated: a 66 or 70 fps game with a 50 ms hitch each second held 57 and 95 frames (M9d's wait: none), and the 120 fps pause test sent 236 stale frames | **Fixed:** a lone change is judged by the held frame's own gap, and the source's rate leaves hitches out. The hitch cases now hold none, and the pause test at 120 fps none. New tests with hitches |
| The new tests checked only the longest wait, which M9d's 120 fps numbers also met | **Fixed:** they bound how many frames wait too |
| The docs called the last change before a still picture a "lone change"; it follows its source's rule | **Fixed** in the pacer's doc, CLAUDE.md, `architecture.md` and here |
| This document's release counts (31 in 18 s, and 10 to 14 a second) seemed to disagree | **Clarified:** the first is without the first 3 s, the second about those seconds |
| The pacing trace was created again at each capture start, so a viewer joining after the last left truncated it | **Fixed:** opened once per stream |
| `viewers.ps1` left the clock running if the host never gave a ticket; its header still said the windows change the screen every frame | **Fixed;** `-ClockFps` now implies `-Clock`, which needs `-Screen` |
| Overflow, the busy branch alone, couch-media's purity, nothing printed to stdout, the version in all three files | Checked, nothing to do |

## Your run

The default profile hosted at 120 fps on the 240 Hz screen, and alice watched on the same PC. This was the first version, before the code review's fix, which changes only what follows a hitch.

| Source | Result |
|---|---|
| Graveyard Keeper (about 200 fps) | sometimes 120 fps |
| Witcher 3's menu (240 fps) | a steady 120 fps |
| Witcher 3 in game (about 110 fps) | about 58 fps, sometimes 64; the same unpaced (`COUCH_HOST_PACING=0`) |
| Notepad | few frames while still (nothing changes), 120 fps while the mouse moved |

- **Pacing isn't what held Witcher 3 back:** unpaced capture gave the same 58 fps.
- **The pacer's simulation** of a 100 fps game into a 120 fps stream gives 100 fps (98.8 with ±2 ms of jitter).
- **So the capture itself delivered about 60 frames a second** while the game kept the GPU busy. That becomes **M9f** (`docs/architecture.md`): capture under a GPU-bound game.
