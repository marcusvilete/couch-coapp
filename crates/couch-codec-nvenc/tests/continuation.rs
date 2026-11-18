//! M12d-2's spike (ADR 0016, part 3): does NVENC at p1 continue a stream
//! after frames the viewer never got, and do FFmpeg's decoders follow?
//!
//! Each case encodes the test pattern, leaves runs of frames out of one
//! decoder ("dropped"), invalidates them in the encoder, and goes on. Another
//! decoder gets every frame ("whole"). If the continuation is right, every
//! picture from it on is the same in both, bit for bit, since both decode the
//! same units from the same references; a decoder that took a different
//! reference shows as a difference, an error as an error.
//!
//! Needs an NVIDIA GPU:
//! `cargo test -p couch-codec-nvenc --features gpu-tests --test continuation -- --nocapture`.
//! With `COUCH_NVENC_DUMP=<folder>`, each case's streams are written there
//! (`<case>-whole.<ext>`, `<case>-dropped.<ext>`), for decoding with
//! `ffmpeg -hwaccel d3d12va` or VAAPI on Linux.

#![cfg(all(windows, feature = "gpu-tests"))]

use std::sync::Arc;

use couch_codec_ffmpeg::FfmpegDecoder;
use couch_codec_nvenc::{Continuation, FrameControl, NvencEncoder};
use couch_media::{
    Codec, DecodedFrame, EncoderConfig, Nv12Frame, VideoDecoder, clock::ProcessClock,
    testpattern::TestPattern,
};
use couch_platform_win::gpu_frames::Uploader;

/// The picture's size: 1280x720, or `COUCH_SPIKE_SIZE=<w>x<h>` (FFmpeg's own
/// D3D12VA fails HEVC at 720 lines on NVIDIA, which our decoder works around).
fn size() -> (u32, u32) {
    std::env::var("COUCH_SPIKE_SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1280, 720))
}
const FRAMES: usize = 160;
/// Runs of frames the viewer never gets: past a short-term DPB's reach (20
/// frames, a third of a second at 60 fps), within it (2), and longer (30).
const DROPS: [std::ops::Range<usize>; 3] = [41..61, 80..82, 100..130];
/// A long-term mark at most every 100 ms.
const MARK_EVERY: usize = 6;

#[derive(Debug, Clone, Copy)]
struct Case {
    name: &'static str,
    continuation: Continuation,
    /// Long-term marks, two slots as ADR 0016 has them.
    marks: bool,
    /// Name the held long-term slot on the continuing frame (`ltrUseFrames`).
    use_held: bool,
    /// Invalidate the dropped frames before continuing.
    invalidate: bool,
}

const CASES: [Case; 4] = [
    // Invalidation alone, with the driver's DPB: whatever NVENC does then.
    Case {
        name: "invalidate",
        continuation: Continuation {
            ltr_frames: 0,
            max_ref_frames: 0,
            refs_l0: 0,
        },
        marks: false,
        use_held: false,
        invalidate: true,
    },
    // Two long-term slots, invalidation, NVENC picking the reference.
    Case {
        name: "ltr",
        continuation: Continuation {
            ltr_frames: 2,
            max_ref_frames: 0,
            refs_l0: 0,
        },
        marks: true,
        use_held: false,
        invalidate: true,
    },
    // With LTR on, the driver predicts from one short-term frame: ask for
    // three, its choice without LTR.
    Case {
        name: "ltr-refs3",
        continuation: Continuation {
            ltr_frames: 2,
            max_ref_frames: 0,
            refs_l0: 3,
        },
        marks: true,
        use_held: false,
        invalidate: true,
    },
    // A larger DPB, as the SDK advises for invalidation, four references,
    // and the held slot named on the continuing frame.
    Case {
        name: "ltr-dpb8-refs4-use",
        continuation: Continuation {
            ltr_frames: 2,
            max_ref_frames: 8,
            refs_l0: 4,
        },
        marks: true,
        use_held: true,
        invalidate: true,
    },
];

fn config(codec: Codec) -> EncoderConfig {
    let (width, height) = size();
    EncoderConfig {
        codec,
        width,
        height,
        fps: 60,
        bitrate_bps: 10_000_000,
    }
}

fn nv12(decoded: Vec<DecodedFrame>) -> Option<Nv12Frame> {
    decoded.into_iter().next().map(|d| match d {
        DecodedFrame::CpuNv12(f) => f,
        _ => panic!("expected an NV12 frame"),
    })
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

fn dropped(i: usize) -> bool {
    DROPS.iter().any(|d| d.contains(&i))
}

/// What one case gave, per decoder kind.
#[derive(Debug, Default)]
struct Outcome {
    errors: Vec<String>,
    /// Pictures from a continuation on that differ from the whole stream's.
    differing: Vec<(usize, f64)>,
    compared: usize,
}

#[test]
fn continuing_after_dropped_frames() {
    let dump = std::env::var("COUCH_NVENC_DUMP").ok();
    let mut failures = Vec::new();
    for codec in [Codec::H264, Codec::Hevc] {
        for case in CASES {
            let label = format!("{codec:?}/{}", case.name);
            let (w, h) = size();
            let mut uploader = Uploader::new(w, h).expect("D3D11 device");
            let mut pattern = TestPattern::new(w, h, 60, Arc::new(ProcessClock::new()));
            let first = uploader.upload(&pattern.render()).unwrap().unwrap();
            let mut encoder =
                NvencEncoder::open_continuing(config(codec), &first, case.continuation)
                    .expect("NVENC directly");
            if case.name == "invalidate" {
                println!("{codec:?}: {:?}", encoder.continuation_caps().unwrap());
            }

            let mut units = Vec::new();
            let mut reports = Vec::new();
            // Long-term slots: which frame each holds, and whether the viewer
            // got it (the host would know from the receiver's reports).
            let mut slots: [Option<(usize, bool)>; 2] = [None, None];
            let mut last_mark = 0usize;
            let mut pending_invalid: Vec<u64> = Vec::new();
            let mut continuing_frames = Vec::new();
            for i in 0..FRAMES {
                let on_gpu = if i == 0 {
                    first.clone()
                } else {
                    uploader
                        .upload(&pattern.render())
                        .unwrap()
                        .expect("a free texture")
                };
                let continuing = i > 0 && !dropped(i) && dropped(i - 1);
                if continuing {
                    if case.invalidate {
                        for &t in &pending_invalid {
                            encoder.invalidate(t).unwrap();
                        }
                    }
                    // A mark the viewer never got is invalidated with its
                    // frame: its slot is free again.
                    for slot in &mut slots {
                        if matches!(slot, Some((_, false))) {
                            *slot = None;
                        }
                    }
                }
                // The slot holding the newest mark the viewer got; the next
                // mark goes into the other, and only once that one was got.
                let held = (0..2)
                    .filter_map(|s| slots[s].filter(|&(_, got)| got).map(|(f, _)| (f, s)))
                    .max()
                    .map(|(_, s)| s as u32);
                let newest_got = slots
                    .iter()
                    .flatten()
                    .max_by_key(|&&(f, _)| f)
                    .is_none_or(|&(_, got)| got);
                let mark = (i == 0 || i >= last_mark + MARK_EVERY && newest_got)
                    .then(|| held.map_or(0, |h| 1 - h));
                let control = FrameControl {
                    keyframe: i == 0,
                    mark_ltr: if case.marks { mark } else { None },
                    use_ltr: (continuing && case.use_held)
                        .then(|| held.map(|h| 1 << h))
                        .flatten(),
                };
                let (unit, report) = encoder.encode_frame(&on_gpu, control).unwrap().unwrap();
                if let (true, Some(slot)) = (case.marks, control.mark_ltr) {
                    slots[slot as usize] = Some((i, !dropped(i)));
                    last_mark = i;
                    assert_eq!(
                        report.ltr_marked,
                        Some(slot),
                        "{label} frame {i}: asked to mark slot {slot}"
                    );
                }
                if dropped(i) {
                    pending_invalid.push(report.timestamp);
                } else if continuing {
                    pending_invalid.clear();
                    continuing_frames.push(i);
                    println!(
                        "{label} frame {i} continues: type {} ({} bytes, {} before), long-term used {:#b}, held slot {held:?}",
                        report.picture_type,
                        unit.data.len(),
                        units
                            .last()
                            .map_or(0, |u: &couch_media::EncodedFrame| u.data.len()),
                        report.ltr_used
                    );
                }
                units.push(unit);
                reports.push(report);
            }
            let ltr_marked = reports.iter().filter(|r| r.ltr_marked.is_some()).count();
            println!("{label}: {ltr_marked} frames marked long-term");

            if let Some(dir) = &dump {
                let ext = if codec == Codec::Hevc { "hevc" } else { "h264" };
                let name = format!("{codec:?}-{}", case.name).to_lowercase();
                let whole: Vec<u8> = units.iter().flat_map(|u| u.data.to_vec()).collect();
                let kept: Vec<u8> = units
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !dropped(*i))
                    .flat_map(|(_, u)| u.data.to_vec())
                    .collect();
                std::fs::write(format!("{dir}/{name}-whole.{ext}"), whole).unwrap();
                std::fs::write(format!("{dir}/{name}-dropped.{ext}"), kept).unwrap();
            }

            // The viewer got none of a drop's frames, or (reports lagging, a
            // window that fell behind naming an older frame) decoded the first
            // few past the frame it continues from.
            let variants: [Variant; 2] = [
                ("dropped", |i| !dropped(i)),
                ("partly", |i| !dropped(i) || partly(i)),
            ];
            for hardware in [false, true] {
                for (variant, gets) in variants {
                    let mut whole = FfmpegDecoder::new(codec, hardware).unwrap();
                    let mut cut = FfmpegDecoder::new(codec, hardware).unwrap();
                    let kind = format!("{:?}, {variant}", whole.kind());
                    let mut outcome = Outcome::default();
                    let mut since_continuation = false;
                    for (i, unit) in units.iter().enumerate() {
                        let a = whole.decode(&unit.data, unit.captured_at);
                        if !gets(i) || dropped(i) {
                            if let Err(e) = a {
                                outcome.errors.push(format!("whole, frame {i}: {e}"));
                            }
                            if gets(i)
                                && let Err(e) = cut.decode(&unit.data, unit.captured_at)
                            {
                                outcome.errors.push(format!("{variant}, frame {i}: {e}"));
                            }
                            continue;
                        }
                        since_continuation |= continuing_frames.contains(&i);
                        let b = cut.decode(&unit.data, unit.captured_at);
                        match (a, b) {
                            (Ok(a), Ok(b)) => {
                                let (Some(a), Some(b)) = (nv12(a), nv12(b)) else {
                                    outcome.errors.push(format!("frame {i}: no picture"));
                                    continue;
                                };
                                if since_continuation {
                                    outcome.compared += 1;
                                    if a.y != b.y || a.uv != b.uv {
                                        outcome.differing.push((i, psnr(&a, &b)));
                                    }
                                }
                            }
                            (a, b) => {
                                for (which, r) in [("whole", a.err()), (variant, b.err())] {
                                    if let Some(e) = r {
                                        outcome.errors.push(format!("{which}, frame {i}: {e}"));
                                    }
                                }
                            }
                        }
                    }
                    println!(
                        "{label} {kind}: {} compared, {} differ {:?}, errors {:?}",
                        outcome.compared,
                        outcome.differing.len(),
                        &outcome.differing[..outcome.differing.len().min(6)],
                        &outcome.errors[..outcome.errors.len().min(6)],
                    );
                    if !outcome.errors.is_empty() || !outcome.differing.is_empty() {
                        failures.push(format!("{label} {kind}"));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "cases that didn't continue cleanly: {failures:?}"
    );
}

/// A viewer's decoder: its name, and whether it gets frame `i`.
type Variant = (&'static str, fn(usize) -> bool);

/// The first frames of each drop, which a viewer may have decoded anyway.
fn partly(i: usize) -> bool {
    DROPS.iter().any(|d| d.contains(&i) && i < d.start + 4)
}

/// The host's own bookkeeping (`couch_media::continuation::Continuity`, as
/// the encode loop drives it) against NVENC opened as hosts open it
/// (`CONTINUING`), in HEVC: what it invalidates must be all NVENC may still
/// refer to (the review's question: only frames within `REACH` and
/// long-term ones). Recoveries: a catch-up after 20 frames left out with the
/// viewer's reports lagging (it decoded a few frames past the point), a
/// window that fell behind naming an older frame than it received, two
/// continuations from the same frame, a drop of 22 frames (past `REACH`),
/// and one of 40, past `MAX_AGE`, which must get a keyframe. The viewer's
/// decoder gets what it would decode; another gets every frame; from the
/// first recovery on, the pictures must be the same, bit for bit.
#[test]
fn the_hosts_bookkeeping_against_nvenc() {
    use couch_codec_nvenc::CONTINUING;
    use couch_media::{
        LongTerm,
        continuation::{Continuity, MAX_AGE},
    };
    use couch_proto::media::FrameRef;
    use std::time::{Duration, Instant};

    /// What happens at a frame.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Viewer {
        /// It gets and decodes the frame.
        Decodes,
        /// It never gets it.
        Lost,
        /// It gets it but doesn't decode it (fallen behind).
        Behind,
    }
    const FRAMES: usize = 400;
    /// Reports say what arrived this many frames ago.
    const LAG: usize = 3;
    let viewer = |i: usize| match i {
        40..60 | 240..262 | 300..340 => Viewer::Lost,
        111..120 => Viewer::Behind,
        _ => Viewer::Decodes,
    };
    // Recoveries: at these frames, continue from what the viewer received
    // (`None`) or from the frame a fell-behind window names.
    let recover_at = |i: usize| -> Option<Option<usize>> {
        match i {
            60 | 180 | 262 | 340 => Some(None),
            120 => Some(Some(110)),
            // Again from the frame the one at 180 continued from.
            184 => Some(Some(usize::MAX)),
            _ => None,
        }
    };

    let (w, h) = size();
    let mut uploader = Uploader::new(w, h).expect("D3D11 device");
    let mut pattern = TestPattern::new(w, h, 60, Arc::new(ProcessClock::new()));
    let first = uploader.upload(&pattern.render()).unwrap().unwrap();
    let mut encoder =
        NvencEncoder::open_continuing(config(Codec::Hevc), &first, CONTINUING).expect("NVENC");
    let mut c = Continuity::new();
    let mut t = Instant::now();
    let (mut epoch, mut seq) = (1u32, 0u32);
    // Each frame's name, unit, and whether the viewer's decoder gets it.
    let mut frames: Vec<FrameRef> = Vec::new();
    let mut units = Vec::new();
    let mut delivered: Vec<FrameRef> = Vec::new();
    let mut last_from: Option<FrameRef> = None;
    let (mut continued, mut keyframes_after) = (Vec::new(), Vec::new());
    for i in 0..FRAMES {
        let on_gpu = if i == 0 {
            first.clone()
        } else {
            uploader
                .upload(&pattern.render())
                .unwrap()
                .expect("a free texture")
        };
        let mut keyframe = i == 0;
        if let Some(point) = recover_at(i) {
            let from = match point {
                None => c.newest_received(),
                Some(usize::MAX) => last_from,
                Some(j) => Some(frames[j]),
            };
            epoch += 1;
            seq = 0;
            match from.and_then(|f| c.plan(f, t)) {
                Some(plan) => {
                    for &ts in &plan.invalidate {
                        encoder.invalidate(ts).unwrap();
                    }
                    last_from = Some(plan.from);
                    continued.push(i);
                }
                None => {
                    keyframe = true;
                    keyframes_after.push(i);
                }
            }
        }
        let control = FrameControl {
            keyframe,
            mark_ltr: c.mark(t),
            use_ltr: None,
        };
        let (unit, report) = encoder.encode_frame(&on_gpu, control).unwrap().unwrap();
        let frame = FrameRef { epoch, seq };
        seq += 1;
        let long_term = LongTerm {
            timestamp: report.timestamp,
            marked: report.ltr_marked,
        };
        assert!(c.sent(frame, unit.keyframe, long_term, t));
        if viewer(i) != Viewer::Lost {
            delivered.push(frame);
            if delivered.len() > LAG {
                c.received(delivered[delivered.len() - 1 - LAG]);
            }
        }
        frames.push(frame);
        units.push(unit);
        t += Duration::from_micros(16_667);
    }
    println!("continued at {continued:?}, keyframes at {keyframes_after:?}");
    assert_eq!(
        continued,
        [60, 120, 180, 184, 262],
        "every recovery within reach continues"
    );
    assert_eq!(keyframes_after, [340], "past {MAX_AGE:?}, a keyframe");

    for hardware in [false, true] {
        let mut whole = FfmpegDecoder::new(Codec::Hevc, hardware).unwrap();
        let mut cut = FfmpegDecoder::new(Codec::Hevc, hardware).unwrap();
        let kind = format!("{:?}", whole.kind());
        let (mut compared, mut differing, mut errors) = (0, Vec::new(), Vec::new());
        for (i, unit) in units.iter().enumerate() {
            let a = whole.decode(&unit.data, unit.captured_at);
            if viewer(i) != Viewer::Decodes {
                if let Err(e) = a {
                    errors.push(format!("whole, frame {i}: {e}"));
                }
                continue;
            }
            match (a, cut.decode(&unit.data, unit.captured_at)) {
                (Ok(a), Ok(b)) => {
                    let (Some(a), Some(b)) = (nv12(a), nv12(b)) else {
                        errors.push(format!("frame {i}: no picture"));
                        continue;
                    };
                    if i >= 60 {
                        compared += 1;
                        if a.y != b.y || a.uv != b.uv {
                            differing.push((i, psnr(&a, &b)));
                        }
                    }
                }
                (a, b) => errors.push(format!("frame {i}: {:?} {:?}", a.err(), b.err())),
            }
        }
        println!(
            "{kind}: {compared} compared, {} differ {:?}, errors {:?}",
            differing.len(),
            &differing[..differing.len().min(6)],
            &errors[..errors.len().min(6)]
        );
        assert!(
            differing.is_empty() && errors.is_empty(),
            "{kind}: the bookkeeping left a reference the viewer lacks"
        );
    }
}
