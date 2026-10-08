//! Fakes for tests. Only ever a dev-dependency.

pub mod relay;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use couch_input::{
    HidUsage, InputError, KbmInjector, MouseButton, PadState, Rumble, VirtualPad, VirtualPadFactory,
};
use couch_media::{
    BgraFrame, CaptureEvent, Capturer, EncodedFrame, EncoderCaps, EncoderInput, MediaError,
    VideoEncoder, clock::Clock,
};

/// Plain gray frames at a steady rate, until told the source closed.
pub struct FakeCapturer {
    width: u32,
    height: u32,
    interval: Duration,
    next_due: Instant,
    clock: Arc<dyn Clock>,
    closed: Arc<AtomicBool>,
    reported: bool,
}

impl FakeCapturer {
    /// Setting `closed` makes it report [`CaptureEvent::SourceClosed`], as a
    /// closing window would.
    pub fn new(
        width: u32,
        height: u32,
        fps: u32,
        clock: Arc<dyn Clock>,
        closed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            width,
            height,
            interval: Duration::from_secs(1) / fps.max(1),
            next_due: Instant::now(),
            clock,
            closed,
            reported: false,
        }
    }
}

impl Capturer for FakeCapturer {
    fn next_event(&mut self, timeout: Duration) -> Result<Option<CaptureEvent>, MediaError> {
        if self.closed.load(Ordering::SeqCst) {
            if self.reported {
                return Err(MediaError::Closed);
            }
            self.reported = true;
            return Ok(Some(CaptureEvent::SourceClosed));
        }
        let now = Instant::now();
        if now < self.next_due {
            let wait = self.next_due - now;
            if wait > timeout {
                std::thread::sleep(timeout);
                return Ok(None);
            }
            std::thread::sleep(wait);
        }
        self.next_due = Instant::now() + self.interval;
        let stride = self.width * 4;
        Ok(Some(CaptureEvent::Frame(BgraFrame {
            width: self.width,
            height: self.height,
            stride,
            data: Bytes::from(vec![128; (stride * self.height) as usize]),
            captured_at: self.clock.now(),
        })))
    }
}

/// An encoder that emits a small access unit per frame: a keyframe when
/// asked for one (or first), filled with 1s; otherwise 0s.
pub struct FakeEncoder {
    caps: EncoderCaps,
    started: bool,
}

impl Default for FakeEncoder {
    fn default() -> Self {
        Self {
            caps: EncoderCaps {
                name: "fake".into(),
                hardware: false,
                bitrate_change_costs_keyframe: false,
            },
            started: false,
        }
    }
}

impl VideoEncoder for FakeEncoder {
    fn caps(&self) -> &EncoderCaps {
        &self.caps
    }

    fn encode(
        &mut self,
        input: EncoderInput<'_>,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedFrame>, MediaError> {
        let EncoderInput::Cpu(frame) = input else {
            return Err(MediaError::Unsupported("only CPU frames".into()));
        };
        let keyframe = force_keyframe || !self.started;
        self.started = true;
        Ok(vec![EncodedFrame {
            data: Bytes::from(vec![u8::from(keyframe); 200]),
            keyframe,
            captured_at: frame.captured_at,
            encode_time_us: 100,
        }])
    }

    fn set_bitrate(&mut self, _bps: u32) -> Result<(), MediaError> {
        Ok(())
    }
}

/// One call made on a [`RecordingInjector`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbmCall {
    Key(HidUsage, bool),
    MouseMove(i32, i32),
    MouseButton(MouseButton, bool),
    Wheel(i32, i32),
}

/// Records injected keyboard and mouse input instead of sending it to the OS.
#[derive(Debug, Clone, Default)]
pub struct RecordingInjector {
    pub calls: Arc<Mutex<Vec<KbmCall>>>,
}

impl RecordingInjector {
    pub fn calls(&self) -> Vec<KbmCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl KbmInjector for RecordingInjector {
    fn key(&mut self, key: HidUsage, down: bool) -> Result<(), InputError> {
        self.calls.lock().unwrap().push(KbmCall::Key(key, down));
        Ok(())
    }

    fn mouse_move(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        self.calls.lock().unwrap().push(KbmCall::MouseMove(dx, dy));
        Ok(())
    }

    fn mouse_button(&mut self, button: MouseButton, down: bool) -> Result<(), InputError> {
        self.calls
            .lock()
            .unwrap()
            .push(KbmCall::MouseButton(button, down));
        Ok(())
    }

    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        self.calls.lock().unwrap().push(KbmCall::Wheel(dx, dy));
        Ok(())
    }
}

/// Records submitted pad states and replays queued rumble.
#[derive(Debug, Clone, Default)]
pub struct RecordingPad {
    pub states: Arc<Mutex<Vec<PadState>>>,
    pub rumble: Arc<Mutex<Option<Rumble>>>,
}

impl RecordingPad {
    pub fn states(&self) -> Vec<PadState> {
        self.states.lock().unwrap().clone()
    }

    /// Makes the next [`VirtualPad::take_rumble`] return `rumble`, as if a game sent it.
    pub fn queue_rumble(&self, rumble: Rumble) {
        *self.rumble.lock().unwrap() = Some(rumble);
    }
}

impl VirtualPad for RecordingPad {
    fn submit(&mut self, state: &PadState) -> Result<(), InputError> {
        self.states.lock().unwrap().push(*state);
        Ok(())
    }

    fn take_rumble(&mut self) -> Option<Rumble> {
        self.rumble.lock().unwrap().take()
    }
}

/// Plugs [`RecordingPad`]s and keeps a handle to each for assertions.
#[derive(Debug, Clone, Default)]
pub struct RecordingPadFactory {
    pub plugged: Arc<Mutex<Vec<RecordingPad>>>,
}

impl VirtualPadFactory for RecordingPadFactory {
    fn plug(&self) -> Result<Box<dyn VirtualPad>, InputError> {
        let pad = RecordingPad::default();
        self.plugged.lock().unwrap().push(pad.clone());
        Ok(Box::new(pad))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injector_records_in_order() {
        let mut injector = RecordingInjector::default();
        injector.key(HidUsage(0x04), true).unwrap();
        injector.mouse_move(3, -2).unwrap();
        assert_eq!(
            injector.calls(),
            vec![
                KbmCall::Key(HidUsage(0x04), true),
                KbmCall::MouseMove(3, -2)
            ]
        );
    }

    #[test]
    fn factory_keeps_handles_to_plugged_pads() {
        let factory = RecordingPadFactory::default();
        let mut pad = factory.plug().unwrap();
        pad.submit(&PadState::NEUTRAL).unwrap();
        let plugged = factory.plugged.lock().unwrap();
        assert_eq!(plugged[0].states(), vec![PadState::NEUTRAL]);
    }
}
