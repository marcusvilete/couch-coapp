//! Fakes for tests. Only ever a dev-dependency.

pub mod net;
pub mod relay;

use std::{
    ffi::c_void,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use couch_input::{
    Bounds, HidUsage, InputError, KbmInjector, MouseButton, PadState, Rumble, VirtualPad,
    VirtualPadFactory,
};
use couch_media::{
    BgraFrame, CaptureEvent, Capturer, EncodedFrame, EncoderCaps, EncoderInput, GpuFrame,
    GpuTexture, MediaError, VideoEncoder, VideoFrame, clock::Clock,
};

/// What a [`FakeCapturer`] making GPU frames shares with the test.
#[derive(Debug, Clone, Default)]
pub struct FakeGpu {
    /// The texture pool frames come from; changing it is a new capture device.
    pub source: Arc<AtomicU64>,
    /// How many frames were copied to memory, by any encoder or by the
    /// capture after pacing (M12a).
    pub downloads: Arc<AtomicU32>,
    /// Copies to memory fail while set.
    pub download_fails: Arc<AtomicBool>,
    /// Each copy to memory takes this long, in milliseconds: a GPU busy
    /// with a game (M12a).
    pub download_ms: Arc<AtomicU32>,
}

/// A "GPU" texture that is really memory, counting its downloads.
struct FakeGpuTexture {
    pixels: Bytes,
    stride: u32,
    downloads: Arc<AtomicU32>,
    fails: Arc<AtomicBool>,
    delay_ms: Arc<AtomicU32>,
}

impl GpuTexture for FakeGpuTexture {
    fn d3d11_texture(&self) -> *mut c_void {
        std::ptr::null_mut()
    }

    fn d3d11_device_owned(&self) -> *mut c_void {
        std::ptr::null_mut()
    }

    fn download(&self) -> Result<(u32, Bytes), MediaError> {
        if self.fails.load(Ordering::SeqCst) {
            return Err(MediaError::Backend("the fake GPU won't copy".into()));
        }
        let delay = self.delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(u64::from(delay)));
        }
        self.downloads.fetch_add(1, Ordering::SeqCst);
        Ok((self.stride, self.pixels.clone()))
    }

    fn keep_alive(&self) -> Arc<dyn std::any::Any + Send + Sync> {
        Arc::new(())
    }
}

/// Plain gray frames at a steady rate, until told the source closed.
pub struct FakeCapturer {
    width: u32,
    height: u32,
    interval: Duration,
    next_due: Instant,
    clock: Arc<dyn Clock>,
    closed: Arc<AtomicBool>,
    reported: bool,
    gpu: Option<FakeGpu>,
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
            gpu: None,
        }
    }

    /// Frames "on the GPU", from the pool `gpu.source` names at each frame.
    pub fn on_gpu(mut self, gpu: FakeGpu) -> Self {
        self.gpu = Some(gpu);
        self
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
        let pixels = Bytes::from(vec![128; (stride * self.height) as usize]);
        let captured_at = self.clock.now();
        let frame = match &self.gpu {
            None => VideoFrame::Cpu(BgraFrame {
                width: self.width,
                height: self.height,
                stride,
                data: pixels,
                captured_at,
            }),
            Some(gpu) => VideoFrame::Gpu(GpuFrame::new(
                Arc::new(FakeGpuTexture {
                    pixels,
                    stride,
                    downloads: gpu.downloads.clone(),
                    fails: gpu.download_fails.clone(),
                    delay_ms: gpu.download_ms.clone(),
                }),
                self.width,
                self.height,
                captured_at,
                gpu.source.load(Ordering::SeqCst),
            )),
        };
        Ok(Some(CaptureEvent::Frame(frame)))
    }
}

/// An encoder that emits a small access unit per frame: a keyframe when
/// asked for one (or first), filled with 1s; otherwise 0s.
pub struct FakeEncoder {
    caps: EncoderCaps,
    started: bool,
    /// With GPU input: encoding fails while set.
    fail: Arc<AtomicBool>,
}

impl Default for FakeEncoder {
    fn default() -> Self {
        Self {
            caps: EncoderCaps {
                name: "fake".into(),
                options: "fake".into(),
                hardware: false,
                gpu_input: false,
                bitrate_change_costs_keyframe: false,
            },
            started: false,
            fail: Arc::default(),
        }
    }
}

impl FakeEncoder {
    /// An encoder taking GPU frames only, as NVENC opened on the capture's
    /// device does. Every encode fails while `fail` is set.
    pub fn gpu(fail: Arc<AtomicBool>) -> Self {
        let mut encoder = Self::default();
        encoder.caps.gpu_input = true;
        encoder.fail = fail;
        encoder
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
        // Like the real one: an encoder takes the input it was opened for.
        let captured_at = match (input, self.caps.gpu_input) {
            (EncoderInput::Cpu(frame), false) => frame.captured_at,
            (EncoderInput::Gpu(frame), true) => {
                if self.fail.load(Ordering::SeqCst) {
                    return Err(MediaError::Backend("the fake GPU encode failed".into()));
                }
                frame.captured_at
            }
            _ => return Err(MediaError::Unsupported("the wrong kind of frame".into())),
        };
        let keyframe = force_keyframe || !self.started;
        self.started = true;
        Ok(vec![EncodedFrame {
            data: Bytes::from(vec![u8::from(keyframe); 200]),
            keyframe,
            captured_at,
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
    /// The pointer kept on a screen, or let go (M25b).
    Bounds(Option<Bounds>),
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

    fn set_bounds(&mut self, bounds: Option<Bounds>) {
        self.calls.lock().unwrap().push(KbmCall::Bounds(bounds));
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
