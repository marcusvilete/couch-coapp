---
name: performance-checks
description: Measure whether a Couch Co-App change made streaming faster or lighter, before and after on the same build — one viewer's latency (`couch-cli latency-probe` at 60 and 120 fps), load with several viewers (`scripts\viewers.ps1 -Screen`), GPU-path correctness (`--features gpu-tests`) and `couch-cli probe` — with the switches that turn a change off for comparison, known artifacts that aren't regressions, and where results go. Use for M9's performance work (zero-copy on the host or viewer, 120 fps, pacing, thread priorities), after changing capture (couch_platform_win::capture, gpu_frames), encoding or decoding (couch-codec-ffmpeg), the host pipeline's frame path, or the viewer's decode and render (couch-viewer gpu.rs, viewer.rs), and whenever asked whether something is faster. The `rate-control-gates` skill answers the different question of whether rate control still behaves.
---

# Performance checks

**The question here is "is it better, and by how much?",** not "does it still pass?" (that is `rate-control-gates`). Answer it by comparing the change against itself turned off, on the same build, in the same sitting. Numbers from an older milestone's results differ by enough (Windows updates, drivers, what else runs) to hide a few milliseconds.

## Before measuring

- **Ask the user to leave the PC alone, once, for the whole block,** with its length and what will open. These runs open windows, capture the screen and load the GPU, and anything else on screen skews them. Ask them to close the app (`npm run tauri dev`, profiles) first: a running `couch-cli.exe` also blocks the release build.
- **Build release:** `cargo build --release -p couch-cli -p couch-viewer`. Debug builds measure the debug build.
- **Correctness first, without the screen:** the GPU tests and the probe (below) don't need the PC idle, so run them before asking.

## The measurements

| Question | Command | About |
|---|---|---|
| Does the GPU path give the same pictures? | Host: `cargo test -p couch-codec-ffmpeg --features gpu-tests --test gpu_input -- --test-threads 1` (NVIDIA); the pool alone `cargo test -p couch-platform-win gpu_frames -- --ignored`. Viewer: `cargo test -p couch-viewer zero_copy -- --ignored --nocapture` (bit-exact against D3D11VA; says whether the D3D12 debug layer was active) | 10 to 30 s |
| What does this machine take? | `.\target\release\couch-cli.exe probe` ("GPU frames (M9b)", each encoder's option set) | 10 s |
| One viewer's latency (G1) | `couch-cli latency-probe --secs 30 --relay none --fps 60`, and `--fps 120`. `--fps 100` checks pacing on this 239 Hz screen (a rate that isn't a divisor of its refresh: 80 fps without pacing) | 45 s each |
| A game near the stream's rate (pacing's hard case) | `latency-probe ... --fps 60 --clock-fps 61` (or 66): the clock paints like a game capped at that rate. Every frame should go as it arrives: clock to present like an uncapped run's | 45 s each |
| Under a game's CPU load, or on a weaker PC | `latency-probe ... --cpu-load` (busy threads on every core; reports `cpu_load_work`, the game's share), plus `--cpus 16-19` (4 efficiency cores; the whole run is confined) | 45 s each |
| Several viewers' load | `.\scripts\viewers.ps1 -Screen` (4) and `-Viewers 7`; `-Fps 120` for 120 fps; `-CopyThroughMemory` for the host's path from before M9b, `-CopyFrames` for the viewers' path from before M9c. `-Clock` (with `-Screen`) for a busy screen independent of the stream, `-Clock -ClockFps 61` for a game near its rate; `-Trace` for the pacing trace (below). It also prints the stream windows' CPU, and the host's lines count the frames the pacer held and replaced | 40 s each |
| What pacing did, frame by frame | `viewers.ps1 -Screen -Trace` (or `couch-cli host --pacing-trace <csv>`) writes `pacing.csv` (each captured frame: `sent`, `held`, or `released` after its wait) and each viewer's presented frames (`view<N>.csv`, with `seq`) to `%TEMP%\couch-viewers-<N>`; copy them out before the next run, which clears the folder. Read from them: arrival gaps (stalls, bunching), the gaps between sends, how old released frames were, and whether a viewer skipped frames (`seq` gaps) or the host sent fewer. Skip the first few seconds: the windows are still opening | 40 s |
| Picture quality at one bitrate (a codec or encoder change) | `couch-cli capture-test --window couch-psnr --secs 8 --bitrate-mbps 5 --codec hevc --psnr` (and `--codec h264`), on a borderless ffplay window started afresh for each run so every run sees the same frames: `third_party\ffmpeg\bin\ffplay -noborder -window_title couch-psnr -f lavfi -i mandelbrot=s=1280x720:rate=60` (or `life=...:seed=7`, `testsrc2`). Luma PSNR, decoded against captured; `--monitor` for a game on screen | 10 s each |
| Still passing? | `couch-cli gates --quick` (the `rate-control-gates` skill) | 6 min |

- **Measure where the change acts.** One viewer on one PC rarely shows a host-side saving: the copies M9b removed never limited one viewer's median, but halved seven viewers' frame rate. Use `viewers.ps1 -Screen` for anything on the host's per-viewer path, and `latency-probe` at 120 fps for anything on pacing or presentation.
- **The development app is a debug build.** Its stream window leaves wgpu's validation off like a release build (M9c: the D3D12 debug layer added 0.6 ms or more per frame and made zero-copy look slower). Don't set `WGPU_VALIDATION=1` while measuring. Viewer-side switches go in the **watcher's** environment (`.\scripts\profiles.ps1 alice` from a shell that has them), not the host's.
- **`latency-probe` streams a small clock window (1088x184),** so per-pixel costs (copies, uploads) barely show in it. `viewers.ps1 -Screen` streams 1080p.
- **"arrive → decoded" doesn't compare across the viewer's paths:** with D3D12VA (M9c) it ends when decoding is submitted, not finished. Compare capture to present and clock to present.
- **`viewers.ps1` without `-Screen` streams the test pattern, made in memory: it never takes the GPU path.** Only `-Screen` (the primary screen, kept changing by the viewer windows on it) measures capture into NVENC.
- **Capture to present before P5 read low:** Windows' capture timestamp runs ahead of delivery, so `latency-probe` showed 0.0 and `viewers.ps1` a few ms too little. Since P5 the capture time is never later than delivery: the probe reads about 2 to 2.6 ms, and `viewers.ps1 -Screen` about 3 ms more than before. Don't compare capture-to-present numbers across P5; clock to present is unaffected.
- **PSNR needs the encoder's own colour matrix** (M12c). FFmpeg's NVENC wrapper converts RGB with BT.601 whatever colorspace we set (`nvenc.c`), so `capture-test` measures against the matrix the stream declares (`YuvMatrix` on the decoded frame); the wrong one reads about 20 dB on colourful content, whatever the bitrate. Restart the source for each run too: a Mandelbrot zoom left running gets deeper (and slower to draw) from run to run, so later runs measure other pictures.
- **`frame_gap_ms`** shows how even the pacing is: on this 239 Hz screen, 100 fps alternates two and three refreshes (8.3 and 12.5 ms). Since P5 the gaps are between delivery times, so they include a little delivery jitter.
- **Only a real GPU-bound game shows capture problems** (M9f). Witcher 3 at about 100 fps lost a third of its frames with one capture buffer. The GPU-bound clock (`latency-probe --gpu-load 130`, about 108 fps, the GPU at 100%) never did, with one buffer or two. Measure capture changes with a game: `couch-cli host --source window --title <game> --fps 120 --pacing-trace <csv>` with `COUCH_CAPTURE_TRACE=<csv>`, `view <ticket> --timings-csv <csv>` from another PowerShell, then click back into the game. Alternate the runs, and stay in the same place in the game: the scene moves latency by about 5 ms. In PowerShell 5.1 run the host through `cmd /c "... 2>&1"`: redirecting stderr there prints a harmless NativeCommandError.
- **Reading `COUCH_CAPTURE_TRACE`:** `entry_us - windows_us` is how late Windows delivered each frame (on time is about -1.5 ms at p50, since Windows stamps a frame about when it will be shown); `lock_us` is our copy waiting for the device's lock (an encoder holds it); the gaps between `windows_us` show how many frames Windows produced at all. Late frames with a fast callback mean Windows' own capture work is waiting on the GPU.
- **`viewers.ps1 -Screen` alone is a loop** (M9e): the only things changing on the captured screen are the stream windows, so it changes only after the host sends a frame. Paced, that costs about 3 fps and a p95 of 22 to 25 ms at 60 fps, mostly in the first seconds while the windows open; unpaced, 60 fps and 10 ms. The loop's frames look like a game near the stream's rate, whose held frames must wait an interval, so pacing can't tell it apart. No real host has the loop. To measure pacing on a busy screen, add `-Clock`: paced and unpaced then both give 60 fps and a p95 of 10 ms. At 120 fps the loop costs little.
- **Under `--cpu-load`, the latency clock is the stand-in game, and renders late itself:** p95 rose from 12 to 20–25 ms in M9d whatever our side did. To see our pipeline alone, raise only the clock from outside (`couch-viewer.exe` whose command line has `--clock`, `PriorityClass = 'High'`): then it stayed at 13 ms. Raising every `couch-viewer.exe` raises the clock too, which once made a priority change look like a win.

## Before and after: the switches

Each performance change keeps a switch that restores the old path, so the comparison runs on one build:

| Switch | Old path | Since |
|---|---|---|
| `COUCH_HOST_ZERO_COPY=0` | the host copies captured frames through memory | M9b |
| `COUCH_VIEWER_ZERO_COPY=0` | the viewer decodes with D3D11VA and copies frames through memory, on wgpu's default backend (Vulkan on NVIDIA) | M9c |
| `WGPU_BACKEND=dx12` (or `vulkan`) | wgpu's backend alone, e.g. DX12's presentation without zero-copy (about 0.5 ms slower than Vulkan here) | M9c |
| `COUCH_VIEWER_FRAME_LATENCY=1..3` | the viewer's swap-chain frame latency (1 is the default) | M9a |
| `COUCH_HOST_PACING=0` | the capture at Windows' one-frame interval, unpaced (loses frames when the screen's refresh isn't a multiple of the rate) | M9d |
| `COUCH_CAPTURE_BUFFERS=1` | the capture's frame pool with one buffer, like `windows-capture`'s before M9f (the rest of our capture stays: the lock taken first, the cached staging texture); 2 by default | M9f |
| `COUCH_HOST_CODEC=h264` | every viewer in H.264 (HEVC goes to viewers that decode it, from an NVIDIA host taking GPU frames); `COUCH_VIEWER_HEVC=0` on the watcher's side does the same for one viewer | M12c |
| `COUCH_HOST_ENCODER=<name>` | not an old path: one FFmpeg encoder taking frames in memory, e.g. `libopenh264`, the software fallback of a host without a hardware encoder | M9d |

Add a row here, and to CLAUDE.md's commands, when a new change gets one. Set it in the shell that starts the runs; `couch-cli` passes its environment to the host and viewer it starts. `viewers.ps1 -CopyThroughMemory` and `-CopyFrames` set the M9b and M9c ones themselves. Alternate the runs (new, old, new, old) rather than all of one then all of the other, so a drift in the machine shows as noise, not as a difference.

## Keep the output

Write each run's whole output to a file in the scratchpad, then read it; don't filter while running. The summary lines start with the metric name (`clock_to_present_ms p50 ... p95 ... max ...`), not with "clock -> present", and a narrow filter once lost a whole batch. Per-second lines start with `[view]` or `[host]`; drop those, keep the rest:

```powershell
$log = "<scratchpad>\perf.txt"
foreach ($run in @(@(60,'new'),@(60,'old'),@(60,'new'),@(60,'old'),@(120,'new'),@(120,'old'))) {
    if ($run[1] -eq 'old') { $env:COUCH_HOST_ZERO_COPY = '0' } else { Remove-Item Env:COUCH_HOST_ZERO_COPY -ErrorAction SilentlyContinue }
    "== $($run[0]) fps, $($run[1])" | Out-File $log -Append -Encoding utf8
    .\target\release\couch-cli.exe latency-probe --secs 30 --relay none --fps $run[0] 2>$null |
        Where-Object { $_ -notmatch '^\[(view|host)\]' -and $_.Trim() -ne '' } | Out-File $log -Append -Encoding utf8
}
Remove-Item Env:COUCH_HOST_ZERO_COPY -ErrorAction SilentlyContinue
```

`viewers.ps1` prints its own summary; send it to a file the same way. Its numbers use the pt-BR decimal comma.

## Reading the results

- **Within noise** on this PC (RTX 3060 Ti): one viewer's p50 moves about ±0.3 ms and p95 about ±1 ms between runs. Call a difference real only if it holds in both runs of each side.
- **Known artifacts, not regressions:**
  - **"decode errors 1" counts the stream window's keyframe requests,** not only corrupt video. Before M9c, a window's start-up wait tripped it (`decoding fell behind ... seq=1` in its log); since M9c that wait doesn't count. With 7 viewers on the copy path (`-CopyFrames`), some windows still skip once about 230 ms after starting, working through the frames queued meanwhile at 270% CPU. Real decode errors come later in a run, or several at once. `viewers.ps1` sends the windows' logs to nowhere; to read them, run a copy of it with `2>nul` replaced by a file (the windows log their path and decoder at info level by default).
  - **A maximum near 150 ms on the host's memory path:** the start of the stream through memory. With GPU frames the maximum stays under about 50 ms.
  - **G1's p95 with viewer zero-copy:** 13.4 to 13.8 ms usually, but 16 to 17 ms in 2 of 12 runs in M9c. Repeat before calling a p95 change real.
  - **Bitrate below 20 Mbit/s for the first seconds:** the rate controller ramping up (keyframes at each step on NVENC).
- **With many viewers on one PC,** the GPU also decodes every stream, so the numbers overstate a real host's load. Compare like with like.
- **A change that shows no gain is dropped,** not kept as insurance (M9d dropped MMCSS and process priority after `--cpu-load` and `--cpus` runs showed nothing either way).
- **Reference numbers** (this PC):
  - `docs/m9f-results.md`: our capture, two buffers. Witcher 3 (about 100 fps) into a 120 fps stream, watched on this PC: 107 frames captured and about 101 shown, capture to present p50 17 ms, p95 33–39 ms; one buffer 72 and 70, p50 24, p95 41. Without a game the two are the same (latency-probe 60/100/120 fps at 60.0/100.1/120.0, clock to present p50 11.2–11.5/10.9/10.8 ms).
  - `docs/m9e-results.md`: the held frame's wait. 4 viewers at 60 fps: `-Screen -Clock` 60.0–60.1 fps, p95 10.1–10.2 ms (paced or not); `-Screen` alone 56.9–57.3 fps, p95 21.6–24.7 ms (unpaced 59.5–59.9, 10.3–10.6); `-Clock -ClockFps 61` 59.0–59.2 fps, p95 10.3; at 120 fps (`-Screen`) 113–116 fps, p95 6.8–7.0 ms (unpaced 117.7–118.5, 6.7–6.8).
  - `docs/m9d-results.md`: pacing; 100 fps gives 100 (80 before); one viewer's clock to present 10.8–11.3 ms p50 at 60 and 100 fps; a 61 or 66 fps clock into a 60 fps stream about 59.7 fps and p95 about 14 ms (about 22.5 ms unpaced); 4 viewers at 120 fps 114–116 fps, p95 4.0–4.6 ms; on 4 efficiency cores with 4 busy threads, p50 about 28 ms and 55 fps.
  - `docs/m9c-results.md`: zero-copy on both sides. 7 viewers at 59 to 60 fps, p50 5.4 to 6.8 ms, p95 about 9 ms, the stream windows at 37% of one core; one viewer's clock to present p50 12.4 ms at 60 fps.
  - `docs/m9b-results.md`: GPU frames on the host; 7 viewers at 58 fps, p50 about 14 ms.
  - `docs/m9a-results.md`: frame latency, 120 fps.
  - The gate references in the `rate-control-gates` skill.

## Recording

- The milestone's results file gets the before-and-after tables, how many runs, and anything odd with its explanation.
- When the change moves the numbers other checks compare against, update them: the `rate-control-gates` skill's viewer references, and CLAUDE.md's switches.
- Report to the user both what improved and what didn't move, with the numbers.
