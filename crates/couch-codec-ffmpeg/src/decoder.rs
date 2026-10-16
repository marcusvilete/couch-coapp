//! H.264 and HEVC decoding through FFmpeg. Low-delay mode and a single
//! thread, so every packet yields its frame at once. Three ways:
//! - D3D12VA on the renderer's own device, frames left on the GPU (M9c);
//! - D3D11VA, frames copied to memory as NV12;
//! - software, when neither is available.

use std::ffi::c_void;

use bytes::Bytes;
use couch_media::{
    Codec, DecodedFrame, GpuNv12Frame, MediaError, Micros, Nv12Frame, VideoDecoder, YuvMatrix,
};
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
    codec: Codec,
}

// Only ever used from the thread that owns the decoder.
unsafe impl Send for FfmpegDecoder {}

impl FfmpegDecoder {
    /// A `codec` decoder that uses D3D11VA if `prefer_hardware` and the GPU
    /// allows it.
    pub fn new(codec: Codec, prefer_hardware: bool) -> Result<Self, MediaError> {
        let mut ctx = Self::context(codec)?;
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
        Self::open(ctx, kind, codec)
    }

    /// A `codec` decoder with D3D12VA on `device_owned`, an `ID3D12Device` carrying one
    /// reference of its own, released when the decoder and its last frame are
    /// gone (or here, if this fails). Its frames are [`DecodedFrame::Gpu`],
    /// resources on that device.
    ///
    /// The hardware decoder itself starts at the first keyframe. If it can't
    /// (FFmpeg would then decode in software), `decode` returns
    /// [`MediaError::Unsupported`]: open another decoder and ask for a
    /// keyframe.
    pub fn new_d3d12(codec: Codec, device_owned: *mut c_void) -> Result<Self, MediaError> {
        let device = d3d12::device_context(device_owned).map_err(MediaError::Unsupported)?;
        let mut ctx = Self::context(codec)?;
        // SAFETY: the context is ours and not yet opened; it takes the
        // device context's reference and unreferences it when freed.
        unsafe { (*ctx.as_mut_ptr()).hw_device_ctx = device.into_raw() };
        ctx.set_get_format(Some(prefer_d3d12));
        Self::open(ctx, DecoderKind::D3d12, codec)
    }

    fn context(codec: Codec) -> Result<AVCodecContext, MediaError> {
        let id = match codec {
            Codec::Hevc => ffi::AV_CODEC_ID_HEVC,
            _ => ffi::AV_CODEC_ID_H264,
        };
        let found = AVCodec::find_decoder(id).ok_or_else(|| {
            MediaError::Unsupported(format!("no {codec:?} decoder in this FFmpeg build"))
        })?;
        let mut ctx = AVCodecContext::new(&found);
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

    fn open(mut ctx: AVCodecContext, kind: DecoderKind, codec: Codec) -> Result<Self, MediaError> {
        ctx.open(None)
            .map_err(|e| MediaError::Backend(format!("opening the {codec:?} decoder: {e}")))?;
        Ok(Self { ctx, kind, codec })
    }

    pub fn kind(&self) -> DecoderKind {
        self.kind
    }

    /// What it decodes.
    pub fn codec(&self) -> Codec {
        self.codec
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
    ctx: *mut ffi::AVCodecContext,
    formats: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    // SAFETY: as above.
    let chosen = unsafe { prefer(formats, ffi::AV_PIX_FMT_D3D12) };
    // SAFETY: FFmpeg passes its live, opened context.
    if chosen == ffi::AV_PIX_FMT_D3D12 && unsafe { (*ctx).codec_id } == ffi::AV_CODEC_ID_HEVC {
        // SAFETY: as above, inside FFmpeg's get_format call.
        if let Err(code) = unsafe { aligned_d3d12_frames(ctx) } {
            tracing::warn!("D3D12 frames for HEVC not set up ({code}); FFmpeg's own pool");
        }
    }
    chosen
}

/// What FFmpeg's D3D11VA path rounds HEVC decode surfaces up to
/// (`dxva2.c`, `ff_dxva2_common_frame_params`).
const HEVC_SURFACE_ALIGNMENT: i32 = 128;

/// Gives an HEVC decoder a D3D12 frame pool sized as D3D11VA's is: rounded
/// up to [`HEVC_SURFACE_ALIGNMENT`] (M12c). FFmpeg 8.1's D3D12VA path sizes
/// it to the picture alone (`d3d12va_decode.c`), and NVIDIA's driver then
/// refuses to make the texture for heights not a multiple of 32 such as 720
/// or 1070 ("hardware accelerator failed to decode picture"). Decoded frames
/// keep the picture's size; their resources are larger.
///
/// # Safety
/// Only inside FFmpeg's `get_format` callback, with its context.
unsafe fn aligned_d3d12_frames(ctx: *mut ffi::AVCodecContext) -> Result<(), i32> {
    let align =
        |v: i32| (v + HEVC_SURFACE_ALIGNMENT - 1) / HEVC_SURFACE_ALIGNMENT * HEVC_SURFACE_ALIGNMENT;
    // SAFETY: FFmpeg's documented way to supply a decoder's frames from
    // get_format: the parameters it would use, adjusted, initialised, and
    // handed over in `hw_frames_ctx` (whose old reference is released).
    unsafe {
        let mut frames: *mut ffi::AVBufferRef = std::ptr::null_mut();
        let got = ffi::avcodec_get_hw_frames_parameters(
            ctx,
            (*ctx).hw_device_ctx,
            ffi::AV_PIX_FMT_D3D12,
            &mut frames,
        );
        if got < 0 {
            return Err(got);
        }
        let params = (*frames).data.cast::<ffi::AVHWFramesContext>();
        (*params).width = align((*ctx).coded_width);
        (*params).height = align((*ctx).coded_height);
        let ready = ffi::av_hwframe_ctx_init(frames);
        if ready < 0 {
            ffi::av_buffer_unref(&mut frames);
            return Err(ready);
        }
        ffi::av_buffer_unref(&mut (*ctx).hw_frames_ctx);
        (*ctx).hw_frames_ctx = frames;
    }
    Ok(())
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
                let matrix = matrix(&frame);
                let surface = D3d12Surface::new(frame).map_err(MediaError::Backend)?;
                out.push(DecodedFrame::Gpu(GpuNv12Frame {
                    width,
                    height,
                    matrix,
                    captured_at,
                    surface: Box::new(surface),
                }));
                continue;
            }
            let matrix = matrix(&frame);
            let frame = if frame.format == ffi::AV_PIX_FMT_D3D11 {
                let mut cpu = AVFrame::new();
                cpu.hwframe_transfer_data(&frame)
                    .map_err(|e| MediaError::Backend(format!("D3D11 to CPU copy: {e}")))?;
                cpu
            } else {
                frame
            };
            out.push(DecodedFrame::CpuNv12(to_nv12(&frame, matrix, captured_at)?));
        }
        Ok(out)
    }
}

/// The matrix the stream declares for this picture. NVENC's RGB input says
/// BT.470BG (BT.601); our other encoders BT.709. A stream that says nothing
/// is taken as BT.709, as before.
fn matrix(frame: &AVFrame) -> YuvMatrix {
    match frame.colorspace {
        ffi::AVCOL_SPC_BT470BG | ffi::AVCOL_SPC_SMPTE170M => YuvMatrix::Bt601,
        _ => YuvMatrix::Bt709,
    }
}

/// Copies a decoded NV12 or YUV420P frame into a tightly packed [`Nv12Frame`].
fn to_nv12(
    frame: &AVFrame,
    matrix: YuvMatrix,
    captured_at: Micros,
) -> Result<Nv12Frame, MediaError> {
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
        matrix,
        captured_at,
    })
}
