//! How loud the sound is, and changing it (P6, P7):
//! - [`ramp_gain`] scales samples by a gain that moves smoothly from the last
//!   one to a new one, so a change never clicks;
//! - [`soft_limit`] rounds off peaks a boost pushed past full scale, so they
//!   never crackle;
//! - [`LevelMeter`] measures the peak and the average (RMS) level over
//!   short windows, in dB relative to full scale, for the host's meter and
//!   the stream window's stats.

use super::{CHANNELS, SAMPLE_RATE};

/// The quietest level reported: below this is silence.
pub const SILENCE_DB: f32 = -90.0;

/// The host's stream level goes up to this (P7): four times the game, to
/// make up for a game turned down to a quarter in Windows' volume mixer.
pub const MAX_BOOST_PERCENT: u16 = 400;

/// Where the soft limiter starts bending: peaks above this are rounded
/// toward full scale, never past it.
const LIMIT_KNEE: f32 = 0.9;

/// Keeps every sample within full scale after a boost: below the knee
/// untouched, above it bent smoothly toward 1 (a `tanh` curve), so a loud
/// moment sounds compressed, not crackling. Returns whether it bent any.
pub fn soft_limit(samples: &mut [f32]) -> bool {
    let room = 1.0 - LIMIT_KNEE;
    let mut bent = false;
    for sample in samples {
        let level = sample.abs();
        if level > LIMIT_KNEE && level.is_finite() {
            let over = (level - LIMIT_KNEE) / room;
            *sample = sample.signum() * (LIMIT_KNEE + room * over.tanh());
            bent = true;
        } else if !sample.is_finite() {
            *sample = 0.0;
        }
    }
    bent
}

/// The host's stream level (P7), 0 to [`MAX_BOOST_PERCENT`]: up to 100%,
/// the squared curve ([`gain_for_percent`]); above it, a boost by that
/// factor (200% doubles the amplitude, +6 dB), which [`soft_limit`] keeps
/// within full scale.
pub fn host_gain_for_percent(percent: u16) -> f32 {
    match percent.min(MAX_BOOST_PERCENT) {
        p @ 0..=100 => gain_for_percent(p as u8),
        p => f32::from(p) / 100.0,
    }
}

/// Scales interleaved `samples` by a gain going from `from` (before the
/// first frame) to `to` (at the last). At 1 to 1 the samples are untouched.
pub fn ramp_gain(samples: &mut [f32], from: f32, to: f32) {
    let channels = usize::from(CHANNELS);
    let frames = samples.len() / channels;
    if frames == 0 || (from == 1.0 && to == 1.0) {
        return;
    }
    for (i, frame) in samples.chunks_exact_mut(channels).enumerate() {
        let gain = from + (to - from) * (i + 1) as f32 / frames as f32;
        for sample in frame {
            *sample *= gain;
        }
    }
}

/// The gain for a level in percent (0 to 100): its square, so equal steps
/// sound about equal (50% is -12 dB). The stream window's volume uses the
/// same curve.
pub fn gain_for_percent(percent: u8) -> f32 {
    let level = f32::from(percent.min(100)) / 100.0;
    level * level
}

/// One window's levels, in dB relative to full scale (0 is the loudest a
/// sample can be).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Level {
    pub peak_db: f32,
    pub rms_db: f32,
}

impl Level {
    pub const SILENT: Self = Self {
        peak_db: SILENCE_DB,
        rms_db: SILENCE_DB,
    };
}

/// Measures interleaved samples over windows of a fixed length.
#[derive(Debug, Clone)]
pub struct LevelMeter {
    /// Interleaved samples per window.
    window: usize,
    seen: usize,
    peak: f32,
    sum_squares: f64,
}

impl LevelMeter {
    /// Windows of `window_ms` of sound.
    pub fn new(window_ms: u32) -> Self {
        let frames = (SAMPLE_RATE as usize * window_ms as usize / 1000).max(1);
        Self {
            window: frames * usize::from(CHANNELS),
            seen: 0,
            peak: 0.0,
            sum_squares: 0.0,
        }
    }

    /// Adds `samples`; returns a window's levels each time one fills (the
    /// last, if `samples` fill several).
    pub fn push(&mut self, samples: &[f32]) -> Option<Level> {
        let mut done = None;
        for &sample in samples {
            let sample = if sample.is_finite() {
                sample.abs()
            } else {
                0.0
            };
            self.peak = self.peak.max(sample);
            self.sum_squares += f64::from(sample) * f64::from(sample);
            self.seen += 1;
            if self.seen == self.window {
                let rms = (self.sum_squares / self.window as f64).sqrt() as f32;
                done = Some(Level {
                    peak_db: to_db(self.peak),
                    rms_db: to_db(rms),
                });
                (self.seen, self.peak, self.sum_squares) = (0, 0.0, 0.0);
            }
        }
        done
    }
}

fn to_db(amplitude: f32) -> f32 {
    if amplitude <= 0.0 {
        return SILENCE_DB;
    }
    (20.0 * amplitude.log10()).max(SILENCE_DB)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gain_ramps_without_a_jump() {
        let mut samples = vec![1.0f32; 8];
        ramp_gain(&mut samples, 1.0, 0.0);
        let left: Vec<f32> = samples.iter().step_by(2).copied().collect();
        assert_eq!(left, [0.75, 0.5, 0.25, 0.0]);
        assert!(samples.chunks(2).all(|f| f[0] == f[1]));
        let mut samples = vec![0.5f32; 4];
        ramp_gain(&mut samples, 0.5, 0.5);
        assert_eq!(samples, [0.25; 4]);
        let mut samples = vec![0.3f32; 4];
        ramp_gain(&mut samples, 1.0, 1.0);
        assert_eq!(samples, [0.3; 4]);
    }

    #[test]
    fn percent_maps_to_a_squared_gain_never_above_one() {
        assert_eq!(gain_for_percent(100), 1.0);
        assert_eq!(gain_for_percent(0), 0.0);
        assert!((gain_for_percent(50) - 0.25).abs() < 1e-6);
        assert_eq!(gain_for_percent(250), 1.0);
    }

    #[test]
    fn the_hosts_level_turns_down_squared_and_boosts_by_its_factor() {
        assert_eq!(host_gain_for_percent(100), 1.0);
        assert!((host_gain_for_percent(50) - 0.25).abs() < 1e-6);
        assert_eq!(host_gain_for_percent(200), 2.0);
        assert_eq!(host_gain_for_percent(400), 4.0);
        assert_eq!(host_gain_for_percent(1000), 4.0, "capped at 400%");
    }

    #[test]
    fn the_limiter_keeps_a_boost_within_full_scale() {
        // Below the knee: untouched.
        let mut quiet = vec![0.5f32, -0.8, 0.9];
        assert!(!soft_limit(&mut quiet));
        assert_eq!(quiet, [0.5, -0.8, 0.9]);
        // A 4x boost of a loud game: every sample within full scale, the
        // sign kept, louder input still louder output.
        let mut loud: Vec<f32> = [0.3f32, -0.6, 0.95].iter().map(|s| s * 4.0).collect();
        assert!(soft_limit(&mut loud));
        assert!(loud.iter().all(|s| s.abs() <= 1.0), "{loud:?}");
        assert!(loud[1] < 0.0);
        assert!(loud[2] >= loud[0]);
        // Never anything not a number.
        let mut odd = vec![f32::NAN, f32::INFINITY];
        soft_limit(&mut odd);
        assert_eq!(odd, [0.0, 0.0]);
    }

    #[test]
    fn the_meter_reads_peak_and_average() {
        // 10 ms windows: 480 frames, 960 samples.
        let mut meter = LevelMeter::new(10);
        assert_eq!(meter.push(&[0.5; 959]), None);
        let level = meter.push(&[0.5]).expect("a window");
        assert!((level.peak_db - -6.02).abs() < 0.01, "{level:?}");
        assert!((level.rms_db - -6.02).abs() < 0.01, "{level:?}");
        // A square wave at full scale for half the window: peak 0 dB, RMS
        // -3 dB.
        let mut half = vec![1.0f32; 480];
        half.extend([0.0; 480]);
        let level = meter.push(&half).expect("a window");
        assert!(level.peak_db.abs() < 0.01);
        assert!((level.rms_db - -3.01).abs() < 0.01);
        // Silence, and a sample that isn't a number, read as silence.
        let mut quiet = vec![0.0f32; 959];
        quiet.push(f32::NAN);
        assert_eq!(meter.push(&quiet), Some(Level::SILENT));
    }
}
