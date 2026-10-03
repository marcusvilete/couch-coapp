//! Opus for the audio channel (docs/protocol.md): 48 kHz stereo, 10 ms
//! frames, 96 kbit/s constant bitrate, low-delay mode.
//!
//! libopus comes from the `opus` crate, which builds it from source with
//! CMake (BSD-3-Clause). FFmpeg's libopus wrapper can't be used for decoding:
//! it has no way to ask for loss concealment, which is how the viewer fills a
//! lost packet.

use couch_media::MediaError;
use couch_proto::media::audio::{CHANNELS, FRAME_SAMPLES, MAX_PACKET, SAMPLE_RATE};

/// The encoder's bitrate.
pub const BITRATE_BPS: i32 = 96_000;

/// Interleaved samples in one frame (all channels).
pub const FRAME_LEN: usize = FRAME_SAMPLES * CHANNELS as usize;

fn backend(e: opus::Error) -> MediaError {
    MediaError::Backend(format!("opus: {e}"))
}

pub struct OpusEncoder {
    inner: opus::Encoder,
    packet: Vec<u8>,
}

impl OpusEncoder {
    pub fn new() -> Result<Self, MediaError> {
        // Low-delay mode is CELT only: 2.5 ms of look-ahead instead of 6.5 ms.
        let mut inner = opus::Encoder::new(
            SAMPLE_RATE,
            opus::Channels::Stereo,
            opus::Application::LowDelay,
        )
        .map_err(backend)?;
        inner
            .set_bitrate(opus::Bitrate::Bits(BITRATE_BPS))
            .map_err(backend)?;
        inner.set_vbr(false).map_err(backend)?;
        inner.set_signal(opus::Signal::Music).map_err(backend)?;
        Ok(Self {
            inner,
            packet: vec![0; MAX_PACKET],
        })
    }

    /// Encodes one frame of [`FRAME_LEN`] interleaved samples.
    pub fn encode(&mut self, frame: &[f32]) -> Result<&[u8], MediaError> {
        if frame.len() != FRAME_LEN {
            return Err(MediaError::Backend(format!(
                "an Opus frame is {FRAME_LEN} samples, got {}",
                frame.len()
            )));
        }
        let len = self
            .inner
            .encode_float(frame, &mut self.packet)
            .map_err(backend)?;
        Ok(&self.packet[..len])
    }
}

pub struct OpusDecoder {
    inner: opus::Decoder,
    pcm: Vec<f32>,
}

impl OpusDecoder {
    pub fn new() -> Result<Self, MediaError> {
        Ok(Self {
            inner: opus::Decoder::new(SAMPLE_RATE, opus::Channels::Stereo).map_err(backend)?,
            pcm: vec![0.0; FRAME_LEN],
        })
    }

    /// Decodes one packet into [`FRAME_LEN`] interleaved samples. A packet
    /// that isn't exactly one 10 ms frame is an error, so the caller's
    /// timing (one packet, one frame) always holds.
    pub fn decode(&mut self, packet: &[u8]) -> Result<&[f32], MediaError> {
        if packet.is_empty() {
            return Err(MediaError::Backend("empty Opus packet".into()));
        }
        let samples = self
            .inner
            .decode_float(packet, &mut self.pcm, false)
            .map_err(backend)?;
        if samples != FRAME_SAMPLES {
            return Err(MediaError::Backend(format!(
                "Opus packet of {samples} samples, expected {FRAME_SAMPLES}"
            )));
        }
        Ok(&self.pcm)
    }

    /// One frame that stands in for a lost packet: Opus extrapolates from
    /// what it decoded last and fades to silence over consecutive losses.
    pub fn conceal(&mut self) -> Result<&[f32], MediaError> {
        let samples = self
            .inner
            .decode_float(&[], &mut self.pcm, false)
            .map_err(backend)?;
        if samples != FRAME_SAMPLES {
            self.pcm.fill(0.0);
        }
        Ok(&self.pcm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frame: usize) -> Vec<f32> {
        (0..FRAME_LEN)
            .map(|i| {
                let t = (frame * FRAME_SAMPLES + i / 2) as f32 / SAMPLE_RATE as f32;
                (t * 440.0 * std::f32::consts::TAU).sin() * 0.5
            })
            .collect()
    }

    /// `couch-media` can't depend on `couch-proto`, so it repeats the
    /// stream format; the two must not drift apart.
    #[test]
    fn media_and_wire_agree_on_the_format() {
        use couch_media::audio as media;
        use couch_proto::media::audio as wire;
        assert_eq!(media::SAMPLE_RATE, wire::SAMPLE_RATE);
        assert_eq!(media::CHANNELS, wire::CHANNELS);
        assert_eq!(media::FRAME_SAMPLES, wire::FRAME_SAMPLES);
        assert_eq!(media::FRAME_US, wire::FRAME_US);
        assert_eq!(media::FRAME_LEN, FRAME_LEN);
    }

    #[test]
    fn constant_bitrate_packets_round_trip() {
        let mut encoder = OpusEncoder::new().unwrap();
        let mut decoder = OpusDecoder::new().unwrap();
        let mut energy = 0.0f32;
        for frame in 0..50 {
            let packet = encoder.encode(&tone(frame)).unwrap().to_vec();
            // 96 kbit/s over 10 ms.
            assert_eq!(packet.len(), 120);
            let pcm = decoder.decode(&packet).unwrap();
            assert_eq!(pcm.len(), FRAME_LEN);
            if frame >= 10 {
                energy += pcm.iter().map(|s| s * s).sum::<f32>();
            }
        }
        // A 0.5-amplitude sine has mean square 0.125.
        let mean_square = energy / (40 * FRAME_LEN) as f32;
        assert!((0.1..0.15).contains(&mean_square), "{mean_square}");
    }

    #[test]
    fn concealment_continues_the_signal_then_fades() {
        let mut encoder = OpusEncoder::new().unwrap();
        let mut decoder = OpusDecoder::new().unwrap();
        for frame in 0..20 {
            let packet = encoder.encode(&tone(frame)).unwrap().to_vec();
            decoder.decode(&packet).unwrap();
        }
        let rms = |pcm: &[f32]| (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32).sqrt();
        let first = rms(decoder.conceal().unwrap());
        assert!(
            first > 0.1,
            "the first concealed frame keeps the tone: {first}"
        );
        let mut last = first;
        for _ in 0..20 {
            last = rms(decoder.conceal().unwrap());
        }
        assert!(
            last < first / 4.0,
            "long losses fade out: {first} -> {last}"
        );
    }

    #[test]
    fn rejects_bad_input() {
        let mut encoder = OpusEncoder::new().unwrap();
        assert!(encoder.encode(&[0.0; 100]).is_err());
        let mut decoder = OpusDecoder::new().unwrap();
        assert!(decoder.decode(&[]).is_err());
        // A 20 ms packet: longer than a frame.
        let mut long =
            opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::LowDelay)
                .unwrap();
        let packet = long.encode_vec_float(&[0.0; 1920], 1275).unwrap();
        assert!(decoder.decode(&packet).is_err());
    }
}
