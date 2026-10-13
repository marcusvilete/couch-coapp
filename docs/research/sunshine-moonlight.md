# What Sunshine and Moonlight do, and what we could learn (before M9)

Read on 2026-10-13, before planning M9 (performance). Both projects stream games at low latency on the same Windows APIs we use, and have been tuned for years. This document records what they do in M9's areas, where in their code, how it maps to ours, and what it might be worth. It changes nothing in the app.

**What was read:**
- **[Sunshine](https://github.com/LizardByte/Sunshine)** (the host), commit `7c124b43926f22b272376e564f0b5f10d3371080` (2026-10-13).
- **[Moonlight](https://github.com/moonlight-stream/moonlight-qt)** (the viewer, moonlight-qt), commit `71c3191e0c5be46fd775549cbc014fe5bb0d068a` (2026-10-12).

Both are **GPL-3.0**: their LICENSE files are GPL version 3, and their source files carry no other licence headers. We may reuse their code under our GPL-3.0-only licence, never silently (CLAUDE.md, Licensing):
- keep the copyright notice;
- name the project, file and commit where it's used;
- add it to the third-party notices.

Paths below are relative to each repository at those commits.

## Our pipeline today: four trips between GPU and CPU per frame

| Step | What happens | Where |
|---|---|---|
| Capture | Windows.Graphics.Capture gives a GPU texture; we copy it to CPU memory (BGRA) | `couch_platform_win::capture` (`capture.rs`, `Bytes::copy_from_slice`) |
| Encode | FFmpeg uploads the BGRA frame back to the GPU for NVENC (or AMF, Quick Sync, Media Foundation), converting to YUV on the way | `couch-codec-ffmpeg` `encoder.rs` |
| Decode | D3D11VA decodes on the GPU; we copy the NV12 result to CPU memory | `decoder.rs` (`hwframe_transfer_data`) |
| Render | wgpu uploads the Y and UV planes again and converts to RGB in a shader | `couch-viewer` `gpu.rs` (`write_texture`) |

At 1080p, each trip moves about 3 to 8 MB per frame, per viewer on the host. M1's 1080p60 latency (median about 14 ms on one PC) already includes them, so they cost milliseconds, not tens of milliseconds. Their bigger cost is GPU and memory bandwidth on a host encoding for several viewers: seven viewers on this PC fell to about 36 fps (M5a).

## Findings

### 1. Capture to encoder without the CPU (Sunshine)

**What it does.**
- **The conversion runs on the GPU.** The captured BGRA texture is converted to the encoder's format (NV12, or P010 for 10-bit) by two small pixel-shader passes: one draws the Y plane, the other the UV plane at half size. The shaders are HLSL; colour matrices and the HDR variants are selected per format.
- **Capture and encoder share the texture.** It's shared between the capture device and the encoder's D3D11 device, guarded by a keyed mutex, so the two never touch it at the same time.
- **For FFmpeg's encoders (AMD AMF, Intel Quick Sync):**
  1. FFmpeg's D3D11VA hardware device is created on Sunshine's own D3D11 device.
  2. FFmpeg's hardware frame pool is set to one texture with `D3D11_BIND_RENDER_TARGET`.
  3. The conversion shaders draw straight into that frame's texture, which is then passed to `avcodec_send_frame` as `AV_PIX_FMT_D3D11`.

  Quick Sync needs the device to be multithread-protected, and its frames are reached through `av_hwframe_map`.
- **For NVIDIA:** Sunshine bypasses FFmpeg and drives the NVENC SDK directly, registering the D3D11 texture as NVENC's input (`NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX`). A CUDA interop path is used only for 10-bit 4:4:4.

**Where:**
- `src/platform/windows/display_vram.cpp`: `d3d_base_encode_device::convert`, `d3d_avcodec_encode_device_t` (`init_hwframes`, `set_frame`, `prepare_to_derive_context`), `d3d_nvenc_encode_device_t`;
- `src/nvenc/nvenc_d3d11_native.cpp`;
- the shaders under `src_assets/windows/assets/shaders/directx/`.

**For us:**
- **This is M9's main host item, and Sunshine shows the shape:** keep the capture texture on the GPU; convert BGRA to NV12 with a shader on the encoder's device; hand FFmpeg a D3D11 frame.
- **We can stay on FFmpeg for NVENC too.** FFmpeg's `h264_nvenc` also accepts `AV_PIX_FMT_D3D11` frames, so one code path could serve all three vendors. Sunshine's direct NVENC path buys features we don't use yet (item 2).
- **The work:**
  - the capture crate (`windows-capture`) must hand us the texture, not a CPU copy;
  - capture and encode must share a device, or a shared texture;
  - a conversion shader, written in our own code, or a translated Sunshine shader with credit.
- **Per-viewer encoders fit:** convert once per captured frame, then each viewer's encoder reads the same NV12 texture.
- **Gain:** fewer bytes moved per frame, and much less GPU and memory load with several viewers. Probably 1 to 3 ms off encode time per frame at 1080p60, and more viewers per host. Measure with `viewers.ps1` before and after.

### 2. Encoder settings

| Setting | Sunshine | Ours (`encoder.rs` `low_latency_options`) |
|---|---|---|
| NVENC tuning | Preset by quality setting, `NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY`, CBR, `zeroReorderDelay`, no lookahead, `lowDelayKeyFrameScale = 1`, infinite GOP, P-frames only | p1, ull, CBR, zerolatency, no lookahead, forced IDR: equivalent |
| Video buffer (VBV) | One frame's worth: bitrate ÷ frame rate (optionally plus a percentage) | One frame's worth: equivalent |
| Slices | `sliceMode 3` with the client's slices per frame | Not set |
| Intra refresh | Optional (the client asks): `intraRefreshPeriod 300`, `intraRefreshCnt 299`, with recovery-point SEI | Not used |
| **Reference frame invalidation** | When the client reports lost frames, `nvEncInvalidateRefFrames` stops NVENC referencing them: the next frame refers to an older good one, with no keyframe. It falls back to an IDR if the range is too large | Our catch-up always forces a keyframe |
| AMD AMF (H.264) | `usage ultralowlatency`, `rc vbr_latency` (default), `quality balanced`, `latency 1`, `async_depth 1`, `forced_idr 1`, `frame_skipping 0`, no filler data, `enforce_hrd` configurable; **if it fails to open, retry with `usage low_latency`** (AMF issue 410) | Only `usage ultralowlatency` and `rc cbr` |
| Intel Quick Sync (H.264) | `preset medium` (default), `async_depth 1`, `low_delay_brc 1`, `forced_idr 1`, **`low_power 1` with a retry without it** | `preset veryfast`, `async_depth 1`, `low_delay_brc 1`, `forced_idr 1`; no `low_power` |

**Where:**
- `src/nvenc/nvenc_base.cpp`: `create_encoder` (around lines 260 to 450), `invalidate_ref_frames`;
- `src/video.cpp`: the `amdvce`, `quicksync` and `nvenc` encoder tables;
- `src/config.cpp`: the defaults;
- `src/stream.cpp`: the client's invalidation request.

**For us:**
- **AMF and Quick Sync options: a cheap fix,** and worth doing even before M9. Ours were never tuned on real AMD or Intel hardware.
  - **AMF:** add `latency 1`, `async_depth 1`, `forced_idr 1` and `frame_skipping 0`, and the retry with `usage low_latency`.
  - **Quick Sync:** add `low_power 1` with a retry without it.

  Without `forced_idr`, a keyframe request may not give a real IDR, which a fresh epoch needs. These options are FFmpeg's public names, not Sunshine's code, so they need no credit beyond a pointer.
- **Reference frame invalidation could make catch-ups cheaper.** Today a catch-up resets the epoch and forces a keyframe: a large frame at the worst moment. With invalidation, the viewer reports what it lost, and the encoder continues from an older good frame.

  Our epochs make this fit less directly: a reset stream drops every frame after the reset. A design would need care. It's NVENC-only via the SDK; FFmpeg doesn't expose it, so it would need direct NVENC. **A larger M9 or later item, worth a design note.**
- **Slices and intra refresh** mainly help on lossy UDP; our QUIC streams are reliable. Low priority.

### 3. Capture method and pacing (Sunshine)

- **Desktop Duplication first.** Sunshine tries Desktop Duplication (DXGI) and falls back to Windows.Graphics.Capture. It captures whole displays, which also works for exclusive full-screen games (our known limit). We capture one window with WGC, which keeps the stream to the game.
- **Its WGC setup is close to ours:**
  - a free-threaded frame pool with 2 buffers, keeping only the latest frame;
  - the border off where supported;
  - `MinUpdateInterval(4ms)`, i.e. up to 250 Hz, so WGC doesn't cap at 60 fps on some Windows versions. **Check ours for 120 fps:** what minimum interval we set, and what WGC delivers at 120.
- **Pacing:** the capture loop sleeps to an exact target computed from the rational frame rate, in "pacing groups", so there's no drift, using a high-resolution waitable timer (`CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`).
- **Where:**
  - `src/platform/windows/display_wgc.cpp` (frame pool, `MinUpdateInterval`);
  - `display_base.cpp` (the capture loop and the choice between methods);
  - `misc.cpp` (the timer).
- **For us:**
  - **A Desktop Duplication fallback** is already on M9's list, for exclusive full screen. Sunshine is a working reference.
  - **The pacing approach** is worth comparing with ours at 120 fps.

### 4. Streaming-time system settings (Sunshine)

While a stream runs, Sunshine (`src/platform/windows/misc.cpp`, around lines 1195 to 1300; undone when streaming stops):
- `DwmEnableMMCSS(true)`: lets the desktop compositor use Windows' multimedia scheduler;
- the timer resolution goes to 0.5 ms (`NtSetTimerResolution`, falling back to `timeBeginPeriod(1)`);
- its process gets `HIGH_PRIORITY_CLASS`;
- **Wi-Fi "media streaming mode"** on every wireless adapter (`WlanSetInterface`, `wlan_intf_opcode_media_streaming_mode`). Windows then stops background scans, which cause periodic packet loss and jitter on Wi-Fi.

**For us:**
- **The Wi-Fi setting is a cheap, likely noticeable win,** on both the host and viewers, since friends are often on Wi-Fi. It's a documented Windows API, so we'd write it ourselves (`couch_platform_win`), turned on while streaming or watching and undone after.
- **MMCSS and priority** are on M9's list already ("Windows' multimedia thread priorities").
- **Raising the whole process's priority** is something to measure; a game on the same PC competes with it.

### 5. Decode to display without the CPU (Moonlight)

- **Decoding:** D3D11VA into FFmpeg's hardware frame pool, with `D3D11_BIND_SHADER_RESOURCE` added so the renderer can read the decoder's output textures directly.
- **Two ways to display:**
  - **bind:** draw straight from the decoder's texture; the default on Intel, where a copy hurt;
  - **copy:** one GPU-to-GPU copy into its own texture (`CopySubresourceRegion1`).

  Either way, the frame never visits the CPU. A pixel shader converts NV12 to RGB.
- **Separate decode and render devices** are supported, with shared NT-handle textures and shared fences between them.
- **Where:** `app/streaming/video/ffmpeg-renderers/d3d11va.cpp` (around lines 200 to 260, 720 to 750, 1040 to 1060, 1840 to 1930).
- **For us:**
  - **This is M9's main viewer item.** Our renderer is wgpu, on its own device (Direct3D 12 or Vulkan), so we're in Moonlight's "separate devices" case.
  - **The plan would be:** FFmpeg decodes into D3D11 textures created as shared (NT handle); wgpu opens them as Direct3D 12 textures; a fence orders decode before draw.
  - **That needs wgpu's backend-specific ("hal") interop, which is unsafe code.** Risky enough to deserve a spike first.
  - **Gain:** two copies per frame off the viewer, and lower GPU load on the viewer's PC, which is also often a gaming PC.

### 6. Presenting frames (Moonlight)

- **The swap chain:** a flip-model swap chain (`DXGI_SWAP_EFFECT_FLIP_DISCARD`) with tearing allowed where supported (`DXGI_PRESENT_ALLOW_TEARING`, `Present(0, …)`), and 5 buffers. With fewer, AMD GPUs starve.
- **Its comment** (`d3d11va.cpp` around line 565): *don't* call `SetMaximumFrameLatency(1)`. With non-blocking presents it makes `Present` wait on the desktop compositor, acting like V-sync.
- **Ours:** we set wgpu's `desired_maximum_frame_latency = 1` with Mailbox or Immediate presentation (`couch-viewer/src/gpu.rs`). Our measured latency is already good (G1), so this is **something to measure, not a known problem**: compare 1 against 2 or 3 with the latency probe at 60 and 120 fps.
- **Pacing:** with V-sync on, Moonlight's pacer keeps a short frame queue and drops frames when it stays above one for a while, judged over a rolling 500 ms history (`pacer/pacer.cpp`). We present at once without a queue, which suits our "latest frame" design.

### 7. Decoder flags (Moonlight)

Moonlight (`app/streaming/video/ffmpeg.cpp`, around lines 510 to 550):
- `AV_CODEC_FLAG_LOW_DELAY` and `AV_CODEC_FLAG2_SHOW_ALL`;
- `err_recognition = AV_EF_EXPLODE`, so corrupt data fails at once and it asks for an IDR;
- for hardware decoding, one thread; for software decoding, slice threads;
- `extra_hw_frames` for its pacer.

We set `LOW_DELAY` and one thread. **`AV_EF_EXPLODE`** could make our "decoder error, ask for a keyframe" path react sooner. A small item.

## What not to take

- **Their network layer** (RTP over UDP with forward error correction, ENet for control, in `moonlight-common-c`). Ours is QUIC over iroh, with our own rate controller, and it meets our gates.
- **Pairing and the client model.** Their one host and paired clients don't map to our rooms, several viewers and seat passing.
- **Sunshine's whole-display capture as the default.** We stream one window, which is better for privacy and for knowing what's streamed. Desktop Duplication fits as a fallback only.

## Suggested order for M9

1. **Cheap fixes first,** each measured with the gates:
   - **AMF and Quick Sync options,** with their retries (item 2);
   - **Wi-Fi media streaming mode** (item 4);
   - **`AV_EF_EXPLODE`** (item 7);
   - **the frame-latency setting,** measured (item 6).
2. **Zero-copy on the host,** following item 1: GPU conversion and D3D11 frames into FFmpeg. The biggest win for several viewers.
3. **Zero-copy on the viewer,** following item 5, after a spike on D3D11-to-wgpu sharing.
4. **120 fps,** once the copies are gone: check WGC's minimum update interval and pacing (item 3).
5. **Later, with a design note:**
   - reference frame invalidation (item 2), for cheaper catch-ups;
   - a Desktop Duplication fallback (item 3);
   - HEVC.

## Attribution, if code is reused

The most likely candidates are Sunshine's colour-conversion shaders, which we'd translate or adapt. Any reused file or function gets:
- a comment naming Sunshine (or Moonlight), the file and the commit above;
- the project's copyright notice;
- a section in `xtask/src/notices.rs`, so THIRD-PARTY-NOTICES.txt credits it.

Using FFmpeg's or Windows' public options and APIs, as in items 2, 4 and 7, isn't reuse of their code. It only needs a pointer to where we learned it.
