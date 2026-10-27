//! Sessions end to end: three engines over real iroh on loopback, in one
//! room, with a fake backend instead of the machine: gray frames, an encoder
//! that emits tiny access units, a recording pad and injector, and an
//! in-process viewer that speaks the viewer pipe. The rules are the real
//! ones; nothing here needs a GPU or a gamepad driver.

use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use couch_engine::{
    Command, Engine, EngineError, EngineOptions, Event, HidingView, HostController,
    HostControllerKind, InputSetup, KeyboardMouseSeat, MemberLink, Profile, Response, RoomView,
    SessionEnd, SessionRole, SessionView, SourceChoice, StreamStateView, StreamWindowRequest,
    StreamWindowSettingsView, WatchEnd,
    pipeline::{
        AudioKind, Backend, CaptureRequest, Focus, GameProcess, Hiding, NativeId, Source, Sources,
        ViewerOptions, ViewerProcess, VirtualDevice, WindowSource, ipc,
        virtual_hiding::CHECK_EVERY,
    },
    secrets::Unprotected,
};
use couch_input::{
    HidUsage, InputError, KbmInjector, PadButtons, PadState, PendingPad, Plugged, Rumble,
    VirtualPad, VirtualPadFactory,
    local::{
        ControllerChoice, ControllerIds, ControllerKind, LOCAL_PAD_EVERY, LocalController,
        LocalPad, LocalPads,
    },
    shortcuts::Shortcut,
};
use couch_media::{
    AudioSource, Capturer, Codec, EncoderConfig, GpuFrame, MediaError, VideoEncoder,
    audio::ToneSource,
    clock::{Clock, ProcessClock},
};
use couch_net::endpoint::{RelayChoice, RelayToken};
use couch_proto::{
    input::PadSnapshot,
    ipc::{
        HudState, LocalInput, SeatState, ShortcutKeys, StreamVolume, UserAction, VideoFrameHeader,
        WindowRequest, WindowState,
    },
    session::{EndReason, SeatKind, StreamState},
};
use couch_room::{limits::LimitConfig, room::Timings};
use couch_testkit::{
    FakeCapturer, FakeEncoder, FakeGpu, RecordingInjector, RecordingPadFactory, relay::TestRelay,
};
use tempfile::TempDir;
use tokio::sync::{broadcast, mpsc};

const PASSWORD: &str = "pizza night";
const WAIT: Duration = Duration::from_secs(20);
/// The fake window everyone streams.
const WINDOW: NativeId = 7;

// ---- The fake machine ----

/// A pad Windows is still setting up (P10): it becomes a recording pad once
/// the test says so, and counts as given up if dropped before.
struct FakePending {
    pads: RecordingPadFactory,
    set_up: Arc<AtomicBool>,
    given_up: Arc<AtomicUsize>,
    done: bool,
}

impl PendingPad for FakePending {
    fn poll(&mut self, wait: Duration) -> Result<Option<Box<dyn VirtualPad>>, InputError> {
        if self.set_up.load(Ordering::SeqCst) {
            self.done = true;
            return self.pads.plug().map(Some);
        }
        std::thread::sleep(wait);
        Ok(None)
    }
}

impl Drop for FakePending {
    fn drop(&mut self) {
        if !self.done {
            self.given_up.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[derive(Default)]
struct Fake {
    kbm: RecordingInjector,
    pads: RecordingPadFactory,
    /// The next this many pads plugged come back pending, Windows still
    /// setting them up (P10), until `pads_set_up`.
    pads_setting_up: AtomicUsize,
    pads_set_up: Arc<AtomicBool>,
    /// Pending pads given up, so unplugged, so far.
    pads_given_up: Arc<AtomicUsize>,
    /// How many had been given up when the host's controller last showed.
    given_up_when_shown: Arc<Mutex<Option<usize>>>,
    viewers: Mutex<Vec<FakeViewer>>,
    /// The game: its process, and the window it shows.
    game: Arc<FakeGame>,
    /// The current capture's window closed: capture says so.
    capture_closed: Mutex<Arc<AtomicBool>>,
    clock: Arc<ProcessClock>,
    /// The panic key's handler while the engine has it hooked.
    panic_key: PanicHook,
    /// The key the panic-key hook was last installed on.
    panic_key_on: Mutex<Option<HidUsage>>,
    /// Every key the host-keys hook was last installed on.
    host_keys_on: Mutex<Vec<(HidUsage, Shortcut)>>,
    /// The hook refuses any set of keys beyond the panic key alone.
    answer_keys_fail: AtomicBool,
    /// The game window isn't in front (another window took the focus).
    away: AtomicBool,
    /// The stream windows allowed to take the foreground, by process (P11).
    foreground_allowed: Mutex<Vec<u32>>,
    /// The host's own controller.
    local: FakeLocal,
    /// How many of the host's controllers are hidden from games now.
    hidden: Arc<AtomicUsize>,
    /// Hiding it fails (HidHide busy).
    hide_fails: AtomicBool,
    /// The remapper pads' devices hidden now (M22), and how many hides.
    virtual_hidden: Arc<Mutex<Vec<String>>>,
    virtual_hides: AtomicUsize,
    /// Where each such device stands, if not plugged in and theirs.
    virtual_now: Mutex<Vec<(String, VirtualDevice)>>,
    /// Frames on the GPU (M9b).
    gpu: FakeGpuSide,
    /// The capture's frame rate, if not the stream's: a screen refreshing
    /// faster than the stream (M9d).
    capture_fps: AtomicU32,
    /// Every capture started, as asked for.
    captures: Mutex<Vec<CaptureRequest>>,
    /// This PC decodes HEVC in hardware (M12c), for watching.
    decodes_hevc: AtomicBool,
    /// HEVC encoders fail to open (another GPU's, or a broken driver).
    hevc_fails: AtomicBool,
    /// Captures fail to start (M19: a stream failing as it starts).
    capture_fails: AtomicBool,
    /// The codec of every encoder opened, in order.
    codecs: Mutex<Vec<Codec>>,
}

/// GPU frames on the fake machine: off unless a scenario turns them on.
#[derive(Default)]
struct FakeGpuSide {
    /// The capture makes GPU frames.
    capture_on_gpu: AtomicBool,
    /// Their pool, and how often they were copied to memory.
    frames: FakeGpu,
    /// Encoders opened for a GPU frame read it (as NVENC on the capture's
    /// device); otherwise they take frames in memory (another GPU's encoder).
    encoders_take_it: AtomicBool,
    /// Encoders reading GPU frames fail while set.
    fail: Arc<AtomicBool>,
    /// Every encoder opened: the pool it reads GPU frames from, or `None`
    /// for one taking frames in memory.
    opened: Mutex<Vec<Option<u64>>>,
}

/// The host's own controllers: a DualSense and a GameSir (M23), each
/// followed by a thread that reports its state on change and every 100 ms,
/// as the real reader does; and a remapper's pad (M22) while it has a device.
#[derive(Default)]
struct FakeLocal {
    state: Arc<Mutex<PadState>>,
    rumble: Arc<Mutex<Vec<Rumble>>>,
    /// The GameSir's.
    second_state: Arc<Mutex<PadState>>,
    second_rumble: Arc<Mutex<Vec<Rumble>>>,
    /// The remapper's pad: its device while plugged in.
    remapper_device: Arc<Mutex<Option<String>>>,
    remapper_state: Arc<Mutex<PadState>>,
    remapper_rumble: Arc<Mutex<Vec<Rumble>>>,
}

/// The device ids a remapper's pad takes (ViGEm's).
const PAD_01: &str = r"USB\VID_045E&PID_028E\01";
const PAD_02: &str = r"USB\VID_045E&PID_028E\02";

const XBOX_360: ControllerIds = ControllerIds {
    vendor: 0x045E,
    product: 0x028E,
};

const DUALSENSE: ControllerIds = ControllerIds {
    vendor: 0x054C,
    product: 0x0CE6,
};

const GAMESIR: ControllerIds = ControllerIds {
    vendor: 0x3537,
    product: 0x1040,
};

fn dualsense() -> HostController {
    HostController {
        name: "DualSense Wireless Controller".into(),
        vendor: DUALSENSE.vendor,
        product: DUALSENSE.product,
        kind: HostControllerKind::Physical,
    }
}

fn gamesir() -> HostController {
    HostController {
        name: "GameSir T4n Lite".into(),
        vendor: GAMESIR.vendor,
        product: GAMESIR.product,
        kind: HostControllerKind::Physical,
    }
}

/// DS4Windows's pad, say (M22).
fn remapper() -> HostController {
    HostController {
        name: "Xbox 360 Controller".into(),
        vendor: XBOX_360.vendor,
        product: XBOX_360.product,
        kind: HostControllerKind::OtherVirtual,
    }
}

impl LocalPads for FakeLocal {
    fn list(&self) -> Result<Vec<LocalController>, InputError> {
        let mut list = vec![
            LocalController {
                ids: DUALSENSE,
                name: "DualSense Wireless Controller".into(),
                kind: ControllerKind::Physical,
                device: None,
            },
            LocalController {
                ids: GAMESIR,
                name: "GameSir T4n Lite".into(),
                kind: ControllerKind::Physical,
                device: None,
            },
        ];
        if let Some(device) = self.remapper_device.lock().unwrap().clone() {
            list.push(LocalController {
                ids: XBOX_360,
                name: "Xbox 360 Controller".into(),
                kind: ControllerKind::OtherVirtual,
                device: Some(device),
            });
        }
        Ok(list)
    }

    fn follow(
        &self,
        choice: ControllerChoice,
        on_state: Box<dyn Fn(PadState) + Send>,
        on_device: Box<dyn Fn(Option<String>) + Send>,
    ) -> Result<Box<dyn LocalPad>, InputError> {
        let (state, rumble, device) = match (choice.ids, choice.kind) {
            (DUALSENSE, ControllerKind::Physical) => {
                (self.state.clone(), self.rumble.clone(), None)
            }
            (GAMESIR, ControllerKind::Physical) => {
                (self.second_state.clone(), self.second_rumble.clone(), None)
            }
            (XBOX_360, ControllerKind::OtherVirtual) => (
                self.remapper_state.clone(),
                self.remapper_rumble.clone(),
                Some(self.remapper_device.clone()),
            ),
            other => panic!("no such controller {other:?}"),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::spawn(move || {
            let mut sent: Option<(PadState, Instant)> = None;
            let mut told: Option<String> = None;
            while !stopped.load(Ordering::SeqCst) {
                // As the reader: a remapper's pad tells each device it has.
                if let Some(device) = &device {
                    let now = device.lock().unwrap().clone();
                    if now != told {
                        told = now.clone();
                        on_device(now);
                    }
                }
                let now = *state.lock().unwrap();
                if sent.is_none_or(|(s, at)| s != now || at.elapsed() >= LOCAL_PAD_EVERY) {
                    on_state(now);
                    sent = Some((now, Instant::now()));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        Ok(Box::new(FakeFollowed { stop, rumble }))
    }
}

struct FakeFollowed {
    stop: Arc<AtomicBool>,
    rumble: Arc<Mutex<Vec<Rumble>>>,
}

impl LocalPad for FakeFollowed {
    fn rumble(&self, rumble: Rumble) {
        self.rumble.lock().unwrap().push(rumble);
    }
}

impl Drop for FakeFollowed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Shows a remapper's pad to games again when the engine drops it (M22).
struct VirtualShown(Arc<Mutex<Vec<String>>>, String);

impl Drop for VirtualShown {
    fn drop(&mut self) {
        let mut hidden = self.0.lock().unwrap();
        if let Some(at) = hidden.iter().position(|d| *d == self.1) {
            hidden.remove(at);
        }
    }
}

/// Shows the host's controller to games again when the engine drops it,
/// noting how many pads being set up had been given up by then (P10).
struct Shown {
    hidden: Arc<AtomicUsize>,
    given_up: Arc<AtomicUsize>,
    given_up_when_shown: Arc<Mutex<Option<usize>>>,
}

impl Drop for Shown {
    fn drop(&mut self) {
        *self.given_up_when_shown.lock().unwrap() = Some(self.given_up.load(Ordering::SeqCst));
        self.hidden.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The streamed game's process.
struct FakeGame {
    exited: AtomicBool,
    /// The window it shows; 0 for none.
    window: AtomicIsize,
    /// Other windows of the same process (another browser window, say).
    others: Mutex<Vec<NativeId>>,
}

impl Default for FakeGame {
    fn default() -> Self {
        Self {
            exited: AtomicBool::new(false),
            window: AtomicIsize::new(WINDOW),
            others: Mutex::new(Vec::new()),
        }
    }
}

struct WatchedFakeGame(Arc<FakeGame>);

impl GameProcess for WatchedFakeGame {
    fn running(&self) -> bool {
        !self.0.exited.load(Ordering::SeqCst)
    }

    fn windows(&self) -> Vec<NativeId> {
        let mut windows = self.0.others.lock().unwrap().clone();
        match self.0.window.load(Ordering::SeqCst) {
            0 => {}
            window => windows.insert(0, window),
        }
        windows
    }
}

impl Fake {
    /// The game's window closes; its process goes on.
    fn close_window(&self) {
        self.game.window.store(0, Ordering::SeqCst);
        self.capture_closed
            .lock()
            .unwrap()
            .store(true, Ordering::SeqCst);
    }

    /// The game destroys its window and shows `window` instead.
    fn swap_window(&self, window: NativeId) {
        self.game.window.store(window, Ordering::SeqCst);
        self.capture_closed
            .lock()
            .unwrap()
            .store(true, Ordering::SeqCst);
    }

    /// The game exits, and its window with it.
    fn exit_game(&self) {
        self.game.exited.store(true, Ordering::SeqCst);
        self.close_window();
    }

    /// The pad's latest state, if one was plugged.
    fn pad(&self) -> Option<PadState> {
        let plugged = self.pads.plugged.lock().unwrap();
        plugged.last()?.states().last().copied()
    }

    /// The latest state of the `n`th pad plugged in (seat order).
    fn pad_at(&self, n: usize) -> Option<PadState> {
        let plugged = self.pads.plugged.lock().unwrap();
        plugged.get(n)?.states().last().copied()
    }

    fn pads_plugged(&self) -> usize {
        self.pads.plugged.lock().unwrap().len()
    }

    /// The host holds these buttons on their own controller.
    fn local_press(&self, buttons: PadButtons) {
        *self.local.state.lock().unwrap() = pressed(buttons);
    }

    /// The host's second controller, the GameSir (M23).
    fn second_press(&self, buttons: PadButtons) {
        *self.local.second_state.lock().unwrap() = pressed(buttons);
    }

    fn hidden(&self) -> bool {
        self.hidden.load(Ordering::SeqCst) > 0
    }

    fn hidden_count(&self) -> usize {
        self.hidden.load(Ordering::SeqCst)
    }

    /// The remapper's pad, with these buttons down (M22).
    fn remapper_press(&self, buttons: PadButtons) {
        *self.local.remapper_state.lock().unwrap() = pressed(buttons);
    }

    /// The remapper's pad plugged in under `device`, or gone.
    fn plug_remapper(&self, device: Option<&str>) {
        *self.local.remapper_device.lock().unwrap() = device.map(String::from);
    }

    fn virtual_hidden(&self) -> Vec<String> {
        self.virtual_hidden.lock().unwrap().clone()
    }

    fn set_virtual_now(&self, device: &str, now: VirtualDevice) {
        let mut all = self.virtual_now.lock().unwrap();
        all.retain(|(d, _)| d != device);
        all.push((device.into(), now));
    }

    fn viewer(&self) -> Option<FakeViewer> {
        self.viewers.lock().unwrap().last().cloned()
    }
}

impl Backend for Fake {
    fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }

    fn sources(&self) -> Result<Sources, MediaError> {
        Ok(Sources {
            windows: vec![WindowSource {
                id: WINDOW,
                title: "Celeste".into(),
                app: "Celeste.exe".into(),
                elevated: false,
                refresh_hz: Some(60),
            }],
            screens: Vec::new(),
        })
    }

    fn capture(
        &self,
        source: &Source,
        request: CaptureRequest,
    ) -> Result<(Box<dyn Capturer>, Option<NativeId>), MediaError> {
        self.captures.lock().unwrap().push(request);
        if self.capture_fails.load(Ordering::SeqCst) {
            return Err(MediaError::Backend("no capture here".into()));
        }
        let window = match source {
            Source::Window(window) => Some(*window),
            _ => None,
        };
        // As Windows.Graphics.Capture does, a window that isn't there any
        // more can't be captured.
        let shown = self.game.window.load(Ordering::SeqCst);
        if window.is_some_and(|w| w != shown) {
            return Err(MediaError::Backend("the window is gone".into()));
        }
        let closed = Arc::new(AtomicBool::new(false));
        *self.capture_closed.lock().unwrap() = closed.clone();
        let fps = match self.capture_fps.load(Ordering::SeqCst) {
            0 => request.fps,
            faster => faster,
        };
        let capturer = FakeCapturer::new(64, 36, fps, self.clock.clone(), closed);
        let capturer = if self.gpu.capture_on_gpu.load(Ordering::SeqCst) && !request.in_memory {
            capturer.on_gpu(self.gpu.frames.clone())
        } else {
            capturer
        };
        Ok((Box::new(capturer), window))
    }

    fn encoder(
        &self,
        config: EncoderConfig,
        gpu: Option<&GpuFrame>,
    ) -> Result<Box<dyn VideoEncoder>, MediaError> {
        self.codecs.lock().unwrap().push(config.codec);
        if config.codec == Codec::Hevc && self.hevc_fails.load(Ordering::SeqCst) {
            return Err(MediaError::Unsupported("no HEVC encoder here".into()));
        }
        let gpu_input = gpu.is_some() && self.gpu.encoders_take_it.load(Ordering::SeqCst);
        self.gpu
            .opened
            .lock()
            .unwrap()
            .push(gpu.filter(|_| gpu_input).map(|f| f.source));
        Ok(Box::new(if gpu_input {
            FakeEncoder::gpu(self.gpu.fail.clone())
        } else {
            FakeEncoder::default()
        }))
    }

    fn audio(&self, _kind: AudioKind) -> Result<Box<dyn AudioSource>, MediaError> {
        Ok(Box::new(ToneSource::new(self.clock.clone())))
    }

    fn decodes(&self) -> u8 {
        if self.decodes_hevc.load(Ordering::SeqCst) {
            Codec::H264.bit() | Codec::Hevc.bit()
        } else {
            Codec::H264.bit()
        }
    }

    fn plug_pad(&self, _real_ids: bool) -> Result<Plugged, InputError> {
        let pending = self
            .pads_setting_up
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if !pending {
            return self.pads.plug().map(Plugged::Ready);
        }
        Ok(Plugged::SettingUp(Box::new(FakePending {
            pads: self.pads.clone(),
            set_up: self.pads_set_up.clone(),
            given_up: self.pads_given_up.clone(),
            done: false,
        })))
    }

    fn kbm(&self) -> Box<dyn KbmInjector> {
        Box::new(self.kbm.clone())
    }

    fn local_pads(&self) -> Option<&dyn LocalPads> {
        Some(&self.local)
    }

    fn hiding(&self) -> Hiding {
        Hiding::Ready
    }

    fn hide_controller(&self, ids: ControllerIds) -> Result<Box<dyn std::any::Any + Send>, String> {
        assert!(ids == DUALSENSE || ids == GAMESIR, "{ids:?}");
        if self.hide_fails.load(Ordering::SeqCst) {
            return Err("HidHide's configuration window is open".into());
        }
        self.hidden.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Shown {
            hidden: self.hidden.clone(),
            given_up: self.pads_given_up.clone(),
            given_up_when_shown: self.given_up_when_shown.clone(),
        }))
    }

    fn hide_virtual(&self, device: &str) -> Result<Box<dyn std::any::Any + Send>, String> {
        // As the platform: only another program's pad, by the markers now.
        if self.virtual_device_now(device) != VirtualDevice::Theirs {
            return Err("that pad isn't proven another program's".into());
        }
        self.virtual_hides.fetch_add(1, Ordering::SeqCst);
        self.virtual_hidden.lock().unwrap().push(device.into());
        Ok(Box::new(VirtualShown(
            self.virtual_hidden.clone(),
            device.into(),
        )))
    }

    fn virtual_device_now(&self, device: &str) -> VirtualDevice {
        self.virtual_now
            .lock()
            .unwrap()
            .iter()
            .find(|(d, _)| d == device)
            .map_or(VirtualDevice::Theirs, |(_, now)| *now)
    }

    fn focus(&self, _window: NativeId) -> Focus {
        let foreground = !self.away.load(Ordering::SeqCst);
        Focus {
            foreground,
            pointer_inside: foreground,
        }
    }

    fn window_exists(&self, window: NativeId) -> bool {
        window != 0 && self.game.window.load(Ordering::SeqCst) == window
    }

    fn game_process(&self, _window: NativeId) -> Option<Box<dyn GameProcess>> {
        Some(Box::new(WatchedFakeGame(self.game.clone())))
    }

    fn launch_viewer(&self, options: &ViewerOptions) -> Result<ViewerProcess, MediaError> {
        let (viewer, process) = FakeViewer::launch(options);
        self.viewers.lock().unwrap().push(viewer);
        Ok(process)
    }

    fn allow_foreground(&self, pid: u32) {
        self.foreground_allowed.lock().unwrap().push(pid);
    }

    fn host_keys(
        &self,
        keys: &[(HidUsage, Shortcut)],
        on_press: Box<dyn Fn(Shortcut) + Send>,
    ) -> Result<Box<dyn std::any::Any + Send>, MediaError> {
        if keys.len() > 1 && self.answer_keys_fail.load(Ordering::SeqCst) {
            return Err(MediaError::Backend("an answer key has no scancode".into()));
        }
        *self.panic_key.lock().unwrap() = Some(on_press);
        *self.host_keys_on.lock().unwrap() = keys.to_vec();
        *self.panic_key_on.lock().unwrap() = keys
            .iter()
            .find(|(_, s)| *s == Shortcut::Panic)
            .map(|(key, _)| *key);
        Ok(Box::new(Unhook(self.panic_key.clone())))
    }
}

type PanicHook = Arc<Mutex<Option<Box<dyn Fn(Shortcut) + Send>>>>;

/// Removes the fake panic-key hook when the engine drops it.
struct Unhook(PanicHook);

impl Drop for Unhook {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = None;
    }
}

impl Fake {
    /// The host presses the panic key on its keyboard; `false` if no hook
    /// was in place to see it.
    fn press_panic_key(&self) -> bool {
        self.press_host_key(Shortcut::Panic)
    }

    /// The host presses one of its keys (the hook reports which).
    fn press_host_key(&self, key: Shortcut) -> bool {
        match &*self.panic_key.lock().unwrap() {
            Some(on_press) => {
                on_press(key);
                true
            }
            None => false,
        }
    }

    fn panic_key_hooked(&self) -> bool {
        self.panic_key.lock().unwrap().is_some()
    }
}

/// Pipe frames the fake viewer sends: (kind, payload).
type Said = mpsc::UnboundedSender<(u8, Vec<u8>)>;

/// A viewer window, in-process: it counts the frames it gets, keeps the
/// seat and HUD state, says it is alive every 100 ms, and sends the input
/// and actions a test asks for.
#[derive(Clone, Default)]
struct FakeViewer {
    frames: Arc<AtomicU64>,
    keyframes: Arc<AtomicU64>,
    first_was_key: Arc<Mutex<Option<bool>>>,
    seat: Arc<Mutex<Option<SeatState>>>,
    hud: Arc<Mutex<Option<HudState>>>,
    /// Pipe frames to send back: (kind, payload).
    say: Arc<Mutex<Option<Said>>>,
    shut_down: Arc<AtomicBool>,
    /// The kinds of the first frames from the main process, in order.
    first_kinds: Arc<Mutex<Vec<u8>>>,
    shortcuts: Arc<Mutex<Option<ShortcutKeys>>>,
    /// The codec of the last frame received (`couch_proto::codec`).
    codec: Arc<AtomicU32>,
    /// The stream volume, as the main process last set it (P6, P11).
    volume: Arc<Mutex<Option<StreamVolume>>>,
    /// What the app asked of the window (P11), in order.
    requests: Arc<Mutex<Vec<WindowRequest>>>,
    /// It was started with its bar hidden (`--no-bar`).
    started_without_bar: Arc<AtomicBool>,
    /// It was started with its stats shown (no `--hide-stats`).
    started_with_stats: Arc<AtomicBool>,
}

/// The process id every fake stream window has, to see the foreground
/// allowed to it (P11).
const FAKE_VIEWER_PID: u32 = 4242;

impl FakeViewer {
    /// Like the real one, it says how it is at once and after each request
    /// (P11).
    fn launch(options: &ViewerOptions) -> (Self, ViewerProcess) {
        let (hide_bar, hide_stats) = (options.hide_bar, options.hide_stats);
        let (main_in, mut viewer_in) = tokio::io::duplex(1 << 20);
        let (mut viewer_out, main_out) = tokio::io::duplex(1 << 20);
        let (say, mut to_say) = mpsc::unbounded_channel::<(u8, Vec<u8>)>();
        let viewer = Self::default();
        viewer.started_without_bar.store(hide_bar, Ordering::SeqCst);
        viewer
            .started_with_stats
            .store(!hide_stats, Ordering::SeqCst);
        *viewer.say.lock().unwrap() = Some(say);
        let v = viewer.clone();
        tokio::spawn(async move {
            let mut alive = tokio::time::interval(Duration::from_millis(100));
            let mut scratch = Vec::new();
            let mut state = WindowState {
                fullscreen: false,
                bar: !hide_bar,
                stats: !hide_stats,
            };
            let payload = couch_proto::ipc::to_payload(&state);
            if ipc::write_frame(
                &mut viewer_out,
                couch_proto::ipc::kinds::WINDOW_STATE,
                &payload,
                &mut scratch,
            )
            .await
            .is_err()
            {
                return;
            }
            loop {
                tokio::select! {
                    frame = ipc::read_frame(&mut viewer_in) => {
                        let Ok(Some(frame)) = frame else { break };
                        {
                            let mut first = v.first_kinds.lock().unwrap();
                            if first.len() < 4 {
                                first.push(frame.kind);
                            }
                        }
                        match frame.kind {
                            couch_proto::ipc::kinds::SHORTCUTS => {
                                *v.shortcuts.lock().unwrap() = couch_proto::ipc::from_payload(&frame.payload);
                            }
                            couch_proto::ipc::kinds::VIDEO_FRAME => {
                                if let Some((header, _)) = VideoFrameHeader::read(&frame.payload) {
                                    v.first_was_key.lock().unwrap().get_or_insert(header.keyframe);
                                    v.frames.fetch_add(1, Ordering::SeqCst);
                                    v.keyframes.fetch_add(u64::from(header.keyframe), Ordering::SeqCst);
                                    v.codec.store(u32::from(header.codec), Ordering::SeqCst);
                                }
                            }
                            couch_proto::ipc::kinds::SEAT => {
                                *v.seat.lock().unwrap() = couch_proto::ipc::from_payload(&frame.payload);
                            }
                            couch_proto::ipc::kinds::HUD_STATE => {
                                *v.hud.lock().unwrap() = couch_proto::ipc::from_payload(&frame.payload);
                            }
                            couch_proto::ipc::kinds::VOLUME => {
                                *v.volume.lock().unwrap() = couch_proto::ipc::from_payload(&frame.payload);
                            }
                            couch_proto::ipc::kinds::WINDOW => {
                                let Some(request) = couch_proto::ipc::from_payload::<WindowRequest>(&frame.payload) else { continue };
                                v.requests.lock().unwrap().push(request);
                                match request {
                                    WindowRequest::Fullscreen(on) => state.fullscreen = on,
                                    WindowRequest::ShowBar(on) => state.bar = on,
                                    WindowRequest::ShowStats(on) => state.stats = on,
                                    WindowRequest::Raise => continue,
                                }
                                let payload = couch_proto::ipc::to_payload(&state);
                                if ipc::write_frame(&mut viewer_out, couch_proto::ipc::kinds::WINDOW_STATE, &payload, &mut scratch).await.is_err() {
                                    break;
                                }
                            }
                            couch_proto::ipc::kinds::SHUTDOWN => {
                                v.shut_down.store(true, Ordering::SeqCst);
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ = alive.tick() => {
                        let payload = couch_proto::ipc::to_payload(&LocalInput::Alive);
                        if ipc::write_frame(&mut viewer_out, couch_proto::ipc::kinds::INPUT, &payload, &mut scratch).await.is_err() {
                            break;
                        }
                    }
                    said = to_say.recv() => {
                        // `None`: the window was closed.
                        let Some((kind, payload)) = said else { break };
                        if ipc::write_frame(&mut viewer_out, kind, &payload, &mut scratch).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let process = ViewerProcess {
            stdin: Box::pin(main_in),
            stdout: Box::pin(main_out),
            guard: Box::new(()),
            pid: Some(FAKE_VIEWER_PID),
        };
        (viewer, process)
    }

    /// The user moves the window's own volume slider (P6).
    fn set_volume(&self, volume: StreamVolume) {
        self.say(
            couch_proto::ipc::kinds::VOLUME_CHANGED,
            couch_proto::ipc::to_payload(&volume),
        );
    }

    /// The user toggles something on the window itself (its shortcut, its
    /// bar): it says how it is now (P11).
    fn state(&self, state: WindowState) {
        self.say(
            couch_proto::ipc::kinds::WINDOW_STATE,
            couch_proto::ipc::to_payload(&state),
        );
    }

    fn say(&self, kind: u8, payload: Vec<u8>) {
        if let Some(say) = &*self.say.lock().unwrap() {
            let _ = say.send((kind, payload));
        }
    }

    /// The local gamepad, with these buttons down.
    fn pad(&self, buttons: PadButtons) {
        self.pad_of(0, buttons);
    }

    /// Local controller `controller` (in connection order), with these
    /// buttons down (M18).
    fn pad_of(&self, controller: u8, buttons: PadButtons) {
        let pad = LocalInput::Pad {
            controller,
            pad: PadSnapshot {
                buttons: buttons.bits(),
                ..Default::default()
            },
        };
        self.say(
            couch_proto::ipc::kinds::INPUT,
            couch_proto::ipc::to_payload(&pad),
        );
    }

    /// The window's first `count` controller slots connected (M18).
    fn controllers(&self, count: u8) {
        self.slots((1u8 << count) - 1);
    }

    /// The window's controller slots connected, as bits (M18).
    fn slots(&self, slots: u8) {
        self.say(
            couch_proto::ipc::kinds::INPUT,
            couch_proto::ipc::to_payload(&LocalInput::ControllerSlots(slots)),
        );
    }

    /// The window's controllers' names, by slot (P12), as the real one
    /// sends them just before its slots.
    fn name_controllers(&self, names: &[(u8, &str)]) {
        let names = couch_proto::ipc::ControllerNames {
            controllers: names
                .iter()
                .map(|(slot, name)| couch_proto::session::GuestController {
                    slot: *slot,
                    name: (*name).into(),
                })
                .collect(),
        };
        self.say(
            couch_proto::ipc::kinds::CONTROLLER_NAMES,
            couch_proto::ipc::to_payload(&names),
        );
    }

    /// A key on the local keyboard (a HID usage).
    fn key(&self, usage: u16, down: bool) {
        self.say(
            couch_proto::ipc::kinds::INPUT,
            couch_proto::ipc::to_payload(&LocalInput::Key { usage, down }),
        );
    }

    fn action(&self, action: UserAction) {
        self.say(
            couch_proto::ipc::kinds::USER_ACTION,
            couch_proto::ipc::to_payload(&action),
        );
    }

    /// The user closes the window: like the real one, it says it leaves.
    fn close(&self) {
        self.action(UserAction::Leave);
        self.say.lock().unwrap().take();
    }

    /// The window process dies without a word.
    fn crash(&self) {
        self.say.lock().unwrap().take();
    }

    fn frames(&self) -> u64 {
        self.frames.load(Ordering::SeqCst)
    }

    fn keyframes(&self) -> u64 {
        self.keyframes.load(Ordering::SeqCst)
    }

    /// The HUD as the main process last sent it.
    fn hud(&self) -> Option<HudState> {
        self.hud.lock().unwrap().clone()
    }

    /// The codec frames come in, once any came.
    fn codec(&self) -> Option<Codec> {
        Codec::from_wire(self.codec.load(Ordering::SeqCst) as u8)
    }

    fn holding(&self) -> bool {
        self.seat
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(SeatState::holding)
    }

    /// The seats the window was told it holds.
    fn held_seats(&self) -> Vec<u8> {
        self.seat
            .lock()
            .unwrap()
            .as_ref()
            .map_or_else(Vec::new, |s| s.seats.iter().map(|h| h.seat).collect())
    }
}

// ---- Members ----

fn options(port: u16, fake: Arc<Fake>) -> EngineOptions {
    EngineOptions {
        port: Some(port),
        relays: RelayChoice::Disabled,
        secret_store: Arc::new(Unprotected),
        limits: LimitConfig::default(),
        timings: Timings {
            repair_every: Duration::from_secs(2),
            first_redial: Duration::from_millis(200),
            max_redial: Duration::from_secs(2),
            after_shutdown: Duration::from_secs(1),
        },
        tick: Duration::from_millis(50),
        invite_valid_for: Duration::from_secs(3600),
        profile_wait: Duration::from_secs(2),
        backend: fake,
        join_wait: Duration::from_secs(5),
        replace_grace: Duration::from_millis(500),
        reconnect_wait: Duration::from_secs(5),
        relay_only: false,
        backup_files: None,
    }
}

struct Member {
    _dir: TempDir,
    name: &'static str,
    engine: Option<Engine>,
    fake: Arc<Fake>,
    events: broadcast::Receiver<Event>,
}

impl Member {
    async fn new(name: &'static str) -> Self {
        Self::with(name, |options| options).await
    }

    /// A member whose options `tweak` changes.
    async fn with(name: &'static str, tweak: impl FnOnce(EngineOptions) -> EngineOptions) -> Self {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        let engine = start(dir.path(), tweak(options(free_port(), fake.clone()))).await;
        engine
            .handle(Command::SetDisplayName { name: name.into() })
            .await
            .unwrap();
        let events = engine.subscribe();
        Self {
            _dir: dir,
            name,
            engine: Some(engine),
            fake,
            events,
        }
    }

    fn engine(&self) -> &Engine {
        self.engine.as_ref().expect("running")
    }

    fn id(&self) -> String {
        self.engine().member_id().to_string()
    }

    async fn handle(&self, command: Command) -> Result<Response, EngineError> {
        self.engine().handle(command).await
    }

    async fn ok(&self, command: Command) {
        self.handle(command).await.unwrap();
    }

    async fn room(&self, id: &str) -> Option<RoomView> {
        match self.handle(Command::ListRooms).await.unwrap() {
            Response::Rooms { rooms } => rooms.into_iter().find(|r| r.id == id),
            other => panic!("expected rooms, got {other:?}"),
        }
    }

    /// `host`'s session in the room, as this member sees it.
    async fn session(&self, room: &str, host: &Member) -> Option<SessionView> {
        let host = host.id();
        self.room(room)
            .await?
            .sessions
            .into_iter()
            .find(|s| s.host == host)
    }

    async fn wait_session(
        &self,
        room: &str,
        host: &Member,
        what: &str,
        check: impl Fn(Option<&SessionView>) -> bool,
    ) {
        let deadline = Instant::now() + WAIT;
        loop {
            let session = self.session(room, host).await;
            if check(session.as_ref()) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{}: timed out waiting for {what}; last: {session:#?}",
                self.name
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// A session, and the game's window streamed in it.
    async fn go_live(&self, room: &str, max_viewers: u8) {
        let reply = self.live(go_live(room, max_viewers)).await;
        assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    }

    /// Starts the session `start` asks for, then streams the game's window
    /// in it (M19); the first refusal comes back.
    async fn live(&self, start: Command) -> Result<Response, EngineError> {
        self.handle(start).await?;
        self.handle(stream_window()).await
    }

    async fn watch(&self, room: &str, host: &Member) -> Result<Response, EngineError> {
        self.handle(Command::Watch {
            room: room.into(),
            host: host.id(),
        })
        .await
    }

    /// The next event `pick` accepts, within the wait.
    async fn event<T>(&mut self, what: &str, pick: impl Fn(&Event) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let event = tokio::time::timeout_at(deadline, self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("{}: no {what}", self.name))
                .expect("events flow");
            if let Some(found) = pick(&event) {
                return found;
            }
        }
    }

    async fn stop(&mut self) {
        if let Some(engine) = self.engine.take() {
            engine.shutdown().await;
        }
    }
}

async fn start(dir: &Path, options: EngineOptions) -> Engine {
    Engine::start(Profile::new("test", dir).unwrap(), options)
        .await
        .expect("engine starts")
}

/// A session with one controller seat, nothing streamed yet (M19).
fn go_live(room: &str, max_viewers: u8) -> Command {
    go_live_playing(room, max_viewers, InputSetup::default(), vec![])
}

/// A session with these seats.
fn go_live_with(room: &str, max_viewers: u8, input: InputSetup) -> Command {
    go_live_playing(room, max_viewers, input, vec![])
}

/// A session with these seats, and the host's own controllers on some.
fn go_live_playing(
    room: &str,
    max_viewers: u8,
    input: InputSetup,
    controllers: Vec<HostController>,
) -> Command {
    Command::StartSession {
        room: room.into(),
        title: "Celeste".into(),
        max_viewers,
        input,
        controllers,
    }
}

/// Streams the game's window in our session.
fn stream_window() -> Command {
    Command::StartStream {
        source: SourceChoice::Window {
            id: WINDOW.to_string(),
        },
        name: "Celeste".into(),
        fps: 30,
        max_mbps: 20,
        upload_mbps: None,
    }
}

/// `controllers` controller seats, and keyboard and mouse as `kbm` says.
const fn setup(controllers: u8, kbm: KeyboardMouseSeat) -> InputSetup {
    InputSetup {
        controllers,
        keyboard_mouse: kbm,
    }
}

/// A (the owner) with B and C, all connected.
async fn three() -> (Member, Member, Member, String) {
    let a = Member::new("Ana").await;
    let b = Member::new("Bia").await;
    let c = Member::new("Caio").await;
    let Response::Room(room) = a
        .handle(Command::CreateRoom {
            name: "Sofá".into(),
            password: PASSWORD.into(),
        })
        .await
        .unwrap()
    else {
        panic!("a room");
    };
    let Response::Invite { link } = a
        .handle(Command::GetInvite {
            room: room.id.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("an invite");
    };
    for m in [&b, &c] {
        m.ok(Command::JoinRoom {
            invite: link.clone(),
            password: PASSWORD.into(),
        })
        .await;
    }
    for m in [&a, &b, &c] {
        let deadline = Instant::now() + WAIT;
        loop {
            let view = m.room(&room.id).await.unwrap();
            let linked = view
                .members
                .iter()
                .filter(|x| matches!(x.link, MemberLink::Direct | MemberLink::Relay))
                .count();
            if view.members.len() == 3 && linked == 2 {
                break;
            }
            assert!(Instant::now() < deadline, "{}: no full mesh", m.name);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    (a, b, c, room.id)
}

/// `viewer` watches `host`'s live session; returns its viewer window once
/// frames arrive.
async fn watching(room: &str, host: &Member, viewer: &Member) -> FakeViewer {
    viewer
        .wait_session(room, host, "the announcement", |s| s.is_some())
        .await;
    let reply = viewer.watch(room, host).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    let window = viewer.fake.viewer().expect("a viewer window");
    wait("frames", || window.frames() >= 5).await;
    window
}

async fn wait(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// B asks for seat 0, A accepts: B plays.
async fn give_control(room: &str, a: &mut Member, b: &Member, window: &FakeViewer) {
    give_seat(room, a, b, window, 0).await;
}

/// B asks for `seat`, A accepts: B plays on it.
async fn give_seat(room: &str, a: &mut Member, b: &Member, window: &FakeViewer, seat: u8) {
    b.ok(Command::RequestControl {
        seat,
        controller: None,
    })
    .await;
    let asker = a
        .event("a request", |e| match e {
            Event::ControlRequested { member, .. } => Some(member.clone()),
            _ => None,
        })
        .await;
    assert_eq!(asker, b.id());
    a.ok(Command::AnswerControl {
        member: b.id(),
        accept: true,
    })
    .await;
    wait("the seat", || window.held_seats().contains(&seat)).await;
    a.wait_session(room, a, "B on the seat", |s| {
        s.is_some_and(|s| holder(s, seat) == Some(b.id()))
    })
    .await;
}

/// Who holds `seat`, as `s` shows it.
fn holder(s: &SessionView, seat: u8) -> Option<String> {
    s.seats.get(usize::from(seat))?.holder.clone()
}

fn pressed(buttons: PadButtons) -> PadState {
    PadState {
        buttons,
        ..PadState::NEUTRAL
    }
}

async fn stop_all(members: &mut [&mut Member]) {
    for m in members.iter_mut() {
        m.stop().await;
    }
}

/// A UDP port free on both IPv4 and IPv6, never handed out twice in this
/// process: the OS may offer a port again once our probe lets go of it,
/// before the engine that got it first has bound it.
fn free_port() -> u16 {
    static GIVEN: Mutex<Vec<u16>> = Mutex::new(Vec::new());
    loop {
        let v4 = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        let port = v4.local_addr().unwrap().port();
        let mut given = GIVEN.lock().unwrap();
        if !given.contains(&port) && std::net::UdpSocket::bind(("::", port)).is_ok() {
            given.push(port);
            return port;
        }
    }
}

fn refused_because(reply: &Result<Response, EngineError>, words: &str) -> bool {
    matches!(reply, Err(EngineError::Refused(why)) if why.contains(words))
}

// ---- Scenarios ----

#[tokio::test(flavor = "multi_thread")]
async fn two_viewers_each_get_a_stream_of_their_own() {
    let (mut a, mut b, mut c, room) = three().await;
    let Response::Sources(sources) = a.handle(Command::ListSources).await.unwrap() else {
        panic!("sources");
    };
    assert_eq!(sources.windows[0].title, "Celeste");
    a.go_live(&room, 4).await;

    let b_window = watching(&room, &a, &b).await;
    let c_window = watching(&room, &a, &c).await;
    assert_eq!(*b_window.first_was_key.lock().unwrap(), Some(true));
    assert_eq!(*c_window.first_was_key.lock().unwrap(), Some(true));
    let (b_frames, c_frames) = (b_window.frames(), c_window.frames());
    wait("frames keep coming to both", || {
        b_window.frames() > b_frames + 5 && c_window.frames() > c_frames + 5
    })
    .await;

    let (bid, cid) = (b.id(), c.id());
    a.wait_session(&room, &a, "two viewers", |s| {
        s.is_some_and(|s| s.viewers == [bid.clone(), cid.clone()] && s.role == SessionRole::Host)
    })
    .await;
    // Everyone in the room sees who watches.
    c.wait_session(&room, &a, "two viewers, from C", |s| {
        s.is_some_and(|s| s.viewers.len() == 2 && s.role == SessionRole::Watching)
    })
    .await;
    // Asking again changes nothing.
    assert!(matches!(b.watch(&room, &a).await, Ok(Response::Room(_))));
    // The HUD knows the session.
    wait("a HUD", || b_window.hud.lock().unwrap().is_some()).await;
    let hud = b_window.hud.lock().unwrap().clone().unwrap();
    assert_eq!((hud.title.as_str(), hud.host.as_str()), ("Celeste", "Ana"));
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// Our relay (M7), with its token, carrying everything: neither member has
/// a direct path, yet B watches A's stream and gets the seat.
#[tokio::test(flavor = "multi_thread")]
async fn watching_and_playing_through_our_relay_alone() {
    const TOKEN: &str = "couch-test-token";
    let relay = TestRelay::start(Some(TOKEN)).await;
    let through = |options: EngineOptions| EngineOptions {
        relays: RelayChoice::Custom {
            urls: vec![relay.url.parse().unwrap()],
            token: RelayToken::new(TOKEN),
        },
        relay_only: true,
        ..options
    };
    let mut a = Member::with("Ana", through).await;
    let mut b = Member::with("Bia", through).await;
    let Response::Room(room) = a
        .handle(Command::CreateRoom {
            name: "Sofá".into(),
            password: PASSWORD.into(),
        })
        .await
        .unwrap()
    else {
        panic!("a room");
    };
    let room = room.id;
    let Response::Invite { link } = a
        .handle(Command::GetInvite { room: room.clone() })
        .await
        .unwrap()
    else {
        panic!("an invite");
    };
    b.ok(Command::JoinRoom {
        invite: link,
        password: PASSWORD.into(),
    })
    .await;
    let bid = b.id();
    let deadline = Instant::now() + WAIT;
    while !a
        .room(&room)
        .await
        .unwrap()
        .members
        .iter()
        .any(|m| m.id == bid && matches!(m.link, MemberLink::Relay))
    {
        assert!(Instant::now() < deadline, "no relayed link to B");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    a.go_live(&room, 4).await;
    let window = watching(&room, &a, &b).await;
    give_control(&room, &mut a, &b, &window).await;
    assert_eq!(relay.refused(), 0);
    stop_all(&mut [&mut a, &mut b]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_session_turns_one_away_and_one_stream_at_a_time() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 1).await;
    watching(&room, &a, &b).await;
    c.wait_session(&room, &a, "the announcement", |s| s.is_some())
        .await;
    let reply = c.watch(&room, &a).await;
    assert!(refused_because(&reply, "all the viewers"), "{reply:?}");

    // One session per profile, never while watching, and keyboard and mouse
    // only with a window streamed.
    assert!(refused_because(
        &a.handle(go_live(&room, 1)).await,
        "session already"
    ));
    assert!(refused_because(
        &b.handle(go_live(&room, 1)).await,
        "stop watching first"
    ));
    let both = setup(1, KeyboardMouseSeat::WithController1);
    assert!(matches!(
        c.handle(go_live_with(&room, 1, both)).await,
        Ok(Response::Room(_))
    ));
    let screen = Command::StartStream {
        source: SourceChoice::Screen { id: "1".into() },
        name: "Desktop".into(),
        fps: 30,
        max_mbps: 20,
        upload_mbps: None,
    };
    assert!(refused_because(&c.handle(screen).await, "need a window"));
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn control_is_asked_for_given_passed_and_given_back() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    let c_window = watching(&room, &a, &c).await;

    give_control(&room, &mut a, &b, &b_window).await;
    b_window.pad(PadButtons::A);
    wait("B's button on A's pad", || {
        a.fake.pad() == Some(pressed(PadButtons::A))
    })
    .await;
    // C watches but doesn't hold the seat: its pad goes nowhere.
    c_window.pad(PadButtons::B);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.fake.pad(), Some(pressed(PadButtons::A)));

    // A passes control to C: B's hold ends, released first.
    a.ok(Command::PassControl {
        member: c.id(),
        seat: 0,
        controller: None,
    })
    .await;
    wait("C's seat", || c_window.holding()).await;
    wait("B's seat gone", || !b_window.holding()).await;
    wait("C's button", || {
        a.fake.pad() == Some(pressed(PadButtons::B))
    })
    .await;
    let states = a.fake.pads.plugged.lock().unwrap()[0].states();
    let a_up = states
        .iter()
        .rposition(|s| *s == pressed(PadButtons::A))
        .unwrap();
    let b_down = states
        .iter()
        .rposition(|s| *s == pressed(PadButtons::B))
        .unwrap();
    assert!(
        states[a_up..b_down].contains(&PadState::NEUTRAL),
        "neutral between holders: {states:?}"
    );
    // B's input no longer counts.
    b_window.pad(PadButtons::X);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.fake.pad(), Some(pressed(PadButtons::B)));

    // C gives it back from the viewer: everything released.
    c_window.action(UserAction::CancelControl);
    wait("C's seat gone", || !c_window.holding()).await;
    wait("the pad neutral", || {
        a.fake.pad() == Some(PadState::NEUTRAL)
    })
    .await;
    a.wait_session(&room, &a, "the host playing", |s| {
        s.is_some_and(|s| holder(s, 0).is_none())
    })
    .await;

    // A takes it back from someone it passed it to.
    a.ok(Command::PassControl {
        member: b.id(),
        seat: 0,
        controller: None,
    })
    .await;
    wait("B's seat again", || b_window.holding()).await;
    a.ok(Command::TakeBackControl { seat: None }).await;
    wait("B's seat gone again", || !b_window.holding()).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declined_viewer_cannot_ask_again_at_once() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    b_window.action(UserAction::RequestControl {
        seat: 0,
        controller: None,
    });
    a.event("a request", |e| {
        matches!(e, Event::ControlRequested { .. }).then_some(())
    })
    .await;
    a.ok(Command::AnswerControl {
        member: b.id(),
        accept: false,
    })
    .await;
    b.wait_session(&room, &a, "B watching again", |s| {
        s.is_some_and(|s| s.role == SessionRole::Watching)
    })
    .await;
    wait("the HUD's note", || {
        b_window
            .hud
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|h| h.note.contains("said no"))
    })
    .await;

    // Straight away: declined again, and A never hears of it.
    b.ok(Command::RequestControl {
        seat: 0,
        controller: None,
    })
    .await;
    b.wait_session(&room, &a, "the second request declined", |s| {
        s.is_some_and(|s| s.role == SessionRole::Watching)
    })
    .await;
    let requests = a.session(&room, &a).await.unwrap().requests;
    assert!(requests.is_empty(), "{requests:?}");
    assert!(!b_window.holding());
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// If the answer keys can't be hooked, the panic key still is, alone.
#[tokio::test(flavor = "multi_thread")]
async fn the_panic_key_never_waits_on_the_answer_keys() {
    let (mut a, mut b, mut c, room) = three().await;
    a.fake.answer_keys_fail.store(true, Ordering::SeqCst);
    a.go_live(&room, 4).await;
    assert_eq!(
        *a.fake.host_keys_on.lock().unwrap(),
        [(HidUsage(0x2A), Shortcut::Panic)]
    );
    let view = a.session(&room, &a).await.unwrap();
    assert_eq!(view.panic_key.as_deref(), Some("Ctrl+Alt+Shift+Backspace"));
    assert_eq!(view.answer_keys, None, "the app answers in its window then");
    assert!(a.fake.press_panic_key());
    a.wait_session(&room, &a, "control paused", |s| {
        s.is_some_and(|s| s.control_paused)
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// The host answers from inside the game (M8): Accept and Decline answer
/// the oldest request, but never one that just arrived.
#[tokio::test(flavor = "multi_thread")]
async fn the_hosts_keys_answer_the_oldest_request() {
    let (mut a, mut b, mut c, room) = three().await;
    // Accept moved to K: the hook gets the moved key with the others.
    a.ok(Command::SetShortcut {
        id: "acceptRequest".into(),
        code: "KeyK".into(),
    })
    .await;
    a.go_live(&room, 4).await;
    let keys = a.fake.host_keys_on.lock().unwrap().clone();
    assert_eq!(
        keys,
        [
            (HidUsage(0x2A), Shortcut::Panic),
            (HidUsage(0x0E), Shortcut::AcceptRequest),
            (HidUsage(0x11), Shortcut::DeclineRequest),
        ]
    );
    let view = a.session(&room, &a).await.unwrap();
    let answer = view.answer_keys.expect("the answer keys while live");
    assert_eq!(
        (answer.accept.as_str(), answer.decline.as_str()),
        ("Ctrl+Alt+Shift+K", "Ctrl+Alt+Shift+N")
    );
    let b_window = watching(&room, &a, &b).await;
    let c_window = watching(&room, &a, &c).await;

    // Nobody asks: nothing happens.
    assert!(a.fake.press_host_key(Shortcut::AcceptRequest));

    // B asks, and the key comes at once: too new to have been seen.
    b_window.action(UserAction::RequestControl {
        seat: 0,
        controller: None,
    });
    a.event("B's request", |e| {
        matches!(e, Event::ControlRequested { .. }).then_some(())
    })
    .await;
    assert!(a.fake.press_host_key(Shortcut::AcceptRequest));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!b_window.holding(), "a request just in isn't answered");
    c_window.action(UserAction::RequestControl {
        seat: 0,
        controller: None,
    });
    a.wait_session(&room, &a, "both asking", |s| {
        s.is_some_and(|s| s.requests.len() == 2)
    })
    .await;

    // Once seen: Decline answers the oldest (B).
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(a.fake.press_host_key(Shortcut::DeclineRequest));
    b.wait_session(&room, &a, "B declined", |s| {
        s.is_some_and(|s| s.role == SessionRole::Watching)
    })
    .await;
    let requests = a.session(&room, &a).await.unwrap().requests;
    assert_eq!(
        requests
            .iter()
            .map(|r| r.member.clone())
            .collect::<Vec<_>>(),
        [c.id()]
    );
    // C has asked for over a second, but is first only now: a key pressed
    // for B a moment late doesn't take C instead.
    assert!(a.fake.press_host_key(Shortcut::AcceptRequest));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !c_window.holding(),
        "a request just moved to the front isn't answered"
    );
    // Once C has been first for a second, Accept answers C.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(a.fake.press_host_key(Shortcut::AcceptRequest));
    wait("C on the seat", || c_window.holding()).await;
    assert!(!b_window.holding());
    a.wait_session(&room, &a, "C on seat 0, nobody asking", |s| {
        s.is_some_and(|s| holder(s, 0) == Some(c.id()) && s.requests.is_empty())
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_panic_key_takes_control_back_until_allowed() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    give_control(&room, &mut a, &b, &b_window).await;
    b_window.pad(PadButtons::A);
    wait("B's button", || {
        a.fake.pad() == Some(pressed(PadButtons::A))
    })
    .await;

    assert_eq!(
        a.session(&room, &a).await.unwrap().panic_key.as_deref(),
        Some("Ctrl+Alt+Shift+Backspace")
    );
    // Pressed on the keyboard: the hook, not the app.
    assert!(a.fake.press_panic_key(), "the hook is in place while live");
    wait("the pad neutral", || {
        a.fake.pad() == Some(PadState::NEUTRAL)
    })
    .await;
    wait("B's seat gone", || !b_window.holding()).await;
    a.wait_session(&room, &a, "control paused", |s| {
        s.is_some_and(|s| s.control_paused && holder(s, 0).is_none())
    })
    .await;
    b.wait_session(&room, &a, "B hears control is paused", |s| {
        s.is_some_and(|s| s.control_paused)
    })
    .await;
    // B's held button stays released, and asking is declined.
    b_window.pad(PadButtons::A | PadButtons::B);
    b.ok(Command::RequestControl {
        seat: 0,
        controller: None,
    })
    .await;
    b.wait_session(&room, &a, "B's request declined", |s| {
        s.is_some_and(|s| s.role == SessionRole::Watching)
    })
    .await;
    assert_eq!(a.fake.pad(), Some(PadState::NEUTRAL));
    assert!(matches!(
        a.handle(Command::PassControl {
            member: b.id(),
            seat: 0,
            controller: None,
        })
        .await,
        Err(EngineError::Session(_))
    ));

    a.ok(Command::AllowControl).await;
    b.wait_session(&room, &a, "control allowed again", |s| {
        s.is_some_and(|s| !s.control_paused)
    })
    .await;
    give_control(&room, &mut a, &b, &b_window).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_closing_its_window_while_playing_is_released_at_once() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    give_control(&room, &mut a, &b, &b_window).await;
    b_window.pad(PadButtons::Y);
    wait("B's button", || {
        a.fake.pad() == Some(pressed(PadButtons::Y))
    })
    .await;

    let closed = Instant::now();
    b_window.close();
    wait("the pad neutral", || {
        a.fake.pad() == Some(PadState::NEUTRAL)
    })
    .await;
    let released = closed.elapsed();
    a.wait_session(&room, &a, "B gone from the viewers", |s| {
        s.is_some_and(|s| s.viewers.is_empty() && holder(s, 0).is_none())
    })
    .await;
    b.wait_session(&room, &a, "B not watching", |s| {
        s.is_some_and(|s| s.role == SessionRole::None)
    })
    .await;
    // Through the session, not the dead-man timer: B's last heartbeat went
    // out at most 100 ms before the close, so the timer can't fire before
    // about 200 ms after it. The session takes a few ms on loopback.
    assert!(released < Duration::from_millis(150), "{released:?}");
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ban_while_watching_ends_it() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    give_control(&room, &mut a, &b, &b_window).await;
    b_window.pad(PadButtons::A);
    wait("B's button", || {
        a.fake.pad() == Some(pressed(PadButtons::A))
    })
    .await;

    a.ok(Command::BanMember {
        room: room.clone(),
        member: b.id(),
    })
    .await;
    // Dropped in the same step as the ban, not when B's connection finally
    // closes (which B could delay), and released before B's heartbeats
    // could stop and let the dead-man timer do it.
    let banned = Instant::now();
    let session = a.session(&room, &a).await.unwrap();
    assert!(session.viewers.is_empty(), "{session:?}");
    wait("the pad neutral", || {
        a.fake.pad() == Some(PadState::NEUTRAL)
    })
    .await;
    assert!(
        banned.elapsed() < Duration::from_millis(100),
        "{:?}",
        banned.elapsed()
    );
    b.event("the ban", |e| {
        matches!(e, Event::RoomRemoved { .. }).then_some(())
    })
    .await;
    wait("B's viewer closed", || {
        b_window.shut_down.load(Ordering::SeqCst)
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_leaving_the_room_ends_its_stream() {
    let (mut a, mut b, mut c, room) = three().await;
    c.go_live(&room, 4).await;
    let b_window = watching(&room, &c, &b).await;
    c.ok(Command::LeaveRoom { room: room.clone() }).await;
    assert!(!c.fake.panic_key_hooked(), "the hook goes with the stream");
    let reason = b
        .event("the end of watching", |e| match e {
            Event::WatchEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert!(
        matches!(reason, WatchEnd::Ended | WatchEnd::ConnectionLost),
        "{reason:?}"
    );
    b.wait_session(&room, &c, "C's session gone", |s| s.is_none())
        .await;
    wait("B's viewer closed", || {
        b_window.shut_down.load(Ordering::SeqCst)
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M19: the window closing ends the stream after the grace, not the session:
/// the viewer stays, and hears why.
#[tokio::test(flavor = "multi_thread")]
async fn the_window_closing_ends_the_stream_after_the_grace() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let window = watching(&room, &a, &b).await;
    a.fake.close_window();
    let closed = Instant::now();
    let reason = a
        .event("the stream's end", |e| match e {
            Event::StreamEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(reason, SessionEnd::SourceLost);
    // The game still runs: it had the grace (500 ms here) to show a window.
    assert!(
        closed.elapsed() >= Duration::from_millis(450),
        "ended after {:?}",
        closed.elapsed()
    );
    assert!(a.fake.panic_key_hooked(), "the hook stays with the session");
    let stopped = StreamStateView::Stopped(SessionEnd::SourceLost);
    b.wait_session(&room, &a, "the stream stopped", |s| {
        s.is_some_and(|s| s.stream == stopped)
    })
    .await;
    wait("the viewer's HUD says so", || {
        window
            .hud()
            .is_some_and(|h| h.stream == StreamState::Stopped(EndReason::SourceLost))
    })
    .await;
    assert!(!window.shut_down.load(Ordering::SeqCst), "still watching");
    a.wait_session(&room, &a, "the session goes on", |s| {
        s.is_some_and(|s| s.stream == stopped)
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_game_swapping_its_window_keeps_streaming() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let window = watching(&room, &a, &b).await;
    a.fake.swap_window(WINDOW + 1);
    // Well past the grace: frames keep coming, from the new window.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let before = window.frames();
    wait("frames from the new window", || {
        window.frames() > before + 5
    })
    .await;
    assert!(a.session(&room, &a).await.is_some(), "still live");
    assert!(b.session(&room, &a).await.is_some(), "still listed");
    // The window it follows can go too: the game exiting ends the stream.
    a.fake.exit_game();
    let reason = a
        .event("the stream's end", |e| match e {
            Event::StreamEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(reason, SessionEnd::GameExited);
    assert!(a.session(&room, &a).await.is_some(), "the session goes on");
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M9b: GPU frames reach encoders that read them as they are, and a new
/// texture pool (the game's window swapped: a new capture device) reopens
/// those encoders, with a keyframe.
#[tokio::test(flavor = "multi_thread")]
async fn gpu_frames_reach_the_encoders_and_a_new_pool_reopens_them() {
    let (mut a, mut b, mut c, room) = three().await;
    let gpu = &a.fake.gpu;
    gpu.capture_on_gpu.store(true, Ordering::SeqCst);
    gpu.encoders_take_it.store(true, Ordering::SeqCst);
    gpu.frames.source.store(1, Ordering::SeqCst);
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    let c_window = watching(&room, &a, &c).await;
    assert_eq!(*gpu.opened.lock().unwrap(), [Some(1), Some(1)]);

    let keyframes = b_window.keyframes();
    gpu.frames.source.store(2, Ordering::SeqCst);
    wait("both encoders reopened on the new pool", || {
        gpu.opened.lock().unwrap()[2..] == [Some(2), Some(2)]
    })
    .await;
    wait("a keyframe from the reopened encoder", || {
        b_window.keyframes() > keyframes
    })
    .await;
    let (b_frames, c_frames) = (b_window.frames(), c_window.frames());
    wait("frames keep coming", || {
        b_window.frames() > b_frames + 5 && c_window.frames() > c_frames + 5
    })
    .await;
    assert_eq!(
        gpu.opened.lock().unwrap().len(),
        4,
        "no reopening without a new pool"
    );
    assert_eq!(
        gpu.frames.downloads.load(Ordering::SeqCst),
        0,
        "nothing copied to memory"
    );
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// Whether `COUCH_HOST_MEMORY_PACING=0` or `COUCH_HOST_PACING=0` is set, left
/// over from measuring against the old memory path (M12a).
fn memory_pacing_off() -> bool {
    ["COUCH_HOST_MEMORY_PACING", "COUCH_HOST_PACING"]
        .iter()
        .any(|v| std::env::var(v).is_ok_and(|v| v.trim() == "0"))
}

/// M9b and M12a: when the encoder can't read GPU frames (not NVIDIA, or
/// another GPU than the capture's), frames reach the encoders in memory.
/// The capture stays on the GPU, paced to the stream's rate, and only the
/// frames sent are read back, once each for every viewer: a 240 Hz capture
/// into a 30 fps stream reads back about 30 a second, not 240, nor 60 for
/// two viewers.
#[tokio::test(flavor = "multi_thread")]
async fn encoders_that_cant_read_gpu_frames_get_paced_frames_read_back() {
    if memory_pacing_off() {
        eprintln!("the old memory path is switched on: skipped");
        return;
    }
    let (mut a, mut b, mut c, room) = three().await;
    let gpu = &a.fake.gpu;
    gpu.capture_on_gpu.store(true, Ordering::SeqCst);
    a.fake.capture_fps.store(240, Ordering::SeqCst);
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    let c_window = watching(&room, &a, &c).await;
    // B's encoder was offered a GPU frame and couldn't take it; C's took
    // frames in memory from the start.
    assert_eq!(*gpu.opened.lock().unwrap(), [None, None]);
    wait("frames to both", || {
        b_window.frames() > 10 && c_window.frames() > 10
    })
    .await;
    let (downloads, b_frames, started) = (
        gpu.frames.downloads.load(Ordering::SeqCst),
        b_window.frames(),
        Instant::now(),
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let secs = started.elapsed().as_secs_f64();
    let read_back = f64::from(gpu.frames.downloads.load(Ordering::SeqCst) - downloads) / secs;
    let b_rate = (b_window.frames() - b_frames) as f64 / secs;
    // Wide bounds, so a busy machine can't fail it.
    assert!(
        (15.0..=45.0).contains(&read_back),
        "{read_back:.1} read back a second"
    );
    assert!(
        (15.0..=45.0).contains(&b_rate),
        "{b_rate:.1} frames a second"
    );
    // Never captured again in memory: the GPU capture stays, paced.
    let captures = a.fake.captures.lock().unwrap().clone();
    assert!(
        captures.iter().all(|r| !r.in_memory && r.paced),
        "{captures:?}"
    );
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M12a: a slow read-back (a GPU busy with a game: 8 ms each, on the
/// capture thread) still keeps the stream at its rate: a 240 Hz capture into
/// a 30 fps stream still gives about 30.
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_read_back_keeps_the_streams_rate() {
    if memory_pacing_off() {
        eprintln!("the old memory path is switched on: skipped");
        return;
    }
    let (mut a, mut b, mut c, room) = three().await;
    let gpu = &a.fake.gpu;
    gpu.capture_on_gpu.store(true, Ordering::SeqCst);
    gpu.frames.download_ms.store(8, Ordering::SeqCst);
    a.fake.capture_fps.store(240, Ordering::SeqCst);
    a.go_live(&room, 4).await;
    let window = watching(&room, &a, &b).await;
    wait("frames", || window.frames() > 10).await;
    let (frames, started) = (window.frames(), Instant::now());
    tokio::time::sleep(Duration::from_secs(2)).await;
    let rate = (window.frames() - frames) as f64 / started.elapsed().as_secs_f64();
    assert!((15.0..=45.0).contains(&rate), "{rate:.1} frames a second");
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M12a: reading frames back after pacing that keeps failing turns the
/// capture to memory, as before, and the viewers keep watching.
#[tokio::test(flavor = "multi_thread")]
async fn failing_read_backs_turn_the_capture_to_memory() {
    if memory_pacing_off() {
        eprintln!("the old memory path is switched on: skipped");
        return;
    }
    let (mut a, mut b, mut c, room) = three().await;
    let gpu = &a.fake.gpu;
    gpu.capture_on_gpu.store(true, Ordering::SeqCst);
    a.go_live(&room, 4).await;
    let window = watching(&room, &a, &b).await;
    gpu.frames.download_fails.store(true, Ordering::SeqCst);
    wait("captured again in memory", || {
        a.fake
            .captures
            .lock()
            .unwrap()
            .last()
            .is_some_and(|r| r.in_memory)
    })
    .await;
    let frames = window.frames();
    wait("frames keep coming", || window.frames() > frames + 5).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M9d: a capture faster than the stream (a 240 Hz screen) is paced to the
/// stream's rate: the viewer gets about 30 frames a second, not 240.
#[tokio::test(flavor = "multi_thread")]
async fn a_capture_faster_than_the_stream_is_paced_to_its_rate() {
    // Left over from measuring against the old path: nothing to check.
    if std::env::var("COUCH_HOST_PACING").is_ok_and(|v| v.trim() == "0") {
        eprintln!("COUCH_HOST_PACING=0 is set: pacing is off, skipped");
        return;
    }
    let (mut a, mut b, mut c, room) = three().await;
    a.fake.capture_fps.store(240, Ordering::SeqCst);
    a.go_live(&room, 4).await;
    let window = watching(&room, &a, &b).await;
    wait("frames", || window.frames() > 10).await;
    let (frames, started) = (window.frames(), Instant::now());
    tokio::time::sleep(Duration::from_secs(2)).await;
    let rate = (window.frames() - frames) as f64 / started.elapsed().as_secs_f64();
    // Wide bounds, so a busy machine can't fail it: unpaced, it would be
    // near 240.
    assert!((15.0..=45.0).contains(&rate), "{rate:.1} frames a second");
    assert!(
        a.fake
            .captures
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.paced && r.fps == 30)
    );
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M12c: each viewer gets the best codec it decodes. B decodes HEVC and gets
/// it; C doesn't and gets H.264, in the same session.
#[tokio::test(flavor = "multi_thread")]
async fn each_viewer_gets_the_best_codec_it_decodes() {
    let (mut a, mut b, mut c, room) = three().await;
    a.fake.gpu.capture_on_gpu.store(true, Ordering::SeqCst);
    a.fake.gpu.encoders_take_it.store(true, Ordering::SeqCst);
    b.fake.decodes_hevc.store(true, Ordering::SeqCst);
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    let c_window = watching(&room, &a, &c).await;
    wait("frames in each viewer's codec", || {
        b_window.codec() == Some(Codec::Hevc) && c_window.codec() == Some(Codec::H264)
    })
    .await;
    assert_eq!(*a.fake.codecs.lock().unwrap(), [Codec::Hevc, Codec::H264]);
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M12c: a host whose HEVC encoder won't open streams H.264 to a viewer that
/// decodes HEVC, rather than lose it; and its other viewers keep GPU frames.
#[tokio::test(flavor = "multi_thread")]
async fn hevc_that_wont_open_falls_back_to_h264() {
    let (mut a, mut b, mut c, room) = three().await;
    let gpu = &a.fake.gpu;
    gpu.capture_on_gpu.store(true, Ordering::SeqCst);
    gpu.encoders_take_it.store(true, Ordering::SeqCst);
    gpu.frames.source.store(1, Ordering::SeqCst);
    a.fake.hevc_fails.store(true, Ordering::SeqCst);
    b.fake.decodes_hevc.store(true, Ordering::SeqCst);
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    wait("B's frames in H.264", || {
        b_window.codec() == Some(Codec::H264)
    })
    .await;
    assert_eq!(*a.fake.codecs.lock().unwrap(), [Codec::Hevc, Codec::H264]);
    let c_window = watching(&room, &a, &c).await;
    wait("C's frames", || c_window.frames() > 0).await;
    // HEVC's failure didn't turn GPU frames off: both encoders read them.
    assert_eq!(*gpu.opened.lock().unwrap(), [Some(1), Some(1)]);
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M12c: with frames in memory (a test pattern, `COUCH_HOST_ZERO_COPY=0`),
/// HEVC isn't even tried: a viewer that decodes it gets H.264.
#[tokio::test(flavor = "multi_thread")]
async fn no_hevc_with_frames_in_memory() {
    let (mut a, mut b, mut c, room) = three().await;
    b.fake.decodes_hevc.store(true, Ordering::SeqCst);
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    wait("B's frames", || b_window.frames() > 0).await;
    assert_eq!(b_window.codec(), Some(Codec::H264));
    assert_eq!(*a.fake.codecs.lock().unwrap(), [Codec::H264]);
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M12c: an HEVC encoder that opens but can't read the GPU frames (another
/// GPU's, as on a laptop) gives way to H.264 for that viewer.
#[tokio::test(flavor = "multi_thread")]
async fn hevc_that_cant_read_gpu_frames_gives_h264() {
    let (mut a, mut b, mut c, room) = three().await;
    a.fake.gpu.capture_on_gpu.store(true, Ordering::SeqCst);
    b.fake.decodes_hevc.store(true, Ordering::SeqCst);
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    wait("B's frames in H.264", || {
        b_window.codec() == Some(Codec::H264)
    })
    .await;
    assert_eq!(*a.fake.codecs.lock().unwrap(), [Codec::Hevc, Codec::H264]);
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M12c: HEVC failing mid-stream gives that viewer H.264 from a keyframe,
/// and it keeps watching (here GPU encoding keeps failing, so H.264 then
/// goes to frames in memory, as in M9b).
#[tokio::test(flavor = "multi_thread")]
async fn hevc_failing_mid_stream_falls_back_to_h264() {
    let (mut a, mut b, mut c, room) = three().await;
    let gpu = &a.fake.gpu;
    gpu.capture_on_gpu.store(true, Ordering::SeqCst);
    gpu.encoders_take_it.store(true, Ordering::SeqCst);
    b.fake.decodes_hevc.store(true, Ordering::SeqCst);
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    wait("B's frames in HEVC", || {
        b_window.codec() == Some(Codec::Hevc)
    })
    .await;
    let keyframes = b_window.keyframes();
    gpu.fail.store(true, Ordering::SeqCst);
    wait("B's frames in H.264", || {
        b_window.codec() == Some(Codec::H264)
    })
    .await;
    let frames = b_window.frames();
    wait("B still watching", || b_window.frames() > frames + 10).await;
    assert!(
        b_window.keyframes() > keyframes,
        "started again from a keyframe"
    );
    assert_eq!(
        a.fake.codecs.lock().unwrap()[..2],
        [Codec::Hevc, Codec::H264]
    );
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

// ---- Sessions before streams (M19) ----

/// `viewer` watches `host`'s session, streamed or not: its window opens,
/// without waiting for frames.
async fn joined(room: &str, host: &Member, viewer: &Member) -> FakeViewer {
    viewer
        .wait_session(room, host, "the announcement", |s| s.is_some())
        .await;
    let reply = viewer.watch(room, host).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    viewer.fake.viewer().expect("a viewer window")
}

/// M19: a session starts with nothing streamed. Its pads are plugged in at
/// once; viewers join, see that nothing is streamed, and get no frames.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_starts_with_nothing_streamed() {
    let (mut a, mut b, mut c, room) = three().await;
    let reply = a.handle(go_live(&room, 4)).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    assert_eq!(a.fake.pads_plugged(), 1, "the pad is there before any game");
    assert!(a.fake.panic_key_hooked(), "and the panic key");
    a.wait_session(&room, &a, "nothing streamed", |s| {
        s.is_some_and(|s| s.stream == StreamStateView::Waiting && s.streaming.is_none())
    })
    .await;
    let window = joined(&room, &a, &b).await;
    wait("the HUD says nothing is streamed", || {
        window
            .hud()
            .is_some_and(|h| h.stream == StreamState::Waiting)
    })
    .await;
    b.wait_session(&room, &a, "B sees it too", |s| {
        s.is_some_and(|s| s.stream == StreamStateView::Waiting)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(window.frames(), 0, "no frames without a stream");
    assert!(
        a.fake.captures.lock().unwrap().is_empty(),
        "nothing captured"
    );

    // Streaming starts: the viewer that waited gets its first keyframe.
    a.ok(stream_window()).await;
    wait("frames", || window.frames() >= 5).await;
    assert_eq!(*window.first_was_key.lock().unwrap(), Some(true));
    a.wait_session(&room, &a, "live", |s| {
        s.is_some_and(|s| s.stream == StreamStateView::Live && s.streaming.is_some())
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M19: a stream stops and starts again in the session: the viewer stays,
/// keeps its seat, and the next stream reaches it (a later epoch, from a
/// keyframe), in the codec chosen afresh.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_stops_and_starts_again_with_the_viewers_staying() {
    let (mut a, mut b, mut c, room) = three().await;
    let gpu = &a.fake.gpu;
    gpu.capture_on_gpu.store(true, Ordering::SeqCst);
    gpu.encoders_take_it.store(true, Ordering::SeqCst);
    b.fake.decodes_hevc.store(true, Ordering::SeqCst);
    a.go_live(&room, 4).await;
    let window = watching(&room, &a, &b).await;
    give_control(&room, &mut a, &b, &window).await;

    a.ok(Command::StopStream).await;
    let stopped = StreamStateView::Stopped(SessionEnd::HostStopped);
    b.wait_session(&room, &a, "the stream stopped", |s| {
        s.is_some_and(|s| s.stream == stopped)
    })
    .await;
    wait("the HUD says so", || {
        window
            .hud()
            .is_some_and(|h| h.stream == StreamState::Stopped(EndReason::HostStopped))
    })
    .await;
    // Frames stop; the viewer and its seat stay.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let frames = window.frames();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(window.frames(), frames, "no frames between streams");
    assert!(window.holding(), "the seat stays");
    assert!(!window.shut_down.load(Ordering::SeqCst));
    assert!(a.fake.panic_key_hooked());

    let keyframes = window.keyframes();
    a.ok(stream_window()).await;
    wait("the second stream's frames", || {
        window.frames() > frames + 5
    })
    .await;
    assert!(window.keyframes() > keyframes, "from a keyframe");
    assert!(window.holding());
    assert_eq!(
        *a.fake.codecs.lock().unwrap(),
        [Codec::Hevc, Codec::Hevc],
        "the codec chosen again for the new stream"
    );
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M19: stopping the stream releases at once what a guest held (keys and
/// pad), keeps their seat, and drops their input until the next stream,
/// which takes it again.
#[tokio::test(flavor = "multi_thread")]
async fn between_streams_a_guests_input_is_released_and_held_off() {
    use couch_input::HidUsage;
    use couch_testkit::KbmCall;

    let (mut a, mut b, mut c, room) = three().await;
    let both = setup(1, KeyboardMouseSeat::WithController1);
    let reply = a.live(go_live_with(&room, 4, both)).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    let window = watching(&room, &a, &b).await;
    give_control(&room, &mut a, &b, &window).await;
    let typed = |key: u16, down: bool| {
        a.fake
            .kbm
            .calls()
            .iter()
            .filter(|c| **c == KbmCall::Key(HidUsage(key), down))
            .count()
    };
    window.key(0x1A, true);
    window.pad(PadButtons::A);
    wait("B's W and A", || {
        typed(0x1A, true) == 1 && a.fake.pad() == Some(pressed(PadButtons::A))
    })
    .await;

    a.ok(Command::StopStream).await;
    wait("B's W released", || typed(0x1A, false) == 1).await;
    wait("the pad neutral", || {
        a.fake.pad() == Some(PadState::NEUTRAL)
    })
    .await;
    // Held off: nothing B sends reaches the game, however long.
    window.pad(PadButtons::B);
    window.key(0x04, true);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(a.fake.pad(), Some(PadState::NEUTRAL));
    assert_eq!(typed(0x04, true), 0);
    assert!(window.holding(), "the seat stays");

    // The next stream takes B's input again.
    a.ok(stream_window()).await;
    window.pad(PadButtons::X);
    wait("B's X on the pad", || {
        a.fake.pad() == Some(pressed(PadButtons::X))
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M19: the host's own controller plays its seat with nothing streamed: the
/// host may start the game with it.
#[tokio::test(flavor = "multi_thread")]
async fn the_hosts_controller_plays_between_streams() {
    let (mut a, mut b, mut c, room) = three().await;
    let reply = a
        .handle(go_live_playing(
            &room,
            4,
            InputSetup::default(),
            vec![dualsense()],
        ))
        .await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    assert!(a.fake.hidden(), "hidden before any game starts");
    a.fake.local_press(PadButtons::Y);
    wait("the host's Y with nothing streamed", || {
        a.fake.pad() == Some(pressed(PadButtons::Y))
    })
    .await;
    a.ok(Command::EndSession).await;
    wait("shown again", || !a.fake.hidden()).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M19: a stop asked for while a stream starts stops that stream: nothing is
/// streamed in the end, and the viewer gets no frames.
#[tokio::test(flavor = "multi_thread")]
async fn stopping_while_a_stream_starts_leaves_nothing_streamed() {
    let (mut a, mut b, mut c, room) = three().await;
    a.ok(go_live(&room, 4)).await;
    let window = joined(&room, &a, &b).await;
    let (_started, stopped) =
        tokio::join!(a.handle(stream_window()), a.handle(Command::StopStream));
    assert!(stopped.is_ok(), "{stopped:?}");
    a.wait_session(&room, &a, "stopped", |s| {
        s.is_some_and(|s| s.stream == StreamStateView::Stopped(SessionEnd::HostStopped))
    })
    .await;
    // Whatever was captured meanwhile has stopped.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let frames = window.frames();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(window.frames(), frames, "nothing streamed");
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M19: a second start while one is pending is refused; the first goes on.
#[tokio::test(flavor = "multi_thread")]
async fn one_stream_starts_at_a_time() {
    let (mut a, mut b, mut c, room) = three().await;
    a.ok(go_live(&room, 4)).await;
    let window = joined(&room, &a, &b).await;
    let (first, second) = tokio::join!(a.handle(stream_window()), a.handle(stream_window()));
    assert!(matches!(first, Ok(Response::Room(_))), "{first:?}");
    assert!(refused_because(&second, "starting already"), "{second:?}");
    wait("frames", || window.frames() >= 5).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M19: a stream whose capture fails as it starts ends, whether the failure
/// or the start's answer reaches the engine first; viewers' input stays held.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_failing_as_it_starts_ends() {
    let (mut a, mut b, mut c, room) = three().await;
    a.ok(go_live(&room, 4)).await;
    let window = joined(&room, &a, &b).await;
    give_control(&room, &mut a, &b, &window).await;
    a.fake.capture_fails.store(true, Ordering::SeqCst);
    let _ = a.handle(stream_window()).await;
    let reason = a
        .event("the stream's end", |e| match e {
            Event::StreamEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(reason, SessionEnd::Error);
    let failed = StreamStateView::Stopped(SessionEnd::Error);
    a.wait_session(&room, &a, "the stream ended", |s| {
        s.is_some_and(|s| s.stream == failed)
    })
    .await;
    // Nothing streamed: B's input stays out.
    window.pad(PadButtons::A);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_ne!(a.fake.pad(), Some(pressed(PadButtons::A)), "held off");
    // And streaming works again once the capture does.
    a.fake.capture_fails.store(false, Ordering::SeqCst);
    a.ok(stream_window()).await;
    wait("frames", || window.frames() >= 5).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M19: streaming needs a session; starting again replaces the stream.
#[tokio::test(flavor = "multi_thread")]
async fn streaming_needs_a_session() {
    let (mut a, mut b, mut c, room) = three().await;
    assert!(a.handle(stream_window()).await.is_err(), "no session yet");
    assert!(a.handle(Command::StopStream).await.is_err());
    a.ok(go_live(&room, 4)).await;
    assert!(
        refused_because(
            &a.handle(Command::StopStream).await,
            "nothing is being streamed"
        ),
        "nothing to stop"
    );
    // Streaming again replaces the stream.
    a.ok(stream_window()).await;
    a.ok(stream_window()).await;
    a.wait_session(&room, &a, "live", |s| {
        s.is_some_and(|s| s.stream == StreamStateView::Live)
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M9b: an encoder reading GPU frames that fails is reopened for frames in
/// memory, with a keyframe, and the viewer keeps watching.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_gpu_encoder_falls_back_to_frames_in_memory() {
    let (mut a, mut b, mut c, room) = three().await;
    let gpu = &a.fake.gpu;
    gpu.capture_on_gpu.store(true, Ordering::SeqCst);
    gpu.encoders_take_it.store(true, Ordering::SeqCst);
    a.go_live(&room, 4).await;
    let window = watching(&room, &a, &b).await;
    let keyframes = window.keyframes();
    gpu.fail.store(true, Ordering::SeqCst);
    wait("reopened for frames in memory", || {
        gpu.opened.lock().unwrap().last() == Some(&None)
    })
    .await;
    wait("a keyframe after the fallback", || {
        window.keyframes() > keyframes
    })
    .await;
    let frames = window.frames();
    wait("frames keep coming", || window.frames() > frames + 5).await;
    // M12a: still captured on the GPU, the frames sent read back after
    // pacing; with the old memory path, captured again in memory.
    assert_eq!(
        a.fake.captures.lock().unwrap().last().map(|r| r.in_memory),
        Some(memory_pacing_off()),
        "captured on the GPU, or in memory with the old path"
    );
    // Later encoders in this stream take frames in memory too.
    let c_window = watching(&room, &a, &c).await;
    assert_eq!(gpu.opened.lock().unwrap().last(), Some(&None));
    assert!(c_window.frames() >= 5);
    assert!(a.session(&room, &a).await.is_some(), "still live");
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M19: the game exiting ends the stream at once, and the session goes on.
#[tokio::test(flavor = "multi_thread")]
async fn the_game_exiting_ends_the_stream_at_once() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let window = watching(&room, &a, &b).await;
    a.fake.exit_game();
    let exited = Instant::now();
    let reason = a
        .event("the stream's end", |e| match e {
            Event::StreamEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(reason, SessionEnd::GameExited);
    assert!(
        exited.elapsed() < Duration::from_millis(450),
        "no grace for an exited game: {:?}",
        exited.elapsed()
    );
    let stopped = StreamStateView::Stopped(SessionEnd::GameExited);
    b.wait_session(&room, &a, "the stream stopped", |s| {
        s.is_some_and(|s| s.stream == stopped)
    })
    .await;
    assert!(!window.shut_down.load(Ordering::SeqCst), "still watching");
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_swap_while_nobody_watches_is_followed_too() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    // Nobody watches, so no capture runs: housekeeping notices.
    a.fake.swap_window(WINDOW + 1);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(a.session(&room, &a).await.is_some(), "still live");
    // Someone comes: the capture starts on the new window.
    let window = watching(&room, &a, &b).await;
    let before = window.frames();
    wait("frames", || window.frames() > before + 5).await;
    a.fake.exit_game();
    let reason = a
        .event("the stream's end", |e| match e {
            Event::StreamEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(reason, SessionEnd::GameExited);
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ending_the_session_and_starting_another() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let first = watching(&room, &a, &b).await;
    a.ok(Command::EndSession).await;
    assert!(!a.fake.panic_key_hooked(), "the hook goes with the session");
    let ended = b
        .event("the end of watching", |e| match e {
            Event::WatchEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(ended, WatchEnd::Ended);
    wait("the first viewer closed", || {
        first.shut_down.load(Ordering::SeqCst)
    })
    .await;
    b.wait_session(&room, &a, "no session", |s| s.is_none())
        .await;

    // A new session on the same connections.
    a.go_live(&room, 4).await;
    let second = watching(&room, &a, &b).await;
    assert_eq!(*second.first_was_key.lock().unwrap(), Some(true));
    b.ok(Command::StopWatching).await;
    wait("the second viewer closed", || {
        second.shut_down.load(Ordering::SeqCst)
    })
    .await;
    a.wait_session(&room, &a, "no viewers", |s| {
        s.is_some_and(|s| s.viewers.is_empty())
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutting_down_while_a_guest_plays_releases_first() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    give_control(&room, &mut a, &b, &b_window).await;
    b_window.pad(PadButtons::A);
    wait("B's button", || {
        a.fake.pad() == Some(pressed(PadButtons::A))
    })
    .await;
    let fake = a.fake.clone();
    a.stop().await;
    assert_eq!(
        fake.pad(),
        Some(PadState::NEUTRAL),
        "released before the end"
    );
    assert!(!fake.panic_key_hooked(), "the hook goes with the stream");
    let ended = b
        .event("the end of watching", |e| match e {
            Event::WatchEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert!(
        matches!(ended, WatchEnd::Ended | WatchEnd::ConnectionLost),
        "{ended:?}"
    );
    stop_all(&mut [&mut b, &mut c]).await;
}

/// The panic key also stops a grant already on its way to the input task:
/// one the host's app sent just before the key was pressed.
#[tokio::test(flavor = "multi_thread")]
async fn after_the_panic_key_no_grant_goes_through_until_allowed() {
    use couch_engine::pipeline::host::{
        HostEvent, HostOptions, HostPipeline, Rates, StreamOptions,
    };
    use couch_input::{MemberId, Permissions, SeatEpoch, SeatId};

    let fake = Arc::new(Fake::default());
    let (stream, mut events) = HostPipeline::start(
        fake.clone(),
        HostOptions {
            seats: vec![SeatKind::Controller],
            real_pad_ids: false,
            session: 1,
            first_epoch: SeatEpoch(0),
            rc_trace: None,
            netem_elapsed: None,
            replace_grace: Duration::from_millis(300),
            local: None,
            pad_setup_limit: couch_input::PAD_SETUP_LIMIT,
        },
    )
    .await;
    stream
        .start_stream(StreamOptions {
            source: Source::Window(WINDOW),
            fps: 30,
            rates: Rates::default(),
            upload_bps: None,
            audio: couch_engine::pipeline::AudioChoice::Off,
            pacing_trace: None,
        })
        .await;
    let guest = MemberId([9; 32]);
    let granted = |events: &mut mpsc::UnboundedReceiver<HostEvent>| {
        std::iter::from_fn(|| events.try_recv().ok())
            .any(|e| matches!(e, HostEvent::Granted { .. }))
    };

    stream.panic_handle().panic();
    stream.grant(SeatId(0), guest, Permissions::GAMEPAD);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!granted(&mut events), "refused while paused");

    stream.allow_grants();
    stream.grant(SeatId(0), guest, Permissions::GAMEPAD);
    let deadline = Instant::now() + WAIT;
    while !granted(&mut events) {
        assert!(Instant::now() < deadline, "granted once allowed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    stream.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crashed_stream_window_is_started_again_once() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let first = watching(&room, &a, &b).await;
    give_control(&room, &mut a, &b, &first).await;
    // The guest holds A when the window crashes.
    first.pad(PadButtons::A);
    wait("A held on the host", || {
        a.fake.pad() == Some(pressed(PadButtons::A))
    })
    .await;

    first.crash();
    wait("a new window", || b.fake.viewers.lock().unwrap().len() == 2).await;
    // What the crashed window held is let go, not repeated by the heartbeat.
    wait("the pad neutral on the host", || {
        a.fake.pad() == Some(PadState::NEUTRAL)
    })
    .await;
    let second = b.fake.viewer().expect("the new window");
    wait("frames in the new window", || second.frames() >= 3).await;
    // It is told it still holds the seat, and its input reaches the game.
    wait("the seat in the new window", || second.holding()).await;
    second.pad(PadButtons::A);
    wait("the new window's input", || {
        a.fake.pad() == Some(pressed(PadButtons::A))
    })
    .await;
    assert_eq!(
        b.session(&room, &a).await.map(|s| s.role),
        Some(SessionRole::Controlling)
    );

    // Again within a minute: watching ends, and the seat goes back.
    second.crash();
    let ended = b
        .event("the end of watching", |e| match e {
            Event::WatchEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(ended, WatchEnd::ViewerFailed);
    a.wait_session(&room, &a, "nobody playing", |s| {
        s.is_some_and(|s| holder(s, 0).is_none() && s.viewers.is_empty())
    })
    .await;
    assert_eq!(b.fake.viewers.lock().unwrap().len(), 2, "no third window");
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn watching_resumes_when_the_connection_to_the_host_comes_back() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let window = watching(&room, &a, &b).await;
    give_control(&room, &mut a, &b, &window).await;

    b.engine().drop_connection(&room, &a.id()).await;
    // The host lets go of the seat with the old connection.
    a.wait_session(&room, &a, "the seat back with the host", |s| {
        s.is_some_and(|s| holder(s, 0).is_none())
    })
    .await;
    wait("the window told it lost the seat", || !window.holding()).await;
    // The room redials; B joins again with the same window.
    b.wait_session(&room, &a, "watching again", |s| {
        s.is_some_and(|s| s.role == SessionRole::Watching)
    })
    .await;
    a.wait_session(&room, &a, "B among the viewers again", |s| {
        s.is_some_and(|s| s.viewers.contains(&b.id()))
    })
    .await;
    let before = window.frames();
    wait("frames again", || window.frames() > before + 5).await;
    assert_eq!(b.fake.viewers.lock().unwrap().len(), 1, "the same window");
    // Asking works as before.
    give_control(&room, &mut a, &b, &window).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_that_ended_meanwhile_is_not_resumed() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let window = watching(&room, &a, &b).await;
    b.engine().drop_connection(&room, &a.id()).await;
    a.ok(Command::EndSession).await;
    a.go_live(&room, 4).await;
    let ended = b
        .event("the end of watching", |e| match e {
            Event::WatchEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(ended, WatchEnd::Ended);
    wait("the window closed", || {
        window.shut_down.load(Ordering::SeqCst)
    })
    .await;
    // The new session is listed, not watched.
    b.wait_session(&room, &a, "the new session", |s| {
        s.is_some_and(|s| s.role == SessionRole::None)
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_games_other_windows_are_never_followed() {
    let (mut a, mut b, mut c, room) = three().await;
    // The game (a browser, say) has another window open all along.
    a.fake.game.others.lock().unwrap().push(99);
    a.go_live(&room, 4).await;
    watching(&room, &a, &b).await;
    // Let housekeeping note the game's windows while the captured one is up.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    a.fake.close_window();
    let reason = a
        .event("the stream's end", |e| match e {
            Event::StreamEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(
        reason,
        SessionEnd::SourceLost,
        "window 99 must not be streamed"
    );
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn watching_during_the_grace_waits_for_the_window() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    b.wait_session(&room, &a, "the announcement", |s| s.is_some())
        .await;
    // Nobody watches; the window closes, and the game shows a new one a
    // moment after someone comes to watch.
    a.fake.close_window();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let reply = b.watch(&room, &a).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    a.fake.game.window.store(WINDOW + 5, Ordering::SeqCst);
    let window = b.fake.viewer().expect("a viewer window");
    wait("frames from the new window", || window.frames() >= 5).await;
    assert!(a.session(&room, &a).await.is_some(), "still live");
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

// ---- Seats and input modes ----

const TWO_PADS: InputSetup = setup(2, KeyboardMouseSeat::None);

#[tokio::test(flavor = "multi_thread")]
async fn two_guests_play_two_pads_at_once() {
    let (mut a, mut b, mut c, room) = three().await;
    let reply = a.live(go_live_with(&room, 4, TWO_PADS)).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    assert_eq!(a.fake.pads_plugged(), 2, "one pad per seat");
    let b_window = watching(&room, &a, &b).await;
    let c_window = watching(&room, &a, &c).await;
    // Everyone sees the seats by name.
    c.wait_session(&room, &a, "two seats", |s| {
        s.is_some_and(|s| {
            s.seats.iter().map(|s| s.label.as_str()).collect::<Vec<_>>()
                == ["Controller 1", "Controller 2"]
        })
    })
    .await;

    give_seat(&room, &mut a, &b, &b_window, 0).await;
    give_seat(&room, &mut a, &c, &c_window, 1).await;
    b_window.pad(PadButtons::A);
    c_window.pad(PadButtons::B);
    wait("each guest on its own pad", || {
        a.fake.pad_at(0) == Some(pressed(PadButtons::A))
            && a.fake.pad_at(1) == Some(pressed(PadButtons::B))
    })
    .await;
    // B's next press reaches only B's pad.
    b_window.pad(PadButtons::X);
    wait("B's X", || a.fake.pad_at(0) == Some(pressed(PadButtons::X))).await;
    assert_eq!(a.fake.pad_at(1), Some(pressed(PadButtons::B)));
    // Both see who holds what.
    b.wait_session(&room, &a, "both holders", |s| {
        s.is_some_and(|s| holder(s, 0) == Some(b.id()) && holder(s, 1) == Some(c.id()))
    })
    .await;

    // Taking one seat back leaves the other playing.
    a.ok(Command::TakeBackControl { seat: Some(0) }).await;
    wait("B's seat gone", || !b_window.holding()).await;
    wait("B's pad neutral", || {
        a.fake.pad_at(0) == Some(PadState::NEUTRAL)
    })
    .await;
    assert!(c_window.holding());
    // The panic key takes every seat back.
    assert!(a.fake.press_panic_key());
    wait("C's seat gone", || !c_window.holding()).await;
    wait("C's pad neutral", || {
        a.fake.pad_at(1) == Some(PadState::NEUTRAL)
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn moving_to_another_seat_lets_go_of_the_old_one_first() {
    let (mut a, mut b, mut c, room) = three().await;
    let reply = a.live(go_live_with(&room, 4, TWO_PADS)).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    let b_window = watching(&room, &a, &b).await;
    give_seat(&room, &mut a, &b, &b_window, 0).await;
    b_window.pad(PadButtons::A);
    wait("A on pad 1", || {
        a.fake.pad_at(0) == Some(pressed(PadButtons::A))
    })
    .await;

    // B asks for the other seat and withdraws: B keeps the first.
    b.ok(Command::RequestControl {
        seat: 1,
        controller: None,
    })
    .await;
    a.wait_session(&room, &a, "B's first request", |s| {
        s.is_some_and(|s| !s.requests.is_empty())
    })
    .await;
    b.ok(Command::CancelControl).await;
    a.wait_session(&room, &a, "the request withdrawn, the seat kept", |s| {
        s.is_some_and(|s| s.requests.is_empty() && holder(s, 0) == Some(b.id()))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(b_window.holding(), "a withdraw never gives the seat back");

    // B asks for the other seat while holding the first, and gets it.
    b.ok(Command::RequestControl {
        seat: 1,
        controller: None,
    })
    .await;
    a.wait_session(&room, &a, "B's request for Controller 2", |s| {
        s.is_some_and(|s| s.requests.iter().any(|r| r.member == b.id() && r.seat == 1))
    })
    .await;
    b.wait_session(&room, &a, "B asking, still holding", |s| {
        s.is_some_and(|s| s.asked_seat == Some(1) && holder(s, 0) == Some(b.id()))
    })
    .await;
    a.ok(Command::AnswerControl {
        member: b.id(),
        accept: true,
    })
    .await;
    a.wait_session(&room, &a, "B on the second seat only", |s| {
        s.is_some_and(|s| holder(s, 0).is_none() && holder(s, 1) == Some(b.id()))
    })
    .await;
    wait("pad 1 let go", || {
        a.fake.pad_at(0) == Some(PadState::NEUTRAL)
    })
    .await;
    b_window.pad(PadButtons::Y);
    wait("B on pad 2", || {
        a.fake.pad_at(1) == Some(pressed(PadButtons::Y))
    })
    .await;
    assert_eq!(a.fake.pad_at(0), Some(PadState::NEUTRAL));
    assert!(b_window.holding());

    // The host moves B back without asking.
    a.ok(Command::PassControl {
        member: b.id(),
        seat: 0,
        controller: None,
    })
    .await;
    a.wait_session(&room, &a, "B back on the first seat", |s| {
        s.is_some_and(|s| holder(s, 0) == Some(b.id()) && holder(s, 1).is_none())
    })
    .await;
    wait("pad 2 let go", || {
        a.fake.pad_at(1) == Some(PadState::NEUTRAL)
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// Waits until the host sees `viewer` with `count` controllers (M18).
async fn host_sees_controllers(room: &str, a: &Member, viewer: &Member, count: u8) {
    a.wait_session(room, a, "the viewer's controllers", |s| {
        s.is_some_and(|s| {
            s.viewers
                .iter()
                .position(|v| *v == viewer.id())
                .and_then(|i| s.viewer_controllers.get(i))
                == Some(&count)
        })
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_with_two_controllers_plays_two_seats_and_the_keyboard() {
    use couch_input::HidUsage;
    use couch_testkit::KbmCall;

    let (mut a, mut b, mut c, room) = three().await;
    let reply = a
        .live(go_live_with(&room, 4, setup(2, KeyboardMouseSeat::OwnSeat)))
        .await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    let b_window = watching(&room, &a, &b).await;
    b_window.controllers(2);
    host_sees_controllers(&room, &a, &b, 2).await;

    // Asked for one seat after another: both kept.
    give_seat(&room, &mut a, &b, &b_window, 0).await;
    give_seat(&room, &mut a, &b, &b_window, 1).await;
    a.wait_session(&room, &a, "B on both pads", |s| {
        s.is_some_and(|s| holder(s, 0) == Some(b.id()) && holder(s, 1) == Some(b.id()))
    })
    .await;
    assert_eq!(b_window.held_seats(), [0, 1]);
    // Each controller drives its own seat, in connection order.
    b_window.pad_of(0, PadButtons::A);
    b_window.pad_of(1, PadButtons::B);
    wait("each controller on its own pad", || {
        a.fake.pad_at(0) == Some(pressed(PadButtons::A))
            && a.fake.pad_at(1) == Some(pressed(PadButtons::B))
    })
    .await;

    // The keyboard seat is added too: it needs no controller.
    give_seat(&room, &mut a, &b, &b_window, 2).await;
    assert_eq!(b_window.held_seats(), [0, 1, 2]);
    let typed = |key: u16, down: bool| {
        a.fake
            .kbm
            .calls()
            .contains(&KbmCall::Key(HidUsage(key), down))
    };
    b_window.key(0x1A, true);
    wait("B's W", || typed(0x1A, true)).await;

    // Giving back one seat keeps the others, each with its controller: the
    // first controller doesn't take the seat left.
    b.ok(Command::GiveBackSeat { seat: 0 }).await;
    wait("seat 1 given back", || b_window.held_seats() == [1, 2]).await;
    wait("pad 1 let go", || {
        a.fake.pad_at(0) == Some(PadState::NEUTRAL)
    })
    .await;
    b_window.pad_of(0, PadButtons::Y);
    b_window.pad_of(1, PadButtons::X);
    wait("controller B still on its seat", || {
        a.fake.pad_at(1) == Some(pressed(PadButtons::X))
    })
    .await;
    assert_eq!(a.fake.pad_at(0), Some(PadState::NEUTRAL));
    assert!(!typed(0x1A, false), "the keyboard seat stayed");
    a.wait_session(&room, &a, "B on seats 2 and 3", |s| {
        s.is_some_and(|s| {
            holder(s, 0).is_none() && holder(s, 1) == Some(b.id()) && holder(s, 2) == Some(b.id())
        })
    })
    .await;

    // The panic key takes every seat B holds, and lets go of all of it.
    assert!(a.fake.press_panic_key());
    wait("every seat gone", || b_window.held_seats().is_empty()).await;
    wait("B's pad and key let go", || {
        a.fake.pad_at(1) == Some(PadState::NEUTRAL) && typed(0x1A, false)
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// Which of the viewer's controllers drives `seat`, as the host sees it
/// (P12).
fn guest_controller(s: &SessionView, seat: usize) -> Option<u8> {
    s.seats.get(seat).and_then(|s| s.guest_controller)
}

/// P12: the host sees a guest's controllers by name, picks which one plays a
/// seat, and moves a seat the guest holds to its other controller; the
/// guest's side follows, and the game sees it.
#[tokio::test(flavor = "multi_thread")]
async fn the_host_picks_which_of_a_guests_controllers_plays() {
    let (mut a, mut b, mut c, room) = three().await;
    let reply = a.live(go_live_with(&room, 4, TWO_PADS)).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    let b_window = watching(&room, &a, &b).await;
    b_window.name_controllers(&[(0, "DualSense\u{7}"), (1, "Xbox Controller")]);
    b_window.controllers(2);
    a.wait_session(&room, &a, "B's controllers by name", |s| {
        s.is_some_and(|s| {
            let names: Vec<&str> = s
                .viewer_pads
                .first()
                .map_or_else(Vec::new, |p| p.iter().map(|c| c.name.as_str()).collect());
            s.viewers == [b.id()] && names == ["DualSense", "Xbox Controller"]
        })
    })
    .await;

    // Seat 1 to B's second controller: it drives, the first doesn't.
    a.ok(Command::PassControl {
        member: b.id(),
        seat: 0,
        controller: Some(1),
    })
    .await;
    a.wait_session(&room, &a, "B on seat 1 with its Xbox pad", |s| {
        s.is_some_and(|s| holder(s, 0) == Some(b.id()) && guest_controller(s, 0) == Some(1))
    })
    .await;
    wait("the window on seat 1", || b_window.held_seats() == [0]).await;
    b_window.pad_of(1, PadButtons::A);
    wait("the Xbox pad on seat 1", || {
        a.fake.pad_at(0) == Some(pressed(PadButtons::A))
    })
    .await;
    b_window.pad_of(0, PadButtons::B);
    b_window.pad_of(1, PadButtons::X);
    wait("still the Xbox pad alone", || {
        a.fake.pad_at(0) == Some(pressed(PadButtons::X))
    })
    .await;

    // Seat 2 to the same controller: added, and the two swap.
    a.ok(Command::PassControl {
        member: b.id(),
        seat: 1,
        controller: Some(1),
    })
    .await;
    a.wait_session(&room, &a, "B on both, swapped", |s| {
        s.is_some_and(|s| {
            holder(s, 1) == Some(b.id())
                && guest_controller(s, 0) == Some(0)
                && guest_controller(s, 1) == Some(1)
        })
    })
    .await;
    b_window.pad_of(0, PadButtons::Y);
    b_window.pad_of(1, PadButtons::B);
    wait("each controller where it was put", || {
        a.fake.pad_at(0) == Some(pressed(PadButtons::Y))
            && a.fake.pad_at(1) == Some(pressed(PadButtons::B))
    })
    .await;

    // A seat B holds moved to its other controller: no new grant, a swap.
    a.ok(Command::PassControl {
        member: b.id(),
        seat: 0,
        controller: Some(1),
    })
    .await;
    a.wait_session(&room, &a, "swapped back", |s| {
        s.is_some_and(|s| guest_controller(s, 0) == Some(1) && guest_controller(s, 1) == Some(0))
    })
    .await;
    b_window.pad_of(0, PadButtons::X);
    b_window.pad_of(1, PadButtons::A);
    wait("the pads follow", || {
        a.fake.pad_at(0) == Some(pressed(PadButtons::A))
            && a.fake.pad_at(1) == Some(pressed(PadButtons::X))
    })
    .await;
    assert_eq!(b_window.held_seats(), [0, 1]);

    // A controller B doesn't have is no choice: the pass to a seat B holds
    // is accepted and does nothing.
    a.ok(Command::PassControl {
        member: b.id(),
        seat: 1,
        controller: Some(5),
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// P13: a guest with two controllers on two seats, given a third: only the
/// controller that moves leaves its seat, whether the host passes it or
/// the guest asks for it; passing it without saying which is refused.
#[tokio::test(flavor = "multi_thread")]
async fn only_the_controller_that_moves_leaves_its_seat() {
    let (mut a, mut b, mut c, room) = three().await;
    let reply = a
        .live(go_live_with(&room, 4, setup(3, KeyboardMouseSeat::None)))
        .await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    let b_window = watching(&room, &a, &b).await;
    b_window.name_controllers(&[(0, "DualSense"), (1, "Xbox")]);
    b_window.controllers(2);
    host_sees_controllers(&room, &a, &b, 2).await;
    for (seat, controller) in [(0, 0), (1, 1)] {
        a.ok(Command::PassControl {
            member: b.id(),
            seat,
            controller: Some(controller),
        })
        .await;
    }
    a.wait_session(&room, &a, "B on seats 1 and 2, by controller", |s| {
        s.is_some_and(|s| guest_controller(s, 0) == Some(0) && guest_controller(s, 1) == Some(1))
    })
    .await;

    // Without saying which: refused, nothing moves.
    let refused = a
        .handle(Command::PassControl {
            member: b.id(),
            seat: 2,
            controller: None,
        })
        .await;
    assert!(refused.is_err(), "{refused:?}");

    // The Xbox pad to seat 3: seat 2 alone goes; the DualSense plays on.
    a.ok(Command::PassControl {
        member: b.id(),
        seat: 2,
        controller: Some(1),
    })
    .await;
    a.wait_session(&room, &a, "the Xbox pad on seat 3, seat 2 free", |s| {
        s.is_some_and(|s| {
            holder(s, 0) == Some(b.id())
                && holder(s, 1).is_none()
                && holder(s, 2) == Some(b.id())
                && guest_controller(s, 2) == Some(1)
        })
    })
    .await;
    wait("the window on seats 1 and 3", || {
        b_window.held_seats() == [0, 2]
    })
    .await;
    b_window.pad_of(0, PadButtons::A);
    b_window.pad_of(1, PadButtons::B);
    wait("each controller on its own pad", || {
        a.fake.pad_at(0) == Some(pressed(PadButtons::A))
            && a.fake.pad_at(2) == Some(pressed(PadButtons::B))
    })
    .await;

    // B asks for seat 2 back with its DualSense: accepted, seat 1 alone
    // goes; the Xbox pad stays on seat 3.
    b.ok(Command::RequestControl {
        seat: 1,
        controller: Some(0),
    })
    .await;
    a.wait_session(&room, &a, "B's request names its DualSense", |s| {
        s.is_some_and(|s| s.requests.first().is_some_and(|r| r.controller == Some(0)))
    })
    .await;
    a.ok(Command::AnswerControl {
        member: b.id(),
        accept: true,
    })
    .await;
    a.wait_session(&room, &a, "the DualSense on seat 2, seat 1 free", |s| {
        s.is_some_and(|s| {
            holder(s, 0).is_none()
                && holder(s, 1) == Some(b.id())
                && holder(s, 2) == Some(b.id())
                && guest_controller(s, 1) == Some(0)
                && guest_controller(s, 2) == Some(1)
        })
    })
    .await;
    b_window.pad_of(0, PadButtons::X);
    wait("the DualSense on seat 2", || {
        a.fake.pad_at(1) == Some(pressed(PadButtons::X))
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_seat_passed_is_added_or_moves_by_the_guests_controllers() {
    let (mut a, mut b, mut c, room) = three().await;
    let reply = a.live(go_live_with(&room, 4, TWO_PADS)).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    let b_window = watching(&room, &a, &b).await;
    b_window.controllers(1);
    host_sees_controllers(&room, &a, &b, 1).await;
    give_seat(&room, &mut a, &b, &b_window, 0).await;

    // One controller: passing the other seat moves B.
    a.ok(Command::PassControl {
        member: b.id(),
        seat: 1,
        controller: None,
    })
    .await;
    a.wait_session(&room, &a, "B moved", |s| {
        s.is_some_and(|s| holder(s, 0).is_none() && holder(s, 1) == Some(b.id()))
    })
    .await;
    wait("the window on seat 2 alone", || {
        b_window.held_seats() == [1]
    })
    .await;

    // A second controller plugged in: passing seat 1 adds it.
    b_window.controllers(2);
    host_sees_controllers(&room, &a, &b, 2).await;
    a.ok(Command::PassControl {
        member: b.id(),
        seat: 0,
        controller: None,
    })
    .await;
    a.wait_session(&room, &a, "B on both", |s| {
        s.is_some_and(|s| holder(s, 0) == Some(b.id()) && holder(s, 1) == Some(b.id()))
    })
    .await;
    wait("the window on both", || b_window.held_seats() == [0, 1]).await;
    // Seat 2 keeps the controller that drove it; the new one drives seat 1.
    b_window.pad_of(0, PadButtons::A);
    b_window.pad_of(1, PadButtons::B);
    wait("each controller on its own pad", || {
        a.fake.pad_at(1) == Some(pressed(PadButtons::A))
            && a.fake.pad_at(0) == Some(pressed(PadButtons::B))
    })
    .await;

    // The second controller goes: its seat is given back, the first
    // controller stays on its own.
    b_window.slots(0b01);
    a.wait_session(&room, &a, "seat 1 freed, B on seat 2", |s| {
        s.is_some_and(|s| holder(s, 0).is_none() && holder(s, 1) == Some(b.id()))
    })
    .await;
    wait("seat 1's pad let go", || {
        a.fake.pad_at(0) == Some(PadState::NEUTRAL)
    })
    .await;
    assert_eq!(a.fake.pad_at(1), Some(pressed(PadButtons::A)));
    wait("the window on seat 2 alone", || {
        b_window.held_seats() == [1]
    })
    .await;

    // With one seat left, its controller going frees nothing.
    b_window.slots(0b00);
    wait("seat 2's pad let go", || {
        a.fake.pad_at(1) == Some(PadState::NEUTRAL)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(b_window.held_seats(), [1], "one seat stays");

    // Closing the window releases every seat at once.
    b_window.close();
    a.wait_session(&room, &a, "both seats free", |s| {
        s.is_some_and(|s| holder(s, 0).is_none() && holder(s, 1).is_none())
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guests_seats_survive_a_stream_stop_held_off_and_end_with_the_session() {
    let (mut a, mut b, mut c, room) = three().await;
    let reply = a.live(go_live_with(&room, 4, TWO_PADS)).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    let b_window = watching(&room, &a, &b).await;
    b_window.controllers(2);
    host_sees_controllers(&room, &a, &b, 2).await;
    give_seat(&room, &mut a, &b, &b_window, 0).await;
    give_seat(&room, &mut a, &b, &b_window, 1).await;
    b_window.pad_of(0, PadButtons::A);
    b_window.pad_of(1, PadButtons::B);
    wait("both pads pressed", || {
        a.fake.pad_at(0) == Some(pressed(PadButtons::A))
            && a.fake.pad_at(1) == Some(pressed(PadButtons::B))
    })
    .await;

    // The stream stops: both pads let go, both seats kept.
    a.ok(Command::StopStream).await;
    wait("both pads let go", || {
        a.fake.pad_at(0) == Some(PadState::NEUTRAL) && a.fake.pad_at(1) == Some(PadState::NEUTRAL)
    })
    .await;
    assert_eq!(b_window.held_seats(), [0, 1]);

    // The session ends: the seats go with it, and B's window closes.
    a.ok(Command::EndSession).await;
    let ended = b
        .event("the end of watching", |e| match e {
            Event::WatchEnded { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(ended, WatchEnd::Ended);
    wait("B's window closed", || {
        b_window.shut_down.load(Ordering::SeqCst)
    })
    .await;
    assert_eq!(a.fake.pad_at(0), Some(PadState::NEUTRAL));
    assert_eq!(a.fake.pad_at(1), Some(PadState::NEUTRAL));
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_keyboard_seat_types_and_never_drives_a_pad() {
    use couch_input::HidUsage;
    use couch_testkit::KbmCall;

    let (mut a, mut b, mut c, room) = three().await;
    let own_seat = setup(1, KeyboardMouseSeat::OwnSeat);
    let reply = a.live(go_live_with(&room, 4, own_seat)).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    assert_eq!(a.fake.pads_plugged(), 1, "no pad for the keyboard seat");
    let b_window = watching(&room, &a, &b).await;
    let c_window = watching(&room, &a, &c).await;
    a.wait_session(&room, &a, "the seats' kinds", |s| {
        s.is_some_and(|s| {
            s.seats.len() == 2
                && s.seats[0].pad
                && !s.seats[0].keyboard_mouse
                && s.seats[1].keyboard_mouse
                && !s.seats[1].pad
                && s.seats[1].label == "Keyboard and mouse"
        })
    })
    .await;
    give_seat(&room, &mut a, &b, &b_window, 1).await;
    give_seat(&room, &mut a, &c, &c_window, 0).await;
    let typed = |key: u16, down: bool| {
        a.fake
            .kbm
            .calls()
            .contains(&KbmCall::Key(HidUsage(key), down))
    };

    // B's keys reach the game; B's pad goes nowhere.
    b_window.key(0x1A, true);
    b_window.pad(PadButtons::A);
    wait("B's W", || typed(0x1A, true)).await;
    // C's pad works; C's keys go nowhere.
    c_window.pad(PadButtons::B);
    c_window.key(0x04, true);
    wait("C's B", || a.fake.pad_at(0) == Some(pressed(PadButtons::B))).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        a.fake.pad_at(0),
        Some(pressed(PadButtons::B)),
        "B's A never landed"
    );
    assert!(!typed(0x04, true), "C has no keyboard");
    // B's seat goes back: its key is let go.
    a.ok(Command::TakeBackControl { seat: Some(1) }).await;
    wait("B's W released", || typed(0x1A, false)).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn viewers_hear_when_the_game_leaves_the_front() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let b_window = watching(&room, &a, &b).await;
    let in_front = |w: &FakeViewer| w.hud.lock().unwrap().as_ref().map(|h| h.game_in_front);
    wait("a HUD", || in_front(&b_window).is_some()).await;
    assert_eq!(in_front(&b_window), Some(true));

    a.fake.away.store(true, Ordering::SeqCst);
    wait("the game out of front", || {
        in_front(&b_window) == Some(false)
    })
    .await;
    // Someone who starts watching now hears it too.
    let c_window = watching(&room, &a, &c).await;
    wait("C hears it", || in_front(&c_window) == Some(false)).await;

    a.fake.away.store(false, Ordering::SeqCst);
    wait("the game back in front", || {
        in_front(&b_window) == Some(true) && in_front(&c_window) == Some(true)
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M19: one input setup. Setups that can't be are refused; keyboard and
/// mouse go to a seat of their own or with Controller 1 (one player on both,
/// single player's old "both").
#[tokio::test(flavor = "multi_thread")]
async fn the_input_setup_is_checked_and_makes_the_seats() {
    let (mut a, mut b, mut c, room) = three().await;
    for bad in [
        setup(5, KeyboardMouseSeat::None),
        setup(0, KeyboardMouseSeat::None),
    ] {
        assert!(
            matches!(
                a.handle(go_live_with(&room, 4, bad)).await,
                Err(EngineError::Session(_))
            ),
            "{bad:?}"
        );
    }
    let kinds = |s: &SessionView| {
        s.seats
            .iter()
            .map(|s| (s.label.clone(), s.pad, s.keyboard_mouse))
            .collect::<Vec<_>>()
    };
    let with_first = setup(2, KeyboardMouseSeat::WithController1);
    let reply = a.handle(go_live_with(&room, 4, with_first)).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    a.wait_session(&room, &a, "the seats", |s| s.is_some())
        .await;
    let view = a.session(&room, &a).await.unwrap();
    assert_eq!(view.seats.len(), 2);
    assert!(
        view.seats[0].pad && view.seats[0].keyboard_mouse,
        "{:?}",
        kinds(&view)
    );
    assert!(view.seats[1].pad && !view.seats[1].keyboard_mouse);
    assert_eq!(a.fake.pads_plugged(), 2);
    a.ok(Command::EndSession).await;

    // Keyboard and mouse alone.
    let alone = setup(0, KeyboardMouseSeat::WithController1);
    let reply = a.handle(go_live_with(&room, 4, alone)).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    a.wait_session(&room, &a, "one keyboard seat", |s| {
        s.is_some_and(|s| s.seats.len() == 1)
    })
    .await;
    let view = a.session(&room, &a).await.unwrap();
    assert!(!view.seats[0].pad && view.seats[0].keyboard_mouse);
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn moved_shortcuts_reach_the_hook_and_the_stream_window() {
    let (mut a, mut b, mut c, room) = three().await;
    let set = |id: &str, code: &str| Command::SetShortcut {
        id: id.into(),
        code: code.into(),
    };
    a.ok(set("panic", "KeyP")).await;
    b.ok(set("fullscreen", "KeyJ")).await;
    b.ok(set("toggleHudAlone", "F2")).await;

    a.go_live(&room, 4).await;
    assert_eq!(*a.fake.panic_key_on.lock().unwrap(), Some(HidUsage(0x13)));
    assert_eq!(
        a.session(&room, &a).await.unwrap().panic_key.as_deref(),
        Some("Ctrl+Alt+Shift+P")
    );
    assert!(a.fake.press_panic_key());
    a.wait_session(&room, &a, "control paused", |s| {
        s.is_some_and(|s| s.control_paused)
    })
    .await;

    // The window hears B's keys right after the version, before anything
    // else; C's window gets an empty list (the defaults).
    let b_window = watching(&room, &a, &b).await;
    wait("B's window has its keys", || {
        b_window.shortcuts.lock().unwrap().is_some()
    })
    .await;
    assert_eq!(
        b_window.first_kinds.lock().unwrap()[..2],
        [
            couch_proto::ipc::kinds::HELLO,
            couch_proto::ipc::kinds::SHORTCUTS
        ]
    );
    assert_eq!(
        b_window.shortcuts.lock().unwrap().clone().unwrap().keys,
        [
            ("fullscreen".to_string(), 0x0D),
            ("toggleHudAlone".to_string(), 0x3B)
        ]
        .into()
    );
    let c_window = watching(&room, &a, &c).await;
    wait("C's window has the defaults", || {
        c_window
            .shortcuts
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.keys.is_empty())
    })
    .await;

    // A change during a session applies at the next session.
    a.ok(set("panic", "KeyO")).await;
    assert_eq!(*a.fake.panic_key_on.lock().unwrap(), Some(HidUsage(0x13)));
    a.ok(Command::EndSession).await;
    a.go_live(&room, 4).await;
    assert_eq!(*a.fake.panic_key_on.lock().unwrap(), Some(HidUsage(0x12)));
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

// ---- The host on a seat ----

/// Whether `s` shows the host on `seat`, with their own controller.
fn host_on(s: &SessionView, seat: u8, host: &Member) -> bool {
    s.seats
        .get(usize::from(seat))
        .is_some_and(|v| v.host && v.holder == Some(host.id()))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hosts_controller_plays_a_seat_and_gets_it_back() {
    let (mut a, mut b, mut c, room) = three().await;
    let list = sources(&a).await;
    assert_eq!(list.host_controllers, vec![dualsense(), gamesir()]);
    assert_eq!(list.hiding, HidingView::Ready);
    let reply = a
        .live(go_live_playing(
            &room,
            4,
            InputSetup::default(),
            vec![dualsense()],
        ))
        .await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    assert!(a.fake.hidden(), "hidden from games while live");
    a.wait_session(&room, &a, "the host on the seat", |s| {
        s.is_some_and(|s| {
            host_on(s, 0, &a) && s.host_controllers == ["DualSense Wireless Controller"]
        })
    })
    .await;
    // The host's controller drives the seat's pad, through the router.
    a.fake.local_press(PadButtons::A);
    wait("the host's A", || {
        a.fake.pad() == Some(pressed(PadButtons::A))
    })
    .await;
    // Everyone sees the host on it, the stream window too.
    let b_window = watching(&room, &a, &b).await;
    c.wait_session(&room, &a, "the host on the seat, from C", |s| {
        s.is_some_and(|s| host_on(s, 0, &a))
    })
    .await;
    wait("the HUD's seat", || {
        b_window
            .hud
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|h| h.seats[0].host && h.seats[0].holder.as_deref() == Some("Ana"))
    })
    .await;

    // Passed to B: B's pad drives it; the host's controller doesn't.
    give_seat(&room, &mut a, &b, &b_window, 0).await;
    b_window.pad(PadButtons::B);
    wait("B's B", || a.fake.pad() == Some(pressed(PadButtons::B))).await;
    a.fake.local_press(PadButtons::X);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        a.fake.pad(),
        Some(pressed(PadButtons::B)),
        "the host is off it"
    );

    // Taken back: it stays empty, and the host's controller drives nothing.
    a.ok(Command::TakeBackControl { seat: Some(0) }).await;
    wait("B's seat gone", || !b_window.holding()).await;
    wait("the pad let go", || a.fake.pad() == Some(PadState::NEUTRAL)).await;
    c.wait_session(&room, &a, "an empty seat, from C", |s| {
        s.is_some_and(|s| s.host_on_seats && holder(s, 0).is_none())
    })
    .await;
    wait("the HUD's empty seat", || {
        b_window
            .hud
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|h| h.host_on_seats && h.seats[0].holder.is_none())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(a.fake.pad(), Some(PadState::NEUTRAL), "nobody on it");
    // Play here puts the host's controller back on it.
    a.ok(Command::PlaySeat {
        seat: 0,
        controller: 0,
    })
    .await;
    wait("the host's X", || {
        a.fake.pad() == Some(pressed(PadButtons::X))
    })
    .await;
    a.wait_session(&room, &a, "the host back on", |s| {
        s.is_some_and(|s| host_on(s, 0, &a))
    })
    .await;

    // The panic key with B on the seat: B out, and it stays empty; the
    // host may still take it while control is paused.
    give_seat(&room, &mut a, &b, &b_window, 0).await;
    assert!(a.fake.press_panic_key());
    wait("B's seat gone again", || !b_window.holding()).await;
    a.wait_session(&room, &a, "paused, the seat empty", |s| {
        s.is_some_and(|s| s.control_paused && holder(s, 0).is_none())
    })
    .await;
    // And it stays so: no claim gives it to the host's controller.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(a.fake.pad(), Some(PadState::NEUTRAL), "still empty");
    a.wait_session(&room, &a, "still empty", |s| {
        s.is_some_and(|s| holder(s, 0).is_none())
    })
    .await;
    a.ok(Command::PlaySeat {
        seat: 0,
        controller: 0,
    })
    .await;
    a.wait_session(&room, &a, "the host on while paused", |s| {
        s.is_some_and(|s| host_on(s, 0, &a))
    })
    .await;
    // The panic key again: the host keeps the seat they play.
    assert!(a.fake.press_panic_key());
    a.fake.local_press(PadButtons::Y);
    wait("the host's Y", || {
        a.fake.pad() == Some(pressed(PadButtons::Y))
    })
    .await;
    a.wait_session(&room, &a, "paused, the host on", |s| {
        s.is_some_and(|s| s.control_paused && host_on(s, 0, &a))
    })
    .await;

    // Ending the session shows the controller to games again.
    a.ok(Command::EndSession).await;
    wait("shown again", || !a.fake.hidden()).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_host_plays_another_seat_and_feels_its_rumble() {
    let (mut a, mut b, mut c, room) = three().await;
    let reply = a
        .live(go_live_playing(&room, 4, TWO_PADS, vec![dualsense()]))
        .await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    a.fake.local_press(PadButtons::A);
    wait("the host on pad 1", || {
        a.fake.pad_at(0) == Some(pressed(PadButtons::A))
    })
    .await;

    a.ok(Command::PlaySeat {
        seat: 1,
        controller: 0,
    })
    .await;
    wait("the host on pad 2", || {
        a.fake.pad_at(1) == Some(pressed(PadButtons::A))
    })
    .await;
    wait("pad 1 let go", || {
        a.fake.pad_at(0) == Some(PadState::NEUTRAL)
    })
    .await;
    a.wait_session(&room, &a, "the host on Controller 2", |s| {
        s.is_some_and(|s| host_on(s, 1, &a) && holder(s, 0).is_none())
    })
    .await;
    assert!(
        a.handle(Command::PlaySeat {
            seat: 5,
            controller: 0,
        })
        .await
        .is_err()
    );

    // The game shakes the host's seat: their controller shakes.
    let shake = Rumble {
        low_frequency: 200,
        high_frequency: 100,
    };
    a.fake.pads.plugged.lock().unwrap()[1].queue_rumble(shake);
    wait("the rumble", || {
        a.fake.local.rumble.lock().unwrap().contains(&shake)
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M23: the host's two controllers, each on its own seat.
#[tokio::test(flavor = "multi_thread")]
async fn the_hosts_two_controllers_play_two_seats() {
    let (mut a, mut b, mut c, room) = three().await;
    let reply = a
        .live(go_live_playing(
            &room,
            4,
            TWO_PADS,
            vec![dualsense(), gamesir()],
        ))
        .await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    assert_eq!(a.fake.hidden_count(), 2, "both hidden from games");
    a.wait_session(&room, &a, "the host on both seats", |s| {
        s.is_some_and(|s| {
            host_on(s, 0, &a)
                && host_on(s, 1, &a)
                && s.host_controllers == ["DualSense Wireless Controller", "GameSir T4n Lite"]
                && s.seats[0].host_controller == Some(0)
                && s.seats[1].host_controller == Some(1)
        })
    })
    .await;
    // Each drives its own seat's pad.
    a.fake.local_press(PadButtons::A);
    a.fake.second_press(PadButtons::B);
    wait("each on its pad", || {
        a.fake.pad_at(0) == Some(pressed(PadButtons::A))
            && a.fake.pad_at(1) == Some(pressed(PadButtons::B))
    })
    .await;
    // Everyone sees the host on both.
    c.wait_session(&room, &a, "the host on both, from C", |s| {
        s.is_some_and(|s| host_on(s, 0, &a) && host_on(s, 1, &a))
    })
    .await;

    // One passed to Bia: hers drives it; the host's other seat plays on.
    let b_window = watching(&room, &a, &b).await;
    give_seat(&room, &mut a, &b, &b_window, 1).await;
    b_window.pad(PadButtons::X);
    wait("Bia's X on pad 2", || {
        a.fake.pad_at(1) == Some(pressed(PadButtons::X))
    })
    .await;
    a.fake.local_press(PadButtons::Y);
    a.fake.second_press(PadButtons::START);
    wait("the DualSense still on pad 1", || {
        a.fake.pad_at(0) == Some(pressed(PadButtons::Y))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        a.fake.pad_at(1),
        Some(pressed(PadButtons::X)),
        "the GameSir is off it"
    );

    // Taken back: empty, until Play here gives it to the GameSir again.
    a.ok(Command::TakeBackControl { seat: Some(1) }).await;
    wait("Bia's seat gone", || !b_window.holding()).await;
    a.wait_session(&room, &a, "pad 2 empty", |s| {
        s.is_some_and(|s| holder(s, 1).is_none() && host_on(s, 0, &a))
    })
    .await;
    a.ok(Command::PlaySeat {
        seat: 1,
        controller: 1,
    })
    .await;
    wait("the GameSir back on pad 2", || {
        a.fake.pad_at(1) == Some(pressed(PadButtons::START))
    })
    .await;

    // The DualSense onto the GameSir's seat: they swap.
    a.ok(Command::PlaySeat {
        seat: 1,
        controller: 0,
    })
    .await;
    a.fake.local_press(PadButtons::LEFT_SHOULDER);
    a.fake.second_press(PadButtons::RIGHT_SHOULDER);
    wait("swapped", || {
        a.fake.pad_at(1) == Some(pressed(PadButtons::LEFT_SHOULDER))
            && a.fake.pad_at(0) == Some(pressed(PadButtons::RIGHT_SHOULDER))
    })
    .await;
    a.wait_session(&room, &a, "the swap, in the view", |s| {
        s.is_some_and(|s| {
            s.seats[0].host_controller == Some(1) && s.seats[1].host_controller == Some(0)
        })
    })
    .await;
    // The game shakes pad 2: the DualSense, driving it now, shakes.
    let shake = Rumble {
        low_frequency: 180,
        high_frequency: 90,
    };
    a.fake.pads.plugged.lock().unwrap()[1].queue_rumble(shake);
    wait("the DualSense's rumble", || {
        a.fake.local.rumble.lock().unwrap().contains(&shake)
    })
    .await;
    assert!(!a.fake.local.second_rumble.lock().unwrap().contains(&shake));

    // The panic key leaves the host both seats.
    assert!(a.fake.press_panic_key());
    a.wait_session(&room, &a, "paused, the host on both", |s| {
        s.is_some_and(|s| s.control_paused && host_on(s, 0, &a) && host_on(s, 1, &a))
    })
    .await;

    // Ending shows both again.
    a.ok(Command::EndSession).await;
    wait("both shown again", || a.fake.hidden_count() == 0).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_controller_that_cant_be_hidden_plays_outside_the_seats() {
    let (mut a, mut b, mut c, room) = three().await;
    a.fake.hide_fails.store(true, Ordering::SeqCst);
    let reply = a
        .live(go_live_playing(
            &room,
            4,
            InputSetup::default(),
            vec![dualsense()],
        ))
        .await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    a.wait_session(&room, &a, "the problem, and a free seat", |s| {
        s.is_some_and(|s| {
            s.host_controllers.is_empty()
                && s.host_controller_problem
                    .as_deref()
                    .is_some_and(|p| p.contains("configuration window"))
                && holder(s, 0).is_none()
        })
    })
    .await;
    // Its input goes nowhere, and seats work as before.
    a.fake.local_press(PadButtons::A);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_ne!(a.fake.pad(), Some(pressed(PadButtons::A)));
    assert!(matches!(
        a.handle(Command::PlaySeat {
            seat: 0,
            controller: 0,
        })
        .await,
        Err(EngineError::Session(_))
    ));
    let b_window = watching(&room, &a, &b).await;
    give_seat(&room, &mut a, &b, &b_window, 0).await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// M22: a remapper's pad (DS4Windows's, say) as the host's controller:
/// hidden by its device, never by ids (no physical controller is touched),
/// kept hidden while the remapper plugs it again, and shown again as soon as
/// the device could be something else's.
#[tokio::test(flavor = "multi_thread")]
async fn a_remappers_pad_plays_a_seat_hidden_by_its_device() {
    let (mut a, mut b, mut c, room) = three().await;
    a.fake.plug_remapper(Some(PAD_01));
    let list = sources(&a).await;
    assert_eq!(
        list.host_controllers,
        vec![dualsense(), gamesir(), remapper()]
    );
    let reply = a
        .live(go_live_playing(
            &room,
            4,
            InputSetup::default(),
            vec![remapper()],
        ))
        .await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    assert_eq!(a.fake.virtual_hidden(), [PAD_01], "hidden as it goes live");
    assert_eq!(a.fake.hidden_count(), 0, "no physical controller hidden");
    a.wait_session(&room, &a, "the host on the seat", |s| {
        s.is_some_and(|s| {
            host_on(s, 0, &a) && s.host_controllers == ["Xbox 360 Controller (a remapper's)"]
        })
    })
    .await;
    // Its remapped input drives the seat's pad, through the router.
    a.fake.remapper_press(PadButtons::A);
    wait("the remapper's A", || {
        a.fake.pad() == Some(pressed(PadButtons::A))
    })
    .await;
    // The game shakes the seat: the remapper's pad gets it.
    let shake = Rumble {
        low_frequency: 120,
        high_frequency: 60,
    };
    a.fake.pads.plugged.lock().unwrap()[0].queue_rumble(shake);
    wait("the rumble", || {
        a.fake
            .local
            .remapper_rumble
            .lock()
            .unwrap()
            .contains(&shake)
    })
    .await;

    // The remapper plugs its pad again (its controller reconnecting): the
    // device stays hidden while it is gone, and while it is back but the
    // reader doesn't have it yet (its player light, its next look), so it
    // is hidden as it comes back.
    a.fake.plug_remapper(None);
    a.fake.set_virtual_now(
        PAD_01,
        VirtualDevice::Gone {
            hosts_plugging: false,
        },
    );
    tokio::time::sleep(CHECK_EVERY * 5 / 2).await;
    assert_eq!(a.fake.virtual_hidden(), [PAD_01], "kept while gone");
    a.fake.set_virtual_now(PAD_01, VirtualDevice::Theirs);
    tokio::time::sleep(CHECK_EVERY * 5 / 2).await;
    assert_eq!(
        a.fake.virtual_hidden(),
        [PAD_01],
        "kept while back but not yet read"
    );
    // A host here plugging a pad meanwhile: in doubt, but plugged in, so no
    // other pad can have its id: kept.
    a.fake.set_virtual_now(PAD_01, VirtualDevice::InDoubt);
    tokio::time::sleep(CHECK_EVERY * 3 / 2).await;
    assert_eq!(a.fake.virtual_hidden(), [PAD_01], "kept while in doubt");
    a.fake.set_virtual_now(PAD_01, VirtualDevice::Theirs);
    a.fake.plug_remapper(Some(PAD_01));
    a.fake.remapper_press(PadButtons::B);
    wait("the remapper's B", || {
        a.fake.pad() == Some(pressed(PadButtons::B))
    })
    .await;
    tokio::time::sleep(CHECK_EVERY * 3 / 2).await;
    assert_eq!(a.fake.virtual_hidden(), [PAD_01]);
    assert_eq!(
        a.fake.virtual_hides.load(Ordering::SeqCst),
        1,
        "never hidden anew"
    );

    // Gone while a host here plugs a pad, which may take its id: the entry
    // comes off. Back under another id: that one is hidden.
    a.fake.plug_remapper(None);
    a.fake.set_virtual_now(
        PAD_01,
        VirtualDevice::Gone {
            hosts_plugging: true,
        },
    );
    wait("the old entry off", || a.fake.virtual_hidden().is_empty()).await;
    a.fake.plug_remapper(Some(PAD_02));
    wait("the new device hidden", || {
        a.fake.virtual_hidden() == [PAD_02]
    })
    .await;

    // The device turns out to be one of ours: shown again, and never
    // hidden again while it is.
    a.fake.set_virtual_now(PAD_02, VirtualDevice::Ours);
    wait("shown again", || a.fake.virtual_hidden().is_empty()).await;
    tokio::time::sleep(CHECK_EVERY * 3 / 2).await;
    assert!(a.fake.virtual_hidden().is_empty(), "not hidden again");

    // Ending shows everything again.
    a.fake.set_virtual_now(PAD_02, VirtualDevice::Theirs);
    wait("hidden again once it is theirs", || {
        a.fake.virtual_hidden() == [PAD_02]
    })
    .await;
    a.ok(Command::EndSession).await;
    wait("all shown again", || a.fake.virtual_hidden().is_empty()).await;
    assert_eq!(a.fake.hidden_count(), 0);
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// The friends' run (2026-10-18): someone let into the room while a session
/// is already live sees it, and can watch it, without the host starting it
/// again. Once with the owner hosting, once with another member hosting
/// (the newcomer meets them by dialling, after the owner admits it).
#[tokio::test(flavor = "multi_thread")]
async fn a_newcomer_sees_a_session_already_live() {
    for owner_hosts in [true, false] {
        let a = Member::new("Ana").await;
        let b = Member::new("Bia").await;
        let mut c = Member::new("Caio").await;
        let Response::Room(room) = a
            .handle(Command::CreateRoom {
                name: "Sofá".into(),
                password: PASSWORD.into(),
            })
            .await
            .unwrap()
        else {
            panic!("a room");
        };
        let Response::Invite { link } = a
            .handle(Command::GetInvite {
                room: room.id.clone(),
            })
            .await
            .unwrap()
        else {
            panic!("an invite");
        };
        let room = room.id;
        let (mut host, mut other) = (a, b);
        if !owner_hosts {
            // B joins first and hosts; A, the owner, only admits.
            other
                .ok(Command::JoinRoom {
                    invite: link.clone(),
                    password: PASSWORD.into(),
                })
                .await;
            std::mem::swap(&mut host, &mut other);
        }
        host.go_live(&room, 4).await;

        // C joins the room only now. Its reply comes from before the host's
        // announcement; the event that brings the session is a newer view,
        // which the app keeps whichever arrives first.
        let Ok(Response::Room(joined)) = c
            .handle(Command::JoinRoom {
                invite: link.clone(),
                password: PASSWORD.into(),
            })
            .await
        else {
            panic!("joined");
        };
        let host_id = host.id();
        let announced = c
            .event("the session announced", |e| match e {
                Event::RoomChanged { room: view }
                    if view.id == room && view.sessions.iter().any(|s| s.host == host_id) =>
                {
                    Some(view.revision)
                }
                _ => None,
            })
            .await;
        assert!(
            announced > joined.revision,
            "the session's view ({announced}) is newer than the join's reply ({})",
            joined.revision
        );
        c.wait_session(
            &room,
            &host,
            "the live session, seen by the newcomer",
            |s| s.is_some_and(|s| s.stream == StreamStateView::Live),
        )
        .await;
        let window = watching(&room, &host, &c).await;
        wait("frames for the newcomer", || window.frames() > 0).await;
        stop_all(&mut [&mut host, &mut other, &mut c]).await;
    }
}

/// P7: the host's stream level scales what guests hear, at once, and the
/// meter shows it; it is remembered for the next session.
#[tokio::test(flavor = "multi_thread")]
async fn the_hosts_stream_level_scales_the_sound_and_is_remembered() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let _window = watching(&room, &a, &b).await;
    // The fake game's clicks peak at 0.8: about -2 dB.
    async fn loudest(a: &mut Member, room: &str, range: std::ops::RangeInclusive<i8>, what: &str) {
        a.event(what, |e| match e {
            Event::StreamLevel {
                room: r, peak_db, ..
            } if r == room && range.contains(peak_db) => Some(()),
            _ => None,
        })
        .await;
    }
    loudest(&mut a, &room, -3..=-1, "the clicks at full level").await;
    a.wait_session(&room, &a, "the level shown", |s| {
        s.is_some_and(|s| s.sound_level == Some(100))
    })
    .await;

    // Half: a quarter of the amplitude, -12 dB lower.
    let reply = a.handle(Command::SetStreamLevel { percent: 50 }).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    loudest(&mut a, &room, -15..=-13, "the clicks at half level").await;
    // Boosted 4x, the clicks (0.8, so 3.2) are rounded off within full
    // scale: never above 0 dB.
    a.ok(Command::SetStreamLevel { percent: 400 }).await;
    loudest(&mut a, &room, -1..=0, "the clicks boosted, limited").await;
    for _ in 0..4 {
        let peak = a
            .event("the next level", |e| match e {
                Event::StreamLevel {
                    room: r, peak_db, ..
                } if *r == room => Some(*peak_db),
                _ => None,
            })
            .await;
        assert!(peak <= 0, "never past full scale: {peak} dB");
    }
    // Nothing above 400%.
    a.ok(Command::SetStreamLevel { percent: 900 }).await;
    a.wait_session(&room, &a, "capped at 400", |s| {
        s.is_some_and(|s| s.sound_level == Some(400))
    })
    .await;
    a.ok(Command::SetStreamLevel { percent: 0 }).await;
    // Silence from then on: the next second of windows all read -90.
    loudest(&mut a, &room, -90..=-90, "silence").await;
    for _ in 0..4 {
        let peak = a
            .event("the next level", |e| match e {
                Event::StreamLevel {
                    room: r, peak_db, ..
                } if *r == room => Some(*peak_db),
                _ => None,
            })
            .await;
        assert_eq!(peak, -90, "nothing above silence at 0%");
    }

    // The next session starts at the level last set.
    a.ok(Command::SetStreamLevel { percent: 30 }).await;
    a.ok(Command::EndSession).await;
    a.go_live(&room, 4).await;
    a.wait_session(&room, &a, "the level remembered", |s| {
        s.is_some_and(|s| s.sound_level == Some(30))
    })
    .await;
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

async fn sources(m: &Member) -> couch_engine::SourceList {
    match m.handle(Command::ListSources).await.unwrap() {
        Response::Sources(s) => s,
        other => panic!("sources: {other:?}"),
    }
}

/// P10: a pad Windows is still setting up doesn't hold the session back; a
/// guest on its seat plays nothing until it's ready, then plays it.
#[tokio::test(flavor = "multi_thread")]
async fn a_pad_windows_sets_up_late_takes_its_seat_when_ready() {
    let (mut a, mut b, mut c, room) = three().await;
    a.fake.pads_setting_up.store(1, Ordering::SeqCst);
    a.go_live(&room, 4).await;
    assert_eq!(a.fake.pads_plugged(), 0, "no pad yet");
    a.wait_session(&room, &a, "the seat's pad being set up", |s| {
        s.is_some_and(|s| s.pads_setting_up == [0] && s.pads_missing.is_empty())
    })
    .await;
    let b_window = watching(&room, &a, &b).await;
    give_seat(&room, &mut a, &b, &b_window, 0).await;
    b_window.pad(PadButtons::A);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.fake.pads_plugged(), 0, "nothing to press yet");

    a.fake.pads_set_up.store(true, Ordering::SeqCst);
    wait("the pad, set up", || a.fake.pads_plugged() == 1).await;
    a.wait_session(&room, &a, "nothing being set up", |s| {
        s.is_some_and(|s| s.pads_setting_up.is_empty() && s.pads_missing.is_empty())
    })
    .await;
    b_window.pad(PadButtons::B);
    wait("B's press on it", || {
        a.fake.pad() == Some(pressed(PadButtons::B))
    })
    .await;
    assert_eq!(a.fake.pads_given_up.load(Ordering::SeqCst), 0);
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// P10: Stop waiting unplugs the pads still being set up; their seats go on
/// without one, and none comes up later.
#[tokio::test(flavor = "multi_thread")]
async fn stopping_the_wait_unplugs_the_pads_still_being_set_up() {
    let (mut a, mut b, mut c, room) = three().await;
    a.fake.pads_setting_up.store(1, Ordering::SeqCst);
    let reply = a.live(go_live_with(&room, 4, TWO_PADS)).await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    assert_eq!(a.fake.pads_plugged(), 1, "the second seat's pad is ready");
    a.wait_session(&room, &a, "the first seat's pad being set up", |s| {
        s.is_some_and(|s| s.pads_setting_up == [0])
    })
    .await;
    a.ok(Command::StopWaitingForPads).await;
    wait("given up", || {
        a.fake.pads_given_up.load(Ordering::SeqCst) == 1
    })
    .await;
    a.wait_session(&room, &a, "the first seat without a pad", |s| {
        s.is_some_and(|s| s.pads_setting_up.is_empty() && s.pads_missing == [0])
    })
    .await;
    a.fake.pads_set_up.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(a.fake.pads_plugged(), 1, "no pad after giving up");
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// P10: ending the session gives a pad still being set up up first, before
/// the host's own controller shows to games again; none comes up after.
#[tokio::test(flavor = "multi_thread")]
async fn ending_the_session_gives_up_a_pad_still_being_set_up() {
    let (mut a, mut b, mut c, room) = three().await;
    a.fake.pads_setting_up.store(1, Ordering::SeqCst);
    let reply = a
        .live(go_live_playing(
            &room,
            4,
            InputSetup::default(),
            vec![dualsense()],
        ))
        .await;
    assert!(matches!(reply, Ok(Response::Room(_))), "{reply:?}");
    assert!(a.fake.hidden(), "hidden while the pad is set up");
    a.ok(Command::EndSession).await;
    wait("shown again", || !a.fake.hidden()).await;
    assert_eq!(
        *a.fake.given_up_when_shown.lock().unwrap(),
        Some(1),
        "given up before the controller showed"
    );
    a.fake.pads_set_up.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(a.fake.pads_plugged(), 0, "no pad after the end");
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// P10: a pad that isn't set up within the session's limit is given up,
/// and says so.
#[tokio::test(flavor = "multi_thread")]
async fn a_pad_not_set_up_in_time_is_given_up() {
    use couch_engine::pipeline::host::{HostEvent, HostOptions, HostPipeline};
    use couch_input::{SeatEpoch, SeatId};

    let fake = Arc::new(Fake::default());
    fake.pads_setting_up.store(1, Ordering::SeqCst);
    let (pipeline, mut events) = HostPipeline::start(
        fake.clone(),
        HostOptions {
            seats: vec![SeatKind::Controller],
            real_pad_ids: false,
            session: 1,
            first_epoch: SeatEpoch(0),
            rc_trace: None,
            netem_elapsed: None,
            replace_grace: Duration::from_millis(300),
            local: None,
            pad_setup_limit: Duration::from_millis(400),
        },
    )
    .await;
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + WAIT;
    while !seen
        .iter()
        .any(|e| matches!(e, HostEvent::PadsMissing { .. }))
    {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("the limit passes")
            .expect("events flow");
        if matches!(
            event,
            HostEvent::PadsSettingUp { .. }
                | HostEvent::PadsMissing { .. }
                | HostEvent::PadReady { .. }
        ) {
            seen.push(event);
        }
    }
    assert!(matches!(&seen[0], HostEvent::PadsSettingUp { seats, .. } if seats == &[SeatId(0)]));
    assert!(matches!(&seen[1], HostEvent::PadsMissing { seats } if seats == &[SeatId(0)]));
    assert_eq!(fake.pads_given_up.load(Ordering::SeqCst), 1);
    assert_eq!(fake.pads_plugged(), 0);
    pipeline.stop().await;
}

/// P11: the app's controls reach the stream window, and the window's own
/// changes reach the app; a restarted window keeps its bar and stats.
#[tokio::test(flavor = "multi_thread")]
async fn the_apps_controls_reach_the_stream_window_and_back() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let window = watching(&room, &a, &b).await;
    let open = |s: Option<&SessionView>| s.and_then(|s| s.stream_window).filter(|w| w.open);
    b.wait_session(&room, &a, "the window's state", |s| {
        open(s).is_some_and(|w| w.bar && !w.fullscreen && !w.stats)
            && s.is_some_and(|s| s.game_in_front == Some(true))
    })
    .await;

    // The app's volume reaches the window; the window's reaches the app.
    b.ok(Command::SetStreamVolume {
        percent: 40,
        muted: true,
    })
    .await;
    let quiet = StreamVolume {
        percent: 40,
        muted: true,
    };
    wait("the volume in the window", || {
        *window.volume.lock().unwrap() == Some(quiet)
    })
    .await;
    window.set_volume(StreamVolume {
        percent: 70,
        muted: false,
    });
    b.wait_session(&room, &a, "the window's volume", |s| {
        open(s).is_some_and(|w| w.volume == 70 && !w.muted)
    })
    .await;

    // Full screen and the front, asked from the app.
    b.ok(Command::StreamWindow {
        request: StreamWindowRequest::Fullscreen { on: true },
    })
    .await;
    b.wait_session(&room, &a, "full screen", |s| {
        open(s).is_some_and(|w| w.fullscreen)
    })
    .await;
    b.ok(Command::StreamWindow {
        request: StreamWindowRequest::Raise,
    })
    .await;
    wait("asked to the front", || {
        window
            .requests
            .lock()
            .unwrap()
            .contains(&WindowRequest::Raise)
    })
    .await;
    // Allowed the foreground first, by its process.
    assert_eq!(
        *b.fake.foreground_allowed.lock().unwrap(),
        [FAKE_VIEWER_PID]
    );

    // The window's own shortcut hides its bar, its button shows the stats.
    window.state(WindowState {
        fullscreen: true,
        bar: false,
        stats: true,
    });
    b.wait_session(&room, &a, "bar off, stats on", |s| {
        open(s).is_some_and(|w| !w.bar && w.stats)
    })
    .await;

    // The game leaving the front on the host shows in the app too.
    a.fake.away.store(true, Ordering::SeqCst);
    b.wait_session(&room, &a, "the game out of front", |s| {
        s.is_some_and(|s| s.game_in_front == Some(false))
    })
    .await;

    // A crashed window starts again as it was: bar off, stats on.
    window.crash();
    wait("a new window", || b.fake.viewers.lock().unwrap().len() == 2).await;
    let second = b.fake.viewer().expect("the new window");
    assert!(
        second.started_without_bar.load(Ordering::SeqCst),
        "the bar stays off"
    );
    assert!(
        second.started_with_stats.load(Ordering::SeqCst),
        "the stats stay on"
    );
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}

/// P11: Settings → Stream window: a new window opens without its bar, and
/// one open follows the setting at once.
#[tokio::test(flavor = "multi_thread")]
async fn the_stream_window_setting_turns_the_bar_off() {
    let (mut a, mut b, mut c, room) = three().await;
    a.go_live(&room, 4).await;
    let reply = b.handle(Command::GetStreamWindow).await;
    assert!(
        matches!(
            reply,
            Ok(Response::StreamWindow(StreamWindowSettingsView {
                bar: true
            }))
        ),
        "on by default: {reply:?}"
    );
    b.ok(Command::SetStreamWindow { bar: false }).await;
    let window = watching(&room, &a, &b).await;
    assert!(window.started_without_bar.load(Ordering::SeqCst));
    b.wait_session(&room, &a, "the bar off", |s| {
        s.and_then(|s| s.stream_window)
            .is_some_and(|w| w.open && !w.bar)
    })
    .await;

    b.ok(Command::SetStreamWindow { bar: true }).await;
    wait("the bar back on", || {
        window
            .requests
            .lock()
            .unwrap()
            .contains(&WindowRequest::ShowBar(true))
    })
    .await;
    b.wait_session(&room, &a, "the bar on", |s| {
        s.and_then(|s| s.stream_window).is_some_and(|w| w.bar)
    })
    .await;
    let reply = b.handle(Command::GetStreamWindow).await;
    assert!(matches!(
        reply,
        Ok(Response::StreamWindow(StreamWindowSettingsView {
            bar: true
        }))
    ));
    stop_all(&mut [&mut a, &mut b, &mut c]).await;
}
