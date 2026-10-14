# M9d results: 120 fps, capture pacing, and thread priorities measured

Step 4, the last, of M9 ([research/sunshine-moonlight.md](research/sunshine-moonlight.md), items 3 and 4), plus the "multimedia thread priorities" M9 had listed since M0.

**What could be tested here:** NVIDIA only (RTX 3060 Ti), on a 239 Hz screen. A weaker PC was stood in for by confining every process to a few CPUs (below).

## What it does

**The capture gets the frame rate asked for, on any screen.** Before M9d, the only pacing was Windows.Graphics.Capture's minimum update interval, one frame long. Windows composes only on the screen's refresh, so a frame came at the first refresh *after* the interval:

| Host screen | 60 fps asked | 120 fps asked |
|---|---|---|
| 60, 120 or 240 Hz | 60 (on the edge at 120 and 240 Hz) | 60, 120, 120 |
| 144 Hz | **48** | **72** |
| 165 Hz | **55** | **82** |
| 75 Hz | **37.5** | 75 |

Measured on this PC before the change (239 Hz): `--fps 100` and `--fps 90` both gave **80.0 fps** (three refreshes), 60 gave 59.9 and 120 gave 119.0. It also cost bitrate, since NVENC budgets bits per frame for the rate it was given.

**The pacer** (`couch_media::pacing`, pure, with simulated screens in its tests)
- **Windows delivers more frames.** With frames on the GPU, the minimum interval is 4 ms (up to 250 a second, as Sunshine asks); each extra frame is a GPU copy into the pool.
- **The pacer** in the host's capture thread (`pipeline/host.rs`, `capture_frames`) picks which go out:
  - due times advance by exactly one interval, so the average is the frame rate;
  - a frame goes at once if it is at or past its due time, or early by less than half the shortest recent gap between frames (at most a quarter of an interval);
  - an earlier one is held, and dropped if a newer one comes within an interval of its due time. The newer one is fresher and, being late, goes at once.
  - a held frame nothing replaces goes after that wait: the last change before a still picture.
- **How long a held frame waits took two corrections:**
  - **First version:** it went at its due time exactly. The frame captured right at that time reaches the capture thread a moment later, so at 120 fps the stale one went instead: p95 10 ms instead of 4 (below).
  - **Second version:** it waited about one gap between frames. The review found that a game a little faster than the stream (61 to 70 fps into 60) kept arriving early, and its frames were held up to an interval each, for seconds. A simulation in the pacer's tests showed 580 frames of a 61 fps game held in 20 s, the longest by 16.6 ms.
  - **Now:** a whole interval. A running game always replaces the held frame first, so its frames go as they arrive, and about one a second is dropped.
- **The wait runs on the clock frames arrive by,** learned from recent frames, not on their capture timestamps (which come from the compositor), so a difference between the two can't stretch it.
- **After a pause** the schedule starts again rather than catching up, and no two frames go closer than a quarter of an interval.
- **Frames in memory aren't sped up.** Each costs a copy, so those hosts (no NVIDIA encoder) keep Windows' one-frame interval, logged. Pacing them needs measuring on that hardware.
- **The switch to memory now restarts the capture,** since Windows can't change a running capture's interval. `capture_frames` returns, and the capture starts again with frames in memory (`CaptureRequest::in_memory`). It happens at most once a stream: at its start on a host whose encoder can't read GPU frames, or later if an encoder reading them, or the capture's own GPU copy, fails. `Capturer::frames_in_memory` is gone. A capture that can't keep frames on the GPU says so (`CaptureEvent::GpuFramesFailed`) and is restarted the same way. If the restart fails, it is tried once more after 250 ms before the stream ends.
- **The switch `COUCH_HOST_PACING=0`:** the one-frame interval and no pacer, exactly as before.
- **The host logs** its capture settings, and `couch-cli host`'s per-viewer line counts the frames held and replaced.

**120 fps in Go live.** The frame rates are 120, 60 (default) and 30.
- **The screen's refresh rate** comes with each source (`WindowView` and `ScreenView`, `refresh_hz`; `couch_platform_win::monitor`). Screens list it.
- **Above it**, Go live warns that the stream can't go faster than the screen.
- **At 120**, a line says it doubles each viewer's encoding, and spreads the same bitrate over twice the frames.

**Measuring tools**
- **The view's `RESULT`** has `frame_gap_ms`: the gaps between consecutive frames' capture times, so how even the pacing is.
- **`viewers.ps1 -Fps`** sets the stream's rate.
- **`latency-probe --cpu-load [N]`** runs N busy threads at normal priority for the whole run (by default one per CPU it may use): a stand-in for a game using every core. It reports their work per second (`cpu_load_work`), the game's share, so a change that takes from the game shows.
- **`latency-probe --cpus 16-19`** confines the run, and every process it starts, to those CPUs: a weaker PC on this one.
- **`latency-probe --clock-fps 61`** caps the latency clock (`couch-viewer --clock --fps`) like a game at that rate: pacing's hard case, a game near the stream's rate.
- **`COUCH_HOST_ENCODER=<FFmpeg name>`** (e.g. `libopenh264`) forces one encoder taking frames in memory: a host without NVIDIA, here.

## Measurements (this PC, release build, alternating runs)

**Pacing** (`latency-probe --secs 20 --relay none`, the final build; clock to present p50 / p95 / max, ms)

| | Paced | Before (`COUCH_HOST_PACING=0`) |
|---|---|---|
| 100 fps | **100.0 fps**; 10.8–10.9 / 11.4 / 23 | 80.0 fps; 10.9–11.3 / 12.6–16.3 / 23–29 |
| 60 fps | 60.1 fps; 11.3 / 12.0 / 33 | 59.9 fps; 11.9–12.2 / 13.1–13.7 / 22–33 |
| 120 fps (the first version) | 120.0 fps; 10.7 / 11.4–11.9 / 26 | 118.6–119.3 fps; 11.1–11.2 / 12.0–13.4 / 22–28 |

- **The rate is now exact,** and the median a little lower (frames go at the refresh nearest their due time).
- **After the review's fix,** the same runs gave the same: 100 fps 100.0 (p50/p95 10.9/11.9 against 11.1/15.1 unpaced), 60 fps 11.3/11.9 against 12.0/13.3.

**A game a little faster than the stream** (`latency-probe --fps 60 --clock-fps 61` or `66`: the clock capped like a game at that rate; final build; two runs each)

| Game | Paced | Before (`COUCH_HOST_PACING=0`) |
|---|---|---|
| 61 fps | 59.6–59.8 fps; p50 10.5–11.0, p95 13.9–14.2 ms | 57.7–58.1 fps; p50 11.3, p95 22.7–22.9 ms |
| 66 fps | 59.8 fps; p50 10.5–11.6, p95 13.6–14.2 ms | 56.1 fps; p50 12.3–12.5, p95 22.4 ms |

- **Windows' own interval delayed these frames too:** the first frame after the interval often waited for the next refresh. Pacing now sends each as it arrives and drops about one a second.
- **The review's case:** before its fix, the pacer's simulation showed 580 frames of a 61 fps game held in 20 s, the longest by 16.6 ms. Now none is.
- **The frame gaps** at 100 fps on a 240 Hz screen alternate between two and three refreshes (p50 8.3, p95 12.5 ms): the best that screen allows.

**Several viewers** (`viewers.ps1 -Screen`, 1080p)

| | fps | capture to present p50 / p95 |
|---|---|---|
| 4 viewers at 120 fps, paced (final build) | 115.2–115.9 | 1.9–2.7 / 3.9–4.7 ms |
| 4 viewers at 120 fps, `COUCH_HOST_PACING=0` | 117.3–118.3 | 1.7–2.9 / 3.7–3.9 ms |
| 4 at 120, the first pacer (held frames released at the due time) | 118.4–118.8 | 2.2–3.1 / **10.1–10.5 ms** |
| 7 viewers at 60 fps, paced (the first pacer) | 60.0–60.1 | 5.7–6.8 / 9.7–13.9 ms |
| 4 viewers at 60 fps, frames through memory (`-CopyThroughMemory`; not sped up) | 59.2–59.3 | 12.2–13.7 / 18.3–19.7 ms |

- **120 fps on this 240 Hz screen is the one place pacing costs a little:** about 0.5 ms of p95 and 2 fps, where the old interval was already right. On a 144 or 165 Hz screen it is the difference between 120 and 72 or 82 fps.
- **Seven viewers** keep M9c's numbers (59–60 fps, p50 5.4–6.8 ms).

**Thread and process priorities: tried, measured, dropped.** The plan was MMCSS (`AvSetMmThreadCharacteristicsW`) for the stream's threads on both sides. All runs at 60 fps; clock to present p50 / p95, ms.

1. **Every core busy** (`--cpu-load`, 28 threads): p95 went from about 12 to 20–25 ms, with or without MMCSS.
2. **Raising the processes from outside** to High brought it back to 12.1. But that also raised the latency clock, which plays the game. With **only the clock raised**, and ours at normal priority, the p95 was 13.0, as idle. So the load slowed the stand-in game, not the stream: Windows already boosts threads that wake for work, which ours do.
3. **High priority built in** (the app while it hosts or watches, and the stream window), with and without MMCSS: 23.3–24.7 p95 under load, the same as neither.
4. **A weaker PC** (`--cpus 16-19`: 4 efficiency cores, with 4 busy threads), two rounds:

   | | p50 | p95 | fps | game's work (M/s) |
   |---|---|---|---|---|
   | NVENC, neither | 27.8–28.0 | 40.7–41.5 | 55.3–55.4 | 2,595–2,602 |
   | NVENC, MMCSS | 27.8–27.9 | 40.2 | 55.7–55.9 | 2,578–2,597 |
   | NVENC, MMCSS and High | 28.0–28.3 | 41.1–41.5 | 54.5–55.7 | 2,599–2,623 |
   | OpenH264, neither | 27.0–27.1 | 46.5–46.6 | 47.0–47.3 | 2,589–2,596 |
   | OpenH264, MMCSS | 25.8–26.3 | 43.9–44.1 | 43.8–45.3 | 2,588–2,605 |
   | OpenH264, MMCSS and High | 26.0–26.1 | 45.0–45.8 | 46.7 | 2,589–2,599 |

   Maxima of 0.3 to 3.4 s came in every variant, "neither" included: the starved setup, not a change.

- **7 viewers**, priorities on against off: one pair favoured them (p95 about 11 against 12.5 ms), the other was the same.
- **So neither change helped,** even on a starved CPU, and neither took anything from the game. You chose to drop both and keep the tools.
- **The risks that weighed against keeping them unmeasured:**
  - High priority covers the whole process. A host's software encoder, a viewer's software decoder, or a bug that spins would go ahead of the game, on exactly the weaker PCs it was meant to help.
  - MMCSS may throttle other network traffic while it runs (`NetworkThrottlingIndex`).
- **Caveat:** the probe streams a small window (1088x184), so even OpenH264 costs little CPU in it. A real game also loads the GPU, which none of this models.

## Checks

| Check | Result |
|---|---|
| `cargo test --workspace` (no GPU), clippy, fmt, `tsc` | pass |
| The pacer's tests: screens at 60, 75, 144, 165, 239.76 and 240 Hz against 30, 60, 100 and 120 fps, held frames released precisely or only by the next frame: the average within 0.5% of the lower of the two, no two frames closer than a quarter interval; a still picture after one early frame; no catching up after a pause; a self-paced source passes straight through; the frame captured at the due time beats the held one | pass |
| Session scenarios (36): a capture at 240 fps paced to the stream's 30; an encoder that can't read GPU frames restarts the capture in memory; a failing GPU encoder too | pass |
| `couch-platform-win`: the primary screen's refresh rate | pass |
| `couch-cli gates --quick` | all pass, twice. Before the review's fix: G1 11.5 / 12.0 ms; G3 p95 78.1 ms, drift +0.3, 19.5 Mbit/s; G4 back in 102 ms with exactly 1 keyframe; G5 settled in 0.37 s, latency back in 0.55 s; G7 the second viewer 12.3 ms (unimpaired 12.7), 20.0 Mbit/s. After it: G1 11.4 / 15.6 ms; G3 77.6 ms, drift +0.8, 19.5 Mbit/s; G4 55 ms, 1 keyframe; G5 0.36 s and 0.52 s; G7 12.4 ms (12.7), 20.0 Mbit/s |
| `couch-cli lifecycle-check` (before the review's fix, which changed the capture loop only in retrying a restart into memory) | all 6 pass (window swap followed in 19 ms; game exit 21 ms; source lost 5.2 s; stream window restarted 36 ms; host lost 4.7 s; held key released 0 ms after restart) |
| The restart into memory on real capture | exercised by the `COUCH_HOST_ENCODER=libopenh264` runs: the capture started on the GPU, the encoder couldn't read GPU frames, and the stream went on in memory |

## Code review

A review by a separate agent, each finding checked against the code before acting on it:

| Finding | Outcome |
|---|---|
| A game a little faster than the stream had frames held up to an interval each, for seconds (the rigid schedule kept it early, and the held frame went before the newer one came) | **Fixed:** a held frame waits a whole interval, so a running game always replaces it. A simulated game at 50 to 100 fps on 144 to 240 Hz screens is now in the pacer's tests, and failed on the old wait. Measured with `--clock-fps` (above) |
| The held frame's wait compared capture timestamps (the compositor's) with our clock; `capture_to_present` reading 0.0 suggests they differ | **Fixed:** the pacer learns the offset from recent frames, and waits on the arrival clock |
| A released frame was recorded as sent at its due time, so the next frame could follow at once | **Fixed:** it counts as sent when it goes, which starts the schedule again |
| Restarting the capture in memory can now end a stream if it fails once | **Fixed:** tried once more after 250 ms |
| A backend handing GPU frames to a capture asked for memory would restart it every frame | **Fixed:** never restarted twice; the encoders copy such frames |
| The pacing scenario fails with `COUCH_HOST_PACING=0` left set | **Fixed:** it skips, saying so |
| The desktop app may run at Windows' coarse timer resolution (15.6 ms), so a held frame could go late | **Left:** since the fix, held frames are released by the clock only for a still picture's last change, where a few ms don't show |
| A refresh-rate lookup per listed window | **Left:** three cheap calls per window, when Go live opens |
| Wording: "the usual gap" (it's the shortest), restarts "only at the start" (also on a later failure), the `viewers.ps1` header | **Fixed** |

## Found later

**In P5, a regression from the review's fix:** with 4 viewers of a screen at 60 fps (`viewers.ps1 -Screen`), pacing gave 55.5 to 57.3 fps, and a p95 from 10 to 28 ms that varied from run to run and viewer to viewer. Unpaced runs gave 59.5 to 59.9 fps and 10.2 to 10.6 ms. Several runs, with and without P5's timestamp change.
- **The likely cause:** a held frame now waits a whole interval for a newer one. That fixed games a little faster than the stream, but when the capture stalls (a GPU busy with 4 encodes and 4 decodes), the held frame goes out up to two intervals old.
- **At 120 fps** the wait is half as long, and 4 viewers were fine.
- **A suspected second factor** (P5's review): since P5 the pacer gets delivery times. Frames queued behind a stall arrive back to back, so the shortest recent gap, and with it the slack, collapses for a while, and slightly early frames get held.
- **The fix is M9e.**

## Left as it is, to watch for

- **`latency-probe`'s `capture_to_present` reads 0.0 at the median** for the clock window, before M9d too (step 1's runs on main). Explained and fixed in P5: Windows' capture timestamps run ahead of delivery, and since P5 a capture time is never later than delivery ([p5-results.md](p5-results.md)).
- **120 fps on a 240 Hz screen:** pacing costs about 0.5 ms of p95 and 2 fps with 4 viewers. Small, and the price of the exact rate elsewhere.
- **G1's p95 was 15.6 ms in the last gates run** (12.0 in the one before): within the gate's normal range, and M9c saw 16–17 ms in 2 of 12 runs too.
- **Hosts with frames in memory** (no NVIDIA encoder) aren't paced: on a 144 Hz screen they still get 48 of 60 fps.
- **144 and 165 Hz screens** were stood in for by `--fps 100` and 90 on this one; your run can check one directly.

## Your run

The app (debug build), on this PC:
- **Streaming from the 240 Hz screen at 60 and 120 fps works.**
- **Go live warns at 120 fps for sources on the 60 Hz screens**, as it should.
- **Not tried:** a 144 or 165 Hz screen, so the case that motivated pacing was only stood in for (`--fps 100` and 90 on this 239 Hz screen).
