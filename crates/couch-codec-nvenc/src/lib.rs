//! NVENC without FFmpeg (M12d, [ADR 0016](../../../docs/adr/0016-nvenc-directly-and-continuation-epochs.md)):
//! H.264 and HEVC from the capture's D3D11 textures, with the stream FFmpeg's
//! `h264_nvenc` and `hevc_nvenc` give for our options, but bitrate changes in
//! place instead of a reset with a keyframe. The only crate that calls NVENC.
//!
//! The driver's `nvEncodeAPI64.dll` is loaded at run time, from System32
//! only; the bindings are generated from the vendored SDK 13.0 header
//! (`cargo xtask gen-nvenc-bindings`), the API version FFmpeg's pinned build
//! asks for, so no driver that runs FFmpeg's NVENC lacks ours. Windows only:
//! elsewhere this crate is empty, and hosts keep FFmpeg's encoders.

#![cfg(windows)]

mod api;
mod encoder;

#[allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    unnecessary_transmutes,
    unsafe_op_in_unsafe_fn,
    unused_imports,
    clippy::all
)]
mod ffi {
    include!("../bindings/nvenc-x86_64-pc-windows-msvc.rs");
}

pub use encoder::{
    CONTINUING, Continuation, ContinuationCaps, FrameControl, FrameReport, NvencEncoder,
};
