//! FFmpeg-backed codecs. The only crate that links FFmpeg, so everything else
//! builds and tests without it.
//!
//! FFmpeg is the LGPL shared build from `third_party/ffmpeg`, linked
//! dynamically; its DLLs must sit next to the executable (`cargo xtask stage-dlls`).

pub mod probe;
