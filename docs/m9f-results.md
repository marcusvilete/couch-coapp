# M9f results: capture under a GPU-bound game

## Why

In M9e's run, Witcher 3 was in game at about 110 fps and streamed at 120 fps, watched from the same PC. The stream showed about 58 fps, paced or not, while the game's menu (240 fps) gave a steady 120.

## What we found

The diagnosis took several turns, each settled by a trace. The tools built for it are kept (below).

1. **Our callback isn't slow.** It took 0.1 ms per frame, 1% of the time. Windows itself produced only about 77 frames a second of the game's ~100: the gaps between Windows' own frame timestamps were 12.5 ms.
2. **A GPU-heavy stand-in didn't reproduce it.** With the latency clock drawing a 4K load without vsync, at about 108 fps and the GPU at 100%, the stream got about 106. Whatever Witcher 3 does differently (DX12, how it presents, VRAM), only the game shows it, so the rest was measured with it (single runs, about 30 s each).
3. **A bug found on the way:** if the game was minimized when someone started watching, its first frame was 146x28. NVENC refused a frame that small, and the stream took that as "this encoder can't read GPU frames": it captured to memory, the slow path, for the whole session (about 41 fps, about 40 ms late). **Fixed:** frames under 160x64 are dropped in the capture.
4. **The capture library's frame pool had one buffer,** fixed in `windows-capture`'s code. With one buffer, Windows can't write the next frame until the GPU has run our copy of the last, and under the game that copy waits. A patched copy with two buffers (a spike, never committed) captured about 25 to 30% more frames.
5. **Sunshine's GPU priority** (the process's GPU scheduling class high, and the capture device's GPU thread priority 7) works without administrator rights. On the spike it looked like it cut latency.
6. **We wrote our own capture** (your choice over vendoring a patched `windows-capture`). Its first version did worse than the spike: Windows calls a free-threaded pool's handler on several threads at once, and a call that took a frame and then waited for our lock held a buffer. **Fixed:** the lock comes first.
7. **The device lock isn't the problem:** splitting each copy's time showed waits on the lock in under 1% of frames (max 16 ms). The copy and flush themselves never waited.
8. **Four alternating runs settled the priority. It made things worse:** it delayed Windows' own capture work (frames reached us 3 ms late at p50 instead of on time). **Dropped** by the rule.

## The result: Witcher 3 (about 100 to 110 fps), 120 fps stream, watched on the same PC

| | Old path (one buffer) | M9f (our capture, two buffers) | Two buffers with priority (dropped) |
|---|---|---|---|
| Frames captured, a second | 72.2 | **106.7–106.9** | 88.9–89.9 |
| Frames reaching us after Windows' time, p50 / p95 | −1.7 / 6.3 ms | −1.5 / 5.9, −1.2 / 7.2 ms | 3.2 / 12.7, 3.3 / 13.2 ms |
| The viewer presented, a second | 70.1 | **100.7–103.1** | 78.5–82.5 |
| Capture to present, p50 / p95 | 23.7 / 41.1 ms | **16.8 / 32.7, 17.7 / 38.9 ms** | 34.3 / 64.0, 38.6 / 59.6 ms |
| Slowest encode | — | 16.0–16.7 ms | 18.2–23.4 ms |

- The old path is one run (G2); M9f and the priority are the alternating runs J1 to J4.
- In the M9e run, inside the app, the stream showed about 58 fps.
- **The stream now shows about all the game renders,** with lower latency than before. The viewer on the same PC still shares the GPU with the game; your two-PC run shows the real case.

## What changed

**Our own Windows.Graphics.Capture** (`couch_platform_win::wgc`, replacing the `windows-capture` crate):
- **a free-threaded frame pool of two buffers** (`COUCH_CAPTURE_BUFFERS=1` for one, to compare);
- **the session's lock taken before each frame,** so a waiting call holds no buffer;
- **closed windows noticed by checking `IsWindow` every 100 ms:** the item's `Closed` event didn't arrive on a free-threaded pool, and `lifecycle-check` caught it (swapped and closed windows went unnoticed until fixed);
- **the cursor, border and minimum interval set only where this Windows has them,** as before;
- **windows and monitors listed by our own code** (`top_windows`, `monitor`), with the same rules: visible, not a tool or child window, and monitors' friendly names. A process's name now comes from a limited query, which also works for a program running as administrator.

**Also kept:**
- frames under 160x64 dropped (the minimized-window bug);
- `COUCH_CAPTURE_TRACE=<csv>`: each GPU frame's arrival, Windows' timestamp, the lock wait, copy and flush times;
- `latency-probe --gpu-load N` (the clock GPU-bound, without vsync) and `--pacing-trace`; the probe always shows the clock's own frame rate as `clock_fps`.

**Dropped:** the GPU priority calls, after the measurement above.

## Measurements without a game (this PC, release build, alternating two buffers and one)

| Run | Two buffers | One buffer |
|---|---|---|
| `latency-probe` 60 fps, two runs | 60.0–60.1 fps; clock to present p50 11.2–11.5 ms | 60.0–60.1; 11.4 |
| 120 fps, two runs | 119.8–120.0; 10.8 | 120.0; 10.8 |
| 100 fps | 100.1; 10.9 / 11.5 | 100.0; 10.9 / 11.6 |
| 60 fps, the clock at 61 | 59.4; p95 14.3 | 59.6; 14.4 |
| 120 fps, the GPU-bound clock (about 108 fps), two runs | 105.5–106.3; p50 22.0–22.1 | 105.6–105.8; 22.0–22.1 |
| `viewers.ps1 -Screen -Clock`, 4 viewers, 60 fps, two runs | 60.0–60.1; p95 9.9–10.1 | 60.0–60.1; 9.9–10.1 |
| the same at 120 fps | 119.9–120.0; p95 6.8–6.9 | 119.9–120.0; 6.9 |

Without a game the two are the same: the gain appears only under a GPU-bound game.

## Checks

| Check | Result |
|---|---|
| `cargo test --workspace`, clippy, fmt | pass |
| `couch-cli windows` | the same windows and monitor name as with `windows-capture` |
| `couch-cli capture-test`, a window and the screen | 60.0 and 59.8 fps |
| `couch-cli lifecycle-check` | all 6 pass: a swapped window followed in 80 ms, game exit in 78 ms, source lost in 5.2 s |
| `couch-cli gates --quick` | all pass: G1 11.6 / 12.2 ms; G3 p95 78.9 ms, drift +0.0, 19.5 Mbit/s; G4 back in 76 ms with exactly 1 keyframe; G5 settled in 0.33 s, latency back in 0.56 s; G7 the second viewer 16.0 ms (unimpaired 12.9), 20.0 Mbit/s |

## Code review

A review by a separate agent; each finding checked against the code before acting on it.

| Finding | Outcome |
|---|---|
| **A monitor capture no longer noticed its monitor going** (unplugged, or the display mode changed): the `IsWindow` check covered windows only, and Windows' Closed event doesn't arrive on our pool. A screen stream would have stayed live on a frozen picture | **Fixed:** every 100 ms, a monitor capture checks that its monitor is still listed (`GetMonitorInfoW` answers even for a stale handle, so it can't serve) |
| The capture's debug line could report no options on a process's first capture: it asked Windows before WinRT was ready | **Fixed:** `wgc::has` readies WinRT first |
| A session that failed partway through starting left its event handlers registered (the sink, device and trace file kept alive) | **Fixed:** the session is built before the handlers, so a failure drops it and its handlers |
| The capture trace showed the previous frame's timings on a dropped or failed frame | **Fixed:** reset for each frame |
| A window under 160x64 streamed nothing, silently | **Logged** once each time it happens ("too small to stream (minimized?)") |
| Nits: a SAFETY comment away from its block, the minimized size given two ways, the clock's frame-rate report described as `--gpu-load` only, the switch's wording, a reader that stopped at a non-UTF-8 line, blank lines left in `Cargo.toml` | **Fixed** |
| `monitor::name` and `top_windows::is_capturable` follow `windows-capture`'s (MIT) behaviour closely | **Kept as ours:** written from Windows' documented route (display configuration source and target names) and the same listing rules, for the same results; no code copied |
| Checked, nothing to do: dropping a session while a handler runs (no deadlock, no use after free), no panics in the handlers, the frame-size clamp, `Recreate`, the time conversion, the `Send` claims, the MTA, the explicit device lock, the CPU path, the callers of the window and monitor lists (same rules and order; titles now read lossily; elevated programs now get a name) | — |

## Your run

In the app (`npm run tauri dev`: debug builds), with Witcher 3 in game and a 120 fps stream, watched from alice on the same PC:

| | The stream window showed |
|---|---|
| Before (M9e's run) | about 58 fps |
| M9f, one buffer (`COUCH_CAPTURE_BUFFERS=1`, same build) | about 60 fps |
| **M9f, two buffers (the default)** | **about 80 fps** |

- **Why less than the command line's ~100:** the app's engine and alice's stream window were debug builds, and alice's window is larger. Both draw on the GPU the game fills. A release installer should land nearer the command-line runs.
- **Notepad and Graveyard Keeper** behaved as before.
- **Minimized, then watched:** the stream recovered normally once the game came back. No "too small" line appeared: Windows usually delivers no frames at all while a window stays minimized.
- **Moving the mouse fast raised the stream to 120 fps,** in Witcher 3's paused menu too. The capture includes the cursor, so each movement is a new picture, and the menu doesn't keep the GPU busy. That's expected.
- **Not tried:** a screen stream losing its monitor.

**With a friend, over the internet** (the 0.10.2 installer, 2026-10-15): everything worked. Witcher 3 streamed at about 80 fps, briefly 100. The bitrate stayed near its 20 Mbit/s ceiling, dipping to 8–12 at times. There were some lag spikes, likely the friend's Wi-Fi; a test on a cable is to come.
