---
name: windows-ffi-pitfalls
description: Checklist for writing or reviewing unsafe Windows, COM and SDL3 code in Couch Co-App with the `windows` 0.62 crate, `windows-core` and `sdl3` 0.20 — hidden Drop impls, packed structs, COM threading, callbacks on foreign threads, teardown order. Use when adding or changing code in crates/couch-platform-win (WASAPI, Windows.Graphics.Capture, SendInput, ViGEm, process and window APIs, and later hooks, Job Objects, DPAPI, overlays), any `unsafe` block that calls Windows or SDL, or an SDL callback in couch-viewer.
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

## Callbacks must not panic

- A panic that unwinds out of an `extern "C"` callback (SDL audio, Windows hooks, window procedures) aborts the process. Inside callbacks:
  - lock with `lock().unwrap_or_else(PoisonError::into_inner)`, since another thread's panic poisons the mutex;
  - avoid `unwrap` and `expect`, and arithmetic that can overflow in debug builds on peer-supplied values (use saturating or wrapping operations);
  - handle `Err` by falling back to something harmless (silence, a no-op).
- Keep them short: no allocation in steady state where it's easy to avoid, and no locks held across anything slow.

## Debugging

- Build a minimal reproduction in the session scratchpad (`cargo new`), with `CARGO_TARGET_DIR` set to a short path such as `C:\dev\couch-coapp\target\scratch`. Deep paths break CMake and MSBuild builds (libopus) at Windows' 260-character limit.
- Run it against the known-good crate to tell your bug from the environment's.
- Remove trace output before finishing. `couch-viewer` must never print to stdout.

## Before calling it done

- Every `unsafe` block has a `// SAFETY:` comment saying why it holds.
- Tests that need hardware or services are `#[ignore]` with a reason (`cargo test --workspace` must pass without them), and you ran them by hand.
- If the code is on the input or teardown path, run the `input-safety-checks` skill; for audio, `audio-checks`; for capture, encoding or transport, `rate-control-gates`.
