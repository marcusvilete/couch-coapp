//! `--input-sink FILE`: a window that records the keyboard and mouse input it
//! receives, for `couch-cli input-sink`, which injects a script into it.
//! Keys arrive as SDL scancodes, which are HID usages, so the file shows
//! exactly what a game reading scancodes would see.
//!
//! One line per event: `ready` once the window has the focus and the mouse;
//! then `key USAGE down|up`, `button CODE down|up`, `motion DX DY` and
//! `wheel DX DY` (in 1/120 notches). Exits when closed, or after a minute.

use std::{
    fs::File,
    io::{LineWriter, Write},
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use sdl3::{
    event::{Event, WindowEvent},
    mouse::MouseButton,
};

pub const TITLE: &str = "Couch Input Sink";

pub fn run(path: &Path) -> Result<()> {
    let mut out = LineWriter::new(File::create(path)?);
    let sdl = sdl3::init().map_err(|e| anyhow!("SDL init: {e}"))?;
    let video = sdl.video().map_err(|e| anyhow!("SDL video: {e}"))?;
    let mut window = video.window(TITLE, 480, 240).position_centered().build()?;
    window.raise();
    let mouse = sdl.mouse();
    let mut events = sdl.event_pump().map_err(|e| anyhow!("SDL events: {e}"))?;
    let started = Instant::now();
    let mut ready = false;

    while started.elapsed() < Duration::from_secs(60) {
        let Some(event) = events.wait_event_timeout(Duration::from_millis(50)) else {
            continue;
        };
        let line = match event {
            Event::Quit { .. } => break,
            Event::Window {
                win_event: WindowEvent::FocusGained,
                ..
            } if !ready => {
                mouse.set_relative_mouse_mode(&window, true);
                ready = true;
                "ready".to_string()
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
