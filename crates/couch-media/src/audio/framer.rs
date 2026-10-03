//! Cuts captured audio, which arrives in chunks of any size, into the
//! codec's 10 ms frames, each stamped with its first sample's capture time.

use super::{CHANNELS, FRAME_LEN, SAMPLE_RATE, duration_us};
use crate::{AudioChunk, MediaError, Micros};

/// A capture time this far from where the buffered samples end means the
/// source stalled or skipped.
const GAP_US: u64 = 20_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub captured_at: Micros,
    /// [`FRAME_LEN`] interleaved samples.
    pub samples: Vec<f32>,
}

#[derive(Debug, Default)]
pub struct Framer {
    buf: Vec<f32>,
    /// Capture time of `buf[0]`.
    start: Micros,
}

impl Framer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a chunk and appends every completed frame to `out`. Chunks must
    /// already be in the stream format.
    pub fn push(&mut self, chunk: &AudioChunk, out: &mut Vec<Frame>) -> Result<(), MediaError> {
        if chunk.sample_rate != SAMPLE_RATE || chunk.channels != CHANNELS {
            return Err(MediaError::Unsupported(format!(
                "{} Hz, {} channels (need {SAMPLE_RATE} Hz stereo)",
                chunk.sample_rate, chunk.channels
            )));
        }
        if chunk.samples.is_empty() {
            return Ok(());
        }
        if self.buf.is_empty() {
            self.start = chunk.captured_at;
        } else {
            let expected = self.start.0 + duration_us(self.buf.len());
            if chunk.captured_at.0.abs_diff(expected) > GAP_US {
                // Finish the old frame with silence rather than glue audio
                // from before the gap to audio after it.
                self.buf.resize(FRAME_LEN, 0.0);
                self.emit(out);
                self.start = chunk.captured_at;
            }
        }
        let mut rest = chunk.samples.as_slice();
        while !rest.is_empty() {
            let take = (FRAME_LEN - self.buf.len()).min(rest.len());
            self.buf.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.buf.len() == FRAME_LEN {
                self.emit(out);
            }
        }
        Ok(())
    }

    fn emit(&mut self, out: &mut Vec<Frame>) {
        out.push(Frame {
            captured_at: self.start,
            samples: std::mem::replace(&mut self.buf, Vec::with_capacity(FRAME_LEN)),
        });
        self.start = Micros(self.start.0 + duration_us(FRAME_LEN));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(at_us: u64, frames: usize, value: f32) -> AudioChunk {
        AudioChunk {
            samples: vec![value; frames * 2],
            channels: 2,
            sample_rate: 48_000,
            captured_at: Micros(at_us),
        }
    }

    #[test]
    fn regroups_chunks_into_stamped_frames() {
        let mut framer = Framer::new();
        let mut out = Vec::new();
        // 7 ms, 7 ms, 7 ms of contiguous audio: two frames and 1 ms left over.
        for i in 0..3 {
            framer
                .push(&chunk(1_000_000 + i * 7_000, 336, i as f32), &mut out)
                .unwrap();
        }
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].captured_at, Micros(1_000_000));
        assert_eq!(out[1].captured_at, Micros(1_010_000));
        assert!(out.iter().all(|f| f.samples.len() == FRAME_LEN));
        // The second frame starts with the rest of chunk 1 (value 1.0).
        assert_eq!(out[1].samples[0], 1.0);
        assert_eq!(*out[1].samples.last().unwrap(), 2.0);
    }

    #[test]
    fn a_gap_flushes_the_partial_frame_with_silence() {
        let mut framer = Framer::new();
        let mut out = Vec::new();
        framer.push(&chunk(0, 240, 0.5), &mut out).unwrap();
        assert!(out.is_empty());
        // The source was silent for a second.
        framer.push(&chunk(1_000_000, 480, 0.25), &mut out).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].captured_at, Micros(0));
        assert_eq!(out[0].samples[479], 0.5);
        assert_eq!(out[0].samples[480], 0.0);
        assert_eq!(out[1].captured_at, Micros(1_000_000));
        assert_eq!(out[1].samples[0], 0.25);
    }

    #[test]
    fn rejects_other_formats() {
        let mut framer = Framer::new();
        let mono = AudioChunk {
            channels: 1,
            ..chunk(0, 480, 0.0)
        };
        assert!(framer.push(&mono, &mut Vec::new()).is_err());
    }
}
