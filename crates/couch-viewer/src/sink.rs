//! `--input-sink FILE`: a window that records the keyboard and mouse input it
//! receives, for `couch-cli input-sink`, which injects a script into it.
//! Keys arrive as SDL scancodes, which are HID usages, so the file shows
//! exactly what a game reading scancodes would see.
//!
//! One line per event: `ready` once the window has the focus and the mouse;
//! then `key USAGE down|up`, `button CODE down|up`, `motion DX DY` and
//! `wheel DX DY` (in 1/120 notches). Exits when closed, or after a minute.
//!
//! With `--grab` (M25b, desktop control), a person checks SDL's keyboard
//! grab: once ready, the window takes the system keys (the Windows key,
//! Alt+Tab and the rest) while it has the focus, and Ctrl+Alt+Shift+M gives
//! them and the mouse back, or takes them again (`grab on|off`). It runs for
//! three minutes.

use std::{
    fs::File,
    io::{LineWriter, Write},
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use sdl3::{
    event::{Event, WindowEvent},
    keyboard::{Mod, Scancode},
    mouse::MouseButton,
};

pub const TITLE: &str = "Couch Input Sink";

pub fn run(path: &Path, grab: bool) -> Result<()> {
    let mut out = LineWriter::new(File::create(path)?);
    if grab {
        // As the stream window does while the keyboard goes to the host:
        // Alt+F4 is a key to send, not a way to close this window.
        sdl3::hint::set("SDL_WINDOWS_CLOSE_ON_ALT_F4", "0");
    }
    let sdl = sdl3::init().map_err(|e| anyhow!("SDL init: {e}"))?;
    let video = sdl.video().map_err(|e| anyhow!("SDL video: {e}"))?;
    let mut window = if grab {
        // Out of the way of the instructions on the person's screen.
        video.window(TITLE, 480, 240).position(40, 40).build()?
    } else {
        video.window(TITLE, 480, 240).position_centered().build()?
    };
    window.raise();
    let mouse = sdl.mouse();
    let mut events = sdl.event_pump().map_err(|e| anyhow!("SDL events: {e}"))?;
    let started = Instant::now();
    let mut ready = false;
    let mut grabbed = false;
    let limit = Duration::from_secs(if grab { 180 } else { 60 });

    while started.elapsed() < limit {
        let Some(event) = events.wait_event_timeout(Duration::from_millis(50)) else {
            continue;
        };
        let line = match event {
            Event::Quit { .. } => {
                writeln!(out, "closed")?;
                break;
            }
            Event::Window {
                win_event: WindowEvent::FocusGained,
                ..
            } if !ready => {
                mouse.set_relative_mouse_mode(&window, true);
                ready = true;
                if grab {
                    grabbed = window.set_keyboard_grab(true);
                    format!("ready, grab {}", if grabbed { "on" } else { "failed" })
                } else {
                    "ready".to_string()
                }
            }
            Event::KeyDown {
                scancode: Some(s),
                keymod,
                repeat: false,
                ..
            } if grab && s == Scancode::M && chord(keymod) => {
                grabbed = !grabbed;
                window.set_keyboard_grab(grabbed);
                mouse.set_relative_mouse_mode(&window, grabbed);
                format!("grab {}", if grabbed { "on" } else { "off" })
            }
            Event::KeyDown {
                scancode: Some(s),
                repeat: false,
                ..
            } => format!("key {:#04x} down", s.to_i32()),
            Event::KeyUp {
                scancode: Some(s), ..
            } => format!("key {:#04x} up", s.to_i32()),
            Event::MouseButtonDown { mouse_btn, .. } => format!("button {} down", code(mouse_btn)),
            Event::MouseButtonUp { mouse_btn, .. } => format!("button {} up", code(mouse_btn)),
            Event::MouseMotion { xrel, yrel, .. } => format!("motion {xrel} {yrel}"),
            Event::MouseWheel { x, y, .. } => format!("wheel {} {}", x * 120.0, y * 120.0),
            _ => continue,
        };
        writeln!(out, "{line}")?;
    }
    Ok(())
}

/// Ctrl+Alt+Shift, either side.
fn chord(m: Mod) -> bool {
    m.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD)
        && m.intersects(Mod::LALTMOD | Mod::RALTMOD)
        && m.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD)
}

/// The wire code (`couch_proto::input::mouse_button`); SDL numbers middle
/// and right the other way round.
fn code(button: MouseButton) -> u8 {
    use couch_proto::input::mouse_button;
    match button {
        MouseButton::Left => mouse_button::LEFT,
        MouseButton::Right => mouse_button::RIGHT,
        MouseButton::Middle => mouse_button::MIDDLE,
        MouseButton::X1 => mouse_button::BACK,
        MouseButton::X2 => mouse_button::FORWARD,
        _ => 0,
    }
}
