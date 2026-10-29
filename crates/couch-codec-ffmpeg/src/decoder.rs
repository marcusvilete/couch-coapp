//! H.264 and HEVC decoding through FFmpeg. Low-delay mode and a single
//! thread, so every packet yields its frame at once. The ways:
//! - D3D12VA on the renderer's own device, frames left on the GPU (M9c);
//! - on Windows D3D11VA, on Linux VAAPI or CUDA (NVDEC; M16b), frames copied
//!   to memory as NV12;
//! - software, when none is available.

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
    /// VAAPI (Linux: AMD, Intel, the Steam Deck); frames are copied to memory.
    Vaapi,
    /// CUDA, NVDEC (Linux on NVIDIA); frames are copied to memory.
    Cuda,
    Software,
}

/// A hardware decoder to try, in order, for frames copied to memory.
struct Hardware {
    device: ffi::AVHWDeviceType,
    get_format: unsafe extern "C" fn(
        *mut ffi::AVCodecContext,
        *const ffi::AVPixelFormat,
    ) -> ffi::AVPixelFormat,
    kind: DecoderKind,
    name: &'static str,
}

#[cfg(windows)]
const HARDWARE: &[Hardware] = &[Hardware {
    device: ffi::AV_HWDEVICE_TYPE_D3D11VA,
    get_format: prefer_d3d11,
    kind: DecoderKind::D3d11,
    name: "D3D11VA",
}];

/// VAAPI first: the Steam Deck's, AMD's and Intel's; NVIDIA's driver offers
/// CUDA instead (M16b).
#[cfg(not(windows))]
const HARDWARE: &[Hardware] = &[
    Hardware {
        device: ffi::AV_HWDEVICE_TYPE_VAAPI,
        get_format: prefer_vaapi,
        kind: DecoderKind::Vaapi,
        name: "VAAPI",
    },
    Hardware {
        device: ffi::AV_HWDEVICE_TYPE_CUDA,
        get_format: prefer_cuda,
        kind: DecoderKind::Cuda,
        name: "CUDA",
    },
];

pub struct FfmpegDecoder {
    ctx: AVCodecContext,
    /// What decodes now: `opened`, or software while FFmpeg falls back.
    kind: DecoderKind,
    /// What it was opened with.
    opened: DecoderKind,
    codec: Codec,
    /// A full-range picture was said once (M16b).
    full_range_said: bool,
}

// Only ever used from the thread that owns the decoder.
unsafe impl Send for FfmpegDecoder {}

/// Each of this system's hardware decoders for frames in memory (D3D11VA;
/// VAAPI, CUDA), and whether its device can be made here, for the probe.
pub fn hardware_decoders() -> Vec<(&'static str, Result<(), String>)> {
    HARDWARE
        .iter()
        .map(|h| {
            let made = AVHWDeviceContext::create(h.device, None, None, 0)
                .map(|_| ())
                .map_err(|e| e.to_string());
            (h.name, made)
        })
        .collect()
}

impl FfmpegDecoder {
    /// A `codec` decoder that uses the first of this system's hardware
    /// decoders ([`HARDWARE`]) the GPU allows, if `prefer_hardware`.
    pub fn new(codec: Codec, prefer_hardware: bool) -> Result<Self, MediaError> {
        let mut ctx = Self::context(codec)?;
        let mut kind = DecoderKind::Software;
        if prefer_hardware {
            let mut tried = Vec::new();
            for hardware in HARDWARE {
                match AVHWDeviceContext::create(hardware.device, None, None, 0) {
                    Ok(device) => {
                        ctx.set_hw_device_ctx(device);
                        ctx.set_get_format(Some(hardware.get_format));
                        kind = hardware.kind;
                        break;
                    }
                    Err(e) => tried.push(format!("{}: {e}", hardware.name)),
                }
            }
            if kind == DecoderKind::Software {
                tracing::warn!(
                    "no hardware decoder ({}); decoding in software",
                    tried.join("; ")
                );
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
        Ok(Self {
            ctx,
            kind,
            opened: kind,
            codec,
            full_range_said: false,
        })
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

#[cfg(windows)]
unsafe extern "C" fn prefer_d3d11(
    _ctx: *mut ffi::AVCodecContext,
    formats: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    // SAFETY: FFmpeg passes a list ended by AV_PIX_FMT_NONE.
    unsafe { prefer(formats, ffi::AV_PIX_FMT_D3D11) }
}

#[cfg(not(windows))]
unsafe extern "C" fn prefer_vaapi(
    _ctx: *mut ffi::AVCodecContext,
    formats: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    // SAFETY: as above.
    unsafe { prefer(formats, ffi::AV_PIX_FMT_VAAPI) }
}

#[cfg(not(windows))]
unsafe extern "C" fn prefer_cuda(
    _ctx: *mut ffi::AVCodecContext,
    formats: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    // SAFETY: as above.
    unsafe { prefer(formats, ffi::AV_PIX_FMT_CUDA) }
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
            // Our shader takes limited range, as our encoders send (M16b).
            if frame.color_range == ffi::AVCOL_RANGE_JPEG && !self.full_range_said {
                self.full_range_said = true;
                tracing::warn!("a full-range picture: shown as limited range, its contrast off");
            }
            // A hardware frame (D3D11VA, VAAPI, CUDA) comes to memory, in the
            // format FFmpeg picks: NV12 for 8-bit video.
            let frame = if !frame.hw_frames_ctx.is_null() {
                // Back in hardware after a fallback (FFmpeg picks the format
                // again on a new picture size): say so too.
                if self.kind != self.opened {
                    tracing::info!("{:?} decodes this stream again", self.opened);
                    self.kind = self.opened;
                }
                let mut cpu = AVFrame::new();
                cpu.hwframe_transfer_data(&frame).map_err(|e| {
                    MediaError::Backend(format!("{:?} to memory copy: {e}", self.kind))
                })?;
                cpu
            } else {
                // FFmpeg falls back to software inside a hardware decoder that
                // can't start: say so, as the stats panel shows it.
                if self.kind != DecoderKind::Software {
                    tracing::warn!(
                        "{:?} couldn't decode this stream; decoding in software",
                        self.kind
                    );
                    self.kind = DecoderKind::Software;
                }
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

/// Copies a decoded NV12 or YUV420P frame (or YUVJ420P, its full-range
/// twin, decoded the same; our encoders don't send it) into a tightly packed
/// [`Nv12Frame`].
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
        f if f == ffi::AV_PIX_FMT_YUV420P || f == ffi::AV_PIX_FMT_YUVJ420P => {
            let cw = w.div_ceil(2);
            let (u, v) = (plane(1, cw, chroma_h), plane(2, cw, chroma_h));
            let mut uv = vec![0; u.len() * 2];
            for (pair, (&u, &v)) in uv.as_chunks_mut::<2>().0.iter_mut().zip(u.iter().zip(&v)) {
                *pair = [u, v];
            }
            uv
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4x2 planar frame, Y counting up, U 10s, V 20s per chroma sample.
    fn planar(format: ffi::AVPixelFormat) -> AVFrame {
        let mut frame = AVFrame::new();
        frame.set_width(4);
        frame.set_height(2);
        frame.set_format(format);
        frame.alloc_buffer().unwrap();
        // SAFETY: the buffers were just allocated for this size and format.
        unsafe {
            for row in 0..2 {
                for x in 0..4 {
                    *frame.data[0].add(row * frame.linesize[0] as usize + x) = (row * 4 + x) as u8;
                }
            }
            for x in 0..2 {
                *frame.data[1].add(x) = 10 + x as u8;
                *frame.data[2].add(x) = 20 + x as u8;
            }
        }
        frame
    }

    /// Software frames (and YUVJ420P, full range) come out as NV12, U and V
    /// interleaved, whatever the planes' strides (M16b).
    #[test]
    fn planar_frames_become_nv12() {
        for format in [ffi::AV_PIX_FMT_YUV420P, ffi::AV_PIX_FMT_YUVJ420P] {
            let nv12 = to_nv12(&planar(format), YuvMatrix::Bt709, Micros(0)).unwrap();
            assert_eq!(&nv12.y[..], &[0, 1, 2, 3, 4, 5, 6, 7]);
            assert_eq!(&nv12.uv[..], &[10, 20, 11, 21]);
            assert_eq!((nv12.y_stride, nv12.uv_stride), (4, 4));
        }
    }
}
