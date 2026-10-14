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
| One viewer's latency (G1) | `couch-cli latency-probe --secs 30 --relay none --fps 60`, and `--fps 120` | 45 s each |
| Several viewers' load | `.\scripts\viewers.ps1 -Screen` (4) and `-Viewers 7`; `-CopyThroughMemory` for the host's path from before M9b, `-CopyFrames` for the viewers' path from before M9c. It also prints the stream windows' CPU | 40 s each |
| Still passing? | `couch-cli gates --quick` (the `rate-control-gates` skill) | 6 min |

- **Measure where the change acts.** One viewer on one PC rarely shows a host-side saving: the copies M9b removed never limited one viewer's median, but halved seven viewers' frame rate. Use `viewers.ps1 -Screen` for anything on the host's per-viewer path, and `latency-probe` at 120 fps for anything on pacing or presentation.
- **The development app is a debug build.** Its stream window leaves wgpu's validation off like a release build (M9c: the D3D12 debug layer added 0.6 ms or more per frame and made zero-copy look slower). Don't set `WGPU_VALIDATION=1` while measuring. Viewer-side switches go in the **watcher's** environment (`.\scripts\profiles.ps1 alice` from a shell that has them), not the host's.
- **`latency-probe` streams a small clock window (1088x184),** so per-pixel costs (copies, uploads) barely show in it. `viewers.ps1 -Screen` streams 1080p.
- **"arrive → decoded" doesn't compare across the viewer's paths:** with D3D12VA (M9c) it ends when decoding is submitted, not finished. Compare capture to present and clock to present.
- **`viewers.ps1` without `-Screen` streams the test pattern, made in memory: it never takes the GPU path.** Only `-Screen` (the primary screen, kept changing by the viewer windows on it) measures capture into NVENC.

## Before and after: the switches

Each performance change keeps a switch that restores the old path, so the comparison runs on one build:

| Switch | Old path | Since |
|---|---|---|
| `COUCH_HOST_ZERO_COPY=0` | the host copies captured frames through memory | M9b |
| `COUCH_VIEWER_ZERO_COPY=0` | the viewer decodes with D3D11VA and copies frames through memory, on wgpu's default backend (Vulkan on NVIDIA) | M9c |
| `WGPU_BACKEND=dx12` (or `vulkan`) | wgpu's backend alone, e.g. DX12's presentation without zero-copy (about 0.5 ms slower than Vulkan here) | M9c |
| `COUCH_VIEWER_FRAME_LATENCY=1..3` | the viewer's swap-chain frame latency (1 is the default) | M9a |

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
- **Reference numbers** (this PC):
  - `docs/m9c-results.md`: zero-copy on both sides. 7 viewers at 59 to 60 fps, p50 5.4 to 6.8 ms, p95 about 9 ms, the stream windows at 37% of one core; one viewer's clock to present p50 12.4 ms at 60 fps.
  - `docs/m9b-results.md`: GPU frames on the host; 7 viewers at 58 fps, p50 about 14 ms.
  - `docs/m9a-results.md`: frame latency, 120 fps.
  - The gate references in the `rate-control-gates` skill.

## Recording

- The milestone's results file gets the before-and-after tables, how many runs, and anything odd with its explanation.
- When the change moves the numbers other checks compare against, update them: the `rate-control-gates` skill's viewer references, and CLAUDE.md's switches.
- Report to the user both what improved and what didn't move, with the numbers.
