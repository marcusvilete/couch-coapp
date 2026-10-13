//! H.264 decoding through FFmpeg: D3D11VA when available, software otherwise.
//! Low-delay mode and a single thread, so every packet yields its frame at once.
//! Frames are copied to CPU memory as NV12; the zero-copy path is M9.

use bytes::Bytes;
use couch_media::{DecodedFrame, MediaError, Micros, Nv12Frame, VideoDecoder};
use rsmpeg::{
    avcodec::{AVCodec, AVCodecContext, AVPacket},
    avutil::{AVFrame, AVHWDeviceContext},
    error::RsmpegError,
    ffi,
};

pub struct FfmpegDecoder {
    ctx: AVCodecContext,
    hardware: bool,
}

// Only ever used from the thread that owns the decoder.
unsafe impl Send for FfmpegDecoder {}

impl FfmpegDecoder {
    /// A decoder that uses D3D11VA if `prefer_hardware` and the GPU allows it.
    pub fn new(prefer_hardware: bool) -> Result<Self, MediaError> {
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

        let mut hardware = false;
        if prefer_hardware {
            match AVHWDeviceContext::create(ffi::AV_HWDEVICE_TYPE_D3D11VA, None, None, 0) {
                Ok(device) => {
                    ctx.set_hw_device_ctx(device);
                    ctx.set_get_format(Some(prefer_d3d11));
                    hardware = true;
                }
                Err(e) => tracing::warn!("D3D11VA unavailable, decoding in software: {e}"),
            }
        }
        ctx.open(None)
            .map_err(|e| MediaError::Backend(format!("opening the H.264 decoder: {e}")))?;
        Ok(Self { ctx, hardware })
    }

    pub fn is_hardware(&self) -> bool {
        self.hardware
    }
}

/// Picks D3D11 surfaces when offered, else the decoder's first (software) choice.
unsafe extern "C" fn prefer_d3d11(
    _ctx: *mut ffi::AVCodecContext,
    formats: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    unsafe {
        let mut p = formats;
        while *p != ffi::AV_PIX_FMT_NONE {
            if *p == ffi::AV_PIX_FMT_D3D11 {
                return *p;
            }
            p = p.add(1);
        }
        *formats
    }
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
