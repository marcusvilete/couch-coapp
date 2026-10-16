//! NVENC fed D3D11 textures (M9b) against NVENC fed the same frames from
//! memory: the pictures must match, textures must come back to their pool,
//! and a new pool must work with a reopened encoder. For H.264 and HEVC
//! (M12c). Needs an NVIDIA GPU.

#![cfg(all(windows, feature = "gpu-tests"))]

use std::sync::Arc;

use couch_codec_ffmpeg::{FfmpegDecoder, FfmpegEncoder, encoder::gpu_input_encoder};
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

fn config(codec: Codec) -> EncoderConfig {
    EncoderConfig {
        codec,
        width: W,
        height: H,
        fps: 60,
        bitrate_bps: 20_000_000,
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

/// Peak signal-to-noise ratio of two luma planes, in dB.
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

#[test]
fn gpu_frames_encode_like_frames_from_memory() {
    for codec in CODECS {
        gpu_frames_encode_like_frames_from_memory_in(codec);
    }
}

fn gpu_frames_encode_like_frames_from_memory_in(codec: Codec) {
    let mut uploader = Uploader::new(W, H).expect("D3D11 device");
    let mut pattern = TestPattern::new(W, H, 60, Arc::new(ProcessClock::new()));
    let first = pattern.render();
    let gpu_first = uploader.upload(&first).unwrap().expect("a free texture");
    let mut gpu =
        FfmpegEncoder::open_gpu(config(codec), &gpu_first).expect("NVENC with D3D11 frames");
    assert!(gpu.caps().gpu_input);
    let mut cpu =
        FfmpegEncoder::open(gpu_input_encoder(codec), config(codec)).expect("NVENC from memory");
    assert!(!cpu.caps().gpu_input);
    let (mut dec_gpu, mut dec_cpu) = (
        FfmpegDecoder::new(codec, false).unwrap(),
        FfmpegDecoder::new(codec, false).unwrap(),
    );

    let (mut worst, mut barcodes) = (f64::INFINITY, 0);
    for i in 0..30 {
        let frame = if i == 0 {
            first.clone()
        } else {
            pattern.render()
        };
        let on_gpu = if i == 0 {
            gpu_first.clone()
        } else {
            uploader.upload(&frame).unwrap().expect("a free texture")
        };
        let from_gpu = gpu
            .encode(EncoderInput::Gpu(&on_gpu), i == 0)
            .expect("encode");
        let from_cpu = cpu
            .encode(EncoderInput::Cpu(&frame), i == 0)
            .expect("encode");
        assert_eq!(from_gpu.len(), 1, "one unit per frame");
        assert_eq!(from_gpu[0].keyframe, i == 0);
        assert_eq!(from_gpu[0].captured_at, frame.captured_at);
        let a = decode(&mut dec_gpu, &from_gpu[0].data, &frame);
        let b = decode(&mut dec_cpu, &from_cpu[0].data, &frame);
        worst = worst.min(psnr(&a, &b));
        let value = barcode::read_luma(
            &a.y,
            a.y_stride as usize,
            (a.width, a.height),
            BARCODE_X,
            BARCODE_Y,
            BARCODE_CELL,
        );
        barcodes += u32::from(value == Some(frame.captured_at.0 & barcode::MAX_VALUE));
    }
    println!("{codec:?}, GPU against memory: worst PSNR {worst:.1} dB, barcodes {barcodes}/30");
    assert!(
        worst > 45.0,
        "{codec:?}: GPU and memory pictures differ: {worst:.1} dB"
    );
    assert!(barcodes >= 28, "{codec:?}: barcode read in {barcodes}/30");
}

#[test]
fn an_encoder_refuses_frames_from_another_pool() {
    for codec in CODECS {
        let mut uploader = Uploader::new(W, H).expect("D3D11 device");
        let mut pattern = TestPattern::new(W, H, 60, Arc::new(ProcessClock::new()));
        let first = uploader.upload(&pattern.render()).unwrap().unwrap();
        let mut encoder =
            FfmpegEncoder::open_gpu(config(codec), &first).expect("NVENC with D3D11 frames");
        encoder
            .encode(EncoderInput::Gpu(&first), true)
            .expect("encode");
        uploader.new_pool().unwrap();
        let other = uploader.upload(&pattern.render()).unwrap().unwrap();
        assert!(encoder.encode(EncoderInput::Gpu(&other), false).is_err());
    }
}

#[test]
fn textures_return_to_the_pool_and_a_new_pool_reopens_cleanly() {
    for codec in CODECS {
        textures_return_to_the_pool_in(codec);
    }
}

fn textures_return_to_the_pool_in(codec: Codec) {
    let mut uploader = Uploader::new(W, H).expect("D3D11 device");
    let mut pattern = TestPattern::new(W, H, 60, Arc::new(ProcessClock::new()));
    let mut decoder = FfmpegDecoder::new(codec, false).unwrap();
    let mut encoder: Option<(u64, FfmpegEncoder)> = None;
    for i in 0..300 {
        if i == 150 {
            uploader.new_pool().unwrap();
        }
        let frame = pattern.render();
        let on_gpu = uploader
            .upload(&frame)
            .unwrap()
            .unwrap_or_else(|| panic!("frame {i}: every texture still held by the encoder"));
        let reopen = encoder.as_ref().is_none_or(|(s, _)| *s != on_gpu.source);
        if reopen {
            encoder = Some((
                on_gpu.source,
                FfmpegEncoder::open_gpu(config(codec), &on_gpu).expect("NVENC with D3D11 frames"),
            ));
        }
        let (_, enc) = encoder.as_mut().unwrap();
        let units = enc
            .encode(EncoderInput::Gpu(&on_gpu), reopen)
            .expect("encode");
        assert_eq!(units[0].keyframe, reopen, "frame {i}");
        decode(&mut decoder, &units[0].data, &frame);
    }
}
