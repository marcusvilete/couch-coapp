//! The main window's size and place, remembered per profile in
//! `window.json` (M21). The window is created hidden, put back where it was
//! (or, the first time, sized to fit the screen), then shown, so it never
//! jumps at start. Only the main window: the host overlay places itself, and
//! must never be shown or moved by anything else.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{LogicalSize, Monitor, PhysicalPosition, PhysicalSize, WebviewWindow};

const FILE: &str = "window.json";
/// The first run's size, in logical pixels, if the screen has room.
const FIRST_WIDTH: f64 = 1200.0;
const FIRST_HEIGHT: f64 = 760.0;
/// At least this much of a remembered window must be on a screen, in
/// physical pixels, or it opens on the main screen instead.
const VISIBLE: i64 = 120;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

pub fn path(profile_dir: &Path) -> PathBuf {
    profile_dir.join(FILE)
}

fn load(file: &Path) -> Option<Placement> {
    serde_json::from_slice(&std::fs::read(file).ok()?).ok()
}

/// Puts the hidden window back where it was, then shows it.
pub fn restore(window: &WebviewWindow, file: &Path) {
    let monitors = window.available_monitors().unwrap_or_default();
    match load(file).filter(|p| on_screen(p, &monitors)) {
        Some(p) => {
            let _ = window.set_size(PhysicalSize::new(p.width, p.height));
            let _ = window.set_position(PhysicalPosition::new(p.x, p.y));
            if p.maximized {
                let _ = window.maximize();
            }
        }
        None => first_size(window),
    }
    let _ = window.show();
    let _ = window.set_focus();
}

/// The first run: 1200×760, shrunk to fit the main screen's work area (a
/// 1366×768 screen, or 1080p at 150%), centred.
fn first_size(window: &WebviewWindow) {
    if let Ok(Some(monitor)) = window.primary_monitor() {
        let scale = monitor.scale_factor();
        let area = monitor.work_area();
        let room_w = f64::from(area.size.width) / scale;
        let room_h = f64::from(area.size.height) / scale;
        let size = LogicalSize::new(
            FIRST_WIDTH.min(room_w - 32.0),
            FIRST_HEIGHT.min(room_h - 32.0),
        );
        let _ = window.set_size(size);
    }
    let _ = window.center();
}

fn on_screen(p: &Placement, monitors: &[Monitor]) -> bool {
    monitors.iter().any(|m| {
        let area = m.work_area();
        let left = i64::from(p.x).max(i64::from(area.position.x));
        let right = (i64::from(p.x) + i64::from(p.width))
            .min(i64::from(area.position.x) + i64::from(area.size.width));
        let top = i64::from(p.y).max(i64::from(area.position.y));
        let bottom = (i64::from(p.y) + i64::from(p.height))
            .min(i64::from(area.position.y) + i64::from(area.size.height));
        right - left >= VISIBLE && bottom - top >= VISIBLE / 2
    })
}

/// Remembers where the window is. A minimized window keeps what was saved;
/// a maximized one keeps its last normal size, maximized.
pub fn save(window: &WebviewWindow, file: &Path) {
    if window.is_minimized().unwrap_or(true) {
        return;
    }
    let maximized = window.is_maximized().unwrap_or(false);
    let placement = if maximized {
        match load(file) {
            Some(p) => Placement {
                maximized: true,
                ..p
            },
            None => return,
        }
    } else {
        let (Ok(position), Ok(size)) = (window.outer_position(), window.inner_size()) else {
            return;
        };
        Placement {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
            maximized: false,
        }
    };
    if let Ok(json) = serde_json::to_vec(&placement)
        && let Err(e) = std::fs::write(file, json)
    {
        tracing::warn!("could not remember the window's place: {e}");
    }
}
