//! `couch-cli hdr-probe` (M29): each monitor's colour as Windows reports it,
//! then one frame of a window or monitor grabbed twice, in 8-bit BGRA (what
//! the host captures today) and in FP16 (scRGB), with what each holds. On a
//! monitor in HDR it shows where SDR white lands in an FP16 capture: at 1.0,
//! or raised to the SDR white level.

use std::{collections::HashMap, time::Duration};

use anyhow::{Context, bail};
use couch_media::{BgraFrame, CaptureEvent, Capturer, hdr::srgb_encode};
use couch_platform_win::{
    capture::list_monitors,
    hdr::{Grabbed, PATTERN_TITLE, Pattern, grab, patches},
    monitor, top_windows,
    wgc::{Format, Target},
};
use couch_platform_win::{
    capture::{CaptureOptions, CaptureSource, FrameOutput, WgcCapturer},
    foreground::WindowId,
};

pub fn parse_point(text: &str) -> Result<(u32, u32), String> {
    let (x, y) = text.split_once(',').ok_or("expected X,Y")?;
    let parse = |v: &str| v.trim().parse::<u32>().map_err(|e| e.to_string());
    Ok((parse(x)?, parse(y)?))
}

pub fn run(
    window: Option<String>,
    monitor: Option<usize>,
    points: &[(u32, u32)],
    pattern_window: bool,
    bench: bool,
    show_pattern: Option<u64>,
) -> anyhow::Result<()> {
    if let Some(secs) = show_pattern {
        let (area, white) = list_monitors()?
            .iter()
            .find_map(|m| {
                let c = monitor::colour(m.monitor.0).filter(|c| c.hdr)?;
                Some((
                    monitor::whole(m.monitor.0)?,
                    c.sdr_white_nits.unwrap_or(80.0) / 80.0,
                ))
            })
            .context("no monitor is in HDR")?;
        let (width, height) = (
            640.min((area.width as u32).saturating_sub(80).max(64)) & !7,
            360.min((area.height as u32).saturating_sub(80).max(48)) / 3 * 3,
        );
        let _shown = Pattern::open(area.x + 40, area.y + 40, width, height, white)?;
        println!("{PATTERN_TITLE:?} shown for {secs} s");
        std::thread::sleep(Duration::from_secs(secs));
        return Ok(());
    }
    if bench {
        println!("GPU time per frame, median of 200 (the capture's device):");
        for (w, h) in [(1920, 1080), (2560, 1440), (3840, 2160)] {
            let (copy, convert) = couch_platform_win::tonemap::bench(w, h, 200)?;
            println!(
                "  {w}x{h}: copy (SDR) {copy:.3} ms, conversion (HDR) {convert:.3} ms, {:.1}x",
                convert / copy.max(1e-6)
            );
        }
        return Ok(());
    }
    let monitors = list_monitors()?;
    println!("Monitors:");
    for (i, m) in monitors.iter().enumerate() {
        let colour = match monitor::colour(m.monitor.0) {
            Some(c) => format!(
                "{} (colour space {}, {} bits), SDR white {}, peak {:.0} nits, full frame {:.0}",
                if c.hdr { "HDR" } else { "SDR" },
                c.colour_space,
                c.bits_per_colour,
                c.sdr_white_nits.map_or("unknown".into(), |w| format!(
                    "{w:.0} nits (scRGB {:.3})",
                    w / 80.0
                )),
                c.max_nits,
                c.max_full_frame_nits,
            ),
            None => "colour unknown (DXGI doesn't list it)".into(),
        };
        println!(
            "  {}. {} {}x{}{}: {colour}",
            i + 1,
            m.name,
            m.width,
            m.height,
            if m.primary { " primary" } else { "" },
        );
    }

    if pattern_window {
        return pattern(&monitors);
    }
    let (target, what, white) = match (window, monitor) {
        (Some(_), Some(_)) => bail!("--window or --monitor, not both"),
        (Some(title), None) => {
            let w = top_windows::find_by_title(&title)
                .with_context(|| format!("no window titled {title:?}"))?;
            let white = monitor::colour_of_window(w.0).and_then(|c| c.sdr_white_nits);
            (Target::Window(w.0), format!("window {title:?}"), white)
        }
        (None, n) => {
            let n = n.unwrap_or_else(|| monitors.iter().position(|m| m.primary).unwrap_or(0) + 1);
            let m = monitors
                .get(n.wrapping_sub(1))
                .with_context(|| format!("no monitor {n}"))?;
            let white = monitor::colour(m.monitor.0).and_then(|c| c.sdr_white_nits);
            (Target::Monitor(m.monitor.0), format!("monitor {n}"), white)
        }
    };
    println!("\nGrabbing {what}:");
    let timeout = Duration::from_secs(3);
    let bgra = grab(target, Format::Bgra8, timeout).context("the 8-bit grab")?;
    let fp16 = grab(target, Format::Fp16, timeout).context("the FP16 grab")?;
    for g in [&bgra, &fp16] {
        summary(g);
    }
    let mut points = points.to_vec();
    if points.is_empty() {
        points.push((fp16.width / 2, fp16.height / 2));
    }
    let scale = white.map(|w| w / 80.0);
    println!(
        "\nPixels (8-bit: stored/255, sRGB; FP16: linear scRGB, then sRGB-encoded after dividing by the SDR white):"
    );
    for (x, y) in points {
        if x >= bgra.width.min(fp16.width) || y >= bgra.height.min(fp16.height) {
            println!("  ({x}, {y}): outside the picture");
            continue;
        }
        let b = bgra.rgb(x, y);
        let f = fp16.rgb(x, y);
        let mapped = scale.map(|s| f.map(|v| srgb_encode(v / s)));
        println!(
            "  ({x}, {y}): 8-bit {}  FP16 {}{}",
            triple(b),
            triple(f),
            mapped.map_or(String::new(), |m| format!("  -> {}", triple(m))),
        );
    }
    Ok(())
}

/// `--pattern`: the HDR pattern opened on the first monitor in HDR, grabbed
/// in both formats, each patch read at its centre and set beside what an SDR
/// picture of it should hold (its scRGB over the SDR white, clipped, in sRGB).
fn pattern(monitors: &[couch_platform_win::capture::MonitorInfo]) -> anyhow::Result<()> {
    let (n, m, colour) = monitors
        .iter()
        .enumerate()
        .find_map(|(i, m)| {
            monitor::colour(m.monitor.0)
                .filter(|c| c.hdr)
                .map(|c| (i + 1, m, c))
        })
        .context("no monitor is in HDR")?;
    let white = colour.sdr_white_nits.unwrap_or(80.0) / 80.0;
    let area = monitor::whole(m.monitor.0).context("the monitor went")?;
    let (width, height) = (
        640.min((area.width as u32).saturating_sub(80).max(64)) & !7,
        360.min((area.height as u32).saturating_sub(80).max(48)) / 3 * 3,
    );
    println!(
        "\nThe pattern on monitor {n} ({}), SDR white scRGB {white:.3}:",
        m.name
    );
    let shown = Pattern::open(area.x + 40, area.y + 40, width, height, white)?;
    std::thread::sleep(Duration::from_millis(1500));
    let target = Target::Window(shown.window());
    let timeout = Duration::from_secs(3);
    let bgra = grab(target, Format::Bgra8, timeout).context("the 8-bit grab")?;
    let fp16 = grab(target, Format::Fp16, timeout).context("the FP16 grab")?;
    // The host's own capture, as a stream gets its frames: on the GPU
    // through the texture pool, and to memory (M29's conversion in both).
    let on_gpu = host_capture(shown.window(), FrameOutput::Gpu).context("the host's capture")?;
    let in_memory =
        host_capture(shown.window(), FrameOutput::Cpu).context("the host's capture to memory")?;
    println!(
        "  {:<26} {:<24} {:<24} {:<24} {:<24} {:<24}",
        "patch",
        "shown (scRGB)",
        "FP16 capture",
        "8-bit (before M29)",
        "host capture",
        "SDR should be"
    );
    let mut off = 0;
    for patch in patches(width, height, white) {
        let (x, y) = patch.centre();
        let should = patch.scrgb.map(|v| srgb_encode(v / white));
        let host = pixel(&on_gpu, x, y);
        let memory = pixel(&in_memory, x, y);
        // One step of 8 bits either way.
        let right = |got: [f32; 3]| (0..3).all(|c| (got[c] - should[c]).abs() <= 1.5 / 255.0);
        let verdict = match (right(host), right(memory)) {
            (true, true) => "",
            (false, true) => "  <- GPU path off",
            (true, false) => "  <- memory path off",
            (false, false) => "  <- off",
        };
        off += usize::from(!verdict.is_empty());
        println!(
            "  {:<26} {:<24} {:<24} {:<24} {:<24} {:<24}{verdict}",
            patch.name,
            triple(patch.scrgb),
            triple(fp16.rgb(x, y)),
            triple(bgra.rgb(x, y)),
            triple(host),
            triple(should),
        );
    }
    if off == 0 {
        println!("\nThe host's capture holds what SDR should, on both paths.");
    } else {
        println!("\n{off} patches differ from what SDR should hold.");
    }
    let grey_20 = patches(width, height, white)[0].centre();
    follow(&shown, monitors, &area, white, grey_20)?;
    Ok(())
}

/// One host capture of the pattern kept running while the window moves to
/// a monitor not in HDR and back: the capture follows (8-bit there, where
/// Windows itself clips the pattern at 80 nits; FP16 and converted back
/// here), and frames keep coming.
fn follow(
    shown: &Pattern,
    monitors: &[couch_platform_win::capture::MonitorInfo],
    hdr_area: &monitor::Area,
    white: f32,
    (x, y): (u32, u32),
) -> anyhow::Result<()> {
    let Some(sdr) = monitors.iter().find_map(|m| {
        monitor::colour(m.monitor.0)
            .is_some_and(|c| !c.hdr)
            .then(|| monitor::whole(m.monitor.0))
            .flatten()
    }) else {
        println!("\nNo monitor without HDR to move the pattern to: the follow check skipped.");
        return Ok(());
    };
    let mut capturer = WgcCapturer::start(
        &CaptureSource::Window(WindowId(shown.window())),
        CaptureOptions {
            min_interval: Duration::from_millis(16),
            cursor: false,
            output: FrameOutput::Gpu,
            hdr: true,
        },
    )?;
    // The 20-nit grey at (`x`, `y`): 0.320 converted under a 240-nit white,
    // 0.537 as Windows clips it on an SDR monitor.
    let expected_here = srgb_encode(0.25 / white);
    println!("\nThe window moved with the host's capture running (the 20-nit grey):");
    for (place, (to_x, to_y), expected) in [
        (
            "on the HDR monitor",
            (hdr_area.x + 40, hdr_area.y + 40),
            expected_here,
        ),
        (
            "moved to an SDR monitor",
            (sdr.x + 40, sdr.y + 40),
            srgb_encode(0.25),
        ),
        (
            "moved back",
            (hdr_area.x + 40, hdr_area.y + 40),
            expected_here,
        ),
    ] {
        shown.move_to(to_x, to_y)?;
        let (frame, count) = latest(&mut capturer, Duration::from_millis(2500))?;
        let got = pixel(&frame, x, y)[0];
        println!(
            "  {place}: {got:.3} (should be {expected:.3}), {count} frames in 2.5 s{}",
            if (got - expected).abs() <= 1.5 / 255.0 {
                ""
            } else {
                "  <- off"
            }
        );
    }
    Ok(())
}

/// The last frame of `capturer` within `wait`, and how many came.
fn latest(capturer: &mut WgcCapturer, wait: Duration) -> anyhow::Result<(BgraFrame, usize)> {
    let until = std::time::Instant::now() + wait;
    let (mut last, mut count) = (None, 0);
    while std::time::Instant::now() < until {
        if let Some(CaptureEvent::Frame(frame)) = capturer.next_event(Duration::from_millis(50))? {
            last = Some(frame);
            count += 1;
        }
    }
    Ok((last.context("no frame came")?.cpu()?, count))
}

/// One frame of `window` through the host's capture ([`WgcCapturer`]), in
/// memory.
fn host_capture(window: isize, output: FrameOutput) -> anyhow::Result<BgraFrame> {
    let mut capturer = WgcCapturer::start(
        &CaptureSource::Window(WindowId(window)),
        CaptureOptions {
            min_interval: Duration::from_millis(16),
            cursor: false,
            output,
            hdr: true,
        },
    )?;
    let until = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < until {
        if let Some(CaptureEvent::Frame(frame)) = capturer.next_event(Duration::from_millis(200))? {
            return Ok(frame.cpu()?);
        }
    }
    bail!("no frame came")
}

fn pixel(frame: &BgraFrame, x: u32, y: u32) -> [f32; 3] {
    let p = frame.pixel(x, y);
    [p[2], p[1], p[0]].map(|v| f32::from(v) / 255.0)
}

fn summary(g: &Grabbed) {
    let mut counts: HashMap<[u32; 3], usize> = HashMap::new();
    let mut brightest = f32::MIN;
    let mut darkest = f32::MAX;
    for y in 0..g.height {
        for x in 0..g.width {
            let p = g.rgb(x, y);
            for v in p {
                brightest = brightest.max(v);
                darkest = darkest.min(v);
            }
            *counts.entry(p.map(f32::to_bits)).or_default() += 1;
        }
    }
    let total = (g.width as usize * g.height as usize).max(1);
    let mut common: Vec<_> = counts.into_iter().collect();
    common.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    println!(
        "  {:?} {}x{}: channels from {darkest:.3} to {brightest:.3}; most common:",
        g.format, g.width, g.height
    );
    for (bits, n) in common.iter().take(3) {
        println!(
            "    {} in {:.1}% of pixels",
            triple(bits.map(f32::from_bits)),
            *n as f64 * 100.0 / total as f64
        );
    }
}

fn triple(v: [f32; 3]) -> String {
    format!("({:.3}, {:.3}, {:.3})", v[0], v[1], v[2])
}
