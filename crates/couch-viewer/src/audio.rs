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
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use anyhow::{Result, anyhow};
use couch_codec_opus::OpusDecoder;
use couch_media::audio::{CHANNELS, FRAME_LEN, JitterBuffer, JitterConfig, Pop, SAMPLE_RATE};
use couch_proto::ipc::{AudioPacket, AudioStats};
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

/// The pipe reader's end: adds arriving packets.
#[derive(Clone)]
pub struct AudioFeed(Arc<Mutex<Shared>>);

impl AudioFeed {
    pub fn push(&self, packet: AudioPacket) {
        let entry = Entry {
            packet: packet.packet,
            capture_us: packet.capture_us,
        };
        lock(&self.0)
            .jitter
            .push(packet.seq, entry, packet.capture_us, now_us().0);
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
}

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
        let player = Player {
            shared: shared.clone(),
            decoder: OpusDecoder::new()?,
            pcm: VecDeque::with_capacity(FRAME_LEN * 4),
            out: Vec::new(),
            ratio: 1.0,
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
        let feed = AudioFeed(shared.clone());
        Ok((Self { stream, shared }, feed))
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

    /// One line for the HUD.
    pub fn hud_line(&self) -> String {
        let s = self.stats();
        let total = (s.played + s.concealed).max(1);
        format!(
            "audio {:.0}/{:.0} ms  concealed {:.1}%  underruns {}  capture -> device {:.0} ms",
            f64::from(s.buffered_us) / 1000.0,
            f64::from(s.target_us) / 1000.0,
            s.concealed as f64 * 100.0 / total as f64,
            s.underruns,
            f64::from(s.capture_to_queue_us) / 1000.0
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
