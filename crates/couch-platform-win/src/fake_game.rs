//! A stand-in game for lifecycle checks (`couch-cli fake-game`): a plain
//! window that, when told, replaces itself with a new window (as games do
//! when changing display mode), closes its window while the process goes on,
//! or exits.

use std::time::Duration;

use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::{CreateSolidBrush, HBRUSH},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow,
            DispatchMessageW, IDC_ARROW, LoadCursorW, MSG, PM_REMOVE, PeekMessageW, RegisterClassW,
            SW_SHOW, ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

/// The title every fake game window has.
pub const TITLE: &str = "Couch fake game";

/// What the fake game is told to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Open a new window and destroy the current one.
    Swap,
    /// Destroy the window, and keep running.
    Close,
    /// Exit, window and all.
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Shown,
    /// A new window replaced the first.
    Swapped,
    /// The window is gone; the process goes on.
    Closed,
    /// About to exit.
    Exiting,
}

extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: the arguments are the ones Windows passed us.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn open(x: i32) -> windows::core::Result<HWND> {
    // SAFETY: the class was registered by `run`; the strings outlive the call.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("CouchFakeGame"),
            &HSTRING::from(TITLE),
            WS_OVERLAPPEDWINDOW,
            x,
            120,
            640,
            360,
            None,
            None,
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?
    };
    // SAFETY: a window we just created.
    let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
    Ok(hwnd)
}

/// Runs the fake game until it is told to exit, asking `next` for an action
/// every 10 ms and reporting each step.
pub fn run(
    mut next: impl FnMut() -> Option<Action>,
    mut on_event: impl FnMut(Event),
) -> windows::core::Result<()> {
    // SAFETY: plain Win32 calls with valid arguments; the brush and the
    // class live as long as the process.
    let (brush, instance, cursor): (HBRUSH, _, _) = unsafe {
        (
            CreateSolidBrush(COLORREF(0x0040_8020)),
            GetModuleHandleW(None)?,
            LoadCursorW(None, IDC_ARROW)?,
        )
    };
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        hCursor: cursor,
        hbrBackground: brush,
        lpszClassName: w!("CouchFakeGame"),
        lpszMenuName: PCWSTR::null(),
        ..Default::default()
    };
    // SAFETY: the class structure is complete and its strings static.
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err(windows::core::Error::from_thread());
    }
    let mut window = Some(open(120)?);
    on_event(Event::Shown);
    loop {
        match next() {
            Some(Action::Exit) => {
                on_event(Event::Exiting);
                if let Some(hwnd) = window.take() {
                    // SAFETY: our own window.
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                return Ok(());
            }
            Some(Action::Swap) => {
                // The new one first, as a game does, then the old one goes.
                let new = open(160)?;
                if let Some(old) = window.replace(new) {
                    // SAFETY: our own window.
                    let _ = unsafe { DestroyWindow(old) };
                }
                on_event(Event::Swapped);
            }
            Some(Action::Close) => {
                if let Some(hwnd) = window.take() {
                    // SAFETY: our own window.
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                on_event(Event::Closed);
            }
            None => {}
        }
        let mut msg = MSG::default();
        // SAFETY: a live MSG for the out-parameter; any window of this thread.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: a message just retrieved.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
