//! `couch-cli capture-test`: the host-side pipeline without a network.
//! Captures a window, encodes it, decodes it again and reads the latency
//! clock's barcode, reporting how long each stage takes.

use std::{
    fs::File,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use couch_codec_ffmpeg::{FfmpegDecoder, FfmpegEncoder};
use couch_media::{
    CaptureEvent, Capturer, Codec, DecodedFrame, EncoderConfig, EncoderInput, VideoDecoder,
    VideoEncoder, barcode, clock::LATENCY_CLOCK_TITLE,
};
use couch_platform_win::{
    capture::{CaptureOptions, CaptureSource, WgcCapturer},
    clock,
};

use crate::stats::Samples;

pub struct Args {
    pub window: Option<String>,
    pub monitor: bool,
    pub secs: u64,
    pub fps: u32,
    pub bitrate_mbps: u32,
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
        },
    )
    .with_context(|| format!("starting capture of {source:?}"))?;

    let first = match capturer.next_event(Duration::from_secs(5))? {
        Some(CaptureEvent::Frame(frame)) => frame.cropped_to_even(),
        Some(CaptureEvent::SourceClosed) => bail!("the source closed"),
        None => bail!("no frame within 5 s (is the window visible and changing?)"),
    };
    let config = EncoderConfig {
        codec: Codec::H264,
        width: first.width,
        height: first.height,
        fps: args.fps,
        bitrate_bps: args.bitrate_mbps * 1_000_000,
    };
    let mut encoder = FfmpegEncoder::open_best(config)?;
    let mut decoder = FfmpegDecoder::new(true)?;
    let mut out = args.out.as_ref().map(File::create).transpose()?;
    println!(
        "capturing {}x{} with {}, decoding with {}",
        config.width,
        config.height,
        encoder.caps().name,
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
    let (mut frames, mut bytes) = (0u64, 0u64);
    let mut barcode_at: Option<(u32, u32)> = None;
    let started = Instant::now();
    let mut pending = Some(first);

    while started.elapsed() < Duration::from_secs(args.secs) {
        let frame = match pending.take() {
            Some(frame) => frame,
            None => match capturer.next_event(Duration::from_millis(100))? {
                Some(CaptureEvent::Frame(frame)) => frame.cropped_to_even(),
                Some(CaptureEvent::SourceClosed) => {
                    println!("source closed");
                    break;
                }
                None => continue,
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
    if let Some(path) = args.out {
        println!("wrote {}", path.display());
    }
    Ok(())
}

fn ms(later_us: u64, earlier_us: u64) -> f64 {
    later_us.saturating_sub(earlier_us) as f64 / 1000.0
}
