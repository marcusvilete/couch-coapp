//! Guests' virtual controllers on a Linux host (M16c, ADR 0012): a `uinput`
//! device each, made like the kernel's own Xbox 360 controller (`xpad`: its
//! ids, name, buttons and axes), so games and SDL map it as one.
//!
//! - **Ours are named** by their `phys`, `couch-coapp/<pid>/pad<N>` (the
//!   process's Nth pad): a stream window on the same PC skips exactly those
//!   ([`is_ours`]).
//! - **A pad goes with the process that made it,** so a crash leaves
//!   nothing held (Windows needs `held_input` for that; a pad here needs
//!   nothing).
//! - **Rumble:** a game uploads its force-feedback effects to the pad and
//!   waits until it's answered, so each pad has a thread that answers at
//!   once and keeps what's playing for [`VirtualPad::take_rumble`].
//! - **Access:** making a device needs `/dev/uinput` writable, which a udev
//!   rule grants the signed-in user ([`available`], [`SET_UP_RULE`]).

use std::{
    collections::HashMap,
    ffi::CString,
    io,
    os::fd::{AsFd, OwnedFd},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use couch_input::{InputError, PadButtons, PadState, Rumble, VirtualPad, VirtualPadFactory};
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventSummary, EventType, FFEffectCode,
    FFEffectKind, InputEvent, InputId, KeyCode, UInputCode, UinputAbsSetup, uinput::VirtualDevice,
};

/// What a pad of ours puts first in its `phys`.
pub const PHYS_PREFIX: &str = "couch-coapp/";

/// The udev rule that lets the signed-in user make input devices (as
/// Steam's controller rules do), and where it goes. Fixed text: nothing of
/// the user's goes into it.
pub const SET_UP_RULE: &str =
    r#"KERNEL=="uinput", SUBSYSTEM=="misc", OPTIONS+="static_node=uinput", TAG+="uaccess""#;
pub const SET_UP_RULE_FILE: &str = "/etc/udev/rules.d/60-couch-coapp-uinput.rules";

/// The name the kernel's `xpad` gives a wired Xbox 360 controller.
const NAME: &str = "Microsoft X-Box 360 pad";
/// `xpad`'s: a genuine controller's device version, which SDL's mapping
/// database keys on with the ids.
const VERSION: u16 = 0x0114;
/// Effects a game may upload at once.
const EFFECTS: u32 = 16;
/// How often the rumble thread looks whether its pad is going.
const STOP_CHECK: Duration = Duration::from_millis(100);

/// Whether this user may make input devices now: `/dev/uinput` opens for
/// writing.
pub fn available() -> bool {
    std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/uinput")
        .is_ok()
}

/// Installs [`SET_UP_RULE`] with the administrator's consent (`pkexec`,
/// which asks for the password), then applies it, loading `uinput` first
/// where it's a module so the rule has a device to apply to. The command is
/// fixed. Undone by deleting [`SET_UP_RULE_FILE`].
pub fn set_up() -> Result<(), String> {
    let script = format!(
        "modprobe uinput 2>/dev/null; \
         printf '%s\\n' '{SET_UP_RULE}' > {SET_UP_RULE_FILE} && \
         udevadm control --reload-rules && udevadm trigger --name-match=uinput"
    );
    let status = std::process::Command::new("pkexec")
        .args(["sh", "-c", &script])
        .status()
        .map_err(|e| format!("couldn't ask for administrator rights (pkexec): {e}"))?;
    match status.code() {
        // The rule applies to this session's device at once, normally; where
        // it doesn't, signing in again does.
        Some(0) if available() => Ok(()),
        Some(0) => Err("set up, but this session can't use it yet: sign out and back in".into()),
        // pkexec: the dialog was dismissed, the password refused, or no
        // agent to ask with.
        Some(126 | 127) => Err(
            "not set up: administrator rights weren't given (or nothing could ask for them)".into(),
        ),
        _ => Err(format!("setting it up failed ({status})")),
    }
}

/// The command [`set_up`] runs, for doing it by hand in a terminal (the
/// Settings page shows the same, `sections.tsx`'s `UINPUT_BY_HAND`).
pub fn set_up_by_hand() -> String {
    format!(
        "sudo modprobe uinput; echo '{SET_UP_RULE}' | sudo tee {SET_UP_RULE_FILE} && sudo udevadm control --reload-rules && sudo udevadm trigger --name-match=uinput"
    )
}

/// Whether the input device at `path` (`/dev/input/eventN`, as SDL names
/// it) is a pad of ours, by its `phys`; `None` if that can't be read.
pub fn is_ours(path: &str) -> Option<bool> {
    let node = std::path::Path::new(path).file_name()?;
    let phys = std::fs::read_to_string(
        std::path::Path::new("/sys/class/input")
            .join(node)
            .join("device/phys"),
    )
    .ok()?;
    Some(phys.trim_end().starts_with(PHYS_PREFIX))
}

/// Pads made by this process so far: each `phys` is its own.
static PLUGGED: AtomicU32 = AtomicU32::new(0);

/// Makes this host's pads, each with its own `phys`.
pub struct UinputPads {
    ids: (u16, u16),
}

impl UinputPads {
    /// `ids`: vendor and product; a real Xbox 360 controller's, or our test
    /// pads' (`couch_input::DEV_PAD_IDS`).
    pub fn new(ids: (u16, u16)) -> Self {
        Self { ids }
    }
}

impl VirtualPadFactory for UinputPads {
    /// At once: a `uinput` device needs no setting up.
    fn plug(&self) -> Result<Box<dyn VirtualPad>, InputError> {
        let n = PLUGGED.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(UinputPad::new(self.ids, n).map_err(refused)?))
    }
}

fn refused(e: io::Error) -> InputError {
    match e.kind() {
        io::ErrorKind::PermissionDenied => InputError::DriverUnavailable(
            "this user can't make virtual controllers yet: Settings → Drivers sets that up".into(),
        ),
        io::ErrorKind::NotFound => {
            InputError::DriverUnavailable("this system has no uinput device".into())
        }
        _ => InputError::DriverUnavailable(format!("uinput: {e}")),
    }
}

/// What the rumble thread shares with the pad.
#[derive(Default)]
struct Feedback {
    /// The motors as last changed, until taken.
    changed: Option<Rumble>,
}

struct UinputPad {
    device: Arc<Mutex<VirtualDevice>>,
    feedback: Arc<Mutex<Feedback>>,
    stop: Arc<AtomicBool>,
    /// Written to wake the rumble thread for `stop`.
    wake: Arc<OwnedFd>,
    thread: Option<JoinHandle<()>>,
}

impl UinputPad {
    fn new(ids: (u16, u16), n: u32) -> io::Result<Self> {
        let keys: AttributeSet<KeyCode> = [
            KeyCode::BTN_SOUTH,
            KeyCode::BTN_EAST,
            KeyCode::BTN_NORTH,
            KeyCode::BTN_WEST,
            KeyCode::BTN_TL,
            KeyCode::BTN_TR,
            KeyCode::BTN_SELECT,
            KeyCode::BTN_START,
            KeyCode::BTN_MODE,
            KeyCode::BTN_THUMBL,
            KeyCode::BTN_THUMBR,
        ]
        .into_iter()
        .collect();
        let stick = AbsInfo::new(0, -32768, 32767, 16, 128, 0);
        let trigger = AbsInfo::new(0, 0, 255, 0, 0, 0);
        let hat = AbsInfo::new(0, -1, 1, 0, 0, 0);
        let ff: AttributeSet<FFEffectCode> = [FFEffectCode::FF_RUMBLE].into_iter().collect();
        let phys = CString::new(format!("{PHYS_PREFIX}{}/pad{n}", std::process::id()))
            .expect("no NUL in it");
        let mut builder = VirtualDevice::builder()?
            .name(NAME)
            .input_id(InputId::new(BusType::BUS_USB, ids.0, ids.1, VERSION))
            .with_phys(&phys)?
            .with_keys(&keys)?;
        for (axis, info) in [
            (AbsoluteAxisCode::ABS_X, stick),
            (AbsoluteAxisCode::ABS_Y, stick),
            (AbsoluteAxisCode::ABS_Z, trigger),
            (AbsoluteAxisCode::ABS_RX, stick),
            (AbsoluteAxisCode::ABS_RY, stick),
            (AbsoluteAxisCode::ABS_RZ, trigger),
            (AbsoluteAxisCode::ABS_HAT0X, hat),
            (AbsoluteAxisCode::ABS_HAT0Y, hat),
        ] {
            builder = builder.with_absolute_axis(&UinputAbsSetup::new(axis, info))?;
        }
        let device = builder.with_ff(&ff)?.with_ff_effects_max(EFFECTS).build()?;
        // The rumble thread waits on it with poll(), then reads what's there.
        rustix::io::ioctl_fionbio(device.as_fd(), true)?;
        let device = Arc::new(Mutex::new(device));
        let feedback = Arc::new(Mutex::new(Feedback::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(rustix::event::eventfd(
            0,
            rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK,
        )?);
        let thread = {
            let (device, feedback, stop, wake) =
                (device.clone(), feedback.clone(), stop.clone(), wake.clone());
            std::thread::Builder::new()
                .name("pad-rumble".into())
                .spawn(move || rumble_loop(&device, &feedback, &stop, &wake))?
        };
        Ok(Self {
            device,
            feedback,
            stop,
            wake,
            thread: Some(thread),
        })
    }
}

impl VirtualPad for UinputPad {
    fn submit(&mut self, state: &PadState) -> Result<(), InputError> {
        let events = events(state);
        self.device
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .emit(&events)
            .map_err(|e| InputError::Injection(format!("uinput: {e}")))
    }

    fn take_rumble(&mut self) -> Option<Rumble> {
        self.feedback
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .changed
            .take()
    }
}

impl Drop for UinputPad {
    fn drop(&mut self) {
        // The thread wakes and stops (within `STOP_CHECK` should the write
        // fail); the device goes with the last reference, the kernel
        // releasing whatever it held.
        self.stop.store(true, Ordering::Relaxed);
        let _ = rustix::io::write(&*self.wake, &1u64.to_ne_bytes());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// `state` as the pad's events: `xpad`'s codes (its X is `BTN_NORTH`, its Y
/// `BTN_WEST`), the D-pad as a hat, sticks with Y pointing down as evdev
/// wants. Guide is never pressed: the seat router blocks it anyway.
fn events(state: &PadState) -> Vec<InputEvent> {
    let b = state.buttons;
    let key = |code: KeyCode, on: bool| InputEvent::new(EventType::KEY.0, code.0, i32::from(on));
    let abs =
        |code: AbsoluteAxisCode, value: i32| InputEvent::new(EventType::ABSOLUTE.0, code.0, value);
    let down = |y: i16| -i32::from(y).clamp(-32767, 32767);
    let hat = |minus: PadButtons, plus: PadButtons| {
        i32::from(b.contains(plus)) - i32::from(b.contains(minus))
    };
    vec![
        key(KeyCode::BTN_SOUTH, b.contains(PadButtons::A)),
        key(KeyCode::BTN_EAST, b.contains(PadButtons::B)),
        key(KeyCode::BTN_NORTH, b.contains(PadButtons::X)),
        key(KeyCode::BTN_WEST, b.contains(PadButtons::Y)),
        key(KeyCode::BTN_TL, b.contains(PadButtons::LEFT_SHOULDER)),
        key(KeyCode::BTN_TR, b.contains(PadButtons::RIGHT_SHOULDER)),
        key(KeyCode::BTN_SELECT, b.contains(PadButtons::BACK)),
        key(KeyCode::BTN_START, b.contains(PadButtons::START)),
        key(KeyCode::BTN_THUMBL, b.contains(PadButtons::LEFT_THUMB)),
        key(KeyCode::BTN_THUMBR, b.contains(PadButtons::RIGHT_THUMB)),
        abs(AbsoluteAxisCode::ABS_X, i32::from(state.left_stick.0)),
        abs(AbsoluteAxisCode::ABS_Y, down(state.left_stick.1)),
        abs(AbsoluteAxisCode::ABS_RX, i32::from(state.right_stick.0)),
        abs(AbsoluteAxisCode::ABS_RY, down(state.right_stick.1)),
        abs(AbsoluteAxisCode::ABS_Z, i32::from(state.left_trigger)),
        abs(AbsoluteAxisCode::ABS_RZ, i32::from(state.right_trigger)),
        abs(
            AbsoluteAxisCode::ABS_HAT0X,
            hat(PadButtons::DPAD_LEFT, PadButtons::DPAD_RIGHT),
        ),
        abs(
            AbsoluteAxisCode::ABS_HAT0Y,
            hat(PadButtons::DPAD_UP, PadButtons::DPAD_DOWN),
        ),
    ]
}

/// One uploaded effect: its motors, and how long it plays once started
/// (`None`: until stopped).
#[derive(Clone, Copy)]
struct Effect {
    rumble: Rumble,
    length: Option<Duration>,
}

/// Answers the game's uploads and erases, follows which effect plays, and
/// reports the motors whenever they change, until `stop` (with `wake`
/// written, so it stops at once).
fn rumble_loop(
    device: &Mutex<VirtualDevice>,
    feedback: &Mutex<Feedback>,
    stop: &AtomicBool,
    wake: &OwnedFd,
) {
    let mut effects: HashMap<u16, Effect> = HashMap::new();
    // What plays now, and until when.
    let mut playing: Option<(u16, Option<Instant>)> = None;
    let mut motors = Rumble::default();
    // Whether reading the device fails (logged once).
    let mut failing = false;
    let fd = match device
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_fd()
        .try_clone_to_owned()
    {
        Ok(fd) => fd,
        Err(e) => {
            tracing::warn!("a virtual pad's rumble can't be followed: {e}");
            return;
        }
    };
    while !stop.load(Ordering::Relaxed) {
        let wait = playing
            .and_then(|(_, until)| until)
            .map_or(STOP_CHECK, |until| {
                until
                    .saturating_duration_since(Instant::now())
                    .min(STOP_CHECK)
            });
        let mut fds = [
            rustix::event::PollFd::new(&fd, rustix::event::PollFlags::IN),
            rustix::event::PollFd::new(wake, rustix::event::PollFlags::IN),
        ];
        let timeout = rustix::event::Timespec::try_from(wait).ok();
        let _ = rustix::event::poll(&mut fds, timeout.as_ref());
        {
            let mut device = device.lock().unwrap_or_else(PoisonError::into_inner);
            let fetched = device
                .fetch_events()
                .map(|events| events.collect::<Vec<InputEvent>>());
            let events = match fetched {
                Ok(events) => events,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => Vec::new(),
                // Kept going: a game's next upload waits on an answer.
                Err(e) => {
                    if !failing {
                        tracing::warn!("a virtual pad's rumble can't be read: {e}");
                        failing = true;
                    }
                    drop(device);
                    std::thread::sleep(STOP_CHECK);
                    continue;
                }
            };
            failing = false;
            for event in events {
                match event.destructure() {
                    EventSummary::UInput(event, UInputCode::UI_FF_UPLOAD, _) => {
                        let Ok(mut upload) = device.process_ff_upload(event) else {
                            continue;
                        };
                        let data = upload.effect();
                        let rumble = match data.kind {
                            FFEffectKind::Rumble {
                                strong_magnitude,
                                weak_magnitude,
                            } => Rumble {
                                low_frequency: (strong_magnitude >> 8) as u8,
                                high_frequency: (weak_magnitude >> 8) as u8,
                            },
                            // Only rumble is offered; anything else stays still.
                            _ => Rumble::default(),
                        };
                        // Played `count` times, each after its delay
                        // (approximated as one span); 0 plays until stopped.
                        let replay = data.replay;
                        let length = (replay.length > 0).then(|| {
                            Duration::from_millis(u64::from(replay.length))
                                + Duration::from_millis(u64::from(replay.delay))
                        });
                        effects.insert(upload.effect_id() as u16, Effect { rumble, length });
                        upload.set_retval(0);
                        // An effect changed while it plays changes the motors.
                        if let Some((id, _)) = playing
                            && id == upload.effect_id() as u16
                        {
                            set(&mut motors, rumble, feedback);
                        }
                    }
                    EventSummary::UInput(event, UInputCode::UI_FF_ERASE, _) => {
                        if let Ok(mut erase) = device.process_ff_erase(event) {
                            let id = erase.effect_id() as u16;
                            effects.remove(&id);
                            erase.set_retval(0);
                            if playing.is_some_and(|(p, _)| p == id) {
                                playing = None;
                                set(&mut motors, Rumble::default(), feedback);
                            }
                        }
                    }
                    // Play (1 or more) or stop (0) an effect by its id; codes
                    // past the effects are settings (gain, autocenter).
                    EventSummary::ForceFeedback(_, code, value) if u32::from(code.0) < EFFECTS => {
                        let id = code.0;
                        match effects.get(&id) {
                            Some(effect) if value > 0 => {
                                let count = u32::try_from(value).unwrap_or(1).min(1000);
                                playing =
                                    Some((id, effect.length.map(|l| Instant::now() + l * count)));
                                set(&mut motors, effect.rumble, feedback);
                            }
                            _ if playing.is_some_and(|(p, _)| p == id) => {
                                playing = None;
                                set(&mut motors, Rumble::default(), feedback);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
        // An effect with a length stops by itself.
        if let Some((_, Some(until))) = playing
            && Instant::now() >= until
        {
            playing = None;
            set(&mut motors, Rumble::default(), feedback);
        }
    }
}

fn set(motors: &mut Rumble, to: Rumble, feedback: &Mutex<Feedback>) {
    if *motors != to {
        *motors = to;
        feedback
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .changed = Some(to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(events: &[InputEvent], kind: EventType, code: u16) -> i32 {
        events
            .iter()
            .find(|e| e.event_type() == kind && e.code() == code)
            .map(InputEvent::value)
            .expect("the event")
    }

    /// Settings shows the very command Set up runs.
    #[test]
    fn the_settings_page_shows_the_command_set_up_runs() {
        let page = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../apps/desktop/src/settings/sections.tsx"),
        )
        .expect("the Settings page's source");
        let quoted = format!("\"{}\"", set_up_by_hand().replace('"', "\\\""));
        assert!(
            page.contains(&quoted),
            "UINPUT_BY_HAND differs from {quoted}"
        );
    }

    #[test]
    fn a_state_becomes_xpads_events() {
        let state = PadState {
            buttons: PadButtons::A | PadButtons::Y | PadButtons::DPAD_LEFT | PadButtons::DPAD_DOWN,
            left_trigger: 200,
            right_trigger: 0,
            left_stick: (1000, i16::MIN),
            right_stick: (i16::MAX, 5),
        };
        let e = events(&state);
        assert_eq!(value(&e, EventType::KEY, KeyCode::BTN_SOUTH.0), 1);
        assert_eq!(
            value(&e, EventType::KEY, KeyCode::BTN_WEST.0),
            1,
            "xpad's Y"
        );
        assert_eq!(value(&e, EventType::KEY, KeyCode::BTN_NORTH.0), 0);
        assert_eq!(
            value(&e, EventType::ABSOLUTE, AbsoluteAxisCode::ABS_HAT0X.0),
            -1
        );
        assert_eq!(
            value(&e, EventType::ABSOLUTE, AbsoluteAxisCode::ABS_HAT0Y.0),
            1
        );
        // Up on the stick is down on evdev's axis, without overflowing.
        assert_eq!(
            value(&e, EventType::ABSOLUTE, AbsoluteAxisCode::ABS_Y.0),
            32767
        );
        assert_eq!(
            value(&e, EventType::ABSOLUTE, AbsoluteAxisCode::ABS_RY.0),
            -5
        );
        assert_eq!(
            value(&e, EventType::ABSOLUTE, AbsoluteAxisCode::ABS_Z.0),
            200
        );
        // Guide is never on the device's wire, pressed or not.
        assert!(
            !e.iter()
                .any(|e| e.event_type() == EventType::KEY && e.code() == KeyCode::BTN_MODE.0)
        );
        assert!(
            !events(&PadState {
                buttons: PadButtons::GUIDE,
                ..PadState::NEUTRAL
            })
            .iter()
            .any(|e| e.code() == KeyCode::BTN_MODE.0 && e.event_type() == EventType::KEY)
        );
    }

    #[test]
    fn neutral_releases_everything() {
        let e = events(&PadState::NEUTRAL);
        assert!(e.iter().all(|e| e.value() == 0));
    }

    /// Needs write access to `/dev/uinput` (Settings → Drivers, or root):
    /// a pad is made, named ours, and reads back what it was given.
    #[test]
    #[ignore = "needs /dev/uinput"]
    fn a_pad_is_made_named_and_read_back() {
        let factory = UinputPads::new(couch_input::DEV_PAD_IDS);
        let mut pad = factory.plug().expect("a pad");
        std::thread::sleep(Duration::from_millis(300));
        // Found as a stream window would: its event node, ours by `phys`.
        let path = std::fs::read_dir("/sys/class/input")
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| format!("/dev/input/{}", e.file_name().to_string_lossy()))
            .find(|p| p.contains("/event") && is_ours(p) == Some(true))
            .expect("our pad among the input devices");
        let mut reader = evdev::Device::open(&path).expect("reading it (as its owner, root here)");
        assert_eq!(reader.input_id().vendor(), couch_input::DEV_PAD_IDS.0);
        assert_eq!(reader.name(), Some(NAME));
        pad.submit(&PadState {
            buttons: PadButtons::B,
            ..PadState::NEUTRAL
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let keys = reader.get_key_state().unwrap();
        assert!(keys.contains(KeyCode::BTN_EAST));
        pad.submit(&PadState::NEUTRAL).unwrap();

        // A game's rumble: uploaded (the pad answers), played, stopped.
        let mut effect = reader
            .upload_ff_effect(evdev::FFEffectData {
                direction: 0,
                trigger: evdev::FFTrigger::default(),
                replay: evdev::FFReplay {
                    length: 0,
                    delay: 0,
                },
                kind: FFEffectKind::Rumble {
                    strong_magnitude: 0x8000,
                    weak_magnitude: 0x4000,
                },
            })
            .expect("the pad answers the upload");
        effect.play(1).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            pad.take_rumble(),
            Some(Rumble {
                low_frequency: 0x80,
                high_frequency: 0x40
            })
        );
        effect.stop().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(pad.take_rumble(), Some(Rumble::default()));
        drop(effect);
        drop(pad);
    }
}
