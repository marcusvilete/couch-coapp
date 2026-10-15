//! Top-level windows a capture can take: visible, neither a tool window nor
//! a child window. (Until M9f the `windows-capture` crate listed them, with
//! these same rules; our own capture replaced it.)

use std::ffi::c_void;

use windows::Win32::{
    Foundation::{CloseHandle, HWND, LPARAM, RECT},
    System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GWL_EXSTYLE, GWL_STYLE, GetClientRect, GetWindowLongPtrW,
        GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, WS_CHILD,
        WS_EX_TOOLWINDOW,
    },
};
use windows_core::{BOOL, PWSTR};

use crate::foreground::WindowId;

/// Every capturable top-level window, in Windows' order (front to back).
pub fn capturable() -> Vec<WindowId> {
    let mut found: Vec<WindowId> = Vec::new();
    // SAFETY: the callback runs during this call only, and gets a pointer to
    // `found`, live for the whole call.
    let _ = unsafe { EnumWindows(Some(collect), LPARAM((&raw mut found) as isize)) };
    found
}

unsafe extern "system" fn collect(window: HWND, found: LPARAM) -> BOOL {
    // SAFETY: `capturable` passes a pointer to its live Vec, used only
    // during its EnumWindows call, on its thread.
    let found = unsafe { &mut *(found.0 as *mut Vec<WindowId>) };
    let window = WindowId(window.0 as isize);
    if is_capturable(window) {
        found.push(window);
    }
    BOOL::from(true)
}

/// Whether a capture can take `window`: visible, with a client area, and
/// neither a tool window nor a child window.
pub fn is_capturable(window: WindowId) -> bool {
    let hwnd = hwnd(window);
    let mut rect = RECT::default();
    // SAFETY: these calls take any handle value; for one that isn't a window
    // they fail or return 0, which reads as not capturable.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || GetClientRect(hwnd, &mut rect).is_err() {
            return false;
        }
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        ex_style & WS_EX_TOOLWINDOW.0 as isize == 0 && style & WS_CHILD.0 as isize == 0
    }
}

/// The window's title; empty if it has none.
pub fn title(window: WindowId) -> String {
    let hwnd = hwnd(window);
    // SAFETY: any handle value; 0 for none.
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    let Ok(len) = usize::try_from(len) else {
        return String::new();
    };
    if len == 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len + 1];
    // SAFETY: `buf` is live and its length is passed.
    let copied = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..usize::try_from(copied).unwrap_or(0).min(len)])
}

/// The process that owns the window.
pub fn process_id(window: WindowId) -> Option<u32> {
    let mut pid = 0;
    // SAFETY: a live out-parameter.
    unsafe { GetWindowThreadProcessId(hwnd(window), Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

/// The file name of a process's program (`witcher3.exe`). Works for a
/// process running as administrator too (limited query rights).
pub fn process_name(pid: u32) -> Option<String> {
    // SAFETY: the handle is closed below, on every path.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` is live, and `len` says how many characters it holds.
    let read = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    // SAFETY: opened above, closed once.
    let _ = unsafe { CloseHandle(process) };
    read.ok()?;
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    path.rsplit(['\\', '/']).next().map(str::to_string)
}

/// The first capturable window whose title contains `part`.
pub fn find_by_title(part: &str) -> Option<WindowId> {
    capturable().into_iter().find(|&w| title(w).contains(part))
}

fn hwnd(window: WindowId) -> HWND {
    HWND(window.0 as *mut c_void)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listed_windows_are_capturable_and_strangers_are_not() {
        for window in capturable() {
            assert!(is_capturable(window));
        }
        assert!(!is_capturable(WindowId(0x7fff_0001)));
        assert_eq!(title(WindowId(0x7fff_0001)), "");
        assert_eq!(process_id(WindowId(0x7fff_0001)), None);
    }

    #[test]
    fn this_process_has_a_name() {
        let name = process_name(std::process::id()).unwrap();
        assert!(name.to_lowercase().ends_with(".exe"), "{name}");
    }
}
