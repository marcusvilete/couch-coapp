//! Audio playback. Packets from the IPC pipe go into a jitter buffer; SDL's
//! audio thread pulls 10 ms frames out of it, decodes them (or conceals a
//! missing one) and plays them through an SDL audio stream. The stream's
//! speed follows the buffer's playback ratio, which absorbs the drift
//! between the host's audio clock and ours.
//!
//! SDL hands the samples to Windows, which on a typical device adds a
//! further 25 to 40 ms before they are mixed: its 10 ms periods plus its own
//! processing. A WASAPI renderer of our own, keeping exactly one period
//! queued, measured no better (docs/m3-results.md).

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
};

use anyhow::{Result, anyhow};
use couch_codec_opus::OpusDecoder;
use couch_media::audio::{
    CHANNELS, FRAME_LEN, JitterBuffer, JitterConfig, Level, LevelMeter, Pop, SAMPLE_RATE, ramp_gain,
};
use couch_proto::ipc::{AudioPacket, AudioStats, StreamVolume};
use sdl3::audio::{AudioCallback, AudioFormat, AudioSpec, AudioStream, AudioStreamWithCallback};

use crate::now_us;

struct Entry {
    packet: Vec<u8>,
    capture_us: u64,
}

/// What the pipe reader and the audio thread share.
struct Shared {
    jitter: JitterBuffer<Entry>,
    /// Capture to handing samples to SDL, for recent packets (µs).
    recent: VecDeque<u64>,
    /// The same, for the whole session, in 1 ms buckets (the last one
    /// collects everything longer).
    histogram: Vec<u32>,
}

/// Recent packets kept for the HUD's median: one second.
const RECENT: usize = 100;
const HISTOGRAM_MS: usize = 500;

impl Shared {
    fn record(&mut self, capture_us: u64) {
        let latency = now_us().0.saturating_sub(capture_us);
        if self.recent.len() == RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back(latency);
        let bucket = ((latency / 1000) as usize).min(HISTOGRAM_MS);
        self.histogram[bucket] += 1;
    }

    /// The `p`th percentile of the session, microseconds (to the bucket's top).
    fn session_percentile(&self, p: f64) -> u64 {
        let total: u64 = self.histogram.iter().map(|&n| u64::from(n)).sum();
        if total == 0 {
            return 0;
        }
        let rank = ((p / 100.0) * total as f64).ceil().max(1.0) as u64;
        let mut seen = 0u64;
        for (ms, &n) in self.histogram.iter().enumerate() {
            seen += u64::from(n);
            if seen >= rank {
                return (ms as u64 + 1) * 1000;
            }
        }
        (HISTOGRAM_MS as u64 + 1) * 1000
    }
}

/// The pipe reader's end: adds arriving packets, and sets the volume the
/// main process sends before them, so the first sound already plays at the
/// profile's level.
#[derive(Clone)]
pub struct AudioFeed {
    shared: Arc<Mutex<Shared>>,
    gain: Arc<AtomicU32>,
}

impl AudioFeed {
    pub fn push(&self, packet: AudioPacket) {
        let entry = Entry {
            packet: packet.packet,
            capture_us: packet.capture_us,
        };
        lock(&self.shared)
            .jitter
            .push(packet.seq, entry, packet.capture_us, now_us().0);
    }

    /// How loud to play from now on (P6); the audio thread ramps to it over
    /// its next buffer.
    pub fn set_volume(&self, volume: StreamVolume) {
        self.gain.store(volume.gain().to_bits(), Ordering::Relaxed);
    }
}

/// Locks the shared state even if another thread panicked holding it. A
/// panic inside SDL's audio callback would abort the viewer, and stale
/// audio state is harmless.
fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct AudioOut {
    stream: AudioStreamWithCallback<Player>,
    shared: Arc<Mutex<Shared>>,
    /// The stream volume's gain (P6), an `f32`'s bits: the audio thread reads
    /// it without a lock.
    gain: Arc<AtomicU32>,
    /// The level of the sound as it arrives ([`Self::heard`]).
    heard: Arc<AtomicU64>,
}

/// The stats' level line covers this much sound.
const LEVEL_WINDOW_MS: u32 = 250;

impl AudioOut {
    /// Opens the default playback device. SDL moves playback to the new
    /// default device when it changes.
    pub fn open(sdl: &sdl3::Sdl) -> Result<(Self, AudioFeed)> {
        let audio = sdl.audio().map_err(|e| anyhow!("SDL audio: {e}"))?;
        let shared = Arc::new(Mutex::new(Shared {
            jitter: JitterBuffer::new(JitterConfig::default()),
            recent: VecDeque::with_capacity(RECENT),
            histogram: vec![0; HISTOGRAM_MS + 1],
        }));
        let gain = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let heard = Arc::new(AtomicU64::new(pack(Level::SILENT)));
        let player = Player {
            shared: shared.clone(),
            decoder: OpusDecoder::new()?,
            pcm: VecDeque::with_capacity(FRAME_LEN * 4),
            out: Vec::new(),
            ratio: 1.0,
            gain: gain.clone(),
            level: 1.0,
            meter: LevelMeter::new(LEVEL_WINDOW_MS),
            heard: heard.clone(),
        };
        let spec = AudioSpec::new(
            Some(SAMPLE_RATE as i32),
            Some(i32::from(CHANNELS)),
            Some(AudioFormat::f32_sys()),
        );
        let stream = audio
            .open_playback_stream(&spec, player)
            .map_err(|e| anyhow!("opening audio playback: {e}"))?;
        stream
            .resume()
            .map_err(|e| anyhow!("starting audio playback: {e}"))?;
        let feed = AudioFeed {
            shared: shared.clone(),
            gain: gain.clone(),
        };
        Ok((
            Self {
                stream,
                shared,
                gain,
                heard,
            },
            feed,
        ))
    }

    /// The sound's level as it arrives, before this window's volume (P7).
    pub fn heard(&self) -> Level {
        unpack(self.heard.load(Ordering::Relaxed))
    }

    /// How loud to play from now on (P6); the audio thread ramps to it over
    /// its next buffer.
    pub fn set_volume(&self, volume: StreamVolume) {
        self.gain.store(volume.gain().to_bits(), Ordering::Relaxed);
    }

    pub fn stats(&self) -> AudioStats {
        let shared = lock(&self.shared);
        let counts = shared.jitter.stats();
        let mut recent: Vec<u64> = shared.recent.iter().copied().collect();
        recent.sort_unstable();
        AudioStats {
            played: counts.played,
            concealed: counts.concealed,
            late: counts.late,
            dropped: counts.dropped,
            underruns: counts.underruns,
            buffered_us: shared.jitter.buffered_us() as u32,
            target_us: shared.jitter.target_us() as u32,
            capture_to_queue_us: recent.get(recent.len() / 2).copied().unwrap_or(0) as u32,
            capture_to_queue_p95_us: shared.session_percentile(95.0) as u32,
        }
    }

    /// The stats card's Audio row (built only while the card shows: it takes
    /// the audio thread's lock).
    pub fn hud_line(&self) -> String {
        let s = self.stats();
        let total = (s.played + s.concealed).max(1);
        let heard = self.heard();
        format!(
            "{:.0}/{:.0} ms · concealed {:.1}% · underruns {} · capture → device {:.0} ms · level peak {:.0} / avg {:.0} dBFS",
            f64::from(s.buffered_us) / 1000.0,
            f64::from(s.target_us) / 1000.0,
            s.concealed as f64 * 100.0 / total as f64,
            s.underruns,
            f64::from(s.capture_to_queue_us) / 1000.0,
            heard.peak_db,
            heard.rms_db,
        )
    }
}

impl Drop for AudioOut {
    /// The SDL wrapper frees the callback before destroying the stream, so
    /// stop callbacks first, on every path out (errors and panics too).
    fn drop(&mut self) {
        // SDL checks the pause flag before every callback.
        let _ = self.stream.pause();
        // Waits for a callback in progress to finish.
        drop(self.stream.lock());
    }
}

/// Runs on SDL's audio thread.
struct Player {
    shared: Arc<Mutex<Shared>>,
    decoder: OpusDecoder,
    /// Decoded samples not yet handed to SDL (less than one frame).
    pcm: VecDeque<f32>,
    out: Vec<f32>,
    ratio: f64,
    /// The gain asked for ([`AudioOut::set_volume`]) and the one applied to
    /// the last sample.
    gain: Arc<AtomicU32>,
    level: f32,
    /// The sound as it arrives, before this window's volume (P7): what the
    /// host sends, for the stats.
    meter: LevelMeter,
    heard: Arc<AtomicU64>,
}

/// A [`Level`] in one atomic: the peak's bits high, the RMS's low.
fn pack(level: Level) -> u64 {
    (u64::from(level.peak_db.to_bits()) << 32) | u64::from(level.rms_db.to_bits())
}

fn unpack(bits: u64) -> Level {
    Level {
        peak_db: f32::from_bits((bits >> 32) as u32),
        rms_db: f32::from_bits(bits as u32),
    }
}

impl Player {
    fn next_frame(&mut self) {
        let pop = lock(&self.shared).jitter.pop();
        match pop {
            Pop::Packet(entry) => {
                lock(&self.shared).record(entry.capture_us);
                match self.decoder.decode(&entry.packet) {
                    Ok(pcm) => self.pcm.extend(pcm),
                    Err(e) => {
                        tracing::debug!("audio packet not decoded: {e}");
                        self.conceal();
                    }
                }
            }
            Pop::Conceal => self.conceal(),
            Pop::Silence => self.pcm.extend([0.0; FRAME_LEN]),
        }
    }

    fn conceal(&mut self) {
        match self.decoder.conceal() {
            Ok(pcm) => self.pcm.extend(pcm),
            Err(_) => self.pcm.extend([0.0; FRAME_LEN]),
        }
    }
}

impl AudioCallback<f32> for Player {
    fn callback(&mut self, stream: &mut AudioStream, requested: i32) {
        let want =
            usize::try_from(requested).unwrap_or(0) / usize::from(CHANNELS) * usize::from(CHANNELS);
        if want == 0 {
            return;
        }
        while self.pcm.len() < want {
            self.next_frame();
        }
        self.out.clear();
        self.out.extend(self.pcm.drain(..want));
        if let Some(level) = self.meter.push(&self.out) {
            self.heard.store(pack(level), Ordering::Relaxed);
        }
        let target = f32::from_bits(self.gain.load(Ordering::Relaxed));
        ramp_gain(&mut self.out, self.level, target);
        self.level = target;
        if let Err(e) = stream.put_data_f32(&self.out) {
            tracing::debug!("audio data not queued: {e}");
        }

        let ratio = lock(&self.shared).jitter.playback_ratio();
        if ratio != self.ratio {
            self.ratio = ratio;
            // SAFETY: the stream is valid for the callback's duration; SDL's
            // stream lock is recursive, so this is allowed from inside it.
            unsafe {
                sdl3::sys::audio::SDL_SetAudioStreamFrequencyRatio(stream.raw(), ratio as f32)
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P7: the level passes from the audio thread in one atomic, intact.
    #[test]
    fn a_level_packs_into_one_atomic() {
        let level = Level {
            peak_db: -6.5,
            rms_db: -21.25,
        };
        assert_eq!(unpack(pack(level)), level);
        assert_eq!(unpack(pack(Level::SILENT)), Level::SILENT);
    }
}
