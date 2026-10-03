//! Where keyboard and mouse input would land: the foreground window, and the
//! window under the pointer. Keyboard and mouse go in only while the captured
//! game window is in front, and clicks only while the pointer is over it.
//!
//! Windows are compared at their root (`GA_ROOT`), so a game's child windows
//! count as the game, but another top-level window of the same process (a
//! second browser window, another app hosted by `ApplicationFrameHost`) does
//! not.

use windows::Win32::{
    Foundation::{HWND, POINT},
    UI::WindowsAndMessaging::{
        GA_ROOT, GetAncestor, GetCursorPos, GetForegroundWindow, GetWindowThreadProcessId,
        WindowFromPoint,
    },
};

/// A top-level window, by its handle value. 0 is no window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(pub isize);

fn root(hwnd: HWND) -> Option<WindowId> {
    if hwnd.is_invalid() {
        return None;
    }
    // SAFETY: GetAncestor accepts any handle value and returns null for
    // windows that no longer exist.
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    (!root.is_invalid()).then_some(WindowId(root.0 as isize))
}

/// Whether `window` (or one of its children) is the foreground window.
pub fn is_foreground(window: WindowId) -> bool {
    // SAFETY: no arguments.
    root(unsafe { GetForegroundWindow() }) == Some(window)
}

/// Whether the mouse pointer is over `window` (or one of its children), so a
/// click would reach it.
pub fn pointer_over(window: WindowId) -> bool {
    let mut point = POINT::default();
    // SAFETY: `point` is a live local for the out-parameter.
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        return false;
    }
    // SAFETY: takes the point by value.
    root(unsafe { WindowFromPoint(point) }) == Some(window)
}

/// The process id behind the foreground window, if there is one.
pub fn foreground_process() -> Option<u32> {
    // SAFETY: both calls take no pointers but the out-parameter, which points
    // at a live local.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        (pid != 0).then_some(pid)
    }
}
