//! Keyboard and mouse injection with `SendInput`: keys by scancode
//! ([`crate::keymap`]), the mouse as relative motion.
//!
//! Windows marks injected input (`LLKHF_INJECTED`), and it can't reach
//! windows of a more privileged process: an elevated game or a UAC prompt
//! ignores it.

use std::mem::size_of;

use couch_input::{Bounds, HidUsage, InputError, KbmInjector, MouseButton};
use windows::Win32::{
    Foundation::POINT,
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS,
            KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
            MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
            MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
            MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL,
            MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
        },
        WindowsAndMessaging::{
            GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
            SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
        },
    },
};

use crate::{
    held_input::{self, Held},
    keymap::{KeyCode, key_code},
};

const XBUTTON1: u32 = 1;
const XBUTTON2: u32 = 2;

/// Injects into whatever window has the foreground; the seat router decides
/// whether that is allowed.
#[derive(Debug, Default)]
pub struct SendInputInjector {
    /// Where moves keep the pointer, as absolute moves (M25b).
    bounds: Option<Bounds>,
}

impl SendInputInjector {
    pub fn new() -> Self {
        Self::default()
    }
}

/// F24, which no keyboard has and nothing listens for.
const HARMLESS_KEY: HidUsage = HidUsage(0x73);

/// Taps F24, down and up in one call, so nothing can leave it held: Windows
/// then counts this process as the one that gave the last input, which
/// lets it bring a window to the front (M25b's measurements: without it,
/// `SetForegroundWindow` from a background process is refused).
pub(crate) fn tap_harmless_key() -> Result<(), InputError> {
    let (Some(down), Some(up)) = (
        key_input(HARMLESS_KEY, true),
        key_input(HARMLESS_KEY, false),
    ) else {
        return Err(InputError::Injection("F24 has no key code".into()));
    };
    send(&[down, up])
}

/// The pointer at its position plus `(dx, dy)`, kept within `bounds`, as an
/// absolute move over the whole desktop (no acceleration).
fn move_within(dx: i32, dy: i32, bounds: Bounds) -> Result<(), InputError> {
    let mut at = POINT::default();
    // SAFETY: `at` is a live local for the out-parameter.
    unsafe { GetCursorPos(&mut at) }
        .map_err(|e| InputError::Injection(format!("GetCursorPos: {e}")))?;
    let right = bounds.x + bounds.width.max(1) - 1;
    let bottom = bounds.y + bounds.height.max(1) - 1;
    let x = at.x.saturating_add(dx).clamp(bounds.x, right);
    let y = at.y.saturating_add(dy).clamp(bounds.y, bottom);
    // SAFETY: takes a constant by value.
    let metric = |m| unsafe { GetSystemMetrics(m) };
    let (left, top) = (metric(SM_XVIRTUALSCREEN), metric(SM_YVIRTUALSCREEN));
    let (width, height) = (
        metric(SM_CXVIRTUALSCREEN).max(2),
        metric(SM_CYVIRTUALSCREEN).max(2),
    );
    // 0 to 65535 across the virtual desktop.
    let scale = |v: i32, origin: i32, size: i32| {
        ((i64::from(v - origin) * 65535) / i64::from(size - 1)).clamp(0, 65535) as i32
    };
    send(&[mouse(
        scale(x, left, width),
        scale(y, top, height),
        0,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
    )])
}

fn send(inputs: &[INPUT]) -> Result<(), InputError> {
    // SAFETY: `inputs` is a valid slice of initialized INPUT structures and
    // the size argument matches their type.
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err(InputError::Injection(format!(
            "SendInput accepted {sent} of {} events: {}",
            inputs.len(),
            windows::core::Error::from_thread()
        )))
    }
}

fn keyboard(ki: KEYBDINPUT) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki },
    }
}

fn mouse(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// `amount` as a signed `mouseData` value.
fn signed(amount: i32) -> u32 {
    amount as u32
}

impl KbmInjector for SendInputInjector {
    /// Every key and button this presses is recorded until released
    /// ([`crate::held_input`]), so a crash can't leave it held.
    fn key(&mut self, key: HidUsage, down: bool) -> Result<(), InputError> {
        send_key(key, down)?;
        if key_code(key.0).is_some() {
            held_input::note(Held::Key(key.0), down);
        }
        Ok(())
    }

    fn mouse_move(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        match self.bounds {
            Some(bounds) => move_within(dx, dy, bounds),
            None => send(&[mouse(dx, dy, 0, MOUSEEVENTF_MOVE)]),
        }
    }

    fn set_bounds(&mut self, bounds: Option<Bounds>) {
        self.bounds = bounds;
    }

    fn mouse_button(&mut self, button: MouseButton, down: bool) -> Result<(), InputError> {
        send_button(button, down)?;
        held_input::note(Held::button(button), down);
        Ok(())
    }

    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        let mut events = Vec::with_capacity(2);
        if dy != 0 {
            events.push(mouse(0, 0, signed(dy), MOUSEEVENTF_WHEEL));
        }
        if dx != 0 {
            events.push(mouse(0, 0, signed(dx), MOUSEEVENTF_HWHEEL));
        }
        if events.is_empty() {
            return Ok(());
        }
        send(&events)
    }
}

/// Whether Windows has the virtual key `vk` down right now, from any source
/// (checks only: `couch-cli lifecycle-check` looks for a key a killed host
/// left held).
pub fn vk_down(vk: u16) -> bool {
    // SAFETY: takes the key by value.
    let state = unsafe { GetAsyncKeyState(i32::from(vk)) };
    state as u16 & 0x8000 != 0
}

/// One key, by scancode, without recording it.
pub(crate) fn send_key(key: HidUsage, down: bool) -> Result<(), InputError> {
    match key_input(key, down) {
        Some(input) => send(&[input]),
        None => Ok(()),
    }
}

/// The event for one key, by scancode; `None` for a key we can't send.
fn key_input(key: HidUsage, down: bool) -> Option<INPUT> {
    let code = key_code(key.0)?;
    let up = if down {
        KEYBD_EVENT_FLAGS(0)
    } else {
        KEYEVENTF_KEYUP
    };
    let ki = match code {
        KeyCode::Scan { code, extended } => KEYBDINPUT {
            wVk: VIRTUAL_KEY(0),
            wScan: code,
            dwFlags: KEYEVENTF_SCANCODE
                | up
                | if extended {
                    KEYEVENTF_EXTENDEDKEY
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
            time: 0,
            dwExtraInfo: 0,
        },
        KeyCode::Vk(vk) => KEYBDINPUT {
            wVk: VIRTUAL_KEY(vk),
            wScan: 0,
            dwFlags: up,
            time: 0,
            dwExtraInfo: 0,
        },
    };
    Some(keyboard(ki))
}

/// One mouse button, without recording it.
pub(crate) fn send_button(button: MouseButton, down: bool) -> Result<(), InputError> {
    {
        let (flags, data) = match (button, down) {
            (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
            (MouseButton::Back, false) => (MOUSEEVENTF_XUP, XBUTTON1),
            (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
            (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, XBUTTON2),
        };
        send(&[mouse(0, 0, data, flags)])
    }
}
