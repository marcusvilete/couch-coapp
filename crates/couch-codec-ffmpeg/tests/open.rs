//! Opening every encoder, twice, must never crash the process, whatever this
//! machine has. Before M9a, any failed open that carried options (AMF or Quick
//! Sync on a PC without that GPU) was a double free inside rsmpeg's
//! `AVCodecContext::open`; `OptionSet::open` calls `avcodec_open2` itself.
//! Which encoders open depends on the GPU, so only "no crash" is checked.

use couch_codec_ffmpeg::{encoder::FfmpegEncoder, options::option_sets, probe::H264_ENCODERS};
use couch_media::{Codec, EncoderConfig, VideoEncoder};

#[test]
fn trying_every_encoder_never_crashes() {
    let config = EncoderConfig {
        codec: Codec::H264,
        width: 1280,
        height: 720,
        fps: 60,
        bitrate_bps: 10_000_000,
    };
    for _ in 0..2 {
        for &name in H264_ENCODERS {
            match FfmpegEncoder::open(name, config) {
                Ok(encoder) => {
                    let label = &encoder.caps().options;
                    assert!(
                        option_sets(name).iter().any(|s| s.label == *label),
                        "{name} opened with an unknown set {label}"
                    );
                }
                Err(e) => eprintln!("{name}: {e}"),
            }
        }
    }
}
