//! Gate G1b, automated: capture the whole screen and read two barcodes from
//! the same composited frame, the live latency clock and its copy in the
//! viewer window. Their difference is what a photo of the screen would show:
//! latency up to the compositor, without display scan-out.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use couch_media::{BgraFrame, CaptureEvent, Capturer, barcode, clock::LATENCY_CLOCK_CELL};
use couch_platform_win::capture::{CaptureOptions, CaptureSource, FrameOutput, WgcCapturer};

use crate::stats::Samples;

pub struct ScreenProbe {
    samples: Arc<Mutex<Samples>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ScreenProbe {
    pub fn start() -> anyhow::Result<Self> {
        let mut capturer = WgcCapturer::start(
            &CaptureSource::PrimaryMonitor,
            CaptureOptions {
                min_interval: Duration::from_secs(1) / 60,
                cursor: false,
                output: FrameOutput::Cpu,
            },
        )?;
        let samples = Arc::new(Mutex::new(Samples::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (samples, stop) = (samples.clone(), stop.clone());
            thread::Builder::new()
                .name("screen-probe".into())
                .spawn(move || {
                    let mut found: Vec<(u32, u32)> = Vec::new();
                    while !stop.load(Ordering::SeqCst) {
                        let Ok(Some(CaptureEvent::Frame(frame))) =
                            capturer.next_event(Duration::from_millis(100))
                        else {
                            continue;
                        };
                        let Ok(frame) = frame.cpu() else { continue };
                        if let Some(ms) = measure(&frame, &mut found) {
                            samples.lock().unwrap().push(ms);
                        }
                    }
                })?
        };
        Ok(Self {
            samples,
            stop,
            thread: Some(thread),
        })
    }

    pub fn finish(mut self) -> Samples {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.samples.lock().unwrap().clone()
    }
}

/// Reads the barcodes (rescanning when they move) and returns the live
/// clock's value minus the streamed copy's, in milliseconds.
fn measure(frame: &BgraFrame, found: &mut Vec<(u32, u32)>) -> Option<f64> {
    let cell = LATENCY_CLOCK_CELL;
    // Real barcodes carry times from the last couple of seconds.
    let now = couch_platform_win::clock::now().0 & barcode::MAX_VALUE;
    let plausible = |v: &u64| *v <= now + 100_000 && now.saturating_sub(*v) < 2_000_000;
    let read_all = |found: &[(u32, u32)]| -> Vec<u64> {
        found
            .iter()
            .filter_map(|&(x, y)| barcode::read(frame, x, y, cell))
            .filter(plausible)
            .collect()
    };
    let mut values = read_all(found);
    if values.len() != 2 {
        *found = scan(frame, cell);
        values = read_all(found);
    }
    let [a, b] = values[..] else {
        return None;
    };
    Some(a.abs_diff(b) as f64 / 1000.0)
}

/// `couch-cli screen-scan`: one screen capture, every barcode on it, and
/// optionally the raw BGRA frame for inspection.
pub fn scan_once(out: Option<&std::path::Path>) -> anyhow::Result<()> {
    let mut capturer = WgcCapturer::start(
        &CaptureSource::PrimaryMonitor,
        CaptureOptions {
            min_interval: Duration::from_millis(1),
            cursor: false,
            output: FrameOutput::Cpu,
        },
    )?;
    let frame = match capturer.next_event(Duration::from_secs(3))? {
        Some(CaptureEvent::Frame(frame)) => frame.cpu()?,
        Some(CaptureEvent::SourceClosed) => anyhow::bail!("monitor capture closed"),
        // (Frames in memory: never a GPU failure.)
        Some(CaptureEvent::GpuFramesFailed) | None => anyhow::bail!("no screen frame within 3 s"),
    };
    println!("{}x{} stride {}", frame.width, frame.height, frame.stride);
    for (x, y) in scan(&frame, LATENCY_CLOCK_CELL) {
        println!(
            "barcode at ({x}, {y}): {:?}",
            barcode::read(&frame, x, y, LATENCY_CLOCK_CELL)
        );
    }
    if let Some(path) = out {
        let row = frame.width as usize * 4;
        let mut tight = Vec::with_capacity(row * frame.height as usize);
        for y in 0..frame.height as usize {
            tight.extend_from_slice(&frame.data[y * frame.stride as usize..][..row]);
        }
        std::fs::write(path, tight)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

/// Every barcode on screen, scanning rows half a cell apart.
fn scan(frame: &BgraFrame, cell: u32) -> Vec<(u32, u32)> {
    let width = barcode::width(cell);
    let mut hits: Vec<(u32, u32)> = Vec::new();
    let mut y = 0;
    while y + cell <= frame.height {
        let mut x = 0;
        while x + width <= frame.width {
            let near_hit = hits
                .iter()
                .any(|&(hx, hy)| hx.abs_diff(x) < cell && hy.abs_diff(y) < cell);
            if !near_hit && barcode::read(frame, x, y, cell).is_some() {
                hits.push((x, y));
                x += width;
                continue;
            }
            x += 1;
        }
        y += cell / 2;
    }
    hits
}
