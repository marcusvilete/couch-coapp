//! Every gamepad SDL finds, with what SDL says of it, none of them opened:
//! for `couch-cli pad-origins` (P9), which looks each one up in Windows'
//! device tree; and [`watch`], gamepads coming and going and those opened
//! changing, for `couch-cli remapper-probe` (M22). SDL runs on a thread of
//! its own for the call, so nothing else in the process may start SDL:
//! never call it from the engine's process, where SDL lives only on the
//! host's reader thread.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use couch_proto::input::PadSnapshot;
use sdl3::{GamepadSubsystem, event::Event, gamepad::Gamepad, joystick::JoystickId};

/// One gamepad as SDL lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurveyedPad {
    pub name: String,
    pub vendor: u16,
    pub product: u16,
    /// SDL's path for it: a device interface path for HID and raw input
    /// devices, something else (or nothing) for its other backends.
    pub path: Option<String>,
    /// SDL's idea of its kind, and the kind it really is (they differ when
    /// SDL's mapping says so).
    pub kind: String,
    pub real_kind: String,
}

/// Starts SDL as the host's reader does, lets it find the gamepads for
/// `settle`, and lists them.
pub fn survey(settle: Duration) -> Result<Vec<SurveyedPad>, String> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("pad-survey".into())
        .spawn(move || {
            let _ = tx.send(run(settle));
        })
        .map_err(|e| format!("starting its thread: {e}"))?;
    rx.recv()
        .map_err(|_| "its thread ended early".to_string())?
}

fn run(settle: Duration) -> Result<Vec<SurveyedPad>, String> {
    // As the host's reader and the stream window.
    sdl3::hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
    let sdl = sdl3::init().map_err(|e| format!("SDL: {e}"))?;
    let gamepads = sdl.gamepad().map_err(|e| format!("SDL gamepads: {e}"))?;
    let mut events = sdl.event_pump().map_err(|e| format!("SDL events: {e}"))?;
    let started = Instant::now();
    while started.elapsed() < settle {
        for _ in events.poll_iter() {}
        thread::sleep(Duration::from_millis(10));
    }
    let ids = gamepads.gamepads().map_err(|e| format!("SDL: {e}"))?;
    Ok(ids.into_iter().map(|id| surveyed(&gamepads, id)).collect())
}

fn surveyed(gamepads: &GamepadSubsystem, id: JoystickId) -> SurveyedPad {
    SurveyedPad {
        name: gamepads.name_for_id(id).unwrap_or_default(),
        vendor: gamepads.vendor_for_id(id).unwrap_or(0),
        product: gamepads.product_for_id(id).unwrap_or(0),
        path: gamepads.path_for_id(id).ok().filter(|p| !p.is_empty()),
        kind: format!("{:?}", gamepads.type_for_id(id)),
        real_kind: format!("{:?}", gamepads.real_type_for_id(id)),
    }
}

/// What [`watch`] sees, each pad by SDL's number for it.
#[derive(Debug)]
pub enum Seen<'a> {
    Added(u32, &'a SurveyedPad),
    Removed(u32),
    /// A pad opened for the watch changed.
    Changed(u32, PadSnapshot),
    /// Every 100 ms.
    Tick,
}

/// What a [`watch`]'s callback asks for.
#[derive(Debug, Default)]
pub struct WatchControl {
    open: Vec<u32>,
    rumble: Vec<u32>,
}

impl WatchControl {
    /// Opens the pad, to see its changes.
    pub fn open(&mut self, which: u32) {
        self.open.push(which);
    }

    /// Shakes an opened pad for half a second.
    pub fn rumble(&mut self, which: u32) {
        self.rumble.push(which);
    }
}

/// Watches the gamepads for `secs`, or until `stop` is set, on a thread of
/// SDL's own (M22's probe): `f` hears of each one plugged and unplugged
/// (those plugged already come first), and of the changes of those it opens.
pub fn watch(
    secs: Duration,
    stop: Arc<AtomicBool>,
    f: impl FnMut(Seen<'_>, &mut WatchControl) + Send + 'static,
) -> Result<(), String> {
    thread::Builder::new()
        .name("pad-watch".into())
        .spawn(move || watching(secs, &stop, f))
        .map_err(|e| format!("starting its thread: {e}"))?
        .join()
        .map_err(|_| "its thread panicked".to_string())?
}

fn watching(
    secs: Duration,
    stop: &AtomicBool,
    mut f: impl FnMut(Seen<'_>, &mut WatchControl),
) -> Result<(), String> {
    sdl3::hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
    let sdl = sdl3::init().map_err(|e| format!("SDL: {e}"))?;
    let gamepads = sdl.gamepad().map_err(|e| format!("SDL gamepads: {e}"))?;
    let mut events = sdl.event_pump().map_err(|e| format!("SDL events: {e}"))?;
    let mut open: Vec<(u32, Gamepad, PadSnapshot)> = Vec::new();
    let mut known: Vec<u32> = Vec::new();
    let started = Instant::now();
    let mut tick = Instant::now();
    while started.elapsed() < secs && !stop.load(Ordering::Relaxed) {
        let mut control = WatchControl::default();
        let mut next = events.wait_event_timeout(Duration::from_millis(5));
        while let Some(event) = next {
            match event {
                Event::GamepadAdded { which, .. } if !known.contains(&which.raw()) => {
                    known.push(which.raw());
                    f(
                        Seen::Added(which.raw(), &surveyed(&gamepads, which)),
                        &mut control,
                    );
                }
                Event::GamepadRemoved { which, .. } => {
                    known.retain(|k| *k != which.raw());
                    open.retain(|p| p.0 != which.raw());
                    f(Seen::Removed(which.raw()), &mut control);
                }
                _ => {}
            }
            next = events.poll_event();
        }
        for (which, pad, last) in &mut open {
            let now = crate::read_pad(pad);
            if now != *last {
                *last = now;
                f(Seen::Changed(*which, now), &mut control);
            }
        }
        if tick.elapsed() >= Duration::from_millis(100) {
            tick = Instant::now();
            f(Seen::Tick, &mut control);
        }
        for which in control.open {
            if open.iter().any(|p| p.0 == which) {
                continue;
            }
            match gamepads.open(JoystickId::new(which)) {
                Ok(pad) => {
                    let now = crate::read_pad(&pad);
                    open.push((which, pad, now));
                }
                Err(e) => tracing::warn!("opening pad {which}: {e}"),
            }
        }
        for which in control.rumble {
            if let Some((_, pad, _)) = open.iter_mut().find(|p| p.0 == which)
                && let Err(e) = pad.set_rumble(0xC000, 0xC000, 500)
            {
                tracing::warn!("rumbling pad {which}: {e}");
            }
        }
    }
    Ok(())
}
