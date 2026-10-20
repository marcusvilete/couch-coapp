//! Every gamepad SDL finds, with what SDL says of it, none of them opened:
//! for `couch-cli pad-origins` (P9), which looks each one up in Windows'
//! device tree. SDL runs on a thread of its own for the call, so nothing
//! else in the process may start SDL: never call it from the engine's
//! process, where SDL lives only on the host's reader thread.

use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

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
    Ok(ids
        .into_iter()
        .map(|id| SurveyedPad {
            name: gamepads.name_for_id(id).unwrap_or_default(),
            vendor: gamepads.vendor_for_id(id).unwrap_or(0),
            product: gamepads.product_for_id(id).unwrap_or(0),
            path: gamepads.path_for_id(id).ok().filter(|p| !p.is_empty()),
            kind: format!("{:?}", gamepads.type_for_id(id)),
            real_kind: format!("{:?}", gamepads.real_type_for_id(id)),
        })
        .collect())
}
