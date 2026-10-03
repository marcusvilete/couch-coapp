use bytes::Bytes;

/// Microseconds on a clock that is comparable across processes on one machine
/// (QPC on Windows; see [`crate::clock`]). Frames are stamped with it at
/// capture, so the viewer can compute capture-to-present latency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Micros(pub u64);

impl Micros {
    pub fn saturating_sub(self, earlier: Micros) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

/// A frame in system memory, 32-bit BGRA, top row first.
#[derive(Clone)]
pub struct BgraFrame {
    pub width: u32,
    pub height: u32,
    /// Bytes per row; at least `width * 4`.
    pub stride: u32,
    pub data: Bytes,
    pub captured_at: Micros,
}

impl BgraFrame {
    /// The same frame with an odd width or height trimmed by one pixel, since
    /// 4:2:0 encoders need even dimensions. Shares the pixel data.
    pub fn cropped_to_even(&self) -> BgraFrame {
        BgraFrame {
            width: self.width & !1,
            height: self.height & !1,
            ..self.clone()
        }
    }

    /// The `[b, g, r, a]` value at `(x, y)`.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y * self.stride + x * 4) as usize;
        self.data[i..i + 4].try_into().expect("4 bytes")
    }
}

impl std::fmt::Debug for BgraFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BgraFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .field("captured_at", &self.captured_at)
            .finish_non_exhaustive()
    }
}

/// A decoded frame in system memory: a full-resolution Y plane followed by
/// an interleaved UV plane at half resolution.
#[derive(Clone)]
pub struct Nv12Frame {
    pub width: u32,
    pub height: u32,
    pub y_stride: u32,
    pub uv_stride: u32,
    pub y: Bytes,
    pub uv: Bytes,
    pub captured_at: Micros,
}

/// What an encoder consumes. A GPU-texture variant arrives with the zero-copy
/// work in M9; the enum is non-exhaustive so that is not a breaking change.
#[non_exhaustive]
pub enum EncoderInput<'a> {
    Cpu(&'a BgraFrame),
}

/// What a decoder produces. A GPU-texture variant arrives in M9.
#[non_exhaustive]
pub enum DecodedFrame {
    CpuNv12(Nv12Frame),
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
}

/// One encoded access unit (Annex-B H.264 for [`Codec::H264`]).
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub data: Bytes,
    pub keyframe: bool,
    pub captured_at: Micros,
    pub encode_time_us: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderCaps {
    /// Backend name, e.g. `h264_nvenc`.
    pub name: String,
    pub hardware: bool,
    /// Changing the bitrate forces a keyframe. FFmpeg's NVENC wrapper resets
    /// the encoder on every bitrate change, so rate control batches increases.
    pub bitrate_change_costs_keyframe: bool,
}

/// Interleaved 32-bit float PCM.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub samples: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
    pub captured_at: Micros,
}

/// What a [`crate::Capturer`] reports.
#[derive(Debug)]
pub enum CaptureEvent {
    Frame(BgraFrame),
    /// The captured window or display went away (for example, the game exited).
    SourceClosed,
}

/// Identifies an epoch: a keyframe plus the frames that depend on it, sent in
/// order on one stream (docs/protocol.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EpochId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoParams {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// A frame as it arrives at the viewer side of a transport.
#[derive(Debug, Clone)]
pub struct ReceivedFrame {
    pub epoch: EpochId,
    pub seq: u32,
    pub frame: EncodedFrame,
    pub arrived_at: Micros,
}

/// What the rate controller knows about one viewer's path, whatever the
/// transport. A WebRTC transport could fill the same fields from TWCC.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FeedbackSnapshot {
    /// Receive rate measured by the viewer.
    pub delivery_rate_bps: u64,
    /// Rate at which video was handed to the transport. Below the bitrate
    /// when the encoder has little to send (a static scene).
    pub send_rate_bps: u64,
    /// Bytes sent in the current epoch that the viewer has not reported yet.
    pub outstanding_bytes: u64,
    pub rtt_us: u32,
    pub min_rtt_us: u32,
    /// Growth of one-way delay over its recent minimum.
    pub delay_trend_us: i32,
    /// Packets sent and lost on the current path, cumulative.
    pub sent_packets: u64,
    pub lost_packets: u64,
    pub congestion_events: u64,
    /// The transport's congestion window, if it has one (0 otherwise).
    pub cwnd_bytes: u64,
    /// The path currently goes through a relay server.
    pub relayed: bool,
    /// Time since the last receiver report.
    pub report_age_ms: u32,
}
