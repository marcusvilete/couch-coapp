//! NVENC directly (M12d) on D3D11 textures, against FFmpeg's NVENC on the
//! same frames: the same stream headers and pictures, bitrate changes
//! without a keyframe, textures back to their pool, another pool refused.
//! For H.264 and HEVC. Needs an NVIDIA GPU:
//! `cargo test -p couch-codec-nvenc --features gpu-tests --test gpu`. With
//! `COUCH_NVENC_DUMP=<folder>`, each codec's first unit from both encoders is
//! written there, to compare with `ffmpeg -bsf:v trace_headers`.

#![cfg(all(windows, feature = "gpu-tests"))]

use std::sync::Arc;

use couch_codec_ffmpeg::{FfmpegDecoder, FfmpegEncoder};
use couch_codec_nvenc::NvencEncoder;
use couch_media::{
    BgraFrame, Codec, DecodedFrame, EncoderConfig, EncoderInput, Nv12Frame, VideoDecoder,
    VideoEncoder, barcode,
    clock::ProcessClock,
    testpattern::{BARCODE_CELL, BARCODE_X, BARCODE_Y, TestPattern},
};
use couch_platform_win::gpu_frames::Uploader;

const W: u32 = 1280;
const H: u32 = 720;
const CODECS: [Codec; 2] = [Codec::H264, Codec::Hevc];

fn config(codec: Codec, bitrate_bps: u32) -> EncoderConfig {
    EncoderConfig {
        codec,
        width: W,
        height: H,
        fps: 60,
        bitrate_bps,
    }
}

fn decode(decoder: &mut FfmpegDecoder, unit: &[u8], frame: &BgraFrame) -> Nv12Frame {
    let mut out = decoder.decode(unit, frame.captured_at).expect("decode");
    assert_eq!(out.len(), 1, "one picture per unit");
    let DecodedFrame::CpuNv12(nv12) = out.remove(0) else {
        panic!("expected an NV12 frame");
    };
    nv12
}

fn psnr(a: &Nv12Frame, b: &Nv12Frame) -> f64 {
    let mut sum = 0.0;
    for y in 0..a.height as usize {
        let ra = &a.y[y * a.y_stride as usize..][..a.width as usize];
        let rb = &b.y[y * b.y_stride as usize..][..b.width as usize];
        for (&p, &q) in ra.iter().zip(rb) {
            let d = f64::from(p) - f64::from(q);
            sum += d * d;
        }
    }
    let mse = sum / f64::from(a.width * a.height);
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

/// The NAL units of an Annex B access unit.
fn nal_units(unit: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= unit.len() {
        if unit[i..i + 3] == [0, 0, 1] {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &s)| {
            let end = starts.get(n + 1).map_or(unit.len(), |&next| {
                // The next start code, and a zero byte before it if four long.
                let mut e = next - 3;
                if e > s && unit[e - 1] == 0 {
                    e -= 1;
                }
                e
            });
            &unit[s..end]
        })
        .collect()
}

/// The parameter sets (H.264's SPS and PPS; HEVC's VPS, SPS and PPS).
fn parameter_sets(codec: Codec, unit: &[u8]) -> Vec<Vec<u8>> {
    nal_units(unit)
        .into_iter()
        .filter(|n| match codec {
            Codec::Hevc => matches!((n[0] >> 1) & 0x3f, 32..=34),
            _ => matches!(n[0] & 0x1f, 7 | 8),
        })
        .map(<[u8]>::to_vec)
        .collect()
}

/// The stream viewers get doesn't change: the same parameter sets as
/// FFmpeg's (so the same profile, VUI and colour tags), and pictures as
/// close to FFmpeg's as two encodes of the same frames get.
#[test]
fn the_stream_is_ffmpegs() {
    for codec in CODECS {
        let mut uploader = Uploader::new(W, H).expect("D3D11 device");
        let mut pattern = TestPattern::new(W, H, 60, Arc::new(ProcessClock::new()));
        let first_frame = pattern.render();
        let first = uploader.upload(&first_frame).unwrap().unwrap();
        let mut direct =
            NvencEncoder::open_gpu(config(codec, 20_000_000), &first).expect("NVENC directly");
        let mut ffmpeg = FfmpegEncoder::open_gpu(config(codec, 20_000_000), &first)
            .expect("FFmpeg's NVENC with D3D11 frames");
        assert!(direct.caps().gpu_input);
        let (mut dec_direct, mut dec_ffmpeg) = (
            FfmpegDecoder::new(codec, false).unwrap(),
            FfmpegDecoder::new(codec, false).unwrap(),
        );
        let (mut worst, mut barcodes) = (f64::INFINITY, 0);
        for i in 0..60 {
            let (frame, on_gpu) = if i == 0 {
                (first_frame.clone(), first.clone())
            } else {
                let f = pattern.render();
                let g = uploader.upload(&f).unwrap().expect("a free texture");
                (f, g)
            };
            let a = direct.encode(EncoderInput::Gpu(&on_gpu), i == 0).unwrap();
            let b = ffmpeg.encode(EncoderInput::Gpu(&on_gpu), i == 0).unwrap();
            assert_eq!(a.len(), 1, "one unit per frame");
            assert_eq!(a[0].keyframe, i == 0, "{codec:?} frame {i}");
            assert_eq!(a[0].captured_at, frame.captured_at);
            if i == 0
                && let Ok(dir) = std::env::var("COUCH_NVENC_DUMP")
            {
                std::fs::write(format!("{dir}/direct-{codec:?}.bin"), &a[0].data).unwrap();
                std::fs::write(format!("{dir}/ffmpeg-{codec:?}.bin"), &b[0].data).unwrap();
            }
            if i == 0 {
                let ours = parameter_sets(codec, &a[0].data);
                assert!(
                    !ours.is_empty(),
                    "{codec:?}: the keyframe carries its parameter sets"
                );
                assert_eq!(
                    ours,
                    parameter_sets(codec, &b[0].data),
                    "{codec:?}: parameter sets differ from FFmpeg's"
                );
            }
            let pa = decode(&mut dec_direct, &a[0].data, &frame);
            let pb = decode(&mut dec_ffmpeg, &b[0].data, &frame);
            assert_eq!(pa.matrix, pb.matrix, "{codec:?}: the declared matrix");
            worst = worst.min(psnr(&pa, &pb));
            let value = barcode::read_luma(
                &pa.y,
                pa.y_stride as usize,
                (pa.width, pa.height),
                BARCODE_X,
                BARCODE_Y,
                BARCODE_CELL,
            );
            barcodes += u32::from(value == Some(frame.captured_at.0 & barcode::MAX_VALUE));
        }
        println!(
            "{codec:?}, direct against FFmpeg's: worst PSNR {worst:.1} dB, barcodes {barcodes}/60"
        );
        assert!(worst > 40.0, "{codec:?}: pictures differ: {worst:.1} dB");
        assert!(barcodes >= 56, "{codec:?}: barcode read in {barcodes}/60");
    }
}

/// M12d's point: up or down, a new bitrate costs no keyframe, and the
/// frames after it take the new budget.
#[test]
fn bitrate_changes_cost_no_keyframe() {
    for codec in CODECS {
        // Rates the test pattern fills in each codec, and NVENC reaches at
        // 720p60: H.264 overshoots below about 2 Mbit/s and HEVC below about
        // 1.2, and HEVC needs no more than about 3.3.
        let (start, raised, cut) = match codec {
            Codec::Hevc => (2_000_000, 3_000_000, 1_200_000),
            _ => (4_000_000, 8_000_000, 2_000_000),
        };
        let mut uploader = Uploader::new(W, H).expect("D3D11 device");
        let mut pattern = TestPattern::new(W, H, 60, Arc::new(ProcessClock::new()));
        let first = uploader.upload(&pattern.render()).unwrap().unwrap();
        let mut encoder =
            NvencEncoder::open_gpu(config(codec, start), &first).expect("NVENC directly");
        assert!(
            !encoder.caps().bitrate_change_costs_keyframe,
            "{codec:?}: this GPU can't change the bitrate in place"
        );
        let mut decoder = FfmpegDecoder::new(codec, false).unwrap();
        let mut sizes = Vec::new();
        for i in 0..180 {
            match i {
                60 => encoder.set_bitrate(raised).unwrap(),
                120 => encoder.set_bitrate(cut).unwrap(),
                _ => {}
            }
            let frame = pattern.render();
            let on_gpu = if i == 0 {
                first.clone()
            } else {
                uploader.upload(&frame).unwrap().expect("a free texture")
            };
            let units = encoder.encode(EncoderInput::Gpu(&on_gpu), i == 0).unwrap();
            assert_eq!(units[0].keyframe, i == 0, "{codec:?} frame {i}");
            decode(&mut decoder, &units[0].data, &frame);
            sizes.push(units[0].data.len());
        }
        assert_eq!(encoder.config().bitrate_bps, cut);
        // Bits a second over each stretch's settled part.
        let rate = |range: std::ops::Range<usize>| {
            let n = range.len() as f64;
            sizes[range].iter().sum::<usize>() as f64 * 8.0 * 60.0 / n
        };
        let (at_start, at_raised, at_cut) = (rate(20..60), rate(80..120), rate(140..180));
        println!(
            "{codec:?}: {:.1}, {:.1}, {:.1} Mbit/s",
            at_start / 1e6,
            at_raised / 1e6,
            at_cut / 1e6
        );
        assert!(
            at_raised > at_start * 1.3,
            "{codec:?}: the raise didn't take"
        );
        assert!(at_cut < at_start * 0.8, "{codec:?}: the cut didn't take");
        assert!(
            at_cut < f64::from(cut) * 1.2,
            "{codec:?}: above the new rate"
        );
    }
}

/// NVENC remembers textures by address: a frame from another pool must be
/// refused (the caller reopens), and the pool's textures must come back
/// after each frame.
#[test]
fn another_pool_is_refused_and_textures_return() {
    for codec in CODECS {
        let mut uploader = Uploader::new(W, H).expect("D3D11 device");
        let mut pattern = TestPattern::new(W, H, 60, Arc::new(ProcessClock::new()));
        let first = uploader.upload(&pattern.render()).unwrap().unwrap();
        let mut encoder =
            NvencEncoder::open_gpu(config(codec, 20_000_000), &first).expect("NVENC directly");
        encoder.encode(EncoderInput::Gpu(&first), true).unwrap();
        drop(first);
        for i in 0..150 {
            let on_gpu = uploader
                .upload(&pattern.render())
                .unwrap()
                .unwrap_or_else(|| panic!("frame {i}: every texture still held"));
            encoder.encode(EncoderInput::Gpu(&on_gpu), false).unwrap();
        }
        uploader.new_pool().unwrap();
        let other = uploader.upload(&pattern.render()).unwrap().unwrap();
        assert!(encoder.encode(EncoderInput::Gpu(&other), false).is_err());
        // A new encoder for the new pool, the old one closed first.
        drop(encoder);
        let mut reopened =
            NvencEncoder::open_gpu(config(codec, 20_000_000), &other).expect("NVENC directly");
        assert!(reopened.encode(EncoderInput::Gpu(&other), true).unwrap()[0].keyframe);
    }
}

/// A NAL unit's payload without its emulation-prevention bytes.
fn unescaped(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// The level the stream's SPS names (`level_idc`, H.264's ×10, HEVC's ×30).
fn sps_level(codec: Codec, unit: &[u8]) -> u8 {
    let sps = nal_units(unit)
        .into_iter()
        .find(|n| match codec {
            Codec::Hevc => (n[0] >> 1) & 0x3f == 33,
            _ => n[0] & 0x1f == 7,
        })
        .expect("an SPS");
    let sps = unescaped(sps);
    match codec {
        // NAL header (2), VPS id + sub-layers (1), profile byte (1), 32
        // compatibility flags (4), 48 constraint flags (6), then the level.
        Codec::Hevc => sps[14],
        // NAL header, profile, constraint flags, then the level.
        _ => sps[3],
    }
}

/// The review's question (M12d): the level is chosen as the session opens,
/// and a raise in place keeps the session. A keyframe after a raise far
/// above the start must name the level a session opened at the new bitrate
/// names (NVENC's choice goes by size and rate alone: 3.1 at 720p30, 4.1 at
/// 1080p60, at 2 Mbit/s as at 30, as FFmpeg's encoder labels them).
#[test]
fn a_raise_names_the_level_a_session_opened_there_would() {
    for (codec, (w, h, fps), high) in [
        (Codec::H264, (1280, 720, 30), 20_000_000),
        (Codec::Hevc, (1920, 1080, 60), 30_000_000),
    ] {
        let config = |bitrate_bps| EncoderConfig {
            codec,
            width: w,
            height: h,
            fps,
            bitrate_bps,
        };
        let mut uploader = Uploader::new(w, h).expect("D3D11 device");
        let mut pattern = TestPattern::new(w, h, fps, Arc::new(ProcessClock::new()));
        let first = uploader.upload(&pattern.render()).unwrap().unwrap();
        let mut raised = NvencEncoder::open_gpu(config(2_000_000), &first).expect("NVENC directly");
        let at_start = sps_level(
            codec,
            &raised.encode(EncoderInput::Gpu(&first), true).unwrap()[0].data,
        );
        let mut last = None;
        for i in 1..40 {
            if i == 10 {
                raised.set_bitrate(high).unwrap();
            }
            let on_gpu = uploader
                .upload(&pattern.render())
                .unwrap()
                .expect("a free texture");
            let units = raised.encode(EncoderInput::Gpu(&on_gpu), i == 39).unwrap();
            assert_eq!(units[0].keyframe, i == 39, "{codec:?} frame {i}");
            last = Some(units[0].data.clone());
        }
        let after_raise = sps_level(codec, &last.unwrap());
        drop(raised);
        let on_gpu = uploader.upload(&pattern.render()).unwrap().unwrap();
        let mut fresh = NvencEncoder::open_gpu(config(high), &on_gpu).expect("NVENC directly");
        let opened_high = sps_level(
            codec,
            &fresh.encode(EncoderInput::Gpu(&on_gpu), true).unwrap()[0].data,
        );
        println!(
            "{codec:?} {w}x{h}@{fps}: level {at_start} at 2 Mbit/s, {after_raise} after the raise to {} Mbit/s, {opened_high} opened there",
            high / 1_000_000
        );
        assert!(
            after_raise >= opened_high,
            "{codec:?}: after the raise the stream says level {after_raise}, below the {opened_high} a session opened at its bitrate names"
        );
    }
}
