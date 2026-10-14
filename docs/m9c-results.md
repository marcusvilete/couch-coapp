# M9c results: zero-copy on the viewer

Step 3 of M9 ([research/sunshine-moonlight.md](research/sunshine-moonlight.md), item 5). Before M9c, every decoded frame made a round trip through the CPU on the viewer:
1. D3D11VA decoded it on the GPU;
2. FFmpeg copied it to memory, waiting for the GPU (about 3 MB at 1080p);
3. the decoder copied it again into packed planes;
4. the render thread uploaded both planes to wgpu again.

Now it stays on the GPU from the decoder to the screen. M9c also stops the stream window from showing, or skipping over, the frames that queue while it starts.

**What could be tested here:** NVIDIA only (RTX 3060 Ti; the other PC is NVIDIA too).

## What it does

**A different route from the research's.** The research suggested decoding with D3D11VA into textures shared, through NT handles and a shared fence, with wgpu's D3D12 or Vulkan device. Our FFmpeg 8.1 build also has a **D3D12VA** decoder, which decodes on a D3D12 device we give it. So it decodes on **wgpu's own device**: no texture is shared between APIs or devices.

**Decoding** (`couch-codec-ffmpeg`, `d3d12.rs`, `decoder.rs`)
- **`FfmpegDecoder::new_d3d12(device)`:** a D3D12VA device context on the given `ID3D12Device`, holding a reference of its own.
  - Unlike D3D11VA's (M9b), FFmpeg's D3D12VA context releases the device only if FFmpeg created it. So ours installs the context's `free` callback, which releases it when the last reference goes.
  - The spike test checks that the device's reference count is back where it started.
- **Its frames** are `DecodedFrame::Gpu`. Each holds FFmpeg's frame, so FFmpeg won't decode into that picture again while we have it. Through `couch_media::DecodedSurface` it gives the `ID3D12Resource` (NV12, the frame's size) and the fence FFmpeg signals when the picture is decoded.
- **The structs.** FFmpeg's D3D12 structs aren't in our generated bindings, so `d3d12.rs` mirrors their layout from FFmpeg 8.1's header, with size tests.
- **If the hardware decoder can't start** (FFmpeg would fall back to software at the first keyframe), `decode` says so. The stream window then reopens the decoder as D3D11VA with copies and asks for a keyframe.
- **The same happens** after three decode errors in a row on D3D12, and when the render thread can't bring a frame in. In that case it keeps showing the last frame until the keyframe.

**The stream window** (`couch-viewer`: `gpu.rs`, `zero_copy.rs`, `viewer.rs`)
- **wgpu runs on DX12** (Vulkan before, on NVIDIA), with NV12 textures, when decoded frames can stay on the GPU.
- **Bringing a frame in** (`FrameImporter`), in a submit of its own before the draw:
  1. the resource is wrapped as a wgpu texture, in the COMMON state FFmpeg leaves it in (`PRESENT` to wgpu's DX12 backend);
  2. the submit waits, on the GPU, for FFmpeg's fence (`add_wait_fence`);
  3. its two planes are copied into the textures the video pass draws, so the shader and redraws are unchanged;
  4. the texture goes back to COMMON (`transition_resources`), where FFmpeg's next use expects it.

  The frame is kept until wgpu reports that submit finished, then given back. A new wgpu texture is made per frame, so wgpu's view of its state never disagrees with what FFmpeg did since.
- **One GPU-to-GPU copy, not none.** Sampling FFmpeg's texture directly (Moonlight's "bind mode") would save only that copy (3 MB of GPU memory, a few microseconds), at the cost of holding frames across redraws.
- **The latency barcode.** It is read from the frame's top-left corner (the locator's search area plus one barcode, about 1100x64 pixels), copied into a small buffer in the same submit and read back without waiting. The frame's timing goes out once that strip is read. Timings are used only for measuring; the app ignores them.
- **`couch-viewer` now uses `windows`,** for Direct3D 12 interface types only: it wraps COM pointers for wgpu-hal, and reads a resource's description.

**Choosing the path** (`COUCH_VIEWER_ZERO_COPY`, this change's switch for measuring)
- **Unset:** on for an NVIDIA GPU. AMD and Intel keep the old path until someone can test them; the log says why.
- **`0`:** the old path exactly: wgpu's default backend (Vulkan here), D3D11VA, copies.
- **`1`:** on for any GPU.
- **`WGPU_BACKEND`** (for example `dx12`) now picks wgpu's backend, so DX12's presentation can be measured apart from the copies.
- **`viewer.log` says which path ran, and why,** even without `RUST_LOG`: the stream window now logs its own info lines by default, as the desktop app does since M9a. Before, it logged only errors, so a friend's AMD or Intel PC wouldn't have said what it fell back to.
- **Telling which path ran:**
  - `viewer.log` says which path and why;
  - the stats panel's decoder line says `D3D12VA (GPU frames)`, `D3D11VA (copied)` or `software`;
  - its GPU line names the backend.

**Starting** (`viewer.rs`)
- **The decoder starts once wgpu's device exists,** since it decodes on it. Frames that arrive meanwhile wait in the stream window's queue: about half a second at the start.
- **Those frames no longer count as "falling behind".** A frame is stale only for time spent waiting after the decoder returned its first picture (`is_stale`, with unit tests). Before, the start-up wait itself tripped the 200 ms rule ("decode errors 1" at the start of many runs, M9b's results).
- **Frames behind a newer one aren't shown.** Every queued frame is decoded, since later frames refer to it, but one is shown only if no newer frame is already waiting. Otherwise the first frames shown would be up to 0.6 s old: the first runs measured a maximum of 115 to 628 ms on both paths, which this brought back to 27 to 50 ms. Such frames count in the stats panel's "skipped".

**Note on "arrive → decoded":** with D3D12VA it measures when decoding was *submitted*; the GPU work finishes behind the render thread's wait. Compare paths by capture to present and clock to present only.

## Measurements (this PC, release build, alternating runs)

**The spike** (`cargo test -p couch-viewer zero_copy -- --ignored --nocapture`, headless wgpu on DX12):
- NVENC encoded a moving test pattern. D3D11VA (copied to memory) and D3D12VA (brought into wgpu and read back) decoded the same stream.
- 294 frames at 1280x720, then 1920x1080, with up to 4 frames held at once: **identical, bit for bit,** in both planes.
- The barcode was read from the strip in all 294, and every frame went back to FFmpeg.
- **Validation:** the D3D12 debug layer and GPU-based validation were active (checked through `ID3D12InfoQueue`) and reported nothing.
  - As controls, wrapping the frame with a wrong initial state (copy source, shader resource) wasn't reported either: D3D12 may promote COMMON to read states implicitly.
  - So validation can't vouch for one case: FFmpeg's decoder reading a picture as a reference while our copy reads it, on another queue. Both only read, and the pictures were exact. See "Left as it is".

**DX12's presentation alone** (old copy path, `WGPU_BACKEND`): DX12 presents **about 0.5 ms later** than Vulkan, in every pair. Both used Mailbox, frame latency 1.

| clock to present, p50 / p95 | Vulkan | DX12 |
|---|---|---|
| 60 fps, two runs | 13.4 / 14.1 ms both | 13.9 / 14.7 to 14.8 ms |
| 120 fps | 12.0 / 16.1 ms | 12.5 / 16.8 ms |

**One viewer, G1** (`latency-probe --secs 30 --relay none`; the clock window is small, 1088x184, so the copies saved are small too):

| clock to present p50 / p95 | Zero-copy | Copies (`COUCH_VIEWER_ZERO_COPY=0`) |
|---|---|---|
| 60 fps | **12.4 to 12.5** / 13.5 to 13.7 ms (6 runs) | 13.4 / 14.1 ms (3 runs) |
| 120 fps | **11.3** / 11.9 ms | 12.3 / 13.2 ms |
| capture to present p50, 60 fps | 0.7 to 0.8 ms | 1.6 to 1.7 ms |

About 1 ms better at the median, net of DX12's 0.5 ms. After the start-up change, the maximum was 41 to 50 ms with zero-copy and 27 ms with copies.

**Several viewers** (`viewers.ps1 -Screen`: the host captures the primary screen at 1080p60; host, viewers and windows all on this PC; two runs each way for 7 viewers):

| | Zero-copy | Copies (`-CopyFrames`) |
|---|---|---|
| **7 viewers**, capture to present p50 / p95 | **5.4 to 6.8 / 9.0 to 9.3 ms**, 59.3 to 59.9 fps | 13.4 to 15.0 / 21.8 to 26.5 ms, 57.9 to 60 fps |
| 7 viewers, stream windows' CPU | **36 to 37%** of one core | 272 to 277% |
| **4 viewers**, p50 / p95 | **4.5 to 5.5 / 7.6 to 7.8 ms**, 23% CPU | 10.1 to 11.0 / 15.0 to 16.7 ms, 145% CPU |
| host's slowest encode, 7 viewers | 10 to 11 ms | 11 to 13 ms |

- **Seven viewers wait less than half as long, and their windows use a seventh of the CPU.** M9b's 7-viewer result (p50 about 14 ms) was mostly the viewers' copies.
- **The one-viewer runs aren't comparable** (36 against 50 fps): with one small window the screen barely changes, so the frame rate follows the content.
- **"decode errors 1" on the copy path with 7 viewers** (3 of 7 windows): the window's log says "decoding fell behind" about 230 ms after its decoder started. Working through the frames queued at the start took over 200 ms at 270% CPU. That is the rule doing its job; the zero-copy path had none.

**Gates** (`gates --quick`, after all changes): all pass.

| Gate | Now | M9b |
|---|---|---|
| G1 median / p95 | 13.0 / 17.1 ms; repeated: 12.3 to 12.4 / 13.4 to 16.1 ms (5 runs; copies 13.3 to 13.4 / 14.1 to 14.2) | 13.4 / 14.4 ms |
| G3 p95 under 1% loss | 80.0 ms, drift +0.0 ms, 19.5 Mbit/s, no catch-ups | 82.6 ms |
| G4 after a 2 s blackout | 51 ms, exactly 1 keyframe | 112 ms, 1 keyframe |
| G5 throttle to 5 Mbit/s | settled in 0.35 s, latency back in 0.94 s | 0.35 s, 1.05 s |
| G7 the unthrottled viewer | p95 14.3 ms (unimpaired 14.3), 20.0 Mbit/s | p95 14.5 ms, 20.0 Mbit/s |

**G1's p95 is less steady with zero-copy:** 16.1 and 17.1 ms in 2 of 12 runs, 13.4 to 13.8 ms in the other 10. Copies stayed at 14.1 to 14.2 ms in 4 runs. Six more zero-copy runs, three with per-frame traces, didn't reproduce it. Recorded to watch (below).

**`lifecycle-check`:** all six pass. The stream window killed was restarted after 38 ms; the game swapping its window was followed after 23 ms.

## Checks

| Check | Result |
|---|---|
| `d3d12.rs`: the mirrored structs have FFmpeg's sizes | pass |
| The stale rule: waiting behind start-up isn't stale; falling behind after it still is | pass |
| The barcode locator: every frame once found, every 30th while searching | pass |
| The spike (above), by hand | pass |
| Workspace: all tests, clippy (also with `gpu-tests`), fmt, `cargo deny` | pass |
| Measurements, gates, `lifecycle-check` | above |
| Your run | pass: a game streamed from the default profile to alice. The picture and colours look the same both ways, and the stream window used 1 to 2% CPU. With the development build, capture to present was 0.5 to 1 ms *higher* with zero-copy: see below |

**Why your run showed zero-copy slower, and the fix.** `npm run tauri dev` runs debug builds, and wgpu turns its validation on in debug builds.
- **On DX12 that is the D3D12 debug layer,** installed on this PC, which checks every GPU call on the CPU.
- **On Vulkan,** the copy path, nothing ran: validation needs the Vulkan SDK's layers.
- **Measured** with the debug build at 60 fps, clock to present p50 / p95:
  - zero-copy 13.9 / 15.5 ms;
  - copies 14.3 / 18.2 ms;
  - zero-copy without validation **13.3 / 14.7 ms**.

  The stream window's stats panel makes many more GPU calls than the probe's hidden HUD, so in the app the layer cost more.
- **The fix:** the viewer now leaves validation off in debug builds too (`WGPU_VALIDATION=1` turns it on). The development app's latency is then the installed app's. After the fix, the debug build's zero-copy measured 13.4 / 14.6 ms with no setting.

## Code review

A subagent read the whole change against FFmpeg 8.1.3's source and wgpu 30.0.1's. Each finding was checked against the code before fixing.

**It confirmed:**
- the mirrored structs' layout;
- the fence value read while holding the frame (stable until the frame goes back);
- no frame going back to FFmpeg before the GPU finished with it;
- the staged fence wait being taken by the copy's own submit;
- the resource-state handling and the COM reference counts;
- the strip slots, the read-ahead loop, the stale rule, and the barcode reading on the old path, unchanged frame for frame.

**Fixed from it:**
- **The decoder leaked a reference to wgpu's D3D12 device,** one per decoder, also when one failed to start.
  - FFmpeg's D3D12VA context, unlike D3D11VA's, releases a device only if FFmpeg created it, and our docs claimed otherwise.
  - The context now gets a `free` callback that releases it. The spike test checks the device's reference count before and after a decoder (35 and 35; 36 with the callback removed, as a control).
- **An FFmpeg built without D3D12VA would have panicked the decode thread** (rsmpeg's `alloc` unwraps). It now falls back to D3D11VA.
- **A frame the render thread couldn't bring in** only logged a warning on every frame, drew stale or blank planes, and never left zero-copy. Now the stream window switches to D3D11VA and copies at the next keyframe, and shows the last good frame until then. The same happens after three D3D12 decode errors in a row.
- **The barcode locator,** while searching on the GPU path, asked for up to three readbacks in a row. It now counts the frame when it asks; the old path's behaviour is unchanged, with a test.
- **A frame copied through memory** could be given a stale GPU frame's barcode strip, after a fallback mid-stream.
- **`viewers.ps1`** cleared `COUCH_VIEWER_ZERO_COPY` (and `COUCH_HOST_ZERO_COPY`) from the caller's shell, so `=1` couldn't be used with it. It now sets them only when asked, and restores them.
- **Comments and docs** that didn't match the code (the fence's owner, which submit copies the frame), and two warnings on platforms other than Windows.

**After the fixes**, measured again on a release build:
- **`viewers.ps1 -Screen -Viewers 7`:** zero-copy at 59.4 to 59.8 fps, p50 5.4 to 6.5 ms, p95 8.8 to 8.9 ms, the windows at 32% of one core. Copies: p50 13.2 to 14.1 ms, p95 17.9 to 19.6 ms, 270%.
- **`latency-probe` at 60 fps:** clock to present 12.2 / 13.0 ms (max 24.9) against 13.4 / 14.2 ms.

The same as before the fixes.

## Left as it is, to watch for

- **AMD and Intel stream windows keep the old path** until someone can test zero-copy there (`COUCH_VIEWER_ZERO_COPY=1`).
- **Two queues read one picture.** While our copy reads a decoded picture on wgpu's queue, FFmpeg may read it as a reference for a later frame on its video queue. Both only read, the pictures were exact, and validation reports nothing (it can't tell). If artefacts ever show only with zero-copy, suspect this first, and try `COUCH_VIEWER_ZERO_COPY=0`.
- **DX12 presents about 0.5 ms later than Vulkan** on this PC. Zero-copy more than makes up for it. Importing the decoder's frames into Vulkan instead would avoid it, at the cost of sharing textures across APIs.
- **G1's p95 rose to 16 to 17 ms in 2 of 12 zero-copy runs.**
- **The stream window starts decoding only after its GPU is set up,** about half a second after it opens. Those frames are decoded and not shown, so it costs no keyframe.

## Your run

A normal session with a game: the default profile goes live, and alice watches.
- The stream window's stats panel says `decoder D3D12VA (GPU frames)`, and its GPU line ends in `(Dx12)`.
- The picture and colours look exactly as before. Also try resizing the window, full screen, minimizing and restoring, and a game changing resolution.
- To compare: set `COUCH_VIEWER_ZERO_COPY=0` for the **watcher's** app, which opens the stream window. For alice: `$env:COUCH_VIEWER_ZERO_COPY = '0'; .\scripts\profiles.ps1 alice` (setting it before `npm run tauri dev` only reaches the default profile). The stats then say `D3D11VA (copied)` and `(Vulkan)`. Compare capture to present, and Task Manager's CPU for `couch-viewer.exe`.

**Later, with friends:** an AMD or Intel PC's `viewer.log` says decoded frames go through memory, and why.
