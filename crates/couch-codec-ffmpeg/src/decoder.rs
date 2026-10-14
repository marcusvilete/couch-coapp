//! H.264 decoding through FFmpeg. Low-delay mode and a single thread, so
//! every packet yields its frame at once. Three ways:
//! - D3D12VA on the renderer's own device, frames left on the GPU (M9c);
//! - D3D11VA, frames copied to memory as NV12;
//! - software, when neither is available.

use std::ffi::c_void;

use bytes::Bytes;
use couch_media::{DecodedFrame, GpuNv12Frame, MediaError, Micros, Nv12Frame, VideoDecoder};
use rsmpeg::{
    avcodec::{AVCodec, AVCodecContext, AVPacket},
    avutil::{AVFrame, AVHWDeviceContext},
    error::RsmpegError,
    ffi,
};

use crate::d3d12::{self, D3d12Surface};

/// Where a decoder's frames are decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderKind {
    /// D3D12VA; frames stay on the GPU.
    D3d12,
    /// D3D11VA; frames are copied to memory.
    D3d11,
    Software,
}

pub struct FfmpegDecoder {
    ctx: AVCodecContext,
    kind: DecoderKind,
}

// Only ever used from the thread that owns the decoder.
unsafe impl Send for FfmpegDecoder {}

impl FfmpegDecoder {
    /// A decoder that uses D3D11VA if `prefer_hardware` and the GPU allows it.
    pub fn new(prefer_hardware: bool) -> Result<Self, MediaError> {
        let mut ctx = Self::context()?;
        let mut kind = DecoderKind::Software;
        if prefer_hardware {
            match AVHWDeviceContext::create(ffi::AV_HWDEVICE_TYPE_D3D11VA, None, None, 0) {
                Ok(device) => {
                    ctx.set_hw_device_ctx(device);
                    ctx.set_get_format(Some(prefer_d3d11));
                    kind = DecoderKind::D3d11;
                }
                Err(e) => tracing::warn!("D3D11VA unavailable, decoding in software: {e}"),
            }
        }
        Self::open(ctx, kind)
    }

    /// A D3D12VA decoder on `device_owned`, an `ID3D12Device` carrying one
    /// reference of its own, released when the decoder and its last frame are
    /// gone (or here, if this fails). Its frames are [`DecodedFrame::Gpu`],
    /// resources on that device.
    ///
    /// The hardware decoder itself starts at the first keyframe. If it can't
    /// (FFmpeg would then decode in software), `decode` returns
    /// [`MediaError::Unsupported`]: open another decoder and ask for a
    /// keyframe.
    pub fn new_d3d12(device_owned: *mut c_void) -> Result<Self, MediaError> {
        let device = d3d12::device_context(device_owned).map_err(MediaError::Unsupported)?;
        let mut ctx = Self::context()?;
        // SAFETY: the context is ours and not yet opened; it takes the
        // device context's reference and unreferences it when freed.
        unsafe { (*ctx.as_mut_ptr()).hw_device_ctx = device.into_raw() };
        ctx.set_get_format(Some(prefer_d3d12));
        Self::open(ctx, DecoderKind::D3d12)
    }

    fn context() -> Result<AVCodecContext, MediaError> {
        let codec = AVCodec::find_decoder(ffi::AV_CODEC_ID_H264).ok_or_else(|| {
            MediaError::Unsupported("no H.264 decoder in this FFmpeg build".into())
        })?;
        let mut ctx = AVCodecContext::new(&codec);
        ctx.set_flags(ctx.flags | ffi::AV_CODEC_FLAG_LOW_DELAY as i32);
        // SAFETY: the context is ours and not yet opened.
        unsafe {
            let raw = ctx.as_mut_ptr();
            (*raw).thread_count = 1;
            // Corrupt input is an error at once, not concealed: the viewer
            // then asks for a keyframe instead of showing smeared frames
            // until the next one (as Moonlight does, M9a).
            (*raw).err_recognition = ffi::AV_EF_EXPLODE as i32;
        }
        Ok(ctx)
    }

    fn open(mut ctx: AVCodecContext, kind: DecoderKind) -> Result<Self, MediaError> {
        ctx.open(None)
            .map_err(|e| MediaError::Backend(format!("opening the H.264 decoder: {e}")))?;
        Ok(Self { ctx, kind })
    }

    pub fn kind(&self) -> DecoderKind {
        self.kind
    }

    pub fn is_hardware(&self) -> bool {
        self.kind != DecoderKind::Software
    }
}

/// Picks `wanted` surfaces when offered, else the decoder's first (software)
/// choice. FFmpeg calls this again without `wanted` if its hardware decoder
/// fails to start.
unsafe fn prefer(
    formats: *const ffi::AVPixelFormat,
    wanted: ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    unsafe {
        let mut p = formats;
        while *p != ffi::AV_PIX_FMT_NONE {
            if *p == wanted {
                return *p;
            }
            p = p.add(1);
        }
        *formats
    }
}

unsafe extern "C" fn prefer_d3d11(
    _ctx: *mut ffi::AVCodecContext,
    formats: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    // SAFETY: FFmpeg passes a list ended by AV_PIX_FMT_NONE.
    unsafe { prefer(formats, ffi::AV_PIX_FMT_D3D11) }
}

unsafe extern "C" fn prefer_d3d12(
    _ctx: *mut ffi::AVCodecContext,
    formats: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    // SAFETY: as above.
    unsafe { prefer(formats, ffi::AV_PIX_FMT_D3D12) }
}

impl VideoDecoder for FfmpegDecoder {
    fn decode(
        &mut self,
        access_unit: &[u8],
        captured_at: Micros,
    ) -> Result<Vec<DecodedFrame>, MediaError> {
        let mut packet = AVPacket::new();
        unsafe {
            if ffi::av_new_packet(packet.as_mut_ptr(), access_unit.len() as i32) < 0 {
                return Err(MediaError::Backend("could not allocate a packet".into()));
            }
            std::ptr::copy_nonoverlapping(access_unit.as_ptr(), packet.data, access_unit.len());
        }
        self.ctx
            .send_packet(Some(&packet))
            .map_err(|e| MediaError::Backend(e.to_string()))?;

        let mut out = Vec::new();
        loop {
            let frame = match self.ctx.receive_frame() {
                Ok(frame) => frame,
                Err(RsmpegError::DecoderDrainError) => break,
                Err(e) => return Err(MediaError::Backend(e.to_string())),
            };
            if self.kind == DecoderKind::D3d12 {
                if frame.format != ffi::AV_PIX_FMT_D3D12 {
                    return Err(MediaError::Unsupported(
                        "D3D12VA couldn't decode this stream".into(),
                    ));
                }
                let (width, height) = (frame.width as u32, frame.height as u32);
                let surface = D3d12Surface::new(frame).map_err(MediaError::Backend)?;
                out.push(DecodedFrame::Gpu(GpuNv12Frame {
                    width,
                    height,
                    captured_at,
                    surface: Box::new(surface),
                }));
                continue;
            }
            let frame = if frame.format == ffi::AV_PIX_FMT_D3D11 {
                let mut cpu = AVFrame::new();
                cpu.hwframe_transfer_data(&frame)
                    .map_err(|e| MediaError::Backend(format!("D3D11 to CPU copy: {e}")))?;
                cpu
            } else {
                frame
            };
            out.push(DecodedFrame::CpuNv12(to_nv12(&frame, captured_at)?));
        }
        Ok(out)
    }
}

/// Copies a decoded NV12 or YUV420P frame into a tightly packed [`Nv12Frame`].
fn to_nv12(frame: &AVFrame, captured_at: Micros) -> Result<Nv12Frame, MediaError> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let chroma_h = h.div_ceil(2);
    let plane = |i: usize, row_bytes: usize, rows: usize| -> Vec<u8> {
        let stride = frame.linesize[i] as usize;
        let mut out = Vec::with_capacity(row_bytes * rows);
        for row in 0..rows {
            // SAFETY: plane `i` holds `rows` rows of `stride >= row_bytes` bytes.
            let src =
                unsafe { std::slice::from_raw_parts(frame.data[i].add(row * stride), row_bytes) };
            out.extend_from_slice(src);
        }
        out
    };

    let y = plane(0, w, h);
    let uv = match frame.format {
        f if f == ffi::AV_PIX_FMT_NV12 => plane(1, w.div_ceil(2) * 2, chroma_h),
        f if f == ffi::AV_PIX_FMT_YUV420P => {
            let cw = w.div_ceil(2);
            let (u, v) = (plane(1, cw, chroma_h), plane(2, cw, chroma_h));
            u.iter().zip(&v).flat_map(|(&u, &v)| [u, v]).collect()
        }
        other => {
            return Err(MediaError::Unsupported(format!(
                "decoded pixel format {other}"
            )));
        }
    };
    Ok(Nv12Frame {
        width: w as u32,
        height: h as u32,
        y_stride: w as u32,
        uv_stride: (w.div_ceil(2) * 2) as u32,
        y: Bytes::from(y),
        uv: Bytes::from(uv),
        captured_at,
    })
}
