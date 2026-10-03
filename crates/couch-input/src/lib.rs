//! Input vocabulary and the host-side injection traits.
//!
//! Keys travel as physical positions (USB HID usages), never as characters, so
//! the host's keyboard layout decides what a key types and WASD stays WASD on
//! any layout. The seat router, held-key tracking and safety filters arrive in M2.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use couch_proto::ids::{Permissions, SeatEpoch, SeatId};

/// A keyboard key as a USB HID usage on the Keyboard/Keypad page (0x07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HidUsage(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
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
    fn plug(&self) -> Result<Box<dyn VirtualPad>, InputError>;
}

/// Keyboard and mouse injection on the host.
pub trait KbmInjector: Send {
    fn key(&mut self, key: HidUsage, down: bool) -> Result<(), InputError>;
    fn mouse_move(&mut self, dx: i32, dy: i32) -> Result<(), InputError>;
    fn mouse_button(&mut self, button: MouseButton, down: bool) -> Result<(), InputError>;
    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError>;
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
}
