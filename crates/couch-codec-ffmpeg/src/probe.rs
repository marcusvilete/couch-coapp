//! Checks which encoders and hardware decoders work on this machine.

use couch_media::{Codec, EncoderConfig, VideoEncoder};
use rsmpeg::{avutil::AVHWDeviceContext, ffi};

use crate::encoder::FfmpegEncoder;

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
    /// The option set it opened with at 1080p60, as a stream would open it
    /// (`options`), or why it didn't.
    pub result: Result<String, String>,
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

/// Opens `name` exactly as a stream would (`FfmpegEncoder::open`, with its
/// option sets) at 1080p60, and says which option set it took.
fn try_open_encoder(name: &str) -> Result<String, String> {
    let config = EncoderConfig {
        codec: Codec::H264,
        width: 1920,
        height: 1080,
        fps: 60,
        bitrate_bps: 20_000_000,
    };
    FfmpegEncoder::open(name, config)
        .map(|encoder| encoder.caps().options.clone())
        .map_err(|e| e.to_string())
}
