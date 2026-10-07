//! Where on the desktop the host's overlay goes (M8): the work area (the
//! monitor without the taskbar) of the monitor showing the streamed window,
//! or of the streamed screen. Physical pixels, in desktop coordinates.

use std::ffi::c_void;

use windows::Win32::{
    Foundation::{HWND, POINT},
    Graphics::Gdi::{
        GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
        MonitorFromWindow,
    },
};

/// A rectangle on the desktop, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// The work area of the monitor showing most of `window` (a window handle
/// value); the primary monitor's if the window is gone.
pub fn of_window(window: isize) -> Option<Area> {
    // SAFETY: MonitorFromWindow accepts any handle value. For a window off
    // every monitor it returns the primary one; for a handle that isn't a
    // window, none, so the primary one is asked for below.
    let monitor =
        unsafe { MonitorFromWindow(HWND(window as *mut c_void), MONITOR_DEFAULTTOPRIMARY) };
    work_area(monitor).or_else(primary)
}

/// The work area of `monitor` (a monitor handle value, as screens are
/// listed); the primary monitor's if it is gone.
pub fn of_monitor(monitor: isize) -> Option<Area> {
    work_area(HMONITOR(monitor as *mut c_void)).or_else(primary)
}

/// The primary monitor's work area.
pub fn primary() -> Option<Area> {
    // SAFETY: no pointers; with MONITOR_DEFAULTTOPRIMARY it always returns
    // a monitor.
    work_area(unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) })
}

fn work_area(monitor: HMONITOR) -> Option<Area> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is a live MONITORINFO with its size set; a handle that
    // isn't a monitor makes the call fail, which we report as `None`.
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return None;
    }
    let r = info.rcWork;
    Some(Area {
        x: r.left,
        y: r.top,
        width: r.right - r.left,
        height: r.bottom - r.top,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_primary_monitor_has_an_area_and_strangers_fall_back_to_it() {
        // A machine without a display (a service session) has none to test.
        let Some(primary) = primary() else { return };
        assert!(primary.width > 0 && primary.height > 0);
        // Not a window: the primary monitor's.
        assert_eq!(of_window(0x7fff_0001), Some(primary));
        assert_eq!(of_monitor(0x7fff_0001), Some(primary));
    }
}
