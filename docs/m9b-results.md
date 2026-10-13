# M9b results: zero-copy on the host

Step 2 of M9 ([research/sunshine-moonlight.md](research/sunshine-moonlight.md), item 1). Before M9b, every captured frame made a round trip through the CPU on the host:
1. the capture copied it to memory (about 8 MB at 1080p), waiting on the GPU to do so;
2. each viewer's encoder copied it into its own frame;
3. FFmpeg uploaded it back to the GPU, **once per viewer**.

Now it stays on the GPU from capture to NVENC.

**What could be tested here:** NVIDIA only (RTX 3060 Ti; the other PC is NVIDIA too).

## What it does

**Frames that stay on the GPU** (`couch_media`)
- **`VideoFrame`** is `Cpu(BgraFrame)` or `Gpu(GpuFrame)`; captures and viewers' mailboxes carry it.
- **A `GpuFrame`** points at a texture through the `GpuTexture` trait (raw D3D11 pointers, so `couch-media` stays free of OS types).
- **`GpuFrame::cpu()`** copies it to memory at most once, however many encoders ask.

**The capture's texture pool** (`couch_platform_win::gpu_frames`)
- **The copy.** Windows.Graphics.Capture reuses its one texture as soon as the frame callback returns, so each frame is copied, GPU to GPU and cropped to even size, into a texture of a pool on the capture's own D3D11 device.
- **Reuse.** A texture is free again once no frame points at it; FFmpeg holds a frame until NVENC has read it. Up to 12 textures; with none free, the frame is dropped, as a full mailbox would drop it.
- **The pool's lifetime.** It is replaced only for a new device or size, under a new `source` id. NVENC remembers each texture it registers by address, so two rules apply:
  - an encoder reopens on a new `source`, so it never mistakes a new texture at a freed one's address for the old one;
  - an encoder keeps every texture of its pool alive (`GpuTexture::keep_alive`) until it closes. Otherwise the capture moving on to a new pool (a resize, a window swap) would destroy textures an open NVENC session still has registered.
- **Too small to crop:** a frame under 2 pixels wide or high (a window mid-resize) is skipped.
- **Multithread protection.** The device is made multithread-protected, since the capture thread copies on it while encoder threads run NVENC on it. FFmpeg does that only for devices it creates itself.
- **If anything fails,** the capture logs it and copies frames to memory from then on.

**NVENC on the capture's device** (`couch-codec-ffmpeg`, `encoder.rs`, `d3d11.rs`)
- **`FfmpegEncoder::open_gpu`:**
  - an FFmpeg D3D11VA device context on the capture's device (we hand FFmpeg a reference of its own, which it releases);
  - a frames context with BGRA inside;
  - the same option sets as before (M9a).
- **Each frame** is wrapped as an `AV_PIX_FMT_D3D11` frame whose buffer holds the `GpuFrame` until FFmpeg frees it. A frame from another pool is refused: the encoder must be reopened for it.
- **No conversion shader:** NVENC converts BGRA to YUV itself, exactly as it does for BGRA from memory. The test below found the pictures bit-identical.
- **The structs.** FFmpeg's two D3D11 structs aren't in our generated bindings, so `d3d11.rs` mirrors their layout from FFmpeg 8.1's header, with a size test.
- **Only NVENC takes GPU frames.** AMD and Intel keep frames from memory until someone can test them; Intel's Quick Sync would also need an NV12 conversion shader.

**The pipeline** (`couch_engine::pipeline`)
- **`Backend::encoder(config, gpu)`.** Given a GPU frame, the Windows backend tries NVENC on its device. Where that fails (not NVIDIA, or a laptop capturing on its integrated GPU), it logs why and opens an encoder for frames in memory, as before.
- **The encode loop:**
  - reopens an encoder reading GPU frames when the frame's pool changes;
  - gives every other encoder the frame copied to memory.
- **Frames in memory when nothing reads them on the GPU.** The stream switches to frames in memory for the rest of the session, and the capture is told to deliver them so (`Capturer::frames_in_memory`), in two cases:
  - an encoder opened for a GPU frame can't take it (AMD, Intel, or a laptop capturing on its integrated GPU). This is the path from before M9b, without a copy back from the GPU, which would hold the device's lock while the capture waits;
  - an encoder reading GPU frames fails. That viewer's encoder reopens with a keyframe instead of the viewer being dropped.
- **The host's log line** says which path: `encoding 1920x1080 with h264_nvenc (options: p1+ull, GPU frames)`.
- **`COUCH_HOST_ZERO_COPY=0`** sends frames through memory as before M9b, for measuring.

## Measurements (this PC, release build)

**NVENC from a texture against NVENC from memory** (`tests/gpu_input.rs`, 1280x720):
- the decoded pictures are **identical** (PSNR infinite), with the barcode read in 30 of 30 frames;
- 300 frames from a reused pool, with the pool replaced halfway (and the encoder reopened): every frame encoded, and a texture was always free.

**Several viewers** (`viewers.ps1 -Screen`: the host captures the primary screen at 60 fps; host, viewers and their windows all on this PC):

| | GPU frames | Through memory (`-CopyThroughMemory`) |
|---|---|---|
| **4 viewers** | 59.6 fps; capture to present p50 **10.8**, p95 15.6 ms; 18.7 Mbit/s each; slowest encode 9 to 10 ms | 57.4 fps; p50 **23.1**, p95 30.4 ms; 17.3 Mbit/s; slowest encode 16 to 18 ms |
| **7 viewers** | **58 fps**; p50 **14.4**, p95 20.6 ms; 18.6 Mbit/s each; slowest encode 11 to 13 ms | **40 fps**; p50 **70**, p95 84 ms; 12 Mbit/s each; slowest encode 30 to 34 ms |

M5a's 7-viewer run (the test pattern, made in memory) reached about 36 fps. Seven viewers now get nearly full frame rate at a fifth of the latency.

**One viewer, G1** (`latency-probe --secs 30 --relay none`):

| | GPU frames | Through memory |
|---|---|---|
| 60 fps, two runs each: clock to present p50 / p95 / max | 13.4 to 13.5 / 14.2 to 14.5 / 25 to 31 ms | 13.6 / 15.0 to 15.3 / 138 to 159 ms |
| 120 fps: p50 / p95 / max | 12.4 / 13.3 / 23 ms | 11.8 / 13.2 / 152 ms |

- **The worst frame is gone.** The ~150 ms maximum M9a saw at every frame-latency setting was the memory path's start; with GPU frames the worst is under 31 ms.
- **The median doesn't move with one viewer.** On one PC with one viewer, the copies were never the bottleneck.

**Gates** (`gates --quick`, after all changes): all pass.

| Gate | Now | M9a |
|---|---|---|
| G1 median / p95 | 13.4 / 14.4 ms | 13.6 / 14.8 ms |
| G3 p95 under 1% loss | 82.6 ms, drift +0.5 ms, 19.5 Mbit/s, no catch-ups | 83.3 ms |
| G4 after a 2 s blackout | 112 ms, exactly 1 keyframe | 88 ms, 1 keyframe |
| G5 throttle to 5 Mbit/s | settled in 0.35 s, latency back in 1.05 s | 0.33 s, 1.0 s |
| G7 the unthrottled viewer | p95 14.5 ms (unimpaired 14.6), 20.0 Mbit/s | p95 14.6 ms, 20.0 Mbit/s |

G4's 112 ms is within its 1 s limit and the run-to-run spread M1 to M5a saw (85 to 120 ms); G4 runs the test pattern, which doesn't use the GPU path.

**`lifecycle-check`:** all six pass. The game swapping its window is followed after 16 ms; that's a new capture device and pool, so the encoder reopens.

**`couch-cli probe`:** "GPU frames (M9b): h264_nvenc encodes captured frames on the GPU".

**A start-up note, not a regression.** The first run of a batch can show "decode errors 1": the stream window, still starting its decoder, skips to the next keyframe (`decoding fell behind ... seq=1`). alice's `viewer.log` had the same line on 2026-10-07, before M9b. GPU frames make the host's first frames arrive about 150 ms sooner, so they meet a stream window that isn't ready yet more often. That costs one keyframe request at start; it belongs to the viewer side (M9c).

## Checks

| Check | Result |
|---|---|
| `couch-media`: a GPU frame is copied once for every encoder; frames in memory are cropped and have no pool | pass |
| `gpu_frames` (needs a D3D11 device, `--ignored`): frames come back intact, textures are reused, a new pool has a new id | pass |
| `d3d11.rs`: the mirrored structs have FFmpeg's sizes | pass |
| `gpu_input.rs` (`--features gpu-tests`): above | pass |
| Session scenarios (3 runs, 35 each), with three new ones: GPU frames reach the encoders and a new pool reopens them; encoders that can't read GPU frames turn the capture to memory; a failing GPU encoder falls back with a keyframe | pass |
| `gpu_input.rs`: an encoder refuses a frame from another pool | pass |
| One-off: in one full workspace run, `a_host_leaving_the_room_ends_its_stream` failed at the start, when B asked to watch, before any video (its fake capture is on the memory path). It passed in five more runs; recorded in case it recurs | watch |
| Workspace: all tests, clippy (also with `gpu-tests`), fmt | pass |
| Measurements, gates, `lifecycle-check`, probe | above |
| Your run | pass: a game streamed from the default profile to alice. The log shows `(options: p1+ull, GPU frames)`; the picture looks the same either way. The stream window's stats: capture to present 6.2 ms through memory, **3.3 ms** with GPU frames; arrive to decoded 1.8 and 2.0 ms (the viewer's side, unchanged within noise) |

## Code review

A subagent read the whole change; each finding was checked against the code before fixing. It found no confirmed bug. It confirmed:
- the mirrored structs' layout;
- the device reference handed to FFmpeg and released once, failed init included;
- every failure path of the frame wrapping;
- the pool's free test, the crop and the staging readback;
- the encode loop's reopen and fallback logic.

Fixed from it:
- **Textures destroyed while still registered.** When the capture moved to a new pool, idle textures of the old one were destroyed while an encoder that had registered them could still be open (until its next frame). Encoders now keep their pool's textures alive until they close (`keep_alive`; the codec context, which unregisters them, drops first).
- **The pool rule is enforced by the encoder too.** `wrap` refuses a frame from another pool, and a texture without a device is refused before FFmpeg could dereference it.
- **Hosts whose encoder can't read GPU frames went through the GPU anyway.** Every frame was copied into the pool, then read back while holding the device's lock. The capture now switches to memory as soon as the first encoder can't take GPU frames (above).
- **A window a pixel wide turned off the GPU path** for the rest of the capture; such frames are now skipped.
- **A session test depended on timing** (download counts against frame counts since C joined). It now checks the switch to memory instead.

**After the fixes**, `viewers.ps1 -Screen -Viewers 7` again: 58 fps each, p50 13.6 to 14.5 ms, p95 18.7 to 20.6 ms, slowest encode 11 to 13 ms. The same as before the fixes.

**Left as it is, to watch for:**
- NVENC runs on the capture's own device, shared with the capture thread under multithread protection. OBS and Sunshine use a separate device and share textures under a keyed mutex. Seven viewers showed no problem; if driver errors or GPU resets ever appear with several viewers, suspect this first.
- A CPU-input encoder never retries GPU frames within a stream.

## Your run

A normal session with a game: the default profile goes live, and alice watches and plays.
- The picture and colours should look exactly as before.
- `npm run tauri dev`'s terminal shows `(options: p1+ull, GPU frames)`.
- If you like, the same with `set COUCH_HOST_ZERO_COPY=0` before `npm run tauri dev`, to compare.
- Task Manager's GPU tab shows less "Copy" work while streaming.

**Later, with friends:** an AMD or Intel host's log says its encoder takes frames in memory (expected); a laptop with two GPUs may say the same.
