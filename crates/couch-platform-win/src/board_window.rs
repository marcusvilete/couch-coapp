//! The host's board overlay over the game (M27b, ADR 0013): a transparent
//! window of ours showing what's drawn on the stream, laid exactly over what
//! is captured, so the picture's own coordinates are the window's.
//!
//! The app makes the window (a webview, created unfocusable, click-through
//! and left out of captures); the engine places it and switches it between
//! its two modes here, from its own thread, never through the window
//! toolkit's setters (tao applies a style change with `ShowWindow`, which
//! could activate it):
//!
//! - **click-through** (`WS_EX_TRANSPARENT`, `WS_EX_NOACTIVATE`): clicks
//!   reach the game, and `WindowFromPoint` sees through it (measured), so a
//!   guest's clicks still count as over the game;
//! - **drawing** (the host's own key): it takes the mouse and the focus.
//!   `WS_EX_LAYERED` stays in both: the toolkit drew it that way.

use windows::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
    UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, IsIconic, SWP_ASYNCWINDOWPOS,
        SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
        SetWindowLongPtrW, SetWindowPos, WS_EX_NOACTIVATE, WS_EX_TRANSPARENT,
    },
};

use crate::{
    foreground::{WindowId, window_exists},
    monitor::Area,
};

/// Where the board waits while it has nothing to show: off every screen.
/// Moving a window can't activate it; showing a hidden one could.
pub const PARKED: i32 = -32_000;

fn hwnd(window: WindowId) -> HWND {
    HWND(window.0 as *mut _)
}

/// The part of the desktop a capture of `window` shows: its visible frame
/// (DWM's extended frame bounds, without the invisible resize borders), in
/// physical pixels. `None` if it is gone or minimized.
pub fn window_area(window: WindowId) -> Option<Area> {
    if !window_exists(window) {
        return None;
    }
    // SAFETY: takes a handle value, which Windows checks.
    if unsafe { IsIconic(hwnd(window)) }.as_bool() {
        return None;
    }
    let mut r = RECT::default();
    // SAFETY: `r` is a live RECT, and its size is the size given.
    unsafe {
        DwmGetWindowAttribute(
            hwnd(window),
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut r).cast(),
            size_of::<RECT>() as u32,
        )
    }
    .ok()?;
    let area = Area {
        x: r.left,
        y: r.top,
        width: r.right - r.left,
        height: r.bottom - r.top,
    };
    (area.width > 0 && area.height > 0).then_some(area)
}

/// Lays the board over `area`, topmost, without activating it; `None`
/// parks it off every screen (its size kept). `false` if Windows refused.
pub fn place(board: WindowId, area: Option<Area>) -> bool {
    // Asynchronously: the window belongs to the app's UI thread, and a
    // move posted to it never waits on that thread being busy.
    let flags = SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS;
    // SAFETY: takes a handle value, which Windows checks.
    unsafe {
        match area {
            Some(a) => SetWindowPos(
                hwnd(board),
                Some(HWND_TOPMOST),
                a.x,
                a.y,
                a.width,
                a.height,
                flags,
            ),
            None => SetWindowPos(
                hwnd(board),
                Some(HWND_TOPMOST),
                PARKED,
                PARKED,
                0,
                0,
                flags | SWP_NOSIZE,
            ),
        }
    }
    .is_ok()
}

/// The board takes clicks and may be activated (`true`, drawing), or lets
/// every click through and never activates (`false`). `false` if the board
/// is gone.
pub fn set_interactive(board: WindowId, on: bool) -> bool {
    if !window_exists(board) {
        return false;
    }
    let through = (WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0) as isize;
    // Setting a style sends the window WM_STYLECHANGING and WM_STYLECHANGED,
    // so this waits on the app's UI thread: only at drawing's start and
    // end, never in the follower's loop.
    // SAFETY: reading and writing a window's extended style by its handle,
    // which Windows checks; nothing here dereferences memory of ours.
    unsafe {
        let style = GetWindowLongPtrW(hwnd(board), GWL_EXSTYLE);
        let wanted = if on {
            style & !through
        } else {
            style | through
        };
        if wanted != style {
            SetWindowLongPtrW(hwnd(board), GWL_EXSTYLE, wanted);
            // Styles take effect once the frame is recomputed.
            let _ = SetWindowPos(
                hwnd(board),
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED
                    | SWP_NOMOVE
                    | SWP_NOSIZE
                    | SWP_NOZORDER
                    | SWP_NOACTIVATE
                    | SWP_ASYNCWINDOWPOS,
            );
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_that_isnt_has_no_area_and_isnt_placed() {
        let none = WindowId(0x7fff_fff0);
        assert_eq!(window_area(none), None);
        assert!(!set_interactive(none, true));
    }
}
