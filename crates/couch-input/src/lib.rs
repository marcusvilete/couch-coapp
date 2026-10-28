//! Input vocabulary, the host-side injection traits, and the logic between
//! them:
//! - [`router`]: on the host, decides which remote input reaches the game;
//! - [`forward`]: on a viewer, turns local input into what goes on the wire;
//! - [`filter`]: keys a remote member may never press;
//! - [`shortcuts`]: every local shortcut, in one table;
//! - [`local`]: the host's own controller, read to play through a seat.
//!
//! Keys travel as physical positions (USB HID usages), never as characters, so
//! the host's keyboard layout decides what a key types and WASD stays WASD on
//! any layout.

pub mod filter;
pub mod forward;
pub mod local;
pub mod router;
pub mod shortcuts;

use std::time::{Duration, Instant};

use bitflags::bitflags;
use couch_proto::input::{PadSnapshot, mouse_button};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use couch_proto::ids::{MemberId, Permissions, SeatEpoch, SeatId};

/// USB vendor and product id of a wired Xbox 360 controller, which the host's
/// virtual pad reports: games and Steam know a controller's layout by its ids,
/// and with unknown ones only the D-pad works. While a host on the same PC
/// has a pad plugged in, a viewer skips pads with these ids, so the host's pad
/// isn't captured and sent back to itself.
pub const XBOX_360_IDS: (u16, u16) = (0x045E, 0x028E);

/// A made-up vendor and product id for virtual pads that no game should use:
/// `couch-cli pad-test` and the CLI's host without `--real-pad-ids`. A viewer
/// always skips pads with these ids. 0x1209/0x0001 is pid.codes' test id,
/// reserved for exactly this kind of private use.
pub const DEV_PAD_IDS: (u16, u16) = (0x1209, 0x0001);

/// A keyboard key as a USB HID usage on the Keyboard/Keypad page (0x07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HidUsage(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl MouseButton {
    pub const ALL: [Self; 5] = [
        Self::Left,
        Self::Right,
        Self::Middle,
        Self::Back,
        Self::Forward,
    ];

    /// From a wire code ([`mouse_button`]).
    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            mouse_button::LEFT => Self::Left,
            mouse_button::RIGHT => Self::Right,
            mouse_button::MIDDLE => Self::Middle,
            mouse_button::BACK => Self::Back,
            mouse_button::FORWARD => Self::Forward,
            _ => return None,
        })
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Left => mouse_button::LEFT,
            Self::Right => mouse_button::RIGHT,
            Self::Middle => mouse_button::MIDDLE,
            Self::Back => mouse_button::BACK,
            Self::Forward => mouse_button::FORWARD,
        }
    }

    /// This button's bit in a held-buttons mask.
    pub fn bit(self) -> u8 {
        1 << (self.code() - 1)
    }
}

bitflags! {
    /// Gamepad buttons, in XInput's bit layout so the ViGEm backend can pass
    /// them straight through.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct PadButtons: u16 {
        const DPAD_UP = 0x0001;
        const DPAD_DOWN = 0x0002;
        const DPAD_LEFT = 0x0004;
        const DPAD_RIGHT = 0x0008;
        const START = 0x0010;
        const BACK = 0x0020;
        const LEFT_THUMB = 0x0040;
        const RIGHT_THUMB = 0x0080;
        const LEFT_SHOULDER = 0x0100;
        const RIGHT_SHOULDER = 0x0200;
        /// The Xbox/PS button. Remote peers can't press it: Windows reacts to it.
        const GUIDE = 0x0400;
        const A = 0x1000;
        const B = 0x2000;
        const X = 0x4000;
        const Y = 0x8000;
    }
}

/// The complete state of one gamepad. Sent as a full snapshot every time, so a
/// lost packet can never leave a button stuck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PadState {
    pub buttons: PadButtons,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub left_stick: (i16, i16),
    pub right_stick: (i16, i16),
}

impl PadState {
    /// Nothing pressed, sticks centered. Sent on every handoff and teardown.
    pub const NEUTRAL: Self = Self {
        buttons: PadButtons::empty(),
        left_trigger: 0,
        right_trigger: 0,
        left_stick: (0, 0),
        right_stick: (0, 0),
    };
}

impl Default for PadState {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

impl From<PadSnapshot> for PadState {
    /// Unknown button bits are dropped.
    fn from(s: PadSnapshot) -> Self {
        Self {
            buttons: PadButtons::from_bits_truncate(s.buttons),
            left_trigger: s.left_trigger,
            right_trigger: s.right_trigger,
            left_stick: (s.left_x, s.left_y),
            right_stick: (s.right_x, s.right_y),
        }
    }
}

impl From<PadState> for PadSnapshot {
    fn from(s: PadState) -> Self {
        Self {
            buttons: s.buttons.bits(),
            left_trigger: s.left_trigger,
            right_trigger: s.right_trigger,
            left_x: s.left_stick.0,
            left_y: s.left_stick.1,
            right_x: s.right_stick.0,
            right_y: s.right_stick.1,
        }
    }
}

/// Motor strengths requested by the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rumble {
    pub low_frequency: u8,
    pub high_frequency: u8,
}

#[derive(Debug, Error)]
pub enum InputError {
    #[error("virtual gamepad driver unavailable: {0}")]
    DriverUnavailable(String),
    #[error("input injection failed: {0}")]
    Injection(String),
}

/// One virtual controller on the host. Stays plugged in for a whole session;
/// handoffs switch who drives it rather than unplugging it.
pub trait VirtualPad: Send {
    fn submit(&mut self, state: &PadState) -> Result<(), InputError>;

    /// The latest rumble request from the game, if it changed since the last call.
    fn take_rumble(&mut self) -> Option<Rumble>;
}

pub trait VirtualPadFactory: Send + Sync {
    /// Plugs a pad and waits for it, up to [`PAD_SETUP_LIMIT`].
    fn plug(&self) -> Result<Box<dyn VirtualPad>, InputError>;

    /// Plugs a pad, waiting only a moment for it: one that Windows is still
    /// setting up comes back pending.
    fn plug_pending(&self) -> Result<Plugged, InputError> {
        self.plug().map(Plugged::Ready)
    }
}

/// How long a pad Windows is still setting up may take before it is given up
/// (P10): the first time a virtual controller is plugged on a PC, Windows
/// installs its device, which has taken minutes.
pub const PAD_SETUP_LIMIT: Duration = Duration::from_secs(5 * 60);

/// A virtual pad just plugged in.
pub enum Plugged {
    Ready(Box<dyn VirtualPad>),
    /// Plugged, but not taking input yet: Windows is still setting its
    /// device up.
    SettingUp(Box<dyn PendingPad>),
}

/// A virtual pad plugged in that doesn't take input yet (P10). Dropping it
/// unplugs it.
pub trait PendingPad: Send {
    /// Waits up to `wait` for the pad to take input: `Ok(None)` while it
    /// still doesn't, `Err` when it never will (it's gone). Once it has
    /// answered `Some`, the pad is the caller's and this is spent.
    fn poll(&mut self, wait: Duration) -> Result<Option<Box<dyn VirtualPad>>, InputError>;

    /// Gives the pad up: unplugs it now, and returns what must stay until
    /// the session ends (its marker: a pad whose unplug doesn't take must
    /// still read as ours while the host's controllers are followed).
    fn give_up(self: Box<Self>) -> Box<dyn Send> {
        Box::new(())
    }
}

impl Plugged {
    /// The pad, waiting for it up to `limit` if it's still being set up.
    pub fn wait(self, limit: Duration) -> Result<Box<dyn VirtualPad>, InputError> {
        let mut pending = match self {
            Plugged::Ready(pad) => return Ok(pad),
            Plugged::SettingUp(pending) => pending,
        };
        let until = Instant::now() + limit;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(InputError::DriverUnavailable(
                    "the virtual pad wasn't ready in time: Windows may still be setting it up"
                        .into(),
                ));
            }
            if let Some(pad) = pending.poll(left.min(Duration::from_millis(500)))? {
                return Ok(pad);
            }
        }
    }
}

/// Keyboard and mouse injection on the host.
pub trait KbmInjector: Send {
    fn key(&mut self, key: HidUsage, down: bool) -> Result<(), InputError>;
    fn mouse_move(&mut self, dx: i32, dy: i32) -> Result<(), InputError>;
    fn mouse_button(&mut self, button: MouseButton, down: bool) -> Result<(), InputError>;
    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError>;
    /// Keeps the pointer on a screen (M25b, desktop control of a streamed
    /// screen): while set, each move puts the pointer where it is plus the
    /// motion, within `bounds`, without Windows' acceleration. `None` moves
    /// it relatively again, as games want.
    fn set_bounds(&mut self, _bounds: Option<Bounds>) {}
}

/// A screen's area on the desktop, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_pad_has_nothing_pressed() {
        assert!(PadState::NEUTRAL.buttons.is_empty());
        assert_eq!(PadState::default(), PadState::NEUTRAL);
    }

    #[test]
    fn buttons_use_xinput_bits() {
        assert_eq!(PadButtons::A.bits(), 0x1000);
        assert_eq!(PadButtons::GUIDE.bits(), 0x0400);
    }

    #[test]
    fn pad_snapshots_convert_and_drop_unknown_bits() {
        let state = PadState {
            buttons: PadButtons::A | PadButtons::DPAD_LEFT,
            left_trigger: 9,
            right_trigger: 255,
            left_stick: (-32768, 32767),
            right_stick: (1, -1),
        };
        assert_eq!(PadState::from(PadSnapshot::from(state)), state);
        let odd = PadSnapshot {
            buttons: 0x0800 | 0x1000,
            ..Default::default()
        };
        assert_eq!(PadState::from(odd).buttons, PadButtons::A);
    }

    #[test]
    fn mouse_button_codes_roundtrip() {
        for button in MouseButton::ALL {
            assert_eq!(MouseButton::from_code(button.code()), Some(button));
        }
        assert_eq!(MouseButton::from_code(0), None);
        assert_eq!(MouseButton::Forward.bit(), 0b1_0000);
    }
}
