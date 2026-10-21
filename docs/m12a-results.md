# M12a (part 1) results: pacing for hosts with frames in memory

## Why

M9d paced the capture for hosts whose encoder reads GPU frames (NVENC): the capture runs every 4 ms into a pool on the GPU, and the pacer sends exactly the stream's rate. A host whose encoders take frames in memory (AMD and Intel hosts, the software fallback, NVIDIA when GPU frames fail) kept Windows' one-frame interval and copied each frame to memory inside the capture callback, because copying every captured frame to memory would cost too much. So those hosts still lost frames when the screen's refresh isn't a multiple of the stream's rate (100 fps on a 240 Hz screen gave 80), and a game a little faster than the stream (61 into 60) was held up, as GPU hosts were before M9d.

The split from the rest of M12a (2026-10-19): this part can be built and measured on an NVIDIA PC; GPU frames into AMF and Quick Sync wait for that hardware.

## What changed

**Paced on the GPU, read back after pacing** (`pipeline::host`, `send`). When no encoder reads GPU frames, the capture no longer starts again in memory: it stays on the GPU, paced as for NVENC, and each frame the pacer *sends* is read back to memory once (`GpuFrame::cpu`), for every viewer, before it reaches the encoders. Frames the pacer drops are never read back. The encoders are unchanged: they take frames in memory as before.

**Frames handed out as mapped staging memory** (`gpu_frames`, `MappedStaging`). Measuring showed the read-back's memcpy (8 MB at 1080p) was its larger part: 2.9 ms of 4.8 ms with four encoders busy. So a frame read back is now its staging texture's own mapped memory, held by its `Bytes` and unmapped, back to its pool's set, when the last viewer's encoder drops it; up to 16 per pool, then the memcpy as before.

**Fallbacks.** If the capture can't keep frames on the GPU, or three read-backs fail in a row, the capture starts again in memory, as before (`capture_in_memory`). Unpaced (`COUCH_HOST_PACING=0`), the capture goes to memory too.

**Switches**, for measuring against the old ways on one build:
- `COUCH_HOST_MEMORY_PACING=0`: the capture in memory at Windows' interval, as before;
- `COUCH_HOST_MAPPED_FRAMES=0`: the read-back copies frames out by memcpy.

The host's log says "each frame sent is read back to memory after pacing", and at the stream's end how many were and how long they took.

## Measured (2026-10-20, this PC)

NVENC fed from memory (`COUCH_HOST_ENCODER=h264_nvenc`) stands in for an AMD or Intel host's encoder at realistic speed. RTX 3060 Ti, one 1920x1080 screen at 240 Hz.

**The latency clock's window** (`latency-probe --screen`, 1088x184, so a read-back here is a tenth of 1080p's), old path against new, before mapped frames:

| Run | Old | New |
|---|---|---|
| 100 fps | 80.2 fps, clock to present p50 11.7 / p95 14.1 ms | **100.0 fps**, p50 11.5 / p95 15.1 ms |
| 60 fps, the clock at 61 (a game a little faster than the stream) | p50 14.2 / p95 22.3 ms | **p50 6.9 / p95 10.8 ms** |
| 120 fps | 119.2 fps, p95 14.3 ms | 119.9 fps, p95 13.2 ms |
| 100 fps, a GPU-bound game (`--gpu-load 130`) | 83.2 fps, p50 26.6 / p95 30.9 ms; the game 106.6 fps | **99.2 fps, p50 24.1 / p95 25.4 ms**; the game 102.6 fps |

At 60 and 120 fps on a 240 Hz screen, little changes: they divide 240.

**The full 1080p screen** (`viewers.ps1 -Screen -Clock`, 100 fps), the three in one sitting:

| | Old | New, memcpy | **New, mapped frames** |
|---|---|---|---|
| 4 viewers | 85.3 fps, capture to present p50 17.3 / p95 23.5 ms | 91 fps, p50 21.0 / p95 29.5 ms | **98 fps, p50 15.4 / p95 20.2 ms** |
| 1 viewer | 80.1 fps, p50 8.4 / p95 13.7 ms | 99.8 fps, p50 8.3 / p95 10.0 ms | **99.9 fps, p50 7.0 / p95 10.9 ms** |
| read-back per frame sent (4 / 1 viewers) | — | 5.0 / 2.2 ms | **1.8 / 0.9 ms** |

**On the way:**
- Without mapped frames, four viewers on the new path were 5 ms slower than the old path at p50. The suspicion was the read-back holding the capture's device lock (M9b's finding), but the capture trace showed Windows' callback hardly waiting for it (`lock_us` p95 1 µs, max 192 µs), so no fence was added. Timing the read-back split it into the GPU copy with Map's wait (1.9 ms) and the memcpy (2.9 ms), which mapped frames removed.
- The GPU-bound game ran about 4% slower on the new path (102.6 fps against 106.6), within what one run of each can tell; worth watching with a real game.
- In 3 of 6 runs with four viewers on the new path, one or two stream windows asked for one keyframe (counted as "decode errors"); never on the old path (2 runs), the GPU path (1) or with one viewer. Likely the four decoders on this PC pushed harder at 98 fps than at 85; it never repeated within a run. The review found no cause on the host (sizes, strides and frames are the old path's, and the switch to memory reopens no encoder), and the stream window also asks for a keyframe when its decoding falls behind ("skipping to a keyframe"), which `couch-cli view` counts as a decode error.

**Rate-control gates** (`gates --quick`): all pass on the GPU path and on the new memory path (G1 p50 12.2 / p95 13.2 ms, G3 p95 79.5 ms, G4 back in 48 ms, G5 settled in 0.34 s, G7 the second viewer unharmed).

## Code review

A review found no blocking defect: no way to read a mapped frame after it is unmapped, map a staging texture twice or copy into one while mapped; the device's multithread protection covers every `Unmap`, from any thread; a pool replaced, a device lost (three read-backs fail, the capture goes to memory) or the process ending is safe; no GPU frame reaches an encoder that can't take it. Fixed from it:
- **A staging texture that couldn't be made failed the read-back**, and three such frames would have sent the whole stream back to the old path while the memcpy worked. Now no more are made, and frames go by memcpy.
- **The staging textures had no memory cap:** 16 at 4K would be 530 MB. Now 256 MB at most (8 at 4K), documented as kept while their pool lives.
- **The switches were read from the environment on every frame:** read once.
- **Whether the read-back's blocking Map holds Windows' capture callback under a GPU-bound game** (M9f's buffer starvation), measured again with `--gpu-load 130` at 100 fps: the callback waited on the device's lock at most 2 µs on the new path (p99 0), so nothing more is needed. That run: the stream 93.7 and 93.0 fps on the new path, 86.7 on the old, 96.5 on the GPU path; the game 96.0 and 95.9 fps, 97.4 on the old (1.5% lower; 4% in the first pair), 100.2 on the GPU path; clock to present p50 27.6 to 27.7 against 25.7 ms, p95 31.8 to 32.0 against 32.4 (the first pair had the new path's p50 lower: within two runs' noise).
- A scenario with a slow read-back (8 ms each, a GPU busy with a game): the stream keeps its rate. Stale comments and a test's comment that said the GPU frame doesn't keep its staging mapped (it does: it caches its copy in memory).

## Your run (2026-10-20, this PC)

The app (`npm run tauri dev`) with `COUCH_HOST_ENCODER=h264_nvenc`, a window streamed at 120 fps and watched on this PC: the log said the capture ran on the GPU every 4 ms, paced; `h264_nvenc` took frames in memory; "each frame sent is read back to memory after pacing". The stream looked right at about 100 to 120 fps.

## Tests

- `couch-engine`: `a_slow_read_back_keeps_the_streams_rate`, `encoders_that_cant_read_gpu_frames_get_paced_frames_read_back` (a 240 Hz capture into a 30 fps stream, two viewers: about 30 read-backs a second, not 240 nor 60; the capture never starts again in memory), `failing_read_backs_turn_the_capture_to_memory`, and `a_failing_gpu_encoder_falls_back_to_frames_in_memory` for both paths. All 58 session scenarios, three runs in a row, and with `COUCH_HOST_MEMORY_PACING=0`.
- `couch-platform-win` (needs a D3D11 device, `--ignored`): `frames_read_back_mapped_give_their_staging_back`: the pixels right, the staging texture out of its set while any copy of the frame lives and back after, memcpy once all 16 are out.
- `cargo test --workspace`, clippy, fmt.

## Not in this part

- GPU frames into AMF and Quick Sync (and Quick Sync's NV12 conversion): they need that hardware.
- **Still to see on a friend's AMD or Intel host:** its log names its encoder with "frames in memory", then "read back to memory after pacing", and the read-back's times at the stream's end.
