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
        AllowSetForegroundWindow, GA_ROOT, GetAncestor, GetCursorPos, GetForegroundWindow,
        GetWindowThreadProcessId, IsWindow, WindowFromPoint,
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

/// Whether `window` still exists: a closed game's window doesn't.
pub fn window_exists(window: WindowId) -> bool {
    // SAFETY: IsWindow accepts any handle value.
    unsafe { IsWindow(Some(HWND(window.0 as *mut _))) }.as_bool()
}

/// Lets process `pid` bring a window of its own to the front next (P11: the
/// stream window, when the app's Show window is clicked). Windows refuses a
/// background process the foreground unless the one in front allows it; the
/// app is in front when its button is clicked. `false` if Windows said no
/// (the app isn't in front after all).
pub fn allow_foreground(pid: u32) -> bool {
    // SAFETY: takes a process id by value.
    unsafe { AllowSetForegroundWindow(pid) }.is_ok()
}

/// The process that owns `window`, if it still exists.
pub fn window_process(window: WindowId) -> Option<u32> {
    let mut pid = 0u32;
    // SAFETY: accepts any handle value; the out-parameter is a live local.
    unsafe { GetWindowThreadProcessId(HWND(window.0 as *mut _), Some(&mut pid)) };
    (pid != 0).then_some(pid)
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
