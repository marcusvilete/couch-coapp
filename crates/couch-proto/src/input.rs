//! Input from a seat holder to the host, and rumble back (docs/protocol.md,
//! "Input").
//!
//! Keys and mouse buttons go on the input stream, which is reliable and
//! ordered, framed like the control stream ([`crate::envelope`]). Gamepad
//! state and mouse motion go as datagrams carrying a full snapshot or running
//! totals, so a lost datagram costs nothing: the next one carries everything.

use serde::{Deserialize, Serialize};

use crate::{
    control::{DecodeError, EncodeError, payload},
    envelope::{self, Frame, MUST_UNDERSTAND},
    ids::{SeatEpoch, SeatId},
};

/// Keyboard usages run from 0x04 (A) to 0xE7 (Right GUI); the rest of the
/// page is reserved or vendor-specific.
pub const MIN_KEY_USAGE: u16 = 0x04;
pub const MAX_KEY_USAGE: u16 = 0xE7;

/// Mouse button codes on the wire.
pub mod mouse_button {
    pub const LEFT: u8 = 1;
    pub const RIGHT: u8 = 2;
    pub const MIDDLE: u8 = 3;
    pub const BACK: u8 = 4;
    pub const FORWARD: u8 = 5;
}

/// A gamepad's full state, in XInput's layout: button bits as in
/// `XINPUT_GAMEPAD`, triggers 0 to 255, sticks with up and right positive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct PadSnapshot {
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
}

/// Mouse motion as running totals since the seat epoch began. They wrap, and
/// the host takes differences with wrapping arithmetic. `version` counts
/// changes, so the host can tell newer totals from older ones arriving late
/// on the other channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct MouseTotals {
    pub version: u32,
    pub x: i32,
    pub y: i32,
    /// Wheel totals in 1/120 notches (Windows' `WHEEL_DELTA`).
    pub wheel_x: i32,
    pub wheel_y: i32,
}

/// Datagram [`crate::media::datagram::GAMEPAD`]: sent on every change and
/// every 100 ms while holding a seat, so it doubles as the holder's heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadDatagram {
    pub seat: SeatId,
    pub seat_epoch: SeatEpoch,
    /// Grows with every datagram in the seat epoch; older ones are dropped.
    pub seq: u32,
    pub pad: PadSnapshot,
}

/// Datagram [`crate::media::datagram::MOUSE_MOTION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseDatagram {
    pub seat: SeatId,
    pub seat_epoch: SeatEpoch,
    pub totals: MouseTotals,
}

/// Datagram [`crate::media::datagram::RUMBLE`], host to the seat's holder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RumbleDatagram {
    pub seat: SeatId,
    pub low: u8,
    pub high: u8,
}

/// Type ids for messages on the input stream.
pub mod type_ids {
    use super::MUST_UNDERSTAND;

    pub const KEY: u16 = MUST_UNDERSTAND | 0x0301;
    pub const MOUSE_BUTTON: u16 = MUST_UNDERSTAND | 0x0302;
    pub const HELD: u16 = MUST_UNDERSTAND | 0x0303;
}

/// A key went down or up, by physical position (a USB HID usage).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEvent {
    pub seat: SeatId,
    pub seat_epoch: SeatEpoch,
    pub usage: u16,
    pub down: bool,
}

/// A mouse button went down or up. Carries the motion totals, so the host
/// moves the pointer to where the click happened first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseButtonEvent {
    pub seat: SeatId,
    pub seat_epoch: SeatEpoch,
    pub button: u8,
    pub down: bool,
    pub totals: MouseTotals,
}

/// Everything the holder has down, every second. The host releases anything
/// it injected that isn't in here; it never presses keys from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldInput {
    pub seat: SeatId,
    pub seat_epoch: SeatEpoch,
    pub keys: KeyBitmap,
    /// Bit `code - 1` for each held [`mouse_button`].
    pub buttons: u8,
}

/// A set of keyboard usages below 256.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct KeyBitmap(pub [u8; 32]);

impl KeyBitmap {
    pub fn set(&mut self, usage: u16, held: bool) {
        let Some(byte) = self.0.get_mut(usize::from(usage / 8)) else {
            return;
        };
        let bit = 1 << (usage % 8);
        if held {
            *byte |= bit;
        } else {
            *byte &= !bit;
        }
    }

    pub fn contains(&self, usage: u16) -> bool {
        self.0
            .get(usize::from(usage / 8))
            .is_some_and(|byte| byte & (1 << (usage % 8)) != 0)
    }

    pub fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        (0..256u16).filter(|&u| self.contains(u))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMessage {
    Key(KeyEvent),
    MouseButton(MouseButtonEvent),
    Held(HeldInput),
}

impl InputMessage {
    pub fn type_id(&self) -> u16 {
        match self {
            Self::Key(_) => type_ids::KEY,
            Self::MouseButton(_) => type_ids::MOUSE_BUTTON,
            Self::Held(_) => type_ids::HELD,
        }
    }

    /// Appends this message, framed, to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        let payload = match self {
            Self::Key(m) => postcard::to_stdvec(m)?,
            Self::MouseButton(m) => postcard::to_stdvec(m)?,
            Self::Held(m) => postcard::to_stdvec(m)?,
        };
        envelope::encode(self.type_id(), &payload, out)?;
        Ok(())
    }

    /// Interprets a frame from the input stream; `Ok(None)` for an optional
    /// type this build doesn't know.
    pub fn from_frame(frame: &Frame) -> Result<Option<Self>, DecodeError> {
        Ok(Some(match frame.msg_type {
            type_ids::KEY => Self::Key(payload(frame)?),
            type_ids::MOUSE_BUTTON => Self::MouseButton(payload(frame)?),
            type_ids::HELD => Self::Held(payload(frame)?),
            other if frame.must_understand() => return Err(DecodeError::UnknownRequired(other)),
            _ => return Ok(None),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{datagram, datagram_tag, decode_payload, encode_datagram};

    const SEAT: SeatId = SeatId(0);
    const EPOCH: SeatEpoch = SeatEpoch(7);

    fn roundtrip(message: InputMessage) {
        let mut buf = Vec::new();
        message.encode(&mut buf).unwrap();
        let (frame, used) = envelope::decode(&buf).unwrap().unwrap();
        assert_eq!(used, buf.len());
        assert_eq!(InputMessage::from_frame(&frame).unwrap(), Some(message));
    }

    #[test]
    fn stream_messages_roundtrip() {
        roundtrip(InputMessage::Key(KeyEvent {
            seat: SEAT,
            seat_epoch: EPOCH,
            usage: 0x1A,
            down: true,
        }));
        roundtrip(InputMessage::MouseButton(MouseButtonEvent {
            seat: SEAT,
            seat_epoch: EPOCH,
            button: mouse_button::LEFT,
            down: false,
            totals: MouseTotals {
                version: 3,
                x: -40,
                y: i32::MAX,
                wheel_x: 0,
                wheel_y: -120,
            },
        }));
        let mut keys = KeyBitmap::default();
        keys.set(0x04, true);
        keys.set(0xE7, true);
        roundtrip(InputMessage::Held(HeldInput {
            seat: SEAT,
            seat_epoch: EPOCH,
            keys,
            buttons: 0b101,
        }));
    }

    #[test]
    fn unknown_types_follow_the_must_understand_bit() {
        let optional = Frame {
            msg_type: 0x0399,
            payload: vec![],
        };
        assert_eq!(InputMessage::from_frame(&optional).unwrap(), None);
        let required = Frame {
            msg_type: MUST_UNDERSTAND | 0x0399,
            payload: vec![],
        };
        assert!(InputMessage::from_frame(&required).is_err());
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut payload = postcard::to_stdvec(&KeyEvent {
            seat: SEAT,
            seat_epoch: EPOCH,
            usage: 4,
            down: true,
        })
        .unwrap();
        payload.push(0);
        let frame = Frame {
            msg_type: type_ids::KEY,
            payload,
        };
        assert!(InputMessage::from_frame(&frame).is_err());
    }

    #[test]
    fn key_bitmap_holds_every_keyboard_usage() {
        let mut keys = KeyBitmap::default();
        for usage in [MIN_KEY_USAGE, 0x29, MAX_KEY_USAGE] {
            keys.set(usage, true);
        }
        assert_eq!(keys.iter().collect::<Vec<_>>(), vec![0x04, 0x29, 0xE7]);
        keys.set(0x29, false);
        assert!(!keys.contains(0x29));
        // Out of range is ignored rather than panicking.
        keys.set(300, true);
        assert!(!keys.contains(300));
    }

    #[test]
    fn datagrams_roundtrip_and_stay_small() {
        let pad = PadDatagram {
            seat: SEAT,
            seat_epoch: SeatEpoch(u32::MAX),
            seq: u32::MAX,
            pad: PadSnapshot {
                buttons: u16::MAX,
                left_trigger: 255,
                right_trigger: 255,
                left_x: i16::MIN,
                left_y: i16::MIN,
                right_x: i16::MIN,
                right_y: i16::MIN,
            },
        };
        let bytes = encode_datagram(datagram::GAMEPAD, &pad);
        assert!(bytes.len() <= 32, "{} bytes", bytes.len());
        let (tag, body) = datagram_tag(&bytes).unwrap();
        assert_eq!(tag, datagram::GAMEPAD);
        assert_eq!(decode_payload::<PadDatagram>(body), Some(pad));

        let mouse = MouseDatagram {
            seat: SEAT,
            seat_epoch: EPOCH,
            totals: MouseTotals {
                version: u32::MAX,
                x: i32::MIN,
                y: i32::MIN,
                wheel_x: i32::MIN,
                wheel_y: i32::MIN,
            },
        };
        let bytes = encode_datagram(datagram::MOUSE_MOTION, &mouse);
        assert!(bytes.len() <= 32, "{} bytes", bytes.len());
        assert_eq!(decode_payload::<MouseDatagram>(&bytes[1..]), Some(mouse));
    }
}
