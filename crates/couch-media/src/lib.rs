//! Media vocabulary: frame types and the traits that capture, codec and
//! transport backends implement.
//!
//! Pure Rust by design: no FFmpeg (that is `couch-codec-ffmpeg`) and no OS APIs
//! (those are `couch-platform-*`), so everything here builds and tests anywhere.

pub mod audio;
pub mod barcode;
pub mod clock;
mod frame;
pub mod mailbox;
pub mod ratecontrol;
pub mod testpattern;
mod traits;

pub use frame::*;
pub use traits::*;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Backend(String),
    #[error("closed")]
    Closed,
}
