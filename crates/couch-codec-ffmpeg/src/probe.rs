//! Checks which encoders and hardware decoders work on this machine.

use couch_media::{Codec, EncoderConfig, VideoEncoder};
use rsmpeg::ffi;

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

/// HEVC encoders worth trying (M12c): NVIDIA's only, for now. AMD's and
/// Intel's (`hevc_amf`, `hevc_qsv`) wait for testing on their hardware.
pub const HEVC_ENCODERS: &[&str] = &["hevc_nvenc"];

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
    /// Each of this system's hardware decoders (D3D11VA; on Linux VAAPI and
    /// CUDA) and whether its device could be made here.
    pub decoders: Vec<(&'static str, Result<(), String>)>,
}

impl FfmpegProbe {
    /// The first hardware H.264 encoder that opened.
    pub fn best_hardware_encoder(&self) -> Option<&'static str> {
        self.encoders
            .iter()
            .find(|e| e.hardware && e.result.is_ok() && H264_ENCODERS.contains(&e.name))
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
        .map(|&name| (Codec::H264, name))
        .chain(HEVC_ENCODERS.iter().map(|&name| (Codec::Hevc, name)))
        .map(|(codec, name)| EncoderProbe {
            name,
            hardware: name != "libopenh264",
            result: try_open_encoder(codec, name),
        })
        .collect();
    let decoders = crate::decoder::hardware_decoders();

    unsafe { ffi::av_log_set_level(ffi::AV_LOG_WARNING as i32) };
    FfmpegProbe {
        version,
        encoders,
        decoders,
    }
}

/// Opens `name` exactly as a stream would (`FfmpegEncoder::open`, with its
/// option sets) at 1080p60, and says which option set it took.
fn try_open_encoder(codec: Codec, name: &str) -> Result<String, String> {
    let config = EncoderConfig {
        codec,
        width: 1920,
        height: 1080,
        fps: 60,
        bitrate_bps: 20_000_000,
    };
    FfmpegEncoder::open(name, config)
        .map(|encoder| encoder.caps().options.clone())
        .map_err(|e| e.to_string())
}
