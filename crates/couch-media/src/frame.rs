use std::{
    any::Any,
    ffi::c_void,
    sync::{Arc, Mutex, PoisonError},
};

use bytes::Bytes;

use crate::MediaError;

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

/// A BGRA texture in GPU memory that a [`GpuFrame`] points at (M9b). The
/// platform crate implements it; on Windows it is a D3D11 texture on the
/// capture's device. Pointers are raw so this crate stays free of OS types.
pub trait GpuTexture: Send + Sync {
    /// The `ID3D11Texture2D`, valid while this lives. No reference is added.
    fn d3d11_texture(&self) -> *mut c_void;

    /// The texture's `ID3D11Device`, with one reference added that the caller
    /// now owns (FFmpeg's D3D11 device context releases it when freed).
    fn d3d11_device_owned(&self) -> *mut c_void;

    /// Copies the pixels to memory: the row stride and the rows.
    fn download(&self) -> Result<(u32, Bytes), MediaError>;

    /// Keeps every texture of this one's pool alive while held, those made
    /// later included. An encoder that registers textures (NVENC) holds it
    /// until it closes, so none is destroyed while still registered.
    fn keep_alive(&self) -> Arc<dyn Any + Send + Sync>;
}

/// A captured frame that stays on the GPU (M9b): an encoder that takes GPU
/// input reads the texture itself; any other gets [`GpuFrame::cpu`], copied
/// to memory once per frame however many encoders ask.
#[derive(Clone)]
pub struct GpuFrame {
    /// Always even: the pool copies frames cropped to even size.
    pub width: u32,
    pub height: u32,
    pub captured_at: Micros,
    /// The texture pool the frame came from, new for every capture device and
    /// size. An encoder with GPU input reopens when it changes: NVENC caches
    /// each texture it saw by address, and only a pool's own textures are
    /// guaranteed to outlive the encoder that registered them.
    pub source: u64,
    inner: Arc<GpuInner>,
}

struct GpuInner {
    texture: Arc<dyn GpuTexture>,
    cpu: Mutex<Option<BgraFrame>>,
}

impl GpuFrame {
    pub fn new(
        texture: Arc<dyn GpuTexture>,
        width: u32,
        height: u32,
        captured_at: Micros,
        source: u64,
    ) -> Self {
        debug_assert!(
            width.is_multiple_of(2) && height.is_multiple_of(2),
            "{width}x{height}"
        );
        Self {
            width,
            height,
            captured_at,
            source,
            inner: Arc::new(GpuInner {
                texture,
                cpu: Mutex::new(None),
            }),
        }
    }

    pub fn texture(&self) -> &dyn GpuTexture {
        &*self.inner.texture
    }

    /// The frame in memory, copied from the GPU on the first call and shared
    /// by every later one.
    pub fn cpu(&self) -> Result<BgraFrame, MediaError> {
        let mut cpu = self
            .inner
            .cpu
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(frame) = &*cpu {
            return Ok(frame.clone());
        }
        let (stride, data) = self.inner.texture.download()?;
        let frame = BgraFrame {
            width: self.width,
            height: self.height,
            stride,
            data,
            captured_at: self.captured_at,
        };
        *cpu = Some(frame.clone());
        Ok(frame)
    }
}

impl std::fmt::Debug for GpuFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("captured_at", &self.captured_at)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// A captured frame, in memory or on the GPU.
#[derive(Debug, Clone)]
pub enum VideoFrame {
    Cpu(BgraFrame),
    Gpu(GpuFrame),
}

impl VideoFrame {
    pub fn width(&self) -> u32 {
        match self {
            Self::Cpu(f) => f.width,
            Self::Gpu(f) => f.width,
        }
    }

    pub fn height(&self) -> u32 {
        match self {
            Self::Cpu(f) => f.height,
            Self::Gpu(f) => f.height,
        }
    }

    pub fn captured_at(&self) -> Micros {
        match self {
            Self::Cpu(f) => f.captured_at,
            Self::Gpu(f) => f.captured_at,
        }
    }

    /// The GPU frame's texture pool, or `None` for a frame in memory.
    pub fn gpu_source(&self) -> Option<u64> {
        match self {
            Self::Cpu(_) => None,
            Self::Gpu(f) => Some(f.source),
        }
    }

    /// Even width and height, as 4:2:0 encoders need. A GPU frame already is.
    pub fn cropped_to_even(&self) -> VideoFrame {
        match self {
            Self::Cpu(f) => Self::Cpu(f.cropped_to_even()),
            Self::Gpu(f) => Self::Gpu(f.clone()),
        }
    }

    /// The frame in memory: itself, or the GPU frame copied once.
    pub fn cpu(&self) -> Result<BgraFrame, MediaError> {
        match self {
            Self::Cpu(f) => Ok(f.clone()),
            Self::Gpu(f) => f.cpu(),
        }
    }
}

/// What an encoder consumes: a frame in memory, or a GPU frame for an encoder
/// whose caps say it takes GPU input ([`EncoderCaps::gpu_input`]).
#[non_exhaustive]
pub enum EncoderInput<'a> {
    Cpu(&'a BgraFrame),
    Gpu(&'a GpuFrame),
}

/// A decoded NV12 picture in GPU memory that a [`GpuNv12Frame`] points at
/// (M9c). The codec crate implements it; on Windows it is a D3D12 resource the
/// decoder wrote on the viewer's own device. Pointers are raw so this crate
/// stays free of OS types. Dropping it gives the picture back to the decoder.
pub trait DecodedSurface: Send {
    /// The `ID3D12Resource` (NV12, the frame's size), valid while this lives.
    /// No reference is added.
    fn d3d12_resource(&self) -> *mut c_void;

    /// The `ID3D12Fence` the decoder signals, and the value it reaches once
    /// the picture is decoded. GPU work reading the resource must wait for it.
    fn d3d12_fence(&self) -> (*mut c_void, u64);
}

/// A decoded frame that stays on the GPU (M9c).
pub struct GpuNv12Frame {
    pub width: u32,
    pub height: u32,
    pub captured_at: Micros,
    pub surface: Box<dyn DecodedSurface>,
}

impl std::fmt::Debug for GpuNv12Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuNv12Frame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("captured_at", &self.captured_at)
            .finish_non_exhaustive()
    }
}

/// What a decoder produces: a frame copied to memory, or one left on the GPU
/// for a renderer on the decoder's device.
#[non_exhaustive]
pub enum DecodedFrame {
    CpuNv12(Nv12Frame),
    Gpu(GpuNv12Frame),
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
    /// Which of the backend's option sets opened (e.g. `ultralowlatency`,
    /// `today's`), for the logs: tells a friend's tuned encoder from one that
    /// fell back.
    pub options: String,
    pub hardware: bool,
    /// Takes [`EncoderInput::Gpu`] (M9b: NVENC opened on the capture's
    /// device); otherwise frames reach it in memory.
    pub gpu_input: bool,
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
    Frame(VideoFrame),
    /// The captured window or display went away (for example, the game exited).
    SourceClosed,
    /// Frames can't stay on the GPU here: they come in memory from now on.
    /// A capture asked for GPU frames may run faster than the stream
    /// (M9d), so the pipeline captures again with frames in memory.
    GpuFramesFailed,
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A "GPU" texture in memory that counts its downloads.
    struct Counted {
        pixels: Bytes,
        downloads: AtomicU32,
    }

    impl GpuTexture for Counted {
        fn d3d11_texture(&self) -> *mut c_void {
            std::ptr::null_mut()
        }
        fn d3d11_device_owned(&self) -> *mut c_void {
            std::ptr::null_mut()
        }
        fn download(&self) -> Result<(u32, Bytes), MediaError> {
            self.downloads.fetch_add(1, Ordering::SeqCst);
            Ok((8, self.pixels.clone()))
        }
        fn keep_alive(&self) -> Arc<dyn Any + Send + Sync> {
            Arc::new(())
        }
    }

    #[test]
    fn a_gpu_frame_is_downloaded_once_for_every_encoder() {
        let texture = Arc::new(Counted {
            pixels: Bytes::from(vec![7u8; 8 * 2]),
            downloads: AtomicU32::new(0),
        });
        let frame = VideoFrame::Gpu(GpuFrame::new(texture.clone(), 2, 2, Micros(5), 1));
        let copies = [frame.clone(), frame.clone(), frame];
        for copy in &copies {
            let cpu = copy.cpu().unwrap();
            assert_eq!((cpu.width, cpu.height, cpu.stride), (2, 2, 8));
            assert_eq!(cpu.captured_at, Micros(5));
            assert_eq!(cpu.pixel(1, 1), [7; 4]);
        }
        assert_eq!(texture.downloads.load(Ordering::SeqCst), 1);
        assert_eq!(copies[0].gpu_source(), Some(1));
        assert_eq!(copies[0].cropped_to_even().width(), 2);
    }

    #[test]
    fn a_frame_in_memory_is_cropped_and_has_no_gpu_source() {
        let frame = VideoFrame::Cpu(BgraFrame {
            width: 3,
            height: 5,
            stride: 12,
            data: Bytes::from(vec![0u8; 60]),
            captured_at: Micros(1),
        });
        assert_eq!(frame.gpu_source(), None);
        let even = frame.cropped_to_even();
        assert_eq!((even.width(), even.height()), (2, 4));
    }
}
