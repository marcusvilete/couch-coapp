use std::time::Duration;

use crate::{
    AudioChunk, CaptureEvent, DecodedFrame, EncodedFrame, EncoderCaps, EncoderInput, EpochId,
    FeedbackSnapshot, MediaError, Micros, ReceivedFrame, VideoParams,
};

/// A source of frames: a window, a display, or a test pattern.
pub trait Capturer: Send {
    /// Waits up to `timeout` for the next event. `Ok(None)` means the timeout passed.
    fn next_event(&mut self, timeout: Duration) -> Result<Option<CaptureEvent>, MediaError>;

    /// From now on, deliver frames in memory: no encoder reads them on the
    /// GPU, so keeping them there only adds a copy back (M9b). A capturer
    /// whose frames are in memory already ignores it.
    fn frames_in_memory(&mut self) {}
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
    /// Starts a new epoch. Its first frame must be a keyframe. Any unfinished
    /// epoch is abandoned (its stream is reset).
    fn start_epoch(&mut self, params: &VideoParams) -> Result<EpochId, MediaError>;

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
