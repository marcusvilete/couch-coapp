//! Keyboard and mouse on a Linux host (M16c-2, ADR 0012): a `uinput`
//! keyboard and a `uinput` mouse, which the desktop takes for real ones.
//! Only the seat router drives them (`couch_input::router::SeatRouter`).
//!
//! - **Keys by position:** a HID usage becomes the evdev key at the same
//!   place on the board (the kernel's own HID table), and the desktop applies
//!   the host's layout, as `SendInput`'s scancodes do on Windows: WASD stays
//!   WASD.
//! - **Never SysRq:** the kernel's magic SysRq handler listens to every
//!   keyboard with Alt on it, so the device can't send `KEY_SYSRQ` at all
//!   (Print Screen is the same key). Media and power keys are left out, as
//!   on Windows: a guest has no business with the host's volume or sleep.
//! - **No autorepeat on the device:** the desktop repeats held keys itself.
//! - **A crash holds nothing:** the kernel releases a device's keys and
//!   buttons when the device goes, and it goes with this process.
//! - **The mouse moves relatively,** as games want; the desktop's pointer
//!   acceleration applies to it, and nothing keeps it on one screen
//!   (`set_bounds` does nothing here: Wayland tells no program where the
//!   pointer is).
//! - **A guest's pointer stays inside the game window** (`confine`): each
//!   move is cut to what keeps it in the window's client area, from where
//!   the desktop's helper last said the pointer is (`front::pointer`), by
//!   `couch_input::confine`. Only our moves are cut: the host's own mouse
//!   drives the same pointer, and stays free.
//!
//! Ours are named like the pads, `couch-coapp/<pid>/keyboard` and
//! `/mouse`, in their `phys`.

use std::{ffi::CString, io};

use couch_input::{HidUsage, InputError, KbmInjector, MouseButton, confine::Confiner};
use evdev::{
    AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode, RelativeAxisCode,
    uinput::VirtualDevice,
};

use crate::{front, pads::PHYS_PREFIX};

const KEYBOARD_NAME: &str = "Couch Co-App keyboard";
const MOUSE_NAME: &str = "Couch Co-App mouse";
/// One wheel notch in the high-resolution axes, as `WHEEL_DELTA` on Windows:
/// what the wire carries.
const NOTCH: i32 = 120;

/// `KEY_SYSRQ`: Print Screen's code, and the kernel's magic SysRq key.
const SYSRQ: u16 = 99;

/// The evdev key at `usage`'s place on the board, or `None` for keys we
/// don't send. From the kernel's `hid_keyboard` table (`hid-input.c`), with
/// the keys `couch_platform_win::keymap` sends, and never [`SYSRQ`].
pub fn key_code(usage: u16) -> Option<u16> {
    Some(match usage {
        // Letters A to Z.
        0x04..=0x1D => [
            30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47,
            17, 45, 21, 44,
        ][usize::from(usage - 0x04)],
        // 1 to 9, then 0.
        0x1E..=0x27 => usage - 0x1E + 2,
        0x28 => 28, // Enter
        0x29 => 1,  // Escape
        0x2A => 14, // Backspace
        0x2B => 15, // Tab
        0x2C => 57, // Space
        0x2D => 12, // - _
        0x2E => 13, // = +
        0x2F => 26, // [ {
        0x30 => 27, // ] }
        0x31 => 43, // \ |
        0x32 => 43, // non-US # ~ (same position as \ on ISO boards)
        0x33 => 39, // ; :
        0x34 => 40, // ' "
        0x35 => 41, // ` ~
        0x36 => 51, // , <
        0x37 => 52, // . >
        0x38 => 53, // / ?
        0x39 => 58, // Caps Lock
        // F1 to F10, then F11 and F12.
        0x3A..=0x43 => usage - 0x3A + 59,
        0x44 => 87,
        0x45 => 88,
        // 0x46, Print Screen, is SysRq: never.
        0x47 => 70,  // Scroll Lock
        0x48 => 119, // Pause
        0x49 => 110, // Insert
        0x4A => 102, // Home
        0x4B => 104, // Page Up
        0x4C => 111, // Delete
        0x4D => 107, // End
        0x4E => 109, // Page Down
        0x4F => 106, // Right
        0x50 => 105, // Left
        0x51 => 108, // Down
        0x52 => 103, // Up
        0x53 => 69,  // Num Lock
        0x54 => 98,  // keypad /
        0x55 => 55,  // keypad *
        0x56 => 74,  // keypad -
        0x57 => 78,  // keypad +
        0x58 => 96,  // keypad Enter
        // Keypad 1 to 9, 0 and the decimal point.
        0x59..=0x63 => [79, 80, 81, 75, 76, 77, 71, 72, 73, 82, 83][usize::from(usage - 0x59)],
        0x64 => 86,  // non-US \ | (the extra key by left Shift on ISO boards)
        0x65 => 127, // Application (menu)
        0x67 => 117, // keypad =
        // F13 to F24.
        0x68..=0x73 => usage - 0x68 + 183,
        0x85 => 121, // keypad , (ABNT2 keypad .)
        0x87 => 89,  // International1: Ro, and ABNT2's / ? key
        0x88 => 93,  // International2: Katakana/Hiragana
        0x89 => 124, // International3: Yen
        0x8A => 92,  // International4: Henkan
        0x8B => 94,  // International5: Muhenkan
        0xE0 => 29,  // Left Ctrl
        0xE1 => 42,  // Left Shift
        0xE2 => 56,  // Left Alt
        0xE3 => 125, // Left Meta (Super)
        0xE4 => 97,  // Right Ctrl
        0xE5 => 54,  // Right Shift
        0xE6 => 100, // Right Alt (AltGr)
        0xE7 => 126, // Right Meta
        _ => return None,
    })
}

fn button_code(button: MouseButton) -> KeyCode {
    match button {
        MouseButton::Left => KeyCode::BTN_LEFT,
        MouseButton::Right => KeyCode::BTN_RIGHT,
        MouseButton::Middle => KeyCode::BTN_MIDDLE,
        MouseButton::Back => KeyCode::BTN_SIDE,
        MouseButton::Forward => KeyCode::BTN_EXTRA,
    }
}

/// The host's injected keyboard and mouse.
pub struct UinputKbm {
    keyboard: VirtualDevice,
    mouse: VirtualDevice,
    /// What the wheels moved short of a whole notch, for the axes that count
    /// notches.
    wheel_rest: (i32, i32),
    /// The game window a guest's pointer is kept in, by `front`'s id.
    confined: Option<(isize, Confiner)>,
}

impl UinputKbm {
    /// Makes both devices: `/dev/uinput` must be writable (Settings →
    /// Drivers, as for the pads).
    pub fn new() -> Result<Self, InputError> {
        let pid = std::process::id();
        let phys =
            |what: &str| CString::new(format!("{PHYS_PREFIX}{pid}/{what}")).expect("no NUL in it");
        let id = || InputId::new(BusType::BUS_VIRTUAL, 0, 0, 1);

        let keys: AttributeSet<KeyCode> = (0..=0xFFu16)
            .filter_map(key_code)
            .map(KeyCode::new)
            .collect();
        debug_assert!(!keys.contains(KeyCode::new(SYSRQ)));
        let keyboard = VirtualDevice::builder()
            .and_then(|b| {
                b.name(KEYBOARD_NAME)
                    .input_id(id())
                    .with_phys(&phys("keyboard"))?
                    .with_keys(&keys)?
                    .build()
            })
            .map_err(refused)?;

        let buttons: AttributeSet<KeyCode> =
            MouseButton::ALL.into_iter().map(button_code).collect();
        let axes: AttributeSet<RelativeAxisCode> = [
            RelativeAxisCode::REL_X,
            RelativeAxisCode::REL_Y,
            RelativeAxisCode::REL_WHEEL,
            RelativeAxisCode::REL_HWHEEL,
            RelativeAxisCode::REL_WHEEL_HI_RES,
            RelativeAxisCode::REL_HWHEEL_HI_RES,
        ]
        .into_iter()
        .collect();
        let mouse = VirtualDevice::builder()
            .and_then(|b| {
                b.name(MOUSE_NAME)
                    .input_id(id())
                    .with_phys(&phys("mouse"))?
                    .with_keys(&buttons)?
                    .with_relative_axes(&axes)?
                    .build()
            })
            .map_err(refused)?;
        tracing::info!("keyboard and mouse ready (uinput)");
        Ok(Self {
            keyboard,
            mouse,
            wheel_rest: (0, 0),
            confined: None,
        })
    }
}

fn refused(e: io::Error) -> InputError {
    match e.kind() {
        io::ErrorKind::PermissionDenied => InputError::DriverUnavailable(
            "this user can't make a virtual keyboard and mouse yet: Settings → Drivers sets that \
             up"
            .into(),
        ),
        io::ErrorKind::NotFound => {
            InputError::DriverUnavailable("this system has no uinput device".into())
        }
        _ => InputError::DriverUnavailable(format!("uinput: {e}")),
    }
}

fn injection(e: io::Error) -> InputError {
    InputError::Injection(format!("uinput: {e}"))
}

fn key_event(code: KeyCode, down: bool) -> InputEvent {
    InputEvent::new(EventType::KEY.0, code.0, i32::from(down))
}

fn rel(axis: RelativeAxisCode, value: i32) -> InputEvent {
    InputEvent::new(EventType::RELATIVE.0, axis.0, value)
}

/// Whole notches in `rest + delta`, keeping what's left over in `rest`. A
/// turn the other way starts afresh, as the kernel's own counter does.
fn notches(rest: &mut i32, delta: i32) -> i32 {
    if rest.signum() * delta.signum() < 0 {
        *rest = 0;
    }
    let total = rest.saturating_add(delta);
    let whole = total / NOTCH;
    *rest = total % NOTCH;
    whole
}

/// The events one wheel move makes: the high-resolution axes as they come,
/// and the notch axes once a whole notch has gathered (the kernel's way for
/// a high-resolution wheel).
fn wheel_events(rest: &mut (i32, i32), dx: i32, dy: i32) -> Vec<InputEvent> {
    let mut events = Vec::with_capacity(4);
    if dy != 0 {
        events.push(rel(RelativeAxisCode::REL_WHEEL_HI_RES, dy));
        let n = notches(&mut rest.1, dy);
        if n != 0 {
            events.push(rel(RelativeAxisCode::REL_WHEEL, n));
        }
    }
    if dx != 0 {
        events.push(rel(RelativeAxisCode::REL_HWHEEL_HI_RES, dx));
        let n = notches(&mut rest.0, dx);
        if n != 0 {
            events.push(rel(RelativeAxisCode::REL_HWHEEL, n));
        }
    }
    events
}

impl KbmInjector for UinputKbm {
    fn key(&mut self, key: HidUsage, down: bool) -> Result<(), InputError> {
        // A key we don't send is dropped, as on Windows.
        let Some(code) = key_code(key.0) else {
            return Ok(());
        };
        self.keyboard
            .emit(&[key_event(KeyCode::new(code), down)])
            .map_err(injection)
    }

    fn mouse_move(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        let (dx, dy) = match &mut self.confined {
            Some((window, confiner)) => {
                confiner.cut(front::pointer(*window), dx, dy, std::time::Instant::now())
            }
            None => (dx, dy),
        };
        if dx == 0 && dy == 0 {
            return Ok(());
        }
        // Down is positive in both, as on Windows.
        self.mouse
            .emit(&[
                rel(RelativeAxisCode::REL_X, dx),
                rel(RelativeAxisCode::REL_Y, dy),
            ])
            .map_err(injection)
    }

    fn mouse_button(&mut self, button: MouseButton, down: bool) -> Result<(), InputError> {
        self.mouse
            .emit(&[key_event(button_code(button), down)])
            .map_err(injection)
    }

    /// In `WHEEL_DELTA` units (120 a notch), up and right positive, as on
    /// Windows and in evdev alike.
    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        let events = wheel_events(&mut self.wheel_rest, dx, dy);
        if events.is_empty() {
            return Ok(());
        }
        self.mouse.emit(&events).map_err(injection)
    }

    fn confine(&mut self, window: Option<isize>) {
        if self.confined.as_ref().map(|(w, _)| *w) == window {
            return;
        }
        tracing::info!(
            on = window.is_some(),
            "a guest's pointer kept in the game window"
        );
        self.confined = window.map(|w| (w, Confiner::new()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_positions() {
        assert_eq!(key_code(0x1A), Some(17)); // W: KEY_W
        assert_eq!(key_code(0x04), Some(30)); // A
        assert_eq!(key_code(0x16), Some(31)); // S
        assert_eq!(key_code(0x07), Some(32)); // D
        assert_eq!(key_code(0x1D), Some(44)); // Z
        assert_eq!(key_code(0x1E), Some(2)); // 1
        assert_eq!(key_code(0x27), Some(11)); // 0
        assert_eq!(key_code(0x43), Some(68)); // F10
        assert_eq!(key_code(0x45), Some(88)); // F12
        assert_eq!(key_code(0x52), Some(103)); // Up
        assert_eq!(key_code(0x62), Some(82)); // keypad 0
        assert_eq!(key_code(0x63), Some(83)); // keypad .
        assert_eq!(key_code(0x68), Some(183)); // F13
        assert_eq!(key_code(0x73), Some(194)); // F24
        assert_eq!(key_code(0xE3), Some(125)); // Left Meta
        assert_eq!(key_code(0xE6), Some(100)); // AltGr
    }

    #[test]
    fn codes_are_unique_except_the_iso_hash_key() {
        let mut seen = std::collections::HashMap::new();
        for usage in 0..=0xFF {
            if let Some(code) = key_code(usage)
                && let Some(previous) = seen.insert(code, usage)
            {
                assert_eq!((previous, usage), (0x31, 0x32), "{code}");
            }
        }
    }

    /// The kernel's magic SysRq, power and sleep, and the media keys.
    #[test]
    fn sysrq_power_and_media_are_never_sent() {
        for usage in 0..=0xFF {
            let code = key_code(usage);
            assert_ne!(code, Some(SYSRQ), "{usage:#x}");
            // KEY_POWER, KEY_SLEEP, KEY_WAKEUP, mute and volume.
            for never in [116, 142, 143, 113, 114, 115] {
                assert_ne!(code, Some(never), "{usage:#x}");
            }
        }
        for usage in [0x46, 0x66, 0x7F, 0x80, 0x81] {
            assert_eq!(key_code(usage), None, "{usage:#x}");
        }
    }

    /// The same keys as Windows sends, Print Screen aside.
    #[test]
    fn the_keys_sent_match_windows_but_print_screen() {
        // `couch_platform_win::keymap`'s usages, written out.
        let windows = (0x04..=0x45)
            .chain(0x46..=0x65)
            .chain(0x67..=0x73)
            .chain([0x85, 0x87, 0x88, 0x89, 0x8A, 0x8B])
            .chain(0xE0..=0xE7);
        for usage in windows {
            assert_eq!(key_code(usage).is_some(), usage != 0x46, "{usage:#x}");
        }
    }

    /// The real devices (needs `/dev/uinput` writable: root in WSL, after
    /// `modprobe uinput evdev`): both appear under our names and `phys`, the
    /// keyboard can't send SysRq, a key typed is down until let go, and
    /// both go with the injector.
    #[test]
    #[ignore = "makes uinput devices"]
    fn the_devices_appear_type_and_go() {
        let find = |phys: &str| {
            evdev::enumerate()
                .map(|(_, d)| d)
                .find(|d| d.physical_path() == Some(phys))
        };
        let pid = std::process::id();
        let keyboard_phys = format!("{PHYS_PREFIX}{pid}/keyboard");
        let mouse_phys = format!("{PHYS_PREFIX}{pid}/mouse");
        let mut kbm = UinputKbm::new().expect("uinput is writable");
        // udev makes the nodes a moment later.
        let mut keyboard = None;
        for _ in 0..50 {
            keyboard = find(&keyboard_phys);
            if keyboard.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let keyboard = keyboard.expect("the keyboard is listed");
        assert_eq!(keyboard.name(), Some(KEYBOARD_NAME));
        let keys = keyboard.supported_keys().expect("it has keys");
        assert!(keys.contains(KeyCode::KEY_W));
        assert!(!keys.contains(KeyCode::new(SYSRQ)), "never SysRq");
        assert!(!keys.contains(KeyCode::KEY_POWER));
        let mouse = find(&mouse_phys).expect("the mouse is listed");
        assert_eq!(mouse.name(), Some(MOUSE_NAME));

        kbm.key(HidUsage(0x1A), true).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let down = keyboard.get_key_state().unwrap();
        assert!(down.contains(KeyCode::KEY_W), "W is down");
        kbm.key(HidUsage(0x1A), false).unwrap();
        // Print Screen goes nowhere, and says so to nobody.
        kbm.key(HidUsage(0x46), true).unwrap();
        kbm.mouse_move(5, -3).unwrap();
        kbm.wheel(0, 120).unwrap();
        kbm.mouse_button(MouseButton::Left, true).unwrap();
        kbm.mouse_button(MouseButton::Left, false).unwrap();
        drop(kbm);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(find(&keyboard_phys).is_none(), "the keyboard went");
        assert!(find(&mouse_phys).is_none(), "the mouse went");
    }

    #[test]
    fn the_wheel_sends_notches_once_they_gather() {
        let mut rest = (0, 0);
        let codes = |events: &[InputEvent]| -> Vec<(u16, i32)> {
            events.iter().map(|e| (e.code(), e.value())).collect()
        };
        let hi = RelativeAxisCode::REL_WHEEL_HI_RES.0;
        let notch = RelativeAxisCode::REL_WHEEL.0;
        assert_eq!(codes(&wheel_events(&mut rest, 0, 60)), vec![(hi, 60)]);
        assert_eq!(
            codes(&wheel_events(&mut rest, 0, 60)),
            vec![(hi, 60), (notch, 1)]
        );
        assert_eq!(
            codes(&wheel_events(&mut rest, 0, -240)),
            vec![(hi, -240), (notch, -2)]
        );
        assert_eq!(rest, (0, 0));
        // Half a notch up, then a whole one down: the half is forgotten.
        wheel_events(&mut rest, 0, 60);
        assert_eq!(
            codes(&wheel_events(&mut rest, 0, -120)),
            vec![(hi, -120), (notch, -1)]
        );
        assert_eq!(rest, (0, 0));
        let hhi = RelativeAxisCode::REL_HWHEEL_HI_RES.0;
        let hnotch = RelativeAxisCode::REL_HWHEEL.0;
        assert_eq!(
            codes(&wheel_events(&mut rest, 120, 0)),
            vec![(hhi, 120), (hnotch, 1)]
        );
        assert!(wheel_events(&mut rest, 0, 0).is_empty());
    }
}
