//! Keyboard and mouse injection with `SendInput`: keys by scancode
//! ([`crate::keymap`]), the mouse as relative motion.
//!
//! Windows marks injected input (`LLKHF_INJECTED`), and it can't reach
//! windows of a more privileged process: an elevated game or a UAC prompt
//! ignores it.

use std::mem::size_of;

use couch_input::{HidUsage, InputError, KbmInjector, MouseButton};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
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
pub struct SendInputInjector;

impl SendInputInjector {
    pub fn new() -> Self {
        Self
    }
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
        send(&[mouse(dx, dy, 0, MOUSEEVENTF_MOVE)])
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
    let Some(code) = key_code(key.0) else {
        return Ok(());
    };
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
    send(&[keyboard(ki)])
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
