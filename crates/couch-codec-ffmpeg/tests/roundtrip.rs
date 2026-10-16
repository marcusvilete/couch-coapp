//! Encode the test pattern, decode it, and read the timestamp barcode back
//! out of the decoded luma: proves the codec path keeps frames intact enough
//! for latency measurement.

use std::sync::Arc;

use couch_codec_ffmpeg::{FfmpegDecoder, FfmpegEncoder};
use couch_media::{
    BgraFrame, Codec, DecodedFrame, EncoderConfig, EncoderInput, Micros, VideoDecoder,
    VideoEncoder, YuvMatrix, barcode,
    clock::ProcessClock,
    testpattern::{BARCODE_CELL, BARCODE_X, BARCODE_Y, TestPattern},
};

fn roundtrip(codec: Codec, encoder_name: &str, hardware_decode: bool, width: u32, height: u32) {
    let config = EncoderConfig {
        codec,
        width,
        height,
        fps: 60,
        bitrate_bps: 12_000_000,
    };
    let mut encoder = FfmpegEncoder::open(encoder_name, config).expect("encoder opens");
    let mut decoder = FfmpegDecoder::new(codec, hardware_decode).expect("decoder opens");
    let mut pattern = TestPattern::new(width, height, 60, Arc::new(ProcessClock::new()));

    let (mut decoded, mut barcodes_ok) = (0, 0);
    for i in 0..20 {
        let frame = pattern.render();
        let units = encoder
            .encode(EncoderInput::Cpu(&frame), i == 0)
            .expect("encode");
        assert_eq!(
            units.len(),
            1,
            "low-latency encoders emit one unit per frame"
        );
        assert_eq!(
            units[0].keyframe,
            i == 0,
            "only the forced frame is a keyframe"
        );

        for out in decoder
            .decode(&units[0].data, frame.captured_at)
            .expect("decode")
        {
            let DecodedFrame::CpuNv12(nv12) = out else {
                panic!("expected an NV12 frame");
            };
            assert_eq!((nv12.width, nv12.height), (width, height));
            decoded += 1;
            let value = barcode::read_luma(
                &nv12.y,
                nv12.y_stride as usize,
                (nv12.width, nv12.height),
                BARCODE_X,
                BARCODE_Y,
                BARCODE_CELL,
            );
            if value == Some(frame.captured_at.0 & barcode::MAX_VALUE) {
                barcodes_ok += 1;
            }
        }
    }
    assert_eq!(
        decoded, 20,
        "every frame decodes immediately (no reorder delay)"
    );
    assert!(
        barcodes_ok >= 18,
        "barcode survived in only {barcodes_ok}/20 frames"
    );
}

#[test]
fn software_roundtrip_keeps_the_barcode() {
    roundtrip(Codec::H264, "libopenh264", false, 1280, 720);
}

/// A solid BGRA frame.
fn solid(width: u32, height: u32, bgra: [u8; 4], at: u64) -> BgraFrame {
    BgraFrame {
        width,
        height,
        stride: width * 4,
        data: bgra.repeat((width * height) as usize).into(),
        captured_at: Micros(at),
    }
}

/// Mean luma of the decoded frames of `units`, and the matrix each says it
/// was made with.
fn decoded_luma(decoder: &mut FfmpegDecoder, data: &[u8], at: Micros) -> Vec<(f64, YuvMatrix)> {
    decoder
        .decode(data, at)
        .expect("decode")
        .into_iter()
        .map(|out| {
            let DecodedFrame::CpuNv12(nv12) = out else {
                panic!("expected an NV12 frame");
            };
            let row = &nv12.y[..nv12.width as usize];
            let luma = row.iter().map(|&v| f64::from(v)).sum::<f64>() / row.len() as f64;
            (luma, nv12.matrix)
        })
        .collect()
}

/// Pure red through `encoder_name` and back: its luma and the matrix the
/// stream declares. Pure red is luma 63 in BT.709 limited range, 82 in
/// BT.601, so the two must agree for the viewer to show red (M12c).
fn red_through(codec: Codec, encoder_name: &str, hardware_decode: bool) -> (f64, YuvMatrix) {
    let config = EncoderConfig {
        codec,
        width: 320,
        height: 240,
        fps: 60,
        bitrate_bps: 4_000_000,
    };
    let mut encoder = FfmpegEncoder::open(encoder_name, config).expect("encoder opens");
    let mut decoder = FfmpegDecoder::new(codec, hardware_decode).expect("decoder opens");
    let red = solid(320, 240, [0, 0, 255, 255], 1);
    let units = encoder
        .encode(EncoderInput::Cpu(&red), true)
        .expect("encode");
    decoded_luma(&mut decoder, &units[0].data, red.captured_at)[0]
}

#[test]
fn software_conversion_uses_bt709_and_says_so() {
    let (luma, matrix) = red_through(Codec::H264, "libopenh264", false);
    assert!((luma - 63.0).abs() < 4.0, "luma {luma}");
    assert_eq!(matrix, YuvMatrix::Bt709);
}

/// FFmpeg's NVENC wrapper converts RGB with BT.601 whatever colorspace is
/// set, and declares it; the viewer must follow the declaration.
#[cfg(feature = "gpu-tests")]
#[test]
fn nvenc_converts_with_bt601_and_says_so() {
    for codec in [Codec::H264, Codec::Hevc] {
        let name = couch_codec_ffmpeg::encoder::gpu_input_encoder(codec);
        let (luma, matrix) = red_through(codec, name, true);
        assert!((luma - 82.0).abs() < 4.0, "{name}: luma {luma}");
        assert_eq!(matrix, YuvMatrix::Bt601, "{name}");
    }
}

#[test]
fn a_bitrate_change_restarts_with_a_keyframe() {
    let config = EncoderConfig {
        codec: Codec::H264,
        width: 320,
        height: 240,
        fps: 60,
        bitrate_bps: 4_000_000,
    };
    let mut encoder = FfmpegEncoder::open("libopenh264", config).expect("encoder opens");
    let mut decoder = FfmpegDecoder::new(Codec::H264, false).expect("decoder opens");
    for i in 0..6u64 {
        if i == 3 {
            encoder.set_bitrate(1_000_000).expect("set bitrate");
        }
        let frame = solid(320, 240, [(i * 40) as u8, 128, 64, 255], i + 1);
        let units = encoder
            .encode(EncoderInput::Cpu(&frame), i == 0)
            .expect("encode");
        assert_eq!(units[0].keyframe, i == 0 || i == 3, "frame {i}");
        assert_eq!(
            decoded_luma(&mut decoder, &units[0].data, frame.captured_at).len(),
            1
        );
    }
    assert_eq!(encoder.config().bitrate_bps, 1_000_000);
}

#[cfg(feature = "gpu-tests")]
#[test]
fn nvenc_and_d3d11va_roundtrip_keeps_the_barcode() {
    roundtrip(Codec::H264, "h264_nvenc", true, 1920, 1080);
}

#[cfg(feature = "gpu-tests")]
#[test]
fn hevc_nvenc_and_d3d11va_roundtrip_keeps_the_barcode() {
    roundtrip(Codec::Hevc, "hevc_nvenc", true, 1920, 1080);
}
