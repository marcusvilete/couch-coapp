# ADR 0004: Hardware codecs through FFmpeg, bound with rsmpeg

**Status:** accepted (2026-10-02).

## Context

The host must encode with whichever GPU it has: NVENC, AMF or Quick Sync. Viewers must decode in hardware. Writing a vendor SDK binding per GPU is expensive. FFmpeg exposes all of them through one API, and its LGPL build keeps the license choice open.

## Decision

- **Which FFmpeg.** BtbN's 8.1 LGPL shared build, pinned in `xtask/src/deps.rs`. It is linked dynamically through `rsmpeg` 0.18 (MIT), with `FFMPEG_LINK_MODE=dynamic`.
- **Where the code lives.** Only `couch-codec-ffmpeg` touches FFmpeg.
- **First slice.** BGRA frames go straight into `h264_nvenc`, which converts colour on the GPU. Decode uses D3D11VA, then copies to CPU memory as NV12. Zero-copy paths come in M9.

## Consequences

- **Bindings are pre-generated and committed.**
  - rusty_ffmpeg (under rsmpeg) normally runs bindgen 0.71 at build time. With libclang 22 or later (the dev machine has LLVM 23), that bindgen emits `AVFormatContext` and `AVBitStreamFilter` without their fields, and rsmpeg fails to compile.
  - `cargo xtask gen-ffmpeg-bindings` instead runs bindgen 0.73 with rusty_ffmpeg 0.16.7's options against the pinned headers. It writes `crates/couch-codec-ffmpeg/bindings/ffmpeg-x86_64-pc-windows-msvc.rs`, which rusty_ffmpeg reads through `FFMPEG_BINDING_PATH`.
  - So normal builds need no LLVM. Regenerate after every FFmpeg bump; `cargo xtask doctor` flags stale bindings.
- **Running** needs the FFmpeg DLLs next to the binary (`cargo xtask stage-dlls`) or on PATH (`scripts/dev.ps1`).
- **LGPL obligations.** Ship the DLLs unmodified and replaceable, with the license notice and a source offer (M10). The build must not include `--enable-gpl` or `--enable-nonfree`; `cargo xtask doctor` checks this.
- **Version gap.** rsmpeg 0.18 targets FFmpeg 8.0 and hasn't had a release in 13 months. We build it against 8.1 headers, which keep 8.0's library major versions.
- **Risky code is contained.** Unsafe hardware-frame code stays in one small crate.

## Alternatives, if rsmpeg stalls

- An archived 8.0 build.
- `rusty_ffmpeg` 0.17 used directly.
- `ffmpeg-the-third`, which is WTFPL and therefore needs an explicit license exception.
