//! The maximize button of the app's own title bar (P15): an invisible child
//! window laid over the page's button, so Windows 11 offers its snap layouts
//! there.
//!
//! Windows shows them only for a window that answers `WM_NCHITTEST` with
//! `HTMAXBUTTON`, and the webview covering the app's window answers for
//! itself; it can't be subclassed (Chromium routes its own input). So this
//! window sits above the webview, over exactly the page's button, answers
//! `HTMAXBUTTON` and never paints: the page's drawing shows through. It
//! takes the button's clicks itself, toggling the app window's maximized
//! state with the system's own command, and tells the page how to draw the
//! button (`Look`), since the page sees no pointer there.
//!
//! Everything here runs on the thread that owns the app's window (its UI
//! thread): `attach`, `place` and the window procedure.

use std::{
    cell::Cell,
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi},
            Input::KeyboardAndMouse::{
                ReleaseCapture, SetCapture, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
                TrackMouseEvent,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, GA_ROOT, GWLP_USERDATA, GetAncestor,
                GetClientRect, GetCursorPos, GetWindowLongPtrW, GetWindowRect, HTMAXBUTTON,
                HWND_TOP, IDC_ARROW, IsZoomed, LoadCursorW, PostMessageW, RegisterClassW,
                SC_MAXIMIZE, SC_RESTORE, SM_CYFRAME, SWP_NOACTIVATE, SWP_NOOWNERZORDER,
                SWP_SHOWWINDOW, SetWindowLongPtrW, SetWindowPos, WINDOW_EX_STYLE,
                WM_CAPTURECHANGED, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_NCHITTEST,
                WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE,
                WM_NCRBUTTONDOWN, WM_NCRBUTTONUP, WM_SYSCOMMAND, WNDCLASSW, WS_CHILD,
                WS_CLIPSIBLINGS, WS_VISIBLE,
            },
        },
    },
    core::w,
};

/// How the page should draw the button under this window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Look {
    /// The pointer is over it (and, while pressed, still over it).
    pub hover: bool,
    /// The left button went down on it and hasn't come up.
    pub pressed: bool,
}

/// One button's state, owned by its window (`GWLP_USERDATA`) and freed when
/// the window is destroyed (`WM_NCDESTROY`), with the app's window or
/// before.
struct State {
    on_look: Box<dyn Fn(Look)>,
    hover: Cell<bool>,
    pressed: Cell<bool>,
}

impl State {
    fn look(&self) -> Look {
        Look {
            hover: self.hover.get(),
            pressed: self.pressed.get(),
        }
    }

    /// Sets the look, telling the page only of a change. A panic in the
    /// callback would abort the process from inside a window procedure, so
    /// it is caught; the process's panic hook still runs (`held_input`'s
    /// lets go of a host's held keys), which only an emit during the app
    /// window's destruction could trigger (Tauri's window list borrowed):
    /// the app is ending then anyway.
    fn set(&self, hover: bool, pressed: bool) {
        let before = self.look();
        self.hover.set(hover);
        self.pressed.set(pressed);
        let now = self.look();
        if now != before {
            let _ = catch_unwind(AssertUnwindSafe(|| (self.on_look)(now)));
        }
    }
}

const CLASS: windows::core::PCWSTR = w!("CouchSnapButton");

/// A button made by `attach`: its window's handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapButton(isize);

/// Lays a snap button over the top-level window `parent` (a handle value),
/// at nowhere until `place`; `on_look` hears each change of its look, on
/// this thread. Call on the thread that owns `parent`.
pub fn attach(
    parent: isize,
    on_look: impl Fn(Look) + 'static,
) -> windows::core::Result<SnapButton> {
    // SAFETY: plain Win32 calls with valid arguments; the class name is
    // static. Registering again fails harmlessly (the class exists), and
    // `CreateWindowExW` then finds it.
    let instance = unsafe { GetModuleHandleW(None)? };
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        // SAFETY: a system cursor.
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        lpszClassName: CLASS,
        ..Default::default()
    };
    // SAFETY: the class structure is complete and its strings static.
    unsafe { RegisterClassW(&class) };

    // SAFETY: `parent` is checked by Windows; the class is registered.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!(""),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            0,
            0,
            0,
            0,
            Some(HWND(parent as *mut c_void)),
            None,
            Some(instance.into()),
            None,
        )?
    };
    // Its state only once it exists: until then it answers the hit test
    // alone.
    let state = Box::into_raw(Box::new(State {
        on_look: Box::new(on_look),
        hover: Cell::new(false),
        pressed: Cell::new(false),
    }));
    // SAFETY: our new window, on this thread; it owns the state from here
    // and frees it in `WM_NCDESTROY`.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize) };
    Ok(SnapButton(hwnd.0 as isize))
}

/// Lays the button over the page's: a rectangle of the parent's client
/// area, in physical pixels, above the webview. While the window isn't
/// maximized, its top edge (the frame's width) is left to resizing, as over
/// Windows' own buttons.
pub fn place(button: SnapButton, x: i32, y: i32, width: i32, height: i32) {
    let hwnd = HWND(button.0 as *mut c_void);
    // SAFETY: plain calls on a handle value, which Windows checks.
    let edge = unsafe {
        let root = GetAncestor(hwnd, GA_ROOT);
        if IsZoomed(root).as_bool() {
            0
        } else {
            GetSystemMetricsForDpi(SM_CYFRAME, GetDpiForWindow(root)).clamp(0, height / 2)
        }
    };
    // SAFETY: takes a handle value, which Windows checks.
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            x,
            y + edge,
            width,
            height - edge,
            SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
        )
    };
}

/// Whether the pointer is over the button now.
fn pointer_over(hwnd: HWND) -> bool {
    let mut point = POINT::default();
    let mut r = RECT::default();
    // SAFETY: our own window; live structures.
    let read = unsafe { GetCursorPos(&mut point).is_ok() && GetWindowRect(hwnd, &mut r).is_ok() };
    read && point.x >= r.left && point.x < r.right && point.y >= r.top && point.y < r.bottom
}

/// The state `hwnd` holds, if it holds one.
///
/// # Safety
/// `hwnd` is a window of our class, on its own thread.
unsafe fn state<'a>(hwnd: HWND) -> Option<&'a State> {
    // SAFETY: our windows store a `Box<State>` pointer or nothing.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const State;
    // SAFETY: freed only in `WM_NCDESTROY`, which clears it first.
    unsafe { ptr.as_ref() }
}

/// Whether a client-area point (a mouse message's `lparam`) is inside the
/// button.
fn inside(hwnd: HWND, lparam: LPARAM) -> bool {
    let point = POINT {
        x: (lparam.0 & 0xffff) as u16 as i16 as i32,
        y: ((lparam.0 >> 16) & 0xffff) as u16 as i16 as i32,
    };
    let mut r = RECT::default();
    // SAFETY: our own window; `r` is a live RECT.
    if unsafe { GetClientRect(hwnd, &mut r) }.is_err() {
        return false;
    }
    point.x >= r.left && point.x < r.right && point.y >= r.top && point.y < r.bottom
}

/// Asks to be told when the pointer leaves the button.
fn track_leave(hwnd: HWND) {
    let mut track = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE | TME_NONCLIENT,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    // SAFETY: a complete structure for our own window.
    let _ = unsafe { TrackMouseEvent(&mut track) };
}

/// Maximizes the app's window, or restores it, by the system's own command
/// (the window's own handling of it, as from its system menu).
fn toggle_maximized(hwnd: HWND) {
    // SAFETY: our own window, and the top-level window it lies in.
    unsafe {
        let root = GetAncestor(hwnd, GA_ROOT);
        let command = if IsZoomed(root).as_bool() {
            SC_RESTORE
        } else {
            SC_MAXIMIZE
        };
        let _ = PostMessageW(
            Some(root),
            WM_SYSCOMMAND,
            WPARAM(command as usize),
            LPARAM(0),
        );
    }
}

extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCDESTROY => {
            // SAFETY: our window; cleared before it is freed, so nothing
            // reads it after.
            unsafe {
                let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut State;
                if !ptr.is_null() {
                    drop(Box::from_raw(ptr));
                }
            }
        }
        // The whole window is the maximize button.
        WM_NCHITTEST => return LRESULT(HTMAXBUTTON as isize),
        _ => {}
    }
    // SAFETY: our own window, on its own thread.
    if let Some(state) = unsafe { state(hwnd) } {
        match msg {
            WM_NCMOUSEMOVE => {
                // Each time: a capture in between may have ended the last.
                track_leave(hwnd);
                state.set(true, state.pressed.get());
            }
            WM_NCMOUSELEAVE => state.set(false, state.pressed.get()),
            // Handled here, never by `DefWindowProcW`: its tracking of a
            // caption button would send the maximize command to this child
            // window itself.
            WM_NCLBUTTONDOWN => {
                // SAFETY: our own window.
                unsafe { SetCapture(hwnd) };
                state.set(true, true);
                return LRESULT(0);
            }
            // A double click's second press does nothing, as on Windows' own
            // buttons: the first click already maximized or restored.
            WM_NCLBUTTONDBLCLK => return LRESULT(0),
            // While captured, the pointer comes as client messages.
            WM_MOUSEMOVE if state.pressed.get() => {
                state.set(inside(hwnd, lparam), true);
                return LRESULT(0);
            }
            WM_LBUTTONUP if state.pressed.get() => {
                let over = inside(hwnd, lparam);
                // SAFETY: plain call; `WM_CAPTURECHANGED` follows.
                let _ = unsafe { ReleaseCapture() };
                state.set(over, false);
                if over {
                    track_leave(hwnd);
                    toggle_maximized(hwnd);
                }
                return LRESULT(0);
            }
            // Released, or taken away (Alt+Tab while held): hover is where
            // the pointer is now.
            WM_CAPTURECHANGED => {
                let over = pointer_over(hwnd);
                if over {
                    track_leave(hwnd);
                }
                state.set(over, false);
                return LRESULT(0);
            }
            // No system menu of a child window's own.
            WM_NCRBUTTONDOWN | WM_NCRBUTTONUP => return LRESULT(0),
            _ => {}
        }
    }
    // SAFETY: the arguments are the ones Windows passed us.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
