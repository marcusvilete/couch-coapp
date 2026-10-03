//! Checks which encoders and hardware decoders work on this machine.

use std::ffi::{CStr, CString};

use rsmpeg::{
    avcodec::{AVCodec, AVCodecContext},
    avutil::{AVDictionary, AVHWDeviceContext},
    ffi,
};

/// H.264 encoders worth trying, best first. Only the ones present in the
/// FFmpeg build and supported by this machine's GPU will open.
pub const H264_ENCODERS: &[&str] = &[
    "h264_nvenc",
    "h264_amf",
    "h264_qsv",
    "h264_mf",
    "libopenh264",
];

#[derive(Debug, Clone)]
pub struct EncoderProbe {
    pub name: &'static str,
    pub hardware: bool,
    /// `Ok` if the encoder opened at 1080p60 with low-latency settings.
    pub result: Result<(), String>,
}

#[derive(Debug, Clone)]
pub struct FfmpegProbe {
    pub version: String,
    pub encoders: Vec<EncoderProbe>,
    /// `Ok` if a D3D11VA device could be created for hardware decoding.
    pub d3d11va: Result<(), String>,
}

impl FfmpegProbe {
    /// The first hardware H.264 encoder that opened.
    pub fn best_hardware_encoder(&self) -> Option<&'static str> {
        self.encoders
            .iter()
            .find(|e| e.hardware && e.result.is_ok())
            .map(|e| e.name)
    }
}

pub fn probe() -> FfmpegProbe {
    // Encoders log their own failures; the probe reports them instead.
    unsafe { ffi::av_log_set_level(ffi::AV_LOG_FATAL as i32) };

    let version = rsmpeg::avutil::version_info()
        .to_string_lossy()
        .into_owned();
    let encoders = H264_ENCODERS
        .iter()
        .map(|&name| EncoderProbe {
            name,
            hardware: name != "libopenh264",
            result: try_open_encoder(name),
        })
        .collect();
    let d3d11va = AVHWDeviceContext::create(ffi::AV_HWDEVICE_TYPE_D3D11VA, None, None, 0)
        .map(|_| ())
        .map_err(|e| e.to_string());

    unsafe { ffi::av_log_set_level(ffi::AV_LOG_WARNING as i32) };
    FfmpegProbe {
        version,
        encoders,
        d3d11va,
    }
}

fn try_open_encoder(name: &str) -> Result<(), String> {
    let cname = CString::new(name).expect("encoder names have no NUL");
    let codec = AVCodec::find_encoder_by_name(&cname)
        .ok_or_else(|| "not included in this FFmpeg build".to_string())?;
    let pix_fmt = preferred_pixel_format(&codec)?;

    let mut ctx = AVCodecContext::new(&codec);
    ctx.set_width(1920);
    ctx.set_height(1080);
    ctx.set_pix_fmt(pix_fmt);
    ctx.set_time_base(ffi::AVRational { num: 1, den: 60 });
    ctx.set_framerate(ffi::AVRational { num: 60, den: 1 });
    ctx.set_bit_rate(20_000_000);
    ctx.set_max_b_frames(0);

    let options = (name == "h264_nvenc").then(|| {
        AVDictionary::new(c"preset", c"p1", 0)
            .set(c"tune", c"ull", 0)
            .set(c"rc", c"cbr", 0)
            .set(c"zerolatency", c"1", 0)
            .set(c"delay", c"0", 0)
    });
    ctx.open(options).map(|_| ()).map_err(|e| e.to_string())
}

/// BGRA when the encoder takes it directly (NVENC does), else NV12, else YUV 4:2:0.
fn preferred_pixel_format(codec: &AVCodec) -> Result<i32, String> {
    let supported = codec.pix_fmts().unwrap_or_default();
    [
        ffi::AV_PIX_FMT_BGRA,
        ffi::AV_PIX_FMT_NV12,
        ffi::AV_PIX_FMT_YUV420P,
    ]
    .into_iter()
    .find(|f| supported.contains(f))
    .ok_or_else(|| {
        let name: &CStr = codec.name();
        format!(
            "{} accepts none of BGRA, NV12, YUV420P",
            name.to_string_lossy()
        )
    })
}
