//! Physical gamepads through SDL3, shared by the stream window and the host:
//! - [`read_pad`]: a pad's state in XInput's terms;
//! - [`host_pads`]: the host's own controllers, read on a thread of their own
//!   so the host can play through a seat (`couch_input::local`).
//!
//! The only crates that link SDL3 are this one and `couch-viewer`, each in its
//! own process: the `sdl3` crate takes whichever thread first starts SDL as
//! the main thread, so a process must never start it from two.

mod reader;

use couch_input::PadButtons;
use couch_proto::input::PadSnapshot;
use sdl3::gamepad::{Axis, Button, Gamepad};

pub use reader::host_pads;

/// A pad's state in XInput's terms: up is positive on the sticks, triggers
/// run 0 to 255.
pub fn read_pad(pad: &Gamepad) -> PadSnapshot {
    let mut buttons = PadButtons::empty();
    for (button, bit) in [
        (Button::South, PadButtons::A),
        (Button::East, PadButtons::B),
        (Button::West, PadButtons::X),
        (Button::North, PadButtons::Y),
        (Button::Back, PadButtons::BACK),
        (Button::Guide, PadButtons::GUIDE),
        (Button::Start, PadButtons::START),
        (Button::LeftStick, PadButtons::LEFT_THUMB),
        (Button::RightStick, PadButtons::RIGHT_THUMB),
        (Button::LeftShoulder, PadButtons::LEFT_SHOULDER),
        (Button::RightShoulder, PadButtons::RIGHT_SHOULDER),
        (Button::DPadUp, PadButtons::DPAD_UP),
        (Button::DPadDown, PadButtons::DPAD_DOWN),
        (Button::DPadLeft, PadButtons::DPAD_LEFT),
        (Button::DPadRight, PadButtons::DPAD_RIGHT),
    ] {
        if pad.button(button) {
            buttons |= bit;
        }
    }
    let up = |v: i16| (-i32::from(v)).clamp(-32768, 32767) as i16;
    let trigger = |v: i16| (i32::from(v.max(0)) * 255 / 32767) as u8;
    PadSnapshot {
        buttons: buttons.bits(),
        left_trigger: trigger(pad.axis(Axis::TriggerLeft)),
        right_trigger: trigger(pad.axis(Axis::TriggerRight)),
        left_x: pad.axis(Axis::LeftX),
        left_y: up(pad.axis(Axis::LeftY)),
        right_x: pad.axis(Axis::RightX),
        right_y: up(pad.axis(Axis::RightY)),
    }
}

/// Rumble as SDL takes it: 0 to 65535 per motor.
pub fn rumble_strength(v: u8) -> u16 {
    u16::from(v) * 257
}
