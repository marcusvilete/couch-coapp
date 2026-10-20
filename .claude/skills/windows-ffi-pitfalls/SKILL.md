---
name: windows-ffi-pitfalls
description: Checklist for writing or reviewing unsafe Windows, COM, D3D11 and SDL3 code in Couch Co-App with the `windows` 0.62 crate, `windows-core` and `sdl3` 0.20 — hidden Drop impls, packed structs, COM threading, callbacks on foreign threads, teardown order, COM references handed to FFmpeg, textures NVENC has registered, and D3D12 resources shared between FFmpeg's decoder and wgpu (states, fences). Use when adding or changing code in crates/couch-platform-win (WASAPI, Windows.Graphics.Capture and the GPU texture pool, SendInput, ViGEm, HidHide, low-level hooks, DPAPI, WLAN, process and window APIs), FFmpeg hardware contexts in couch-codec-ffmpeg (d3d11.rs, d3d12.rs, the decoder's D3D11VA and D3D12VA), couch-viewer's zero_copy.rs, any `unsafe` block that calls Windows or SDL, or an SDL callback in couch-viewer.
---

# Windows and SDL FFI pitfalls

Each item below cost time or nearly caused undefined behaviour in M1–M3. Check every one that applies before calling the code done.

## Before trusting a type, read its Drop

- **`windows` adds `Drop` impls outside the generated bindings.** `PROPVARIANT` gets one in `src/extensions/Win32/System/StructuredStorage.rs` that calls `PropVariantClear`. So a `PROPVARIANT` whose blob points at a stack variable (process-loopback activation parameters, for example) has that pointer freed by the COM allocator when it drops: heap corruption.
  - Wrap such values in `ManuallyDrop`.
  - Search the whole crate, not just the generated module: `Get-ChildItem <windows-src>\src -Recurse -Filter *.rs | Select-String "impl Drop for <Type>"`.
  - `0xc0000374 STATUS_HEAP_CORRUPTION` usually surfaces at a *later* allocation than the bad free, so trace lines mislead. Compare with a crate known to work (the `wasapi` crate for WASAPI) before guessing.
- **Wrapper crates have their own drop order.** `sdl3`'s `AudioStreamWithCallback` frees the callback's box *before* destroying the stream. An SDL audio thread calling back in between touches freed memory. So put the stop in a `Drop` of your own, which runs before the wrapper's fields drop; a "close" method that only runs on the happy path isn't enough:
  ```rust
  impl Drop for AudioOut {
      fn drop(&mut self) {
          let _ = self.stream.pause();   // SDL checks the pause flag before each callback
          drop(self.stream.lock());      // waits for a callback in progress
      }
  }
  ```
  Every `?` and panic between opening and the end of the function needs this, not just the normal exit.

## Structs and pointers

- **Packed structs.** `WAVEFORMATEX`, `WAVEFORMATEXTENSIBLE` and many Win32 structs are `packed(1)`. A reference to a field is undefined behaviour (error E0793), and `==` on a field takes one. Copy the field into a local first: `let sub_format = ext.SubFormat; sub_format == X`.
- **Buffers from the OS** (WASAPI `GetBuffer`, capture frames) are valid only until their release call. Copy them out (`to_vec`) before releasing, and release exactly what you got, before the next get.
- **Memory the API allocates** (`GetMixFormat`, `CoTaskMemAlloc`'d strings) must be freed with the matching function (`CoTaskMemFree`) after you have copied what you need.
- **Missing constants:** the `windows` crate files some GUIDs under unexpected features or modules (`KSDATAFORMAT_SUBTYPE_IEEE_FLOAT`). Defining the well-known GUID locally, with a comment naming its header, beats pulling in a feature.

## COM and threads

- **Init per thread, and balance it.** Call `CoInitializeEx(None, COINIT_MULTITHREADED)` on the thread that uses COM, and `CoUninitialize` on every exit path, after every COM object of that thread is dropped. A dedicated thread that owns its COM objects (as `couch_platform_win::audio` does) keeps this simple; report readiness or failure back over a channel.
- **`#[implement]`** expands to paths in `windows_core`, so the crate needs `windows-core` as a direct dependency (it is in the workspace). A plain `#[implement(IActivateAudioInterfaceCompletionHandler)]` object was accepted as `ActivateAudioInterfaceAsync`'s handler, which must be agile.
- **Callbacks arrive on other threads.** Keep their state `Send + Sync` (channels or atomics), and never block them for long.
- **Event handles** (`CreateEventW`) are closed in `Drop`, including on the error paths partway through setup.

## D3D11 and FFmpeg (M9b)

- **COM references handed to FFmpeg become FFmpeg's.** `AVD3D11VADeviceContext.device` is released by FFmpeg when the device context is freed, including after a failed `av_hwdevice_ctx_init`. Hand it a reference of its own (`device.clone().into_raw()`), never `as_raw()` of one you keep.
- **A device several threads use must be multithread-protected** (`ID3D11Multithread::SetMultithreadProtected(true)`). FFmpeg turns it on only for devices it creates; our capture's device (`wgc::new_device`) gets it from `TexturePool::new`. Without it, the capture thread's copies and NVENC on encoder threads race on the immediate context.
- **Windows.Graphics.Capture reuses its frame textures** once a frame is closed (`couch_platform_win::wgc` closes each after the sink's call). Copy it out (`CopySubresourceRegion`, then `Flush`) inside the call; never keep the texture.

## Windows.Graphics.Capture, our own (M9f)

- **A free-threaded frame pool calls `FrameArrived` on several of Windows' threads at once.** Take your lock *before* `TryGetNextFrame`: a call that takes a frame and then waits holds a buffer, and with every buffer held Windows can't write the next frame. Measured under Witcher 3: later and fewer frames than one buffer gave.
- **`GraphicsCaptureItem::Closed` doesn't reliably arrive** without a thread of ours pumping messages (`windows-capture` ran one; a free-threaded pool has none). `lifecycle-check` caught it: closed and swapped windows went unnoticed. The capturer checks `IsWindow` every 100 ms instead; keep that check.
- **WinRT needs the MTA** on threads that start captures: `wgc` calls `CoIncrementMTAUsage` once per process.
- **Raising our GPU priority made capture worse** (`D3DKMTSetProcessSchedulingPriorityClass` high plus `SetGPUThreadPriority(7)`, as Sunshine does). Under Witcher 3 it delayed the compositor's capture work: 107 frames a second captured at p50 17 ms without it, 89 at 34 ms with it, four alternating runs. Both calls work without administrator rights. Don't add them back without measuring under a GPU-bound game. Never the realtime class on NVIDIA (Sunshine: encoder freezes or driver crashes).
- **A minimized window is tiny** (146x28 measured), smaller than NVENC accepts: such frames are dropped in the capture (`MIN_FRAME`). Sent on, one made NVENC refuse to open, which turned GPU frames off for the whole stream.
- **NVENC caches every texture it registers by address**, and unregisters them only when it closes. A texture destroyed while the encoder is open leaves a registration on a dead resource, and its address can be reused by a new texture that NVENC then takes for the old one. Keep every texture an encoder may have seen alive until it closes (`GpuTexture::keep_alive`, the `source` rule in CLAUDE.md).
- **Reading a texture back (`Map`) holds a multithread-protected device's lock while it waits for the GPU,** blocking every other thread on that device (the capture's copies). Don't read GPU frames back per frame on a hot path; capture to memory instead when nothing reads them on the GPU.
- **FFmpeg's D3D11 and D3D12 structs aren't in our generated bindings.** Mirror them from the pinned headers (`third_party/ffmpeg/include/libavutil/hwcontext_d3d11va.h`, `hwcontext_d3d12va.h`) with a size test, and check them again after an FFmpeg bump. FFmpeg's source at the pinned commit is in `third_party/sources/` (read one file with `tar xzf <archive> -O <path>`).

## D3D12, FFmpeg and wgpu (M9c)

- **FFmpeg's D3D12VA device context does *not* release a device you give it** (unlike D3D11VA): only `av_hwdevice_ctx_create`, where FFmpeg makes the device, installs `d3d12va_device_free`. Set `AVHWDeviceContext::free` yourself before `av_hwdevice_ctx_init` (`couch_codec_ffmpeg::d3d12::device_context`); FFmpeg calls it once, on the last unref, also after a failed init. Check such ownership in FFmpeg's source, not its header's comments. To test it, compare the device's COM reference count before and after creating and dropping the context, with nothing else touching the device in between: every D3D12 resource holds a reference to its device.
- **rsmpeg's `AVHWDeviceContext::alloc` unwraps,** so it panics when the FFmpeg build lacks that device type. Call `av_hwdevice_ctx_alloc` yourself and check for null.
- **Resource states are shared bookkeeping.** FFmpeg's D3D12VA decoder transitions a picture COMMON → decode-write → COMMON, and assumes COMMON next time. When wgpu reads it, wrap it with `create_texture_from_hal(…, TextureUses::PRESENT)` (COMMON on DX12) and `transition_resources` it back to `PRESENT` in the same submit. Make a new wgpu texture per use: a cached one keeps wgpu's idea of the state after FFmpeg changed it.
- **Order across queues with the decoder's fence, on the GPU:** `hal::dx12::Queue::add_wait_fence(fence, value)` before the submit that reads, with the value from `AVD3D12VAFrame.sync_ctx` read while holding the frame. Never `fence_value` later: FFmpeg increments it on reuse.
- **Hold FFmpeg's frame until the GPU is done** (`Queue::on_submitted_work_done`, run by `device.poll`). FFmpeg reuses a picture as soon as its last reference goes.
- **The D3D12 debug layer won't catch a wrong initial state that is a read state** (shader resource, copy source): D3D12 may promote COMMON to those implicitly. Bit-exact comparison against another path is the real check. To check that the layer is on at all, `cast::<ID3D12InfoQueue>()` the device: wgpu-hal skips it silently when the Graphics Tools aren't installed, and an inactive layer looks exactly like a clean run.
- **`couch-viewer` is the one crate besides `couch-platform-win` that uses `windows`,** and only to pass COM pointers between FFmpeg and wgpu-hal (`from_raw_borrowed`, `clone`, `into_raw`) and to read a resource's description.

## Video hardware through FFmpeg

- **FFmpeg's D3D12VA frame pool is the picture's exact size** (`ff_d3d12va_common_frame_params`, 8.1 to 9.0); D3D11VA's (`dxva2.c`) aligns HEVC to 128. NVIDIA's driver refuses an HEVC texture of many exact heights (`h mod 32` from 1 to 16: 720, 900, 1200, 2160), and the decode fails as "Could not create the texture" then "hardware accelerator failed to decode picture". The fix is our own pool: in `get_format`, `avcodec_get_hw_frames_parameters`, align the size, `av_hwframe_ctx_init`, and put it in `hw_frames_ctx` (`couch_codec_ffmpeg::decoder`). A decoded texture can then be larger than the picture: copy the picture's extent, never the texture's (`couch-viewer`'s `FrameImporter`). Test a new codec or decoder path at 1280x720, 1920x1080 and an odd height such as 1682x1070.
- **What a GPU decodes:** create a D3D11 device with `D3D11_CREATE_DEVICE_VIDEO_SUPPORT`, cast it to `ID3D11VideoDevice` and walk `GetVideoDecoderProfile(0..GetVideoDecoderProfileCount())` for the profile's GUID (`couch_platform_win::video_caps`). It's a driver call; cache the answer.
- **NVENC's RGB input is converted with BT.601,** limited range, whatever colorspace the codec context says (`nvenc.c`), and the stream declares it. Anything that compares or renders decoded pictures takes the declared matrix from the frame (`YuvMatrix`, read by the decoder), never a fixed one: the viewer assumed BT.709 from M1 to M12c.

## Low-level hooks

- **Raw keyboard input blinds the process's own hooks.** While a process that registered for raw keyboard input (`RegisterRawInputDevices`, usage page 1, usage 6) is in front, Windows doesn't call that process's own `WH_KEYBOARD_LL` hooks. Measured in M5b: 0 of 2 events with the registration, 2 of 2 without it or after `RIDEV_REMOVE`.
  - Tauri's tao registers for raw input by default, so the desktop app calls `device_event_filter(DeviceEventFilter::Always)`. Keep it, and never register raw keyboard input in a process that hosts the panic key.
  - Symptom: the panic key works everywhere except in our own window.
- **Test the hook with a person.** The panic key ignores injected keys, so a script can't press it. For experiments, a throwaway hook that counts injected keys (a C# `Add-Type` script in the scratchpad works) can show whether the hook is called at all.
- A low-level hook runs on the thread that installed it, from that thread's message loop. Keep that thread doing nothing else, and return within milliseconds: Windows silently removes a hook that keeps timing out.

## Callbacks must not panic

- A panic that unwinds out of an `extern "C"` callback (SDL audio, Windows hooks, window procedures) aborts the process. Inside callbacks:
  - lock with `lock().unwrap_or_else(PoisonError::into_inner)`, since another thread's panic poisons the mutex;
  - avoid `unwrap` and `expect`, and arithmetic that can overflow in debug builds on peer-supplied values (use saturating or wrapping operations);
  - handle `Err` by falling back to something harmless (silence, a no-op).
- Keep them short: no allocation in steady state where it's easy to avoid, and no locks held across anything slow.

## Debugging

- Build a minimal reproduction in the session scratchpad (`cargo new`), with `CARGO_TARGET_DIR` set to a short path, such as `target\scratch` under the repository's root (it should be cloned to a short path, `docs/development.md`). Deep paths break CMake and MSBuild builds (libopus) at Windows' 260-character limit.
- Run it against the known-good crate to tell your bug from the environment's.
- Remove trace output before finishing. `couch-viewer` must never print to stdout.

## Before calling it done

- Every `unsafe` block has a `// SAFETY:` comment saying why it holds.
- Tests that need hardware or services are `#[ignore]` with a reason (`cargo test --workspace` must pass without them), and you ran them by hand.
- If the code is on the input or teardown path, run the `input-safety-checks` skill; for audio, `audio-checks`; for capture, encoding or transport, `rate-control-gates`.
