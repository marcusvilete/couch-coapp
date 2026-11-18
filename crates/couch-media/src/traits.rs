use std::time::Duration;

use couch_proto::media::FrameRef;

use crate::{
    AudioChunk, CaptureEvent, DecodedFrame, EncodedFrame, EncoderCaps, EncoderInput, EpochId,
    FeedbackSnapshot, MediaError, Micros, ReceivedFrame, VideoParams,
};

/// What an encoder that continues (M12d-2) says of one frame it encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LongTerm {
    /// The encoder's own name for the frame, which
    /// [`VideoEncoder::invalidate`] takes.
    pub timestamp: u64,
    /// The long-term slot it was marked into, as the encoder reports.
    pub marked: Option<u32>,
}

/// A source of frames: a window, a display, or a test pattern.
pub trait Capturer: Send {
    /// Waits up to `timeout` for the next event. `Ok(None)` means the timeout passed.
    fn next_event(&mut self, timeout: Duration) -> Result<Option<CaptureEvent>, MediaError>;
}

pub trait VideoEncoder: Send {
    fn caps(&self) -> &EncoderCaps;

    /// Encodes one frame. Returns zero or more access units; low-latency
    /// encoders return exactly one per input.
    fn encode(
        &mut self,
        input: EncoderInput<'_>,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedFrame>, MediaError>;

    /// Changes the target bitrate without restarting the encoder.
    fn set_bitrate(&mut self, bps: u32) -> Result<(), MediaError>;

    /// Whether it can continue a stream after frames a viewer never got
    /// (M12d-2, ADR 0016): it marks long-term frames and invalidates. Only
    /// NVENC directly, opened for it; false again after a fallback.
    fn continues(&self) -> bool {
        false
    }

    /// [`encode`](Self::encode), marking the frame long-term in slot `mark`
    /// where asked, with what the encoder says of each frame (`None` from an
    /// encoder that doesn't continue).
    fn encode_marking(
        &mut self,
        input: EncoderInput<'_>,
        force_keyframe: bool,
        mark: Option<u32>,
    ) -> Result<Vec<(EncodedFrame, Option<LongTerm>)>, MediaError> {
        let _ = mark;
        Ok(self
            .encode(input, force_keyframe)?
            .into_iter()
            .map(|f| (f, None))
            .collect())
    }

    /// No later frame refers to the frame encoded under `timestamp`, nor to
    /// any frame that did.
    fn invalidate(&mut self, timestamp: u64) -> Result<(), MediaError> {
        let _ = timestamp;
        Err(MediaError::Unsupported("invalidating frames".into()))
    }
}

pub trait VideoDecoder: Send {
    /// Decodes one access unit. `captured_at` is passed through to the output
    /// so latency can be measured after decoding.
    fn decode(
        &mut self,
        access_unit: &[u8],
        captured_at: Micros,
    ) -> Result<Vec<DecodedFrame>, MediaError>;
}

/// The host side of a media transport, one per viewer.
pub trait VideoSender: Send {
    /// Starts a new epoch. Its first frame must be a keyframe, or, with
    /// `continues`, a frame referring to that frame or earlier ones the viewer
    /// holds, and to none sent after it (M12d-2). Any unfinished epoch is
    /// abandoned (its stream is reset).
    fn start_epoch(
        &mut self,
        params: &VideoParams,
        continues: Option<FrameRef>,
    ) -> Result<EpochId, MediaError>;

    /// The frame the next [`send_frame`](Self::send_frame) sends, `None`
    /// without an epoch.
    fn next_frame(&self) -> Option<FrameRef>;

    /// The newest frame the viewer is known to have received whole, from its
    /// reports: of the current epoch, or of the one it continues from.
    fn received(&self) -> Option<FrameRef>;

    /// Queues a frame on the current epoch. Never blocks on the network.
    fn send_frame(&mut self, frame: &EncodedFrame) -> Result<(), MediaError>;

    /// Latest view of the path, for the rate controller.
    fn feedback(&self) -> FeedbackSnapshot;
}

/// The viewer side of a media transport.
pub trait VideoReceiver: Send {
    /// Waits up to `timeout` for the next complete frame, in decode order.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<ReceivedFrame>, MediaError>;
}

pub trait AudioSource: Send {
    /// Waits up to `timeout` for the next chunk of captured audio.
    fn next_chunk(&mut self, timeout: Duration) -> Result<Option<AudioChunk>, MediaError>;
}

pub trait AudioSink: Send {
    fn play(&mut self, chunk: &AudioChunk) -> Result<(), MediaError>;
}
