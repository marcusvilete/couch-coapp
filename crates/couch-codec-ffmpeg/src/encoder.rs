//! H.264 encoding through FFmpeg, tuned for latency: no B-frames, an
//! effectively infinite GOP with keyframes on demand, constant bitrate with a
//! one-frame rate buffer, and output after every input frame.

use std::{ffi::CString, time::Instant};

use bytes::Bytes;
use couch_media::{
    BgraFrame, Codec, EncodedFrame, EncoderCaps, EncoderConfig, EncoderInput, MediaError,
    VideoEncoder,
};
use rsmpeg::{
    avcodec::{AVCodec, AVCodecContext},
    avutil::{AVDictionary, AVFrame},
    error::RsmpegError,
    ffi,
    swscale::SwsContext,
};

use crate::probe::H264_ENCODERS;

pub struct FfmpegEncoder {
    ctx: AVCodecContext,
    /// Input frame in the encoder's pixel format, reused every frame.
    frame: AVFrame,
    /// BGRA-to-encoder-format conversion, for encoders that don't take BGRA.
    scaler: Option<SwsContext>,
    caps: EncoderCaps,
    config: EncoderConfig,
    last_pts: i64,
    pending_bitrate: Option<u32>,
}

// The codec context is only ever used from the thread that owns the encoder.
unsafe impl Send for FfmpegEncoder {}

impl FfmpegEncoder {
    /// Opens the best H.264 encoder that works on this machine, hardware first.
    pub fn open_best(config: EncoderConfig) -> Result<Self, MediaError> {
        let mut errors = Vec::new();
        for &name in H264_ENCODERS {
            match Self::open(name, config) {
                Ok(encoder) => return Ok(encoder),
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        Err(MediaError::Backend(format!(
            "no H.264 encoder opened ({})",
            errors.join("; ")
        )))
    }

    pub fn open(name: &str, config: EncoderConfig) -> Result<Self, MediaError> {
        let Codec::H264 = config.codec else {
            return Err(MediaError::Unsupported(format!("{:?}", config.codec)));
        };
        let cname = CString::new(name).map_err(|e| MediaError::Backend(e.to_string()))?;
        let codec = AVCodec::find_encoder_by_name(&cname).ok_or_else(|| {
            MediaError::Unsupported(format!("{name} is not in this FFmpeg build"))
        })?;
        let pix_fmt = input_format(&codec)?;

        let mut ctx = AVCodecContext::new(&codec);
        ctx.set_width(config.width as i32);
        ctx.set_height(config.height as i32);
        ctx.set_pix_fmt(pix_fmt);
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
        ctx.open(Some(low_latency_options(name)))
            .map_err(|e| MediaError::Backend(format!("{name}: {e}")))?;

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

        Ok(Self {
            ctx,
            frame,
            scaler,
            caps: EncoderCaps {
                name: name.to_string(),
                hardware: name != "libopenh264",
                // NVENC resets with an IDR; the others are reopened (`encode`).
                bitrate_change_costs_keyframe: true,
            },
            config,
            last_pts: i64::MIN,
            pending_bitrate: None,
        })
    }

    pub fn config(&self) -> EncoderConfig {
        self.config
    }

    fn fill_frame(&mut self, src: &BgraFrame) -> Result<(), MediaError> {
        self.frame
            .make_writable()
            .map_err(|e| MediaError::Backend(e.to_string()))?;
        match &mut self.scaler {
            None => {
                let dst_stride = self.frame.linesize[0] as usize;
                let dst = self.frame.data[0];
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
                    .scale_frame(&view, 0, src.height as i32, &mut self.frame)
                    .map_err(|e| MediaError::Backend(e.to_string()))?;
                // `view` doesn't own its data; make sure FFmpeg won't try to free it.
                view.data_mut()[0] = std::ptr::null_mut();
            }
        }
        Ok(())
    }
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
        let EncoderInput::Cpu(src) = input else {
            return Err(MediaError::Unsupported("GPU input".into()));
        };
        if (src.width, src.height) != (self.config.width, self.config.height) {
            return Err(MediaError::Backend(format!(
                "frame is {}x{}, encoder was opened for {}x{}",
                src.width, src.height, self.config.width, self.config.height
            )));
        }
        if let Some(bps) = self.pending_bitrate.take() {
            if self.caps.name == "h264_nvenc" {
                self.ctx.set_bit_rate(i64::from(bps));
                unsafe { set_rate_buffer(self.ctx.as_mut_ptr(), bps, self.config.fps) };
                self.config.bitrate_bps = bps;
            } else {
                // The other wrappers (libopenh264, Media Foundation, and AMF
                // and Quick Sync as far as we can tell without the hardware)
                // read the rate only when they open. A fresh encoder starts
                // with a keyframe, the same price NVENC pays.
                let name = self.caps.name.clone();
                *self = Self::open(
                    &name,
                    EncoderConfig {
                        bitrate_bps: bps,
                        ..self.config
                    },
                )?;
            }
        }

        self.fill_frame(src)?;
        let pts = (src.captured_at.0 as i64).max(self.last_pts.saturating_add(1));
        self.last_pts = pts;
        self.frame.set_pts(pts);
        self.frame.set_pict_type(if force_keyframe {
            ffi::AV_PICTURE_TYPE_I
        } else {
            ffi::AV_PICTURE_TYPE_NONE
        });

        let started = Instant::now();
        self.ctx
            .send_frame(Some(&self.frame))
            .map_err(|e| MediaError::Backend(e.to_string()))?;
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
                        captured_at: src.captured_at,
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

fn low_latency_options(name: &str) -> AVDictionary {
    let base = AVDictionary::new(c"profile", c"high", 0);
    match name {
        "h264_nvenc" => base
            .set(c"preset", c"p1", 0)
            .set(c"tune", c"ull", 0)
            .set(c"rc", c"cbr", 0)
            .set(c"zerolatency", c"1", 0)
            .set(c"delay", c"0", 0)
            .set(c"rc-lookahead", c"0", 0)
            .set(c"forced-idr", c"1", 0),
        "h264_amf" => base
            .set(c"usage", c"ultralowlatency", 0)
            .set(c"rc", c"cbr", 0),
        "h264_qsv" => base
            .set(c"preset", c"veryfast", 0)
            .set(c"async_depth", c"1", 0)
            .set(c"low_delay_brc", c"1", 0)
            .set(c"forced_idr", c"1", 0),
        "h264_mf" => base
            .set(c"rate_control", c"cbr", 0)
            .set(c"scenario", c"display_remoting", 0)
            .set(c"hw_encoding", c"1", 0),
        _ => AVDictionary::new(c"allow_skip_frames", c"0", 0),
    }
}
