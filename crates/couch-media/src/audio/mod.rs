//! Audio: the stream format, framing captured audio into codec frames, the
//! viewer's jitter buffer, and the click track used to measure latency.

mod click;
mod framer;
mod jitter;

pub use click::{ClickTrack, OnsetDetector, ToneSource};
pub use framer::{Frame, Framer};
pub use jitter::{JitterBuffer, JitterConfig, JitterStats, Pop};

/// The stream format end to end: 48 kHz, stereo, 32-bit float, 10 ms frames.
/// Matches `couch_proto::media::audio`, which this crate can't depend on.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
/// Samples per channel in one frame.
pub const FRAME_SAMPLES: usize = 480;
/// Interleaved samples in one frame.
pub const FRAME_LEN: usize = FRAME_SAMPLES * CHANNELS as usize;
pub const FRAME_US: u64 = 10_000;

/// Microseconds that `samples` interleaved samples last.
pub fn duration_us(samples: usize) -> u64 {
    (samples / usize::from(CHANNELS)) as u64 * 1_000_000 / u64::from(SAMPLE_RATE)
}
