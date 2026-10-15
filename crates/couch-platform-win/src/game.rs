//! The game behind a streamed window: whether its process still runs, and
//! which of its windows could replace the one being captured.
//!
//! Some games destroy their window and open a new one (switching display
//! mode, leaving a launcher); the host then follows the new window instead
//! of ending the session. When the process itself is gone, the game exited.

use std::{ffi::c_void, mem::size_of};

use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, HWND, RECT, WAIT_TIMEOUT},
    Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
    System::Threading::{
        OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        WaitForSingleObject,
    },
    UI::WindowsAndMessaging::{GetWindowRect, IsIconic},
};

use crate::{capture::is_cloaked, foreground::WindowId, process, top_windows};

/// Windows smaller than this (either side, in pixels) are splash screens,
/// tooltips or hidden helpers, never the game.
const MIN_SIDE: i32 = 200;

/// A running game's process, held open so its id can't be reused under us.
pub struct GameProcess {
    handle: HANDLE,
    pid: u32,
}

// SAFETY: a process handle is a kernel object reference, usable from any
// thread; we only wait on it and close it.
unsafe impl Send for GameProcess {}
// SAFETY: as above; `running` and `windows` only read.
unsafe impl Sync for GameProcess {}

impl Drop for GameProcess {
    fn drop(&mut self) {
        // SAFETY: we opened the handle and close it once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

impl GameProcess {
    /// The process behind `window`, if it still exists and we may watch it.
    /// A process running as administrator can still be watched: waiting on
    /// it needs no more than `SYNCHRONIZE`.
    pub fn of_window(window: WindowId) -> Option<Self> {
        let pid = crate::foreground::window_process(window)?;
        // SAFETY: opening a process by id; closed on drop.
        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                pid,
            )
        }
        .or_else(|_| {
            // SAFETY: as above, with the right waiting needs alone.
            unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }
        })
        .ok()?;
        Some(Self { handle, pid })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Whether the process still runs.
    pub fn running(&self) -> bool {
        // SAFETY: a live handle with SYNCHRONIZE; a zero timeout only polls.
        let result = unsafe { WaitForSingleObject(self.handle, 0) };
        result == WAIT_TIMEOUT
    }

    /// Windows of this process or its descendants that could be the game:
    /// visible, titled, not cloaked or minimized, at least 200 pixels each
    /// way. Largest first.
    /// The process may have exited already: a launcher's children still
    /// name it as their parent, so its tree is still found.
    pub fn windows(&self) -> Vec<WindowId> {
        let mut tree = vec![self.pid];
        tree.extend(process::descendants(self.pid).unwrap_or_default());
        let mut found: Vec<(i64, WindowId)> = top_windows::capturable()
            .into_iter()
            .filter(|&w| top_windows::process_id(w).is_some_and(|pid| tree.contains(&pid)))
            .filter(|&w| !top_windows::title(w).trim().is_empty())
            .filter_map(|window| {
                let area = usable_area(window)?;
                Some((area, window))
            })
            .collect();
        found.sort_by_key(|&(area, _)| std::cmp::Reverse(area));
        found.into_iter().map(|(_, window)| window).collect()
    }
}

/// Whether `pid` runs with more rights than we do (as administrator, while
/// we don't). Guests' keyboard and mouse can't reach such a window, and the
/// panic key's hook doesn't see keys while it is in front (measured in M6). A process whose
/// rights we may not read at all counts as elevated; one we can't open at
/// all, as not (we can't tell, and say nothing).
pub fn runs_elevated(pid: u32) -> bool {
    !token_elevated(std::process::id()).unwrap_or(false) && token_elevated(pid).unwrap_or(true)
}

/// `Some(elevated)`, `None` when the process's token can't be read, or
/// `Some(false)` when the process itself can't be opened.
fn token_elevated(pid: u32) -> Option<bool> {
    // SAFETY: opening a process by id for a query; closed below.
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return Some(false);
    };
    let mut token = HANDLE::default();
    // SAFETY: a process handle with query rights and a live out-parameter.
    let opened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) };
    // SAFETY: we opened it above and close it once.
    let _ = unsafe { CloseHandle(process) };
    opened.ok()?;
    let mut elevation = TOKEN_ELEVATION::default();
    let mut size = 0u32;
    // SAFETY: a token with query rights; the buffer is a live TOKEN_ELEVATION
    // of the size passed.
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
    };
    // SAFETY: we opened it above and close it once.
    let _ = unsafe { CloseHandle(token) };
    read.ok()?;
    Some(elevation.TokenIsElevated != 0)
}

/// The window's area, if it is big enough and showing.
fn usable_area(window: WindowId) -> Option<i64> {
    let hwnd = HWND(window.0 as *mut c_void);
    // SAFETY: accepts any handle value.
    if unsafe { IsIconic(hwnd) }.as_bool() || is_cloaked(window) {
        return None;
    }
    let mut rect = RECT::default();
    // SAFETY: the out-parameter is a live local.
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
    (width >= MIN_SIDE && height >= MIN_SIDE).then(|| i64::from(width) * i64::from(height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_that_never_existed_has_no_game() {
        assert!(GameProcess::of_window(WindowId(0)).is_none());
    }

    #[test]
    fn our_own_process_runs_and_its_tree_is_listed() {
        // SAFETY: opening our own process.
        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                std::process::id(),
            )
        }
        .unwrap();
        let me = GameProcess {
            handle,
            pid: std::process::id(),
        };
        assert!(me.running());
        // A test process has no windows of its own; the call must not fail.
        let _ = me.windows();
    }

    #[test]
    fn our_own_process_is_not_more_privileged_than_us() {
        assert!(!runs_elevated(std::process::id()));
        // No process has this id: nothing to warn about.
        assert!(!runs_elevated(u32::MAX - 2));
    }

    #[test]
    fn an_exited_process_is_not_running() {
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "exit 0"])
            .spawn()
            .unwrap();
        // SAFETY: opening a child we started.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, child.id()) }.unwrap();
        let game = GameProcess {
            handle,
            pid: child.id(),
        };
        child.wait().unwrap();
        assert!(!game.running());
        assert!(game.windows().is_empty());
    }
}
