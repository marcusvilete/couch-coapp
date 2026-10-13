//! FFmpeg-backed codecs. The only crate that links FFmpeg, so everything else
//! builds and tests without it.
//!
//! FFmpeg is the LGPL shared build from `third_party/ffmpeg`, linked
//! dynamically; its DLLs must sit next to the executable (`cargo xtask stage-dlls`).

mod d3d11;
pub mod decoder;
pub mod encoder;
pub mod options;
pub mod probe;

pub use decoder::FfmpegDecoder;
pub use encoder::FfmpegEncoder;
