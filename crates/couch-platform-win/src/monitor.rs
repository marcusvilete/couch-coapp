//! The monitor showing the streamed window, or the streamed screen:
//! - its work area (the monitor without the taskbar), where the host's
//!   overlay goes (M8), in physical pixels and desktop coordinates;
//! - its refresh rate, which caps how many frames a capture gets (M9d).

use std::ffi::c_void;

use windows::Win32::{
    Foundation::{HWND, POINT},
    Graphics::Gdi::{
        DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW, HMONITOR,
        MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MONITORINFOEXW, MonitorFromPoint, MonitorFromWindow,
    },
};
use windows_core::PCWSTR;

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
    work_area(showing(window)).or_else(primary)
}

/// The refresh rate, in whole hertz as Windows reports it (59 for 59.94),
/// of the monitor showing most of `window`; the primary monitor's if the
/// window is gone. `None` if unknown.
pub fn refresh_of_window(window: isize) -> Option<u32> {
    refresh(showing(window)).or_else(|| refresh(primary_monitor()))
}

/// The refresh rate of `monitor` (a monitor handle value, as screens are
/// listed). `None` if unknown.
pub fn refresh_of_monitor(monitor: isize) -> Option<u32> {
    refresh(HMONITOR(monitor as *mut c_void))
}

fn showing(window: isize) -> HMONITOR {
    // SAFETY: MonitorFromWindow accepts any handle value. For a window off
    // every monitor it returns the primary one; for a handle that isn't a
    // window, none, which the callers' calls then fail on.
    unsafe { MonitorFromWindow(HWND(window as *mut c_void), MONITOR_DEFAULTTOPRIMARY) }
}

fn refresh(monitor: HMONITOR) -> Option<u32> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: `info` is a live MONITORINFOEXW, whose size tells the call it
    // may fill the device name too; a handle that isn't a monitor makes it
    // fail. The device name it filled is NUL-terminated within the array,
    // and `mode` is a live DEVMODEW with its size set.
    let found = unsafe {
        GetMonitorInfoW(monitor, (&raw mut info).cast()).as_bool()
            && EnumDisplaySettingsW(
                PCWSTR(info.szDevice.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut mode,
            )
            .as_bool()
    };
    // 0 and 1 mean "the hardware's default", which says nothing.
    (found && mode.dmDisplayFrequency > 1).then_some(mode.dmDisplayFrequency)
}

/// The work area of `monitor` (a monitor handle value, as screens are
/// listed); the primary monitor's if it is gone.
pub fn of_monitor(monitor: isize) -> Option<Area> {
    work_area(HMONITOR(monitor as *mut c_void)).or_else(primary)
}

/// The primary monitor's work area.
pub fn primary() -> Option<Area> {
    work_area(primary_monitor())
}

fn primary_monitor() -> HMONITOR {
    // SAFETY: no pointers; with MONITOR_DEFAULTTOPRIMARY it always returns
    // a monitor.
    unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }
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

    #[test]
    fn the_primary_monitor_has_a_refresh_rate() {
        let Some(_) = primary() else { return };
        // Not a window: the primary monitor's.
        let hz = refresh_of_window(0x7fff_0001);
        assert!(hz.is_some_and(|hz| (24..=1000).contains(&hz)), "{hz:?}");
        assert_eq!(refresh_of_monitor(primary_monitor().0 as isize), hz);
    }
}
