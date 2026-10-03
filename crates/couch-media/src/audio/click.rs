//! A click track for measuring audio latency, the synthetic audio source
//! that goes with the test pattern, and a detector that finds the clicks
//! again in captured audio.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use super::{CHANNELS, FRAME_LEN, SAMPLE_RATE};
use crate::{AudioChunk, AudioSource, MediaError, Micros, clock::Clock};

/// A 5 ms, 1 kHz burst at 0.8 every half second, silence in between.
#[derive(Debug, Clone, Default)]
pub struct ClickTrack {
    /// Samples per channel produced so far.
    position: u64,
}

impl ClickTrack {
    pub const PERIOD_US: u64 = 500_000;
    const PERIOD: u64 = SAMPLE_RATE as u64 / 2;
    const LENGTH: u64 = SAMPLE_RATE as u64 / 200;
    const AMPLITUDE: f32 = 0.8;

    pub fn new() -> Self {
        Self::default()
    }

    /// Fills `out` (interleaved, [`CHANNELS`] channels) with the next samples.
    pub fn fill(&mut self, out: &mut [f32]) {
        for frame in out.chunks_exact_mut(usize::from(CHANNELS)) {
            let i = self.position % Self::PERIOD;
            let v = if i < Self::LENGTH {
                let t = i as f32 / SAMPLE_RATE as f32;
                (t * 1000.0 * std::f32::consts::TAU).sin() * Self::AMPLITUDE
            } else {
                0.0
            };
            frame.fill(v);
            self.position += 1;
        }
    }
}

/// The test pattern's audio: the click track, in 10 ms chunks paced in real
/// time. Chunks are stamped 10 ms apart, like a sound card's, not with the
/// moment the thread woke up: the viewer's jitter buffer reads any
/// unevenness in the stamps as network jitter.
pub struct ToneSource {
    track: ClickTrack,
    next_due: Instant,
    /// The next chunk's capture time; re-read from the clock after a stall.
    next_stamp: Option<u64>,
    clock: Arc<dyn Clock>,
}

impl ToneSource {
    const CHUNK: Duration = Duration::from_millis(10);

    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            track: ClickTrack::new(),
            next_due: Instant::now(),
            next_stamp: None,
            clock,
        }
    }
}

impl AudioSource for ToneSource {
    fn next_chunk(&mut self, timeout: Duration) -> Result<Option<AudioChunk>, MediaError> {
        let now = Instant::now();
        if self.next_due > now {
            let wait = self.next_due - now;
            if wait > timeout {
                std::thread::sleep(timeout);
                return Ok(None);
            }
            std::thread::sleep(wait);
        }
        // Audio must stay contiguous, so a late chunk is still produced; only
        // a long stall (the thread didn't run) skips ahead.
        let now = Instant::now();
        let floor = now.checked_sub(Duration::from_millis(100)).unwrap_or(now);
        let stalled = self.next_due < floor;
        self.next_due = self.next_due.max(floor) + Self::CHUNK;
        let stamp = match self.next_stamp {
            Some(stamp) if !stalled => stamp,
            _ => self.clock.now().0,
        };
        self.next_stamp = Some(stamp + Self::CHUNK.as_micros() as u64);
        let mut samples = vec![0.0; FRAME_LEN];
        self.track.fill(&mut samples);
        Ok(Some(AudioChunk {
            samples,
            channels: CHANNELS,
            sample_rate: SAMPLE_RATE,
            captured_at: Micros(stamp),
        }))
    }
}

/// Finds the start of each click: the first sample at or above a threshold
/// after a quiet spell.
#[derive(Debug, Clone)]
pub struct OnsetDetector {
    threshold: f32,
    holdoff_us: u64,
    last_loud_us: Option<u64>,
}

impl OnsetDetector {
    /// Loud means at least `threshold` in either channel; an onset needs
    /// `holdoff_us` without anything loud before it.
    pub fn new(threshold: f32, holdoff_us: u64) -> Self {
        Self {
            threshold,
            holdoff_us,
            last_loud_us: None,
        }
    }

    /// Scans interleaved stereo whose first sample played at `start_us`,
    /// appending onset times to `out`.
    pub fn feed(&mut self, samples: &[f32], start_us: u64, out: &mut Vec<u64>) {
        for (i, frame) in samples.chunks_exact(usize::from(CHANNELS)).enumerate() {
            let peak = frame.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            if peak < self.threshold {
                continue;
            }
            let t = start_us + i as u64 * 1_000_000 / u64::from(SAMPLE_RATE);
            if self
                .last_loud_us
                .is_none_or(|last| t >= last + self.holdoff_us)
            {
                out.push(t);
            }
            self.last_loud_us = Some(t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ProcessClock;

    #[test]
    fn clicks_are_found_where_they_start() {
        let mut track = ClickTrack::new();
        let mut detector = OnsetDetector::new(0.2, 200_000);
        let mut onsets = Vec::new();
        // Two seconds, in 10 ms chunks, starting at t = 1 s.
        for chunk in 0..200u64 {
            let mut samples = vec![0.0; FRAME_LEN];
            track.fill(&mut samples);
            detector.feed(&samples, 1_000_000 + chunk * 10_000, &mut onsets);
        }
        assert_eq!(onsets.len(), 4, "{onsets:?}");
        for (i, onset) in onsets.iter().enumerate() {
            let expected = 1_000_000 + i as u64 * ClickTrack::PERIOD_US;
            // The sine crosses 0.2 within a few samples.
            assert!((expected..expected + 200).contains(onset), "{onset}");
        }
    }

    #[test]
    fn the_tone_source_keeps_real_time() {
        let mut source = ToneSource::new(Arc::new(ProcessClock::new()));
        let start = Instant::now();
        let mut stamps = Vec::new();
        while stamps.len() < 5 {
            if let Some(chunk) = source.next_chunk(Duration::from_secs(1)).unwrap() {
                assert_eq!((chunk.channels, chunk.sample_rate), (2, 48_000));
                assert_eq!(chunk.samples.len(), FRAME_LEN);
                stamps.push(chunk.captured_at.0);
            }
        }
        // Five chunks: the first at once, then four 10 ms waits.
        assert!(start.elapsed() >= Duration::from_millis(35));
        // Stamped exactly 10 ms apart, however the waits went.
        assert!(
            stamps.windows(2).all(|w| w[1] - w[0] == 10_000),
            "{stamps:?}"
        );
    }
}
