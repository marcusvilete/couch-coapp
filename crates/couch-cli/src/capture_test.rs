//! `couch-cli capture-test`: the host-side pipeline without a network.
//! Captures a window, encodes it, decodes it again and reads the latency
//! clock's barcode, reporting how long each stage takes. With `--psnr`, also
//! how close each decoded picture is to what was captured, as PSNR and SSIM
//! (M12c: H.264 against HEVC at the same bitrate; P8: NVENC's settings).
//! With `--keyframe-every` or `--bitrate-every`, what a forced keyframe or a
//! bitrate change costs the frames after it (M12d); with `--gpu`, frames stay
//! on the GPU and go to NVENC directly (or `--ffmpeg-nvenc`, FFmpeg's). With
//! `--recover-every`, what recovering from frames a viewer never got costs:
//! a keyframe, against continuing from a long-term frame (M12d-2's spike).

use std::{
    fs::File,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use couch_codec_ffmpeg::{FfmpegDecoder, FfmpegEncoder};
use couch_codec_nvenc::{Continuation, FrameControl, NvencEncoder};
use couch_media::{
    BgraFrame, CaptureEvent, Capturer, Codec, DecodedFrame, EncoderConfig, EncoderInput, GpuFrame,
    Nv12Frame, VideoDecoder, VideoEncoder, VideoFrame, barcode, clock::LATENCY_CLOCK_TITLE,
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
    /// Keyframes forced or bitrate changes, with the frames after each
    /// measured against those before (M12d).
    pub events: Option<Events>,
    /// One row per frame.
    pub frames_csv: Option<PathBuf>,
    /// Frames stay on the GPU and go to NVENC directly (M12d)...
    pub gpu: bool,
    /// ...or to FFmpeg's NVENC, to compare.
    pub ffmpeg_nvenc: bool,
}

/// What happens every so often during a run (M12d).
#[derive(Debug, Clone, Copy)]
pub enum Events {
    /// A keyframe forced every `every` seconds.
    Keyframe { every: f64 },
    /// The bitrate switched every `every` seconds between `--bitrate-mbps`
    /// and `to_mbps`.
    Bitrate { every: f64, to_mbps: u32 },
    /// Every `every` seconds, `drop` of frames left undecoded, then a
    /// recovery: a keyframe and a continuation in turn (M12d-2's spike).
    Recover { every: f64, drop: Duration },
}

impl Events {
    fn every(self) -> f64 {
        match self {
            Self::Keyframe { every }
            | Self::Bitrate { every, .. }
            | Self::Recover { every, .. } => every,
        }
    }
}

/// The encoder: NVENC directly kept as itself, for continuations.
enum Encoder {
    Direct(Box<NvencEncoder>),
    Other(Box<dyn VideoEncoder>),
}

impl Encoder {
    fn get(&mut self) -> &mut dyn VideoEncoder {
        match self {
            Self::Direct(e) => e.as_mut(),
            Self::Other(e) => e.as_mut(),
        }
    }
}

/// Long-term frames in two slots, as ADR 0016 has them: the next mark goes
/// into the slot not holding the newest mark the viewer got, and only once
/// the last mark was got, at most every [`MARK_EVERY`].
#[derive(Default)]
struct Marks {
    /// Each slot's frame number, and whether the viewer got it.
    slots: [Option<(u64, bool)>; 2],
    last: Option<Instant>,
}

const MARK_EVERY: Duration = Duration::from_millis(100);

impl Marks {
    /// The slot to mark this frame into, if any.
    fn next(&self, now: Instant, keyframe: bool) -> Option<u32> {
        let newest = self.slots.iter().flatten().max_by_key(|&&(f, _)| f);
        let due = self.last.is_none_or(|t| now - t >= MARK_EVERY);
        (keyframe || due && newest.is_none_or(|&(_, got)| got)).then(|| {
            let held = (0..2)
                .filter_map(|s| self.slots[s].filter(|&(_, got)| got).map(|(f, _)| (f, s)))
                .max()
                .map(|(_, s)| s as u32);
            held.map_or(0, |h| 1 - h)
        })
    }

    fn marked(&mut self, slot: u32, frame: u64, got: bool, now: Instant) {
        self.slots[slot as usize] = Some((frame, got));
        self.last = Some(now);
    }

    /// The viewer never got these: invalidated with their frames.
    fn forget_lost(&mut self) {
        for slot in &mut self.slots {
            if matches!(slot, Some((_, false))) {
                *slot = None;
            }
        }
    }
}

/// One encoded frame.
#[derive(Debug, Clone, Default)]
struct Row {
    at_ms: f64,
    bytes: usize,
    keyframe: bool,
    bitrate_bps: u32,
    /// An event happened on this frame (a keyframe forced, a new bitrate, a
    /// recovery).
    event: bool,
    /// Which, for the summary.
    kind: &'static str,
    /// Left undecoded (`--recover-every`).
    dropped: bool,
    psnr: Option<f64>,
    ssim: Option<f64>,
}

/// Frames compared before and after each event.
const AROUND: usize = 10;

pub fn run(args: Args) -> anyhow::Result<()> {
    if let Some(events) = args.events {
        let every = events.every();
        if !(every.is_finite() && every > 0.0) {
            bail!("events need a positive interval, not {every}");
        }
        if let Events::Bitrate { to_mbps, .. } = events
            && to_mbps == args.bitrate_mbps
        {
            bail!("--bitrate-to is --bitrate-mbps: nothing would change");
        }
        if let Events::Recover { drop, .. } = events
            && drop.as_secs_f64() * 2.0 >= every
        {
            bail!("a drop must be under half of --recover-every");
        }
    }
    let recover = matches!(args.events, Some(Events::Recover { .. }));
    let measure = args.psnr || args.events.is_some() || args.frames_csv.is_some();
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
            output: if args.gpu {
                FrameOutput::Gpu
            } else {
                FrameOutput::Cpu
            },
            hdr: true,
        },
    )
    .with_context(|| format!("starting capture of {source:?}"))?;

    let first = match capturer.next_event(Duration::from_secs(5))? {
        Some(CaptureEvent::Frame(frame)) => taken(frame)?,
        Some(CaptureEvent::SourceClosed) => bail!("the source closed"),
        Some(CaptureEvent::GpuFramesFailed) => bail!("frames can't stay on the GPU here"),
        None => bail!("no frame within 5 s (is the window visible and changing?)"),
    };
    let (first, first_gpu) = first;
    let config = EncoderConfig {
        codec: args.codec,
        width: first.width,
        height: first.height,
        fps: args.fps,
        bitrate_bps: args.bitrate_mbps * 1_000_000,
    };
    let mut encoder = match &first_gpu {
        Some(gpu) if args.ffmpeg_nvenc => {
            Encoder::Other(Box::new(FfmpegEncoder::open_gpu(config, gpu)?))
        }
        Some(gpu) if recover => {
            // What ADR 0016 settled on: two long-term frames, and three
            // references (with LTR on, the driver would pick one).
            let continuation = Continuation {
                ltr_frames: 2,
                max_ref_frames: 0,
                refs_l0: 3,
            };
            Encoder::Direct(Box::new(NvencEncoder::open_continuing(
                config,
                gpu,
                continuation,
            )?))
        }
        Some(gpu) => Encoder::Direct(Box::new(NvencEncoder::open_gpu(config, gpu)?)),
        None if recover => bail!("--recover-every needs --gpu"),
        None => Encoder::Other(Box::new(FfmpegEncoder::open_best(config)?)),
    };
    let mut decoder = FfmpegDecoder::new(config.codec, true)?;
    let mut out = args.out.as_ref().map(File::create).transpose()?;
    let caps = encoder.get().caps().clone();
    println!(
        "capturing {}x{} with {} ({}), decoding with {}",
        config.width,
        config.height,
        caps.name,
        caps.options,
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
    let mut rows: Vec<Row> = Vec::new();
    let mut bitrate_bps = config.bitrate_bps;
    let mut next_event = args.events.map(|e| Duration::from_secs_f64(e.every()));
    // `--recover-every`: a drop lasting until then, the timestamps of the
    // frames it left out, the recoveries so far, and the long-term marks.
    let mut drop_until: Option<Duration> = None;
    let mut lost: Vec<u64> = Vec::new();
    let mut recoveries = 0usize;
    let mut intra_continuations = 0usize;
    let mut marks = Marks::default();
    let started = Instant::now();
    let pool = first_gpu.as_ref().map(|g| g.source);
    let mut pending = Some((first, first_gpu));

    while started.elapsed() < Duration::from_secs(args.secs) {
        let (frame, gpu) = match pending.take() {
            Some(frame) => frame,
            None => match capturer.next_event(Duration::from_millis(100))? {
                Some(CaptureEvent::Frame(frame)) => taken(frame)?,
                Some(CaptureEvent::SourceClosed) => {
                    println!("source closed");
                    break;
                }
                Some(CaptureEvent::GpuFramesFailed) => bail!("GPU frames failed; restart the test"),
                None => continue,
            },
        };
        // An encoder on GPU frames reads one pool's textures (M9b).
        if gpu.as_ref().map(|g| g.source) != pool {
            bail!("the capture made a new texture pool; restart the test");
        }
        if (frame.width, frame.height) != (config.width, config.height) {
            bail!("the source changed size; restart the test");
        }
        let got = clock::now();
        capture_delivery.push(ms(got.0, frame.captured_at.0));

        let mut force_keyframe = frames == 0;
        let mut continuing = false;
        let mut row = Row {
            at_ms: started.elapsed().as_secs_f64() * 1000.0,
            ..Row::default()
        };
        // The end of a drop: this frame recovers, a keyframe and a
        // continuation in turn.
        if let Some(until) = drop_until {
            if started.elapsed() < until {
                row.dropped = true;
            } else {
                drop_until = None;
                row.event = true;
                if recoveries.is_multiple_of(2) {
                    row.kind = "recovery keyframe";
                    force_keyframe = true;
                    // An IDR empties the encoder's references, marks too.
                    marks = Marks::default();
                } else {
                    row.kind = "continuation";
                    continuing = true;
                    if let Encoder::Direct(direct) = &mut encoder {
                        for &timestamp in &lost {
                            direct.invalidate(timestamp)?;
                        }
                    }
                    marks.forget_lost();
                }
                lost.clear();
                recoveries += 1;
            }
        }
        if let (Some(events), Some(due)) = (args.events, next_event.as_mut())
            && started.elapsed() >= *due
        {
            // Events a stall made late are skipped, not fired back to back.
            let every = Duration::from_secs_f64(events.every());
            while *due <= started.elapsed() {
                *due += every;
            }
            match events {
                Events::Keyframe { .. } => {
                    row.event = true;
                    row.kind = "keyframe";
                    force_keyframe = true;
                }
                Events::Bitrate { to_mbps, .. } => {
                    row.event = true;
                    row.kind = "bitrate change";
                    bitrate_bps = if bitrate_bps == config.bitrate_bps {
                        to_mbps * 1_000_000
                    } else {
                        config.bitrate_bps
                    };
                    encoder.get().set_bitrate(bitrate_bps)?;
                }
                // A drop begins with this frame (none is under way: it
                // lasts under half the interval).
                Events::Recover { drop, .. } => {
                    drop_until = Some(started.elapsed() + drop);
                    row.dropped = true;
                }
            }
        }
        row.bitrate_bps = bitrate_bps;

        let units = match (&mut encoder, &gpu) {
            (Encoder::Direct(direct), Some(gpu)) if recover => {
                let now = Instant::now();
                let mark = marks.next(now, force_keyframe);
                let control = FrameControl {
                    keyframe: force_keyframe,
                    mark_ltr: mark,
                    use_ltr: None,
                };
                let Some((unit, report)) = direct.encode_frame(gpu, control)? else {
                    bail!("NVENC returned no frame");
                };
                if let Some(slot) = mark {
                    marks.marked(slot, frames, !row.dropped, now);
                }
                if row.dropped {
                    lost.push(report.timestamp);
                }
                // P is 0, NONREF_P 7; anything else is coded without the
                // reference it continues from.
                if continuing && !matches!(report.picture_type, 0 | 7) {
                    intra_continuations += 1;
                }
                vec![unit]
            }
            (encoder, gpu) => {
                let input = match gpu {
                    Some(gpu) => EncoderInput::Gpu(gpu),
                    None => EncoderInput::Cpu(&frame),
                };
                encoder.get().encode(input, force_keyframe)?
            }
        };
        let encoded = clock::now();
        encode.push(ms(encoded.0, got.0));
        for unit in units {
            bytes += unit.data.len() as u64;
            row.bytes += unit.data.len();
            row.keyframe |= unit.keyframe;
            if let Some(file) = out.as_mut() {
                file.write_all(&unit.data)?;
            }
            // A viewer that never got it.
            if row.dropped {
                continue;
            }
            for decoded in decoder.decode(&unit.data, frame.captured_at)? {
                let DecodedFrame::CpuNv12(nv12) = decoded else {
                    continue;
                };
                let done = clock::now();
                decode.push(ms(done.0, encoded.0));
                if measure {
                    let l = lumas(&frame, &nv12);
                    let (db, s) = (psnr(&l), ssim(&l));
                    psnr_db.push(db);
                    similarity.push(s);
                    row.psnr = Some(db);
                    row.ssim = Some(s);
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
        rows.push(row);
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
    if measure {
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
    if args.events.is_some() {
        print_events(&rows);
    }
    if recover {
        println!(
            "{} continuations, {intra_continuations} of them coded without a reference",
            recoveries / 2
        );
    }
    if let Some(path) = &args.frames_csv {
        let mut file = File::create(path)?;
        writeln!(
            file,
            "frame,at_ms,bytes,keyframe,event,bitrate_bps,psnr,ssim"
        )?;
        for (i, r) in rows.iter().enumerate() {
            let opt = |v: Option<f64>, digits: usize| {
                v.map_or(String::new(), |v| format!("{v:.digits$}"))
            };
            writeln!(
                file,
                "{i},{:.1},{},{},{},{},{},{}",
                r.at_ms,
                r.bytes,
                u8::from(r.keyframe),
                u8::from(r.event),
                r.bitrate_bps,
                opt(r.psnr, 3),
                opt(r.ssim, 5)
            )?;
        }
        println!("wrote {}", path.display());
    }
    if let Some(path) = args.out {
        println!("wrote {}", path.display());
    }
    Ok(())
}

/// A captured frame in memory, for measuring, and its texture if it came on
/// the GPU (`--gpu`), for the encoder.
fn taken(frame: VideoFrame) -> anyhow::Result<(BgraFrame, Option<GpuFrame>)> {
    Ok(match frame {
        VideoFrame::Cpu(cpu) => (cpu.cropped_to_even(), None),
        // The pool's textures are already cropped to even.
        VideoFrame::Gpu(gpu) => (gpu.cpu()?, Some(gpu)),
    })
}

/// The frames after the events, then after points halfway between them
/// (nothing happens there: what the picture's own changes account for).
fn print_events(rows: &[Row]) {
    let events: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].event).collect();
    let keyframes = events.iter().filter(|&&i| rows[i].keyframe).count();
    let others = (1..rows.len())
        .filter(|&i| rows[i].keyframe && !rows[i].event)
        .count();
    println!(
        "{} events, {keyframes} of them keyframes; {others} other keyframes after the first frame",
        events.len()
    );
    let halfway: Vec<usize> = events.windows(2).map(|w| (w[0] + w[1]) / 2).collect();
    let mut kinds: Vec<&str> = Vec::new();
    for &i in &events {
        if !kinds.contains(&rows[i].kind) {
            kinds.push(rows[i].kind);
        }
    }
    let mut tables: Vec<(String, Vec<usize>)> = kinds
        .iter()
        .map(|&kind| {
            let at = events.iter().copied().filter(|&i| rows[i].kind == kind);
            (format!("after a {kind}"), at.collect())
        })
        .collect();
    tables.push(("halfway between".into(), halfway));
    for (label, at) in &tables {
        let Some(table) = around(rows, at) else {
            println!("{label}: too few events with {AROUND} measured frames each side");
            continue;
        };
        println!(
            "{label} (mean of {}, against the {AROUND} frames before):",
            table.points
        );
        println!("  frame  PSNR dB   SSIM      size");
        for (k, (db, s, size)) in table.rows.iter().enumerate() {
            println!("  +{k:<4} {db:+7.2}  {s:+8.4}  {size:5.2}x");
        }
        // From the frame on which it stays within, not the first that is.
        let healed = table
            .rows
            .iter()
            .rposition(|(db, ..)| *db <= -0.5)
            .map_or(Some(0), |last| (last + 1 < AROUND).then_some(last + 1));
        match healed {
            Some(k) => println!("  within 0.5 dB from frame +{k}"),
            None => println!("  not within 0.5 dB after {AROUND} frames"),
        }
    }
}

/// The mean change after each of `at`, against the frames before it.
struct Around {
    /// How many points counted.
    points: usize,
    /// Per frame after the point: PSNR and SSIM minus the mean of the
    /// frames before, and its size over theirs.
    rows: Vec<(f64, f64, f64)>,
}

/// Points count only with [`AROUND`] measured frames on each side and no
/// other event among them; `None` if none does. The frames before are those
/// the viewer got: before a drop, for a recovery.
fn around(rows: &[Row], at: &[usize]) -> Option<Around> {
    let mut sums = vec![(0.0, 0.0, 0.0); AROUND];
    let mut points = 0;
    for &p in at {
        if p + AROUND > rows.len() {
            continue;
        }
        let before: Vec<&Row> = rows[..p]
            .iter()
            .rev()
            .filter(|r| !r.dropped)
            .take(AROUND)
            .collect();
        let after = &rows[p..p + AROUND];
        let clean = before.len() == AROUND
            && before
                .iter()
                .all(|r| r.psnr.is_some() && r.ssim.is_some() && !r.event)
            && after
                .iter()
                .enumerate()
                .all(|(i, r)| r.psnr.is_some() && r.ssim.is_some() && (i == 0 || !r.event));
        if !clean {
            continue;
        }
        let mean = |f: fn(&Row) -> f64| before.iter().map(|r| f(r)).sum::<f64>() / AROUND as f64;
        let db = mean(|r| r.psnr.unwrap_or_default());
        let s = mean(|r| r.ssim.unwrap_or_default());
        let size = mean(|r| r.bytes as f64).max(1.0);
        for (sum, r) in sums.iter_mut().zip(after) {
            sum.0 += r.psnr.unwrap_or_default() - db;
            sum.1 += r.ssim.unwrap_or_default() - s;
            sum.2 += r.bytes as f64 / size;
        }
        points += 1;
    }
    (points > 0).then(|| Around {
        points,
        rows: sums
            .into_iter()
            .map(|(a, b, c)| {
                let n = points as f64;
                (a / n, b / n, c / n)
            })
            .collect(),
    })
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

    fn frame(psnr: f64, bytes: usize, event: bool) -> Row {
        Row {
            bytes,
            event,
            psnr: Some(psnr),
            ssim: Some(psnr / 100.0),
            ..Row::default()
        }
    }

    /// M12d: each frame after a point is compared with the mean of the ten
    /// before; points too near the ends, or with another event within ten
    /// frames, don't count.
    #[test]
    fn frames_after_a_point_are_measured_against_those_before() {
        let mut rows: Vec<Row> = (0..60)
            .map(|i| frame(40.0 + (i % 2) as f64, 100, false))
            .collect();
        // A dip of 6 dB that heals 2 dB a frame, at a frame twice the size.
        rows[30] = frame(34.5, 200, true);
        rows[31] = frame(36.5, 100, false);
        rows[32] = frame(38.5, 100, false);
        rows[5].event = true; // too near the start
        rows[55].event = true; // too near the end
        let table = around(&rows, &[5, 30, 55]).expect("one point counts");
        assert_eq!(table.points, 1);
        let (db, ssim, size) = table.rows[0];
        assert!((db - -6.0).abs() < 1e-9, "{db}");
        assert!((ssim - -0.06).abs() < 1e-9, "{ssim}");
        assert!((size - 2.0).abs() < 1e-9, "{size}");
        assert!((table.rows[1].0 - -4.0).abs() < 1e-9);
        assert!((table.rows[3].0 - 0.5).abs() < 1e-9);
        // Another event among the frames around a point rules it out.
        rows[35].event = true;
        assert!(around(&rows, &[30]).is_none());
        // A frame not measured does too.
        rows[35].event = false;
        rows[25].psnr = None;
        assert!(around(&rows, &[30]).is_none());
    }

    /// M12d-2: after a drop, a recovery is measured against the frames
    /// before the drop, which the viewer got.
    #[test]
    fn a_recovery_is_measured_against_the_frames_before_its_drop() {
        let mut rows: Vec<Row> = (0..60).map(|_| frame(40.0, 100, false)).collect();
        for r in &mut rows[20..30] {
            *r = Row {
                dropped: true,
                bytes: 100,
                ..Row::default()
            };
        }
        rows[10..20].iter_mut().for_each(|r| r.psnr = Some(42.0));
        rows[30] = frame(37.0, 100, true);
        let table = around(&rows, &[30]).expect("the point counts");
        assert!((table.rows[0].0 - -5.0).abs() < 1e-9, "{}", table.rows[0].0);
        // Without enough measured frames before the drop, it doesn't count.
        rows[15].psnr = None;
        assert!(around(&rows, &[30]).is_none());
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
