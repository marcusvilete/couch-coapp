//! Click-to-playback latency on one machine, for `latency-probe --audio`.
//!
//! The latency clock clicks twice a second. This captures, with process
//! loopback, both the clock's own output (where its clicks enter Windows'
//! mixer) and the viewer's output (where they come back out of the stream),
//! and pairs the clicks up. Both captures go through the same loopback path
//! and are stamped on the QPC clock, so its own delay cancels out: what is
//! left is capture, encoding, the network, the jitter buffer and playback
//! queueing. The audio device's own output buffer is not included, as it is
//! the same for the game played locally.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

use anyhow::Context;
use couch_media::{AudioSource, audio::OnsetDetector};
use couch_platform_win::audio::{LoopbackCapture, LoopbackTarget};

use crate::stats::Samples;

/// Clicks come every 500 ms; anything later than this isn't the same click.
const MAX_LATENCY_US: u64 = 400_000;

pub struct AudioProbe {
    stop: StopOnDrop,
    reference: JoinHandle<anyhow::Result<Vec<u64>>>,
    output: JoinHandle<anyhow::Result<Vec<u64>>>,
}

/// Stops the listeners however the probe ends, an early error included.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

pub struct AudioResult {
    /// Click to playback, milliseconds.
    pub latency: Samples,
    pub clicks: usize,
}

impl AudioProbe {
    /// Listens to `clock_pid` (the latency clock) and `viewer_pid` (the
    /// `view` process, whose child plays the stream).
    pub fn start(clock_pid: u32, viewer_pid: u32) -> anyhow::Result<Self> {
        let stop = StopOnDrop(Arc::new(AtomicBool::new(false)));
        let spawn = |pid: u32, name: &str| {
            let stop = stop.0.clone();
            std::thread::Builder::new()
                .name(name.into())
                .spawn(move || listen(pid, &stop))
                .context("starting an audio listener")
        };
        Ok(Self {
            reference: spawn(clock_pid, "listen-clock")?,
            output: spawn(viewer_pid, "listen-viewer")?,
            stop,
        })
    }

    pub fn finish(self) -> anyhow::Result<AudioResult> {
        self.stop.0.store(true, Ordering::SeqCst);
        let join = |h: JoinHandle<anyhow::Result<Vec<u64>>>| {
            h.join()
                .map_err(|_| anyhow::anyhow!("an audio listener panicked"))?
        };
        let reference = join(self.reference).context("listening to the clock")?;
        let output = join(self.output).context("listening to the viewer")?;
        Ok(AudioResult {
            latency: pair(&reference, &output),
            clicks: reference.len(),
        })
    }
}

fn listen(pid: u32, stop: &AtomicBool) -> anyhow::Result<Vec<u64>> {
    let mut capture = LoopbackCapture::start(LoopbackTarget::ProcessTree(pid))?;
    // Clicks peak at 0.8; the stream's quiet parts are silence.
    let mut detector = OnsetDetector::new(0.2, 200_000);
    let mut onsets = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        if let Some(chunk) = capture.next_chunk(Duration::from_millis(100))? {
            detector.feed(&chunk.samples, chunk.captured_at.0, &mut onsets);
        }
    }
    Ok(onsets)
}

/// For each click, the first output onset after it (within
/// [`MAX_LATENCY_US`]), in milliseconds. Clicks the viewer never played,
/// before the stream started for instance, are left out.
fn pair(reference: &[u64], output: &[u64]) -> Samples {
    let mut latency = Samples::default();
    let mut j = 0;
    for &click in reference {
        while output.get(j).is_some_and(|&o| o <= click) {
            j += 1;
        }
        if let Some(&heard) = output.get(j)
            && heard - click < MAX_LATENCY_US
        {
            latency.push((heard - click) as f64 / 1000.0);
            j += 1;
        }
    }
    latency
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicks_pair_with_the_next_onset() {
        let ms = |v: u64| v * 1000;
        let reference = [ms(1000), ms(1500), ms(2000), ms(2500)];
        // The first click was never played, and the last came too late.
        let output = [ms(1545), ms(2050), ms(2990)];
        let latency = pair(&reference, &output);
        assert_eq!(latency.len(), 2);
        assert_eq!(latency.percentile(0.0), Some(45.0));
        assert_eq!(latency.percentile(100.0), Some(50.0));
    }
}
