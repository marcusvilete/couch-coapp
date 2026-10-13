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
| Does the GPU path give the same pictures? | `cargo test -p couch-codec-ffmpeg --features gpu-tests --test gpu_input -- --test-threads 1` (NVIDIA); the pool alone `cargo test -p couch-platform-win gpu_frames -- --ignored` | 10 s |
| What does this machine take? | `.\target\release\couch-cli.exe probe` ("GPU frames (M9b)", each encoder's option set) | 10 s |
| One viewer's latency (G1) | `couch-cli latency-probe --secs 30 --relay none --fps 60`, and `--fps 120` | 45 s each |
| Several viewers' load | `.\scripts\viewers.ps1 -Screen` (4) and `-Viewers 7`; `-CopyThroughMemory` for the host's path from before M9b | 40 s each |
| Still passing? | `couch-cli gates --quick` (the `rate-control-gates` skill) | 6 min |

- **Measure where the change acts.** One viewer on one PC rarely shows a host-side saving: the copies M9b removed never limited one viewer's median, but halved seven viewers' frame rate. Use `viewers.ps1 -Screen` for anything on the host's per-viewer path, and `latency-probe` at 120 fps for anything on pacing or presentation.
- **`viewers.ps1` without `-Screen` streams the test pattern, made in memory: it never takes the GPU path.** Only `-Screen` (the primary screen, kept changing by the viewer windows on it) measures capture into NVENC.

## Before and after: the switches

Each performance change keeps a switch that restores the old path, so the comparison runs on one build:

| Switch | Old path | Since |
|---|---|---|
| `COUCH_HOST_ZERO_COPY=0` | the host copies captured frames through memory | M9b |
| `COUCH_VIEWER_FRAME_LATENCY=1..3` | the viewer's swap-chain frame latency (1 is the default) | M9a |

Add a row here, and to CLAUDE.md's commands, when a new change gets one (M9c's viewer path, for instance). Set it in the shell that starts the runs; `couch-cli` passes its environment to the host and viewer it starts, and `viewers.ps1 -CopyThroughMemory` sets the M9b one itself. Alternate the runs (new, old, new, old) rather than all of one then all of the other, so a drift in the machine shows as noise, not as a difference.

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
  - **"decode errors 1" on a run's first viewer:** the stream window skipping to a keyframe while its decoder starts (`decoding fell behind ... seq=1` in its log). It costs one keyframe request. It shows more often when the host's first frames arrive sooner (GPU frames). Real decode errors come later in a run, or several at once.
  - **A maximum near 150 ms on the memory path:** the start of the stream through memory. With GPU frames the maximum stays under about 30 ms.
  - **Bitrate below 20 Mbit/s for the first seconds:** the rate controller ramping up (keyframes at each step on NVENC).
- **With many viewers on one PC,** the GPU also decodes every stream, so the numbers overstate a real host's load. Compare like with like.
- **Reference numbers** (this PC): `docs/m9b-results.md` (GPU frames; 7 viewers at 58 fps, p50 about 14 ms), `docs/m9a-results.md` (frame latency, 120 fps), and the gate references in the `rate-control-gates` skill.

## Recording

- The milestone's results file gets the before-and-after tables, how many runs, and anything odd with its explanation.
- When the change moves the numbers other checks compare against, update them: the `rate-control-gates` skill's viewer references, and CLAUDE.md's switches.
- Report to the user both what improved and what didn't move, with the numbers.
