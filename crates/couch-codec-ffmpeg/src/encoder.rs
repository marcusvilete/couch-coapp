//! H.264 and HEVC encoding through FFmpeg, tuned for latency: no B-frames,
//! an effectively infinite GOP with keyframes on demand, constant bitrate
//! with a one-frame rate buffer, and output after every input frame.

use std::{ffi::CString, time::Instant};

use bytes::Bytes;
use couch_media::{
    BgraFrame, Codec, EncodedFrame, EncoderCaps, EncoderConfig, EncoderInput, GpuFrame, MediaError,
    VideoEncoder,
};
use rsmpeg::{
    avcodec::{AVCodec, AVCodecContext},
    avutil::AVFrame,
    error::RsmpegError,
    ffi,
    swscale::SwsContext,
};

use crate::{
    d3d11::D3d11Frames,
    options::{self, OptionSet, option_sets},
    probe::{H264_ENCODERS, HEVC_ENCODERS},
};

/// The one encoder of each codec that takes GPU frames (M9b): NVENC reads
/// the capture's BGRA textures and converts them to YUV itself, as it does
/// with BGRA from memory. AMD and Intel would need testing on their hardware
/// first.
pub fn gpu_input_encoder(codec: Codec) -> &'static str {
    match codec {
        Codec::Hevc => "hevc_nvenc",
        _ => "h264_nvenc",
    }
}

/// The encoders worth trying for `codec`, best first. HEVC is NVENC's only
/// (M12c): AMD's and Intel's wait for their hardware.
pub fn encoders(codec: Codec) -> &'static [&'static str] {
    match codec {
        Codec::Hevc => HEVC_ENCODERS,
        _ => H264_ENCODERS,
    }
}

/// NVENC changes its bitrate in place; the others reopen.
fn is_nvenc(name: &str) -> bool {
    matches!(name, "h264_nvenc" | "hevc_nvenc")
}

pub struct FfmpegEncoder {
    // Field order is drop order: the codec context (NVENC's session, which
    // unregisters its textures) goes before `input`, which keeps them alive.
    ctx: AVCodecContext,
    input: Input,
    caps: EncoderCaps,
    config: EncoderConfig,
    last_pts: i64,
    pending_bitrate: Option<u32>,
    /// The index of the option set that opened (`options::option_sets`), so
    /// reopening for a new bitrate doesn't retry the ones that failed.
    option_set: usize,
}

/// How frames reach the encoder.
enum Input {
    /// Copied into a frame in the encoder's pixel format, reused every frame.
    Cpu {
        frame: AVFrame,
        /// BGRA-to-encoder-format conversion, for encoders that don't take BGRA.
        scaler: Option<SwsContext>,
    },
    /// D3D11 textures on the capture's device.
    Gpu(D3d11Frames),
}

// The codec context is only ever used from the thread that owns the encoder;
// the D3D11 contexts are FFmpeg's reference-counted buffers.
unsafe impl Send for FfmpegEncoder {}

impl FfmpegEncoder {
    /// Opens [`gpu_input_encoder`] for GPU frames like `frame`: on its device,
    /// at its size. Fails where NVENC can't use that device (another GPU's,
    /// such as a laptop's integrated graphics), so the caller can open an
    /// encoder for frames in memory instead.
    pub fn open_gpu(config: EncoderConfig, frame: &GpuFrame) -> Result<Self, MediaError> {
        if (frame.width, frame.height) != (config.width, config.height) {
            return Err(MediaError::Backend(format!(
                "a {}x{} frame for a {}x{} encoder",
                frame.width, frame.height, config.width, config.height
            )));
        }
        Self::open_from(gpu_input_encoder(config.codec), config, 0, Some(frame))
    }

    /// Opens the best encoder of `config.codec` that works on this machine,
    /// hardware first.
    pub fn open_best(config: EncoderConfig) -> Result<Self, MediaError> {
        let mut errors = Vec::new();
        for &name in encoders(config.codec) {
            match Self::open(name, config) {
                Ok(encoder) => return Ok(encoder),
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        Err(MediaError::Backend(format!(
            "no {:?} encoder opened ({})",
            config.codec,
            errors.join("; ")
        )))
    }

    /// Opens `name` with the first of its option sets the driver accepts
    /// (`options::option_sets`; the last is always the settings from before
    /// M9a).
    pub fn open(name: &str, config: EncoderConfig) -> Result<Self, MediaError> {
        Self::open_from(name, config, 0, None)
    }

    /// Like [`Self::open`], trying the option sets from `first` on, for GPU
    /// frames like `gpu` if given.
    fn open_from(
        name: &str,
        config: EncoderConfig,
        first: usize,
        gpu: Option<&GpuFrame>,
    ) -> Result<Self, MediaError> {
        let sets = option_sets(name);
        let mut errors = Vec::new();
        for (index, set) in sets.iter().enumerate().skip(first) {
            match Self::open_with(name, config, index, set, gpu) {
                Ok(encoder) => {
                    if !errors.is_empty() {
                        tracing::info!(
                            "{name} opened with \"{}\" after: {}",
                            set.label,
                            errors.join("; ")
                        );
                    }
                    return Ok(encoder);
                }
                // Not in this FFmpeg build: no set will help.
                Err(e @ MediaError::Unsupported(_)) => return Err(e),
                Err(e) => errors.push(format!("\"{}\": {e}", set.label)),
            }
        }
        Err(MediaError::Backend(format!(
            "{name} opened with none of its option sets ({})",
            errors.join("; ")
        )))
    }

    fn open_with(
        name: &str,
        config: EncoderConfig,
        index: usize,
        set: &OptionSet,
        gpu: Option<&GpuFrame>,
    ) -> Result<Self, MediaError> {
        if !encoders(config.codec).contains(&name) {
            return Err(MediaError::Unsupported(format!(
                "{name} doesn't encode {:?}",
                config.codec
            )));
        }
        let cname = CString::new(name).map_err(|e| MediaError::Backend(e.to_string()))?;
        let codec = AVCodec::find_encoder_by_name(&cname).ok_or_else(|| {
            MediaError::Unsupported(format!("{name} is not in this FFmpeg build"))
        })?;
        let backend = |e: String| MediaError::Backend(format!("{name}: {e}"));
        let gpu = gpu.map(D3d11Frames::new).transpose().map_err(backend)?;
        let pix_fmt = match gpu {
            Some(_) => ffi::AV_PIX_FMT_D3D11,
            None => input_format(&codec)?,
        };

        let mut ctx = AVCodecContext::new(&codec);
        ctx.set_width(config.width as i32);
        ctx.set_height(config.height as i32);
        ctx.set_pix_fmt(pix_fmt);
        if let Some(frames) = &gpu {
            let frames = frames.frames_ref().map_err(backend)?;
            // SAFETY: the context takes the new reference and frees it.
            unsafe { (*ctx.as_mut_ptr()).hw_frames_ctx = frames };
        }
        // Timestamps are capture times in microseconds.
        ctx.set_time_base(ffi::AVRational {
            num: 1,
            den: 1_000_000,
        });
        ctx.set_framerate(ffi::AVRational {
            num: config.fps as i32,
            den: 1,
        });
        ctx.set_bit_rate(i64::from(config.bitrate_bps));
        // `0` means intra-only to NVENC; `i32::MAX` is effectively infinite.
        ctx.set_gop_size(i32::MAX);
        ctx.set_max_b_frames(0);
        ctx.set_flags(ctx.flags | ffi::AV_CODEC_FLAG_LOW_DELAY as i32);
        unsafe {
            let raw = ctx.as_mut_ptr();
            set_rate_buffer(raw, config.bitrate_bps, config.fps);
            (*raw).colorspace = ffi::AVCOL_SPC_BT709;
            (*raw).color_primaries = ffi::AVCOL_PRI_BT709;
            (*raw).color_trc = ffi::AVCOL_TRC_BT709;
            (*raw).color_range = ffi::AVCOL_RANGE_MPEG;
        }
        // P8's measurement switches, over NVENC's set (none unless set).
        let (extra, extra_label) = if is_nvenc(name) {
            options::nvenc_overrides()
        } else {
            (Vec::new(), String::new())
        };
        let unused = set
            .open_with(&mut ctx, &extra)
            .map_err(|e| MediaError::Backend(format!("{name}: {e}")))?;
        if !unused.is_empty() {
            tracing::warn!("{name} ignored options {unused:?} (\"{}\")", set.label);
        }

        let input = match gpu {
            Some(frames) => Input::Gpu(frames),
            None => {
                let mut frame = AVFrame::new();
                frame.set_width(config.width as i32);
                frame.set_height(config.height as i32);
                frame.set_format(pix_fmt);
                frame
                    .alloc_buffer()
                    .map_err(|e| MediaError::Backend(e.to_string()))?;
                let scaler = if pix_fmt == ffi::AV_PIX_FMT_BGRA {
                    None
                } else {
                    Some(bgra_converter(config, pix_fmt)?)
                };
                Input::Cpu { frame, scaler }
            }
        };

        Ok(Self {
            ctx,
            caps: EncoderCaps {
                name: name.to_string(),
                options: if extra_label.is_empty() {
                    set.label.to_string()
                } else {
                    format!("{}, measuring {extra_label}", set.label)
                },
                hardware: name != "libopenh264",
                gpu_input: matches!(input, Input::Gpu(_)),
                // NVENC resets with an IDR; the others are reopened (`encode`).
                bitrate_change_costs_keyframe: true,
            },
            input,
            config,
            last_pts: i64::MIN,
            pending_bitrate: None,
            option_set: index,
        })
    }

    pub fn config(&self) -> EncoderConfig {
        self.config
    }
}

/// Copies `src` into `frame`, converting if the encoder doesn't take BGRA.
fn fill_frame(
    frame: &mut AVFrame,
    scaler: &mut Option<SwsContext>,
    src: &BgraFrame,
) -> Result<(), MediaError> {
    frame
        .make_writable()
        .map_err(|e| MediaError::Backend(e.to_string()))?;
    match scaler {
        None => {
            let dst_stride = frame.linesize[0] as usize;
            let dst = frame.data[0];
            let row = (src.width * 4) as usize;
            for y in 0..src.height as usize {
                let from = &src.data[y * src.stride as usize..][..row];
                // SAFETY: the frame owns `height` rows of `dst_stride >= row` bytes.
                unsafe {
                    std::ptr::copy_nonoverlapping(from.as_ptr(), dst.add(y * dst_stride), row)
                };
            }
        }
        Some(scaler) => {
            // Describe the caller's buffer without copying it; it outlives the call.
            let mut view = AVFrame::new();
            view.set_width(src.width as i32);
            view.set_height(src.height as i32);
            view.set_format(ffi::AV_PIX_FMT_BGRA);
            view.data_mut()[0] = src.data.as_ptr() as *mut u8;
            view.linesize_mut()[0] = src.stride as i32;
            scaler
                .scale_frame(&view, 0, src.height as i32, frame)
                .map_err(|e| MediaError::Backend(e.to_string()))?;
            // `view` doesn't own its data; make sure FFmpeg won't try to free it.
            view.data_mut()[0] = std::ptr::null_mut();
        }
    }
    Ok(())
}

impl VideoEncoder for FfmpegEncoder {
    fn caps(&self) -> &EncoderCaps {
        &self.caps
    }

    fn encode(
        &mut self,
        input: EncoderInput<'_>,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedFrame>, MediaError> {
        let (width, height, captured_at) = match input {
            EncoderInput::Cpu(f) => (f.width, f.height, f.captured_at),
            EncoderInput::Gpu(f) => (f.width, f.height, f.captured_at),
            _ => return Err(MediaError::Unsupported("this input".into())),
        };
        if (width, height) != (self.config.width, self.config.height) {
            return Err(MediaError::Backend(format!(
                "frame is {width}x{height}, encoder was opened for {}x{}",
                self.config.width, self.config.height
            )));
        }
        if let Some(bps) = self.pending_bitrate.take() {
            if is_nvenc(&self.caps.name) {
                self.ctx.set_bit_rate(i64::from(bps));
                unsafe { set_rate_buffer(self.ctx.as_mut_ptr(), bps, self.config.fps) };
                self.config.bitrate_bps = bps;
            } else {
                // The other wrappers (libopenh264, Media Foundation, and AMF
                // and Quick Sync as far as we can tell without the hardware)
                // read the rate only when they open. A fresh encoder starts
                // with a keyframe, the same price NVENC pays. (Only NVENC
                // takes GPU input, so this one reads frames from memory.)
                let name = self.caps.name.clone();
                *self = Self::open_from(
                    &name,
                    EncoderConfig {
                        bitrate_bps: bps,
                        ..self.config
                    },
                    self.option_set,
                    None,
                )?;
            }
        }

        let pts = (captured_at.0 as i64).max(self.last_pts.saturating_add(1));
        self.last_pts = pts;
        let pict_type = if force_keyframe {
            ffi::AV_PICTURE_TYPE_I
        } else {
            ffi::AV_PICTURE_TYPE_NONE
        };
        let started = Instant::now();
        let sent = match (&mut self.input, input) {
            (Input::Cpu { frame, scaler }, EncoderInput::Cpu(src)) => {
                fill_frame(frame, scaler, src)?;
                frame.set_pts(pts);
                frame.set_pict_type(pict_type);
                self.ctx.send_frame(Some(frame))
            }
            (Input::Gpu(frames), EncoderInput::Gpu(src)) => {
                let mut frame = frames.wrap(src).map_err(MediaError::Backend)?;
                frame.set_pts(pts);
                frame.set_pict_type(pict_type);
                self.ctx.send_frame(Some(&frame))
            }
            (Input::Cpu { .. }, _) => {
                return Err(MediaError::Unsupported(
                    "a GPU frame for an encoder opened for frames in memory".into(),
                ));
            }
            (Input::Gpu(_), _) => {
                return Err(MediaError::Unsupported(
                    "a frame in memory for an encoder opened for GPU frames".into(),
                ));
            }
        };
        sent.map_err(|e| MediaError::Backend(e.to_string()))?;
        let mut out = Vec::new();
        loop {
            match self.ctx.receive_packet() {
                Ok(packet) => {
                    // SAFETY: the packet owns `size` bytes at `data`.
                    let data =
                        unsafe { std::slice::from_raw_parts(packet.data, packet.size as usize) };
                    out.push(EncodedFrame {
                        data: Bytes::copy_from_slice(data),
                        keyframe: packet.flags & ffi::AV_PKT_FLAG_KEY as i32 != 0,
                        captured_at,
                        encode_time_us: started.elapsed().as_micros() as u32,
                    });
                }
                Err(RsmpegError::EncoderDrainError) => break,
                Err(e) => return Err(MediaError::Backend(e.to_string())),
            }
        }
        Ok(out)
    }

    fn set_bitrate(&mut self, bps: u32) -> Result<(), MediaError> {
        if bps != self.config.bitrate_bps {
            self.pending_bitrate = Some(bps);
        }
        Ok(())
    }
}

/// BGRA to the encoder's YUV format. swscale converts with BT.601 unless told
/// otherwise; the stream is tagged BT.709 limited range, and the viewer
/// decodes it that way.
fn bgra_converter(config: EncoderConfig, pix_fmt: i32) -> Result<SwsContext, MediaError> {
    let mut scaler = SwsContext::get_context(
        config.width as i32,
        config.height as i32,
        ffi::AV_PIX_FMT_BGRA,
        config.width as i32,
        config.height as i32,
        pix_fmt,
        ffi::SWS_FAST_BILINEAR,
        None,
        None,
        None,
    )
    .ok_or_else(|| MediaError::Backend("could not create the BGRA converter".into()))?;
    // SAFETY: `scaler` is a valid context; the coefficient tables are static.
    let ret = unsafe {
        ffi::sws_setColorspaceDetails(
            scaler.as_mut_ptr(),
            ffi::sws_getCoefficients(ffi::SWS_CS_DEFAULT as i32),
            1, // source: full-range RGB
            ffi::sws_getCoefficients(ffi::SWS_CS_ITU709 as i32),
            0, // destination: limited range
            0,
            1 << 16,
            1 << 16,
        )
    };
    if ret < 0 {
        return Err(MediaError::Backend(
            "the BGRA converter refused BT.709".into(),
        ));
    }
    Ok(scaler)
}

/// CBR with a one-frame rate buffer: every frame, keyframes included, fits
/// in one frame's share of the bitrate, so nothing bursts onto the network.
unsafe fn set_rate_buffer(ctx: *mut ffi::AVCodecContext, bps: u32, fps: u32) {
    unsafe {
        (*ctx).rc_max_rate = i64::from(bps);
        (*ctx).rc_buffer_size = (bps / fps.max(1)) as i32;
    }
}

fn input_format(codec: &AVCodec) -> Result<i32, MediaError> {
    let supported = codec.pix_fmts().unwrap_or_default();
    [
        ffi::AV_PIX_FMT_BGRA,
        ffi::AV_PIX_FMT_NV12,
        ffi::AV_PIX_FMT_YUV420P,
    ]
    .into_iter()
    .find(|f| supported.contains(f))
    .ok_or_else(|| MediaError::Unsupported("no usable input pixel format".into()))
}
