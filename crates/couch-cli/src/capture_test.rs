//! `couch-cli capture-test`: the host-side pipeline without a network.
//! Captures a window, encodes it, decodes it again and reads the latency
//! clock's barcode, reporting how long each stage takes. With `--psnr`, also
//! how close each decoded picture is to what was captured, as PSNR and SSIM
//! (M12c: H.264 against HEVC at the same bitrate; P8: NVENC's settings).

use std::{
    fs::File,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use couch_codec_ffmpeg::{FfmpegDecoder, FfmpegEncoder};
use couch_media::{
    BgraFrame, CaptureEvent, Capturer, Codec, DecodedFrame, EncoderConfig, EncoderInput, Nv12Frame,
    VideoDecoder, VideoEncoder, barcode, clock::LATENCY_CLOCK_TITLE,
};
use couch_platform_win::{
    capture::{CaptureOptions, CaptureSource, FrameOutput, WgcCapturer},
    clock,
};

use crate::stats::Samples;

pub struct Args {
    pub window: Option<String>,
    pub monitor: bool,
    pub secs: u64,
    pub fps: u32,
    pub bitrate_mbps: u32,
    pub codec: Codec,
    /// Measure each decoded picture against the captured one.
    pub psnr: bool,
    pub out: Option<PathBuf>,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let source = if args.monitor {
        CaptureSource::PrimaryMonitor
    } else {
        CaptureSource::WindowTitle(args.window.unwrap_or_else(|| LATENCY_CLOCK_TITLE.into()))
    };
    let mut capturer = WgcCapturer::start(
        &source,
        CaptureOptions {
            min_interval: Duration::from_secs(1) / args.fps,
            cursor: false,
            output: FrameOutput::Cpu,
        },
    )
    .with_context(|| format!("starting capture of {source:?}"))?;

    let first = match capturer.next_event(Duration::from_secs(5))? {
        Some(CaptureEvent::Frame(frame)) => frame.cpu()?.cropped_to_even(),
        Some(CaptureEvent::SourceClosed) => bail!("the source closed"),
        // (Frames in memory: never a GPU failure.)
        Some(CaptureEvent::GpuFramesFailed) | None => {
            bail!("no frame within 5 s (is the window visible and changing?)")
        }
    };
    let config = EncoderConfig {
        codec: args.codec,
        width: first.width,
        height: first.height,
        fps: args.fps,
        bitrate_bps: args.bitrate_mbps * 1_000_000,
    };
    let mut encoder = FfmpegEncoder::open_best(config)?;
    let mut decoder = FfmpegDecoder::new(config.codec, true)?;
    let mut out = args.out.as_ref().map(File::create).transpose()?;
    println!(
        "capturing {}x{} with {} ({}), decoding with {}",
        config.width,
        config.height,
        encoder.caps().name,
        encoder.caps().options,
        if decoder.is_hardware() {
            "D3D11VA"
        } else {
            "software"
        }
    );

    let mut capture_delivery = Samples::default();
    let mut encode = Samples::default();
    let mut decode = Samples::default();
    let mut clock_to_capture = Samples::default();
    let mut clock_to_decoded = Samples::default();
    let mut psnr_db = Samples::default();
    let mut similarity = Samples::default();
    let (mut frames, mut bytes) = (0u64, 0u64);
    let mut barcode_at: Option<(u32, u32)> = None;
    let started = Instant::now();
    let mut pending = Some(first);

    while started.elapsed() < Duration::from_secs(args.secs) {
        let frame = match pending.take() {
            Some(frame) => frame,
            None => match capturer.next_event(Duration::from_millis(100))? {
                Some(CaptureEvent::Frame(frame)) => frame.cpu()?.cropped_to_even(),
                Some(CaptureEvent::SourceClosed) => {
                    println!("source closed");
                    break;
                }
                Some(CaptureEvent::GpuFramesFailed) | None => continue,
            },
        };
        if (frame.width, frame.height) != (config.width, config.height) {
            bail!("the source changed size; restart the test");
        }
        let got = clock::now();
        capture_delivery.push(ms(got.0, frame.captured_at.0));

        let units = encoder.encode(EncoderInput::Cpu(&frame), frames == 0)?;
        let encoded = clock::now();
        encode.push(ms(encoded.0, got.0));
        for unit in units {
            bytes += unit.data.len() as u64;
            if let Some(file) = out.as_mut() {
                file.write_all(&unit.data)?;
            }
            for decoded in decoder.decode(&unit.data, frame.captured_at)? {
                let DecodedFrame::CpuNv12(nv12) = decoded else {
                    continue;
                };
                let done = clock::now();
                decode.push(ms(done.0, encoded.0));
                if args.psnr {
                    let l = lumas(&frame, &nv12);
                    psnr_db.push(psnr(&l));
                    similarity.push(ssim(&l));
                }
                let size = (nv12.width, nv12.height);
                let stride = nv12.y_stride as usize;
                let value = match barcode_at {
                    Some((x, y)) => barcode::read_luma(&nv12.y, stride, size, x, y, 16),
                    None => barcode::locate_luma(&nv12.y, stride, size, 16, 48).map(|(x, y, v)| {
                        barcode_at = Some((x, y));
                        v
                    }),
                };
                if let Some(clock_us) = value {
                    clock_to_capture.push(ms(frame.captured_at.0, clock_us));
                    clock_to_decoded.push(ms(done.0, clock_us));
                }
            }
        }
        frames += 1;
    }

    let secs = started.elapsed().as_secs_f64();
    println!(
        "{frames} frames in {secs:.1} s ({:.1} fps), {:.1} Mbit/s, {} dropped by capture",
        frames as f64 / secs,
        bytes as f64 * 8.0 / secs / 1e6,
        capturer.dropped_frames()
    );
    println!("capture delivery    {}", capture_delivery.summary_ms());
    println!("encode              {}", encode.summary_ms());
    println!("decode              {}", decode.summary_ms());
    println!("clock -> captured   {}", clock_to_capture.summary_ms());
    println!("clock -> decoded    {}", clock_to_decoded.summary_ms());
    if args.psnr {
        // Over 99 dB counts as identical (a still picture).
        println!(
            "PSNR (luma, dB)     p5 {:.2}  p50 {:.2}  min {:.2}  (n={})",
            psnr_db.percentile(5.0).unwrap_or(f64::NAN),
            psnr_db.percentile(50.0).unwrap_or(f64::NAN),
            psnr_db.percentile(0.0).unwrap_or(f64::NAN),
            psnr_db.len()
        );
        println!(
            "SSIM (luma)         p5 {:.4}  p50 {:.4}  min {:.4}  (n={})",
            similarity.percentile(5.0).unwrap_or(f64::NAN),
            similarity.percentile(50.0).unwrap_or(f64::NAN),
            similarity.percentile(0.0).unwrap_or(f64::NAN),
            similarity.len()
        );
    }
    if let Some(path) = args.out {
        println!("wrote {}", path.display());
    }
    Ok(())
}

/// Both pictures' luma, row by row, over the size they share: the captured
/// BGRA becomes luma as the encoder made it, with the matrix the stream
/// declares (NVENC's BT.601, the others' BT.709), limited range (`Y = 16 +
/// 219 x (wr R + wg G + wb B) / 255`).
struct Lumas {
    width: usize,
    height: usize,
    source: Vec<f32>,
    decoded: Vec<f32>,
}

fn lumas(source: &BgraFrame, decoded: &Nv12Frame) -> Lumas {
    let [wr, wg, wb] = decoded.matrix.luma_weights().map(|w| w as f32);
    let (w, h) = (
        source.width.min(decoded.width) as usize,
        source.height.min(decoded.height) as usize,
    );
    let mut src_luma = Vec::with_capacity(w * h);
    let mut dec_luma = Vec::with_capacity(w * h);
    for y in 0..h {
        let src = &source.data[y * source.stride as usize..][..w * 4];
        let (pixels, _) = src.as_chunks::<4>();
        src_luma.extend(pixels.iter().map(|px| {
            let (b, g, r) = (f32::from(px[0]), f32::from(px[1]), f32::from(px[2]));
            16.0 + 219.0 * (wr * r + wg * g + wb * b) / 255.0
        }));
        dec_luma.extend(
            decoded.y[y * decoded.y_stride as usize..][..w]
                .iter()
                .map(|&v| f32::from(v)),
        );
    }
    Lumas {
        width: w,
        height: h,
        source: src_luma,
        decoded: dec_luma,
    }
}

/// Peak signal-to-noise ratio of the decoded luma against the captured
/// picture's, in dB (capped at 99 for identical pictures). Rounding alone
/// keeps it below about 50 dB; the comparison between settings is what
/// counts.
fn psnr(l: &Lumas) -> f64 {
    let sum: f64 = l
        .source
        .iter()
        .zip(&l.decoded)
        .map(|(&a, &b)| f64::from(a - b) * f64::from(a - b))
        .sum();
    let mse = sum / (l.width * l.height).max(1) as f64;
    if mse < 1e-10 {
        99.0
    } else {
        (10.0 * (255.0 * 255.0 / mse).log10()).min(99.0)
    }
}

/// Structural similarity of the decoded luma against the captured picture's
/// (P8), 1 for identical, averaged over 8x8 blocks (the fast form, without
/// the Gaussian window). Unlike PSNR it follows what the eye notices
/// (structure, contrast), so it can rise where adaptive quantization moves
/// bits to flat and dark areas even if PSNR falls. The last rows and
/// columns short of a whole block are left out; a picture smaller than one
/// block scores 1 (captures that small are dropped before encoding).
fn ssim(l: &Lumas) -> f64 {
    const BLOCK: usize = 8;
    const C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
    const C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);
    let (mut sum, mut blocks) = (0.0f64, 0usize);
    for by in (0..l.height / BLOCK).map(|b| b * BLOCK) {
        for bx in (0..l.width / BLOCK).map(|b| b * BLOCK) {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
            for y in by..by + BLOCK {
                let row = y * l.width;
                for x in bx..bx + BLOCK {
                    let (a, b) = (f64::from(l.source[row + x]), f64::from(l.decoded[row + x]));
                    sa += a;
                    sb += b;
                    saa += a * a;
                    sbb += b * b;
                    sab += a * b;
                }
            }
            let n = (BLOCK * BLOCK) as f64;
            let (ma, mb) = (sa / n, sb / n);
            let (va, vb) = (saa / n - ma * ma, sbb / n - mb * mb);
            let cov = sab / n - ma * mb;
            sum += ((2.0 * ma * mb + C1) * (2.0 * cov + C2))
                / ((ma * ma + mb * mb + C1) * (va + vb + C2));
            blocks += 1;
        }
    }
    if blocks == 0 {
        1.0
    } else {
        sum / blocks as f64
    }
}

fn ms(later_us: u64, earlier_us: u64) -> f64 {
    later_us.saturating_sub(earlier_us) as f64 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(width: usize, height: usize, f: impl Fn(usize, usize) -> f32) -> Vec<f32> {
        (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|(x, y)| f(x, y))
            .collect()
    }

    fn pair(source: Vec<f32>, decoded: Vec<f32>) -> Lumas {
        Lumas {
            width: 32,
            height: 16,
            source,
            decoded,
        }
    }

    #[test]
    fn identical_pictures_score_full_marks() {
        let p = picture(32, 16, |x, y| 16.0 + ((x * 7 + y * 3) % 200) as f32);
        let l = pair(p.clone(), p);
        assert_eq!(psnr(&l), 99.0);
        assert!((ssim(&l) - 1.0).abs() < 1e-9);
    }

    /// P8: detail smoothed away leaves SSIM near 0; slight noise lowers it a
    /// little, and PSNR by the noise's strength.
    #[test]
    fn ssim_falls_with_lost_detail() {
        let detail = picture(32, 16, |x, y| if (x + y) % 2 == 0 { 60.0 } else { 180.0 });
        let flat = vec![120.0; 32 * 16];
        let blurred = ssim(&pair(detail.clone(), flat));
        assert!(blurred < 0.1, "a checkerboard turned flat: {blurred}");
        let noisy: Vec<f32> = detail
            .iter()
            .enumerate()
            .map(|(i, v)| v + if i % 3 == 0 { 4.0 } else { -2.0 })
            .collect();
        let close = ssim(&pair(detail.clone(), noisy.clone()));
        assert!(close > 0.99 && close < 1.0, "slight noise: {close}");
        // Mean squared error about (16 + 4 + 4) / 3 = 8: 10 log10(255² / 8) dB.
        let db = psnr(&pair(detail, noisy));
        assert!((db - 39.10).abs() < 0.01, "{db}");
    }
}
