//! Typed control messages. Each has a fixed type id and a postcard payload.
//!
//! Released payloads are frozen: extend the protocol by adding message types,
//! never by changing an existing payload (docs/protocol.md). Payloads with
//! trailing bytes are rejected for the same reason.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

use crate::{
    envelope::{self, EnvelopeError, Frame, MUST_UNDERSTAND},
    ids::{Permissions, SeatEpoch, SeatId},
};

/// Type ids for control messages.
pub mod type_ids {
    use super::MUST_UNDERSTAND;

    pub const HELLO: u16 = MUST_UNDERSTAND | 0x0001;
    pub const KEYFRAME_REQUEST: u16 = MUST_UNDERSTAND | 0x0101;
    pub const SEAT_ASSIGNED: u16 = MUST_UNDERSTAND | 0x0201;
    pub const SEAT_RELEASED: u16 = MUST_UNDERSTAND | 0x0202;
}

/// Host to a member: you now hold this seat. Input must carry `seat_epoch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatAssigned {
    pub seat: SeatId,
    pub seat_epoch: SeatEpoch,
    pub permissions: Permissions,
}

/// Host to a member: you no longer hold this seat (the epoch that ended).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatReleased {
    pub seat: SeatId,
    pub seat_epoch: SeatEpoch,
}

/// A viewer can't decode without a fresh keyframe (start, decoder error).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyframeRequest {
    /// The newest epoch the viewer has seen.
    pub epoch: u32,
    pub reason: KeyframeReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyframeReason {
    Start,
    DecoderError,
}

/// Sent first, by both sides, on every control stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The sender's [`crate::PROTOCOL_MINOR`].
    pub proto_minor: u16,
    /// Human-readable app version, for logs and "please update" messages.
    pub app_version: String,
    /// Feature bits the sender understands. None are defined yet.
    pub features: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlMessage {
    Hello(Hello),
    KeyframeRequest(KeyframeRequest),
    SeatAssigned(SeatAssigned),
    SeatReleased(SeatReleased),
}

/// The result of reading one control frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    Message(ControlMessage),
    /// An optional message type this build doesn't know; safe to ignore.
    SkippedOptional(u16),
}

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error(transparent)]
    Envelope(#[from] EnvelopeError),
    #[error("could not serialize payload: {0}")]
    Payload(#[from] postcard::Error),
}

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("malformed payload for message type {msg_type:#06x}: {reason}")]
    Payload { msg_type: u16, reason: String },
    #[error("unknown message type {0:#06x} that must be understood")]
    UnknownRequired(u16),
}

impl ControlMessage {
    pub fn type_id(&self) -> u16 {
        match self {
            Self::Hello(_) => type_ids::HELLO,
            Self::KeyframeRequest(_) => type_ids::KEYFRAME_REQUEST,
            Self::SeatAssigned(_) => type_ids::SEAT_ASSIGNED,
            Self::SeatReleased(_) => type_ids::SEAT_RELEASED,
        }
    }

    /// Appends this message, framed, to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        let payload = match self {
            Self::Hello(m) => postcard::to_stdvec(m)?,
            Self::KeyframeRequest(m) => postcard::to_stdvec(m)?,
            Self::SeatAssigned(m) => postcard::to_stdvec(m)?,
            Self::SeatReleased(m) => postcard::to_stdvec(m)?,
        };
        envelope::encode(self.type_id(), &payload, out)?;
        Ok(())
    }

    /// Interprets a frame produced by [`envelope::decode`].
    pub fn from_frame(frame: &Frame) -> Result<Decoded, DecodeError> {
        match frame.msg_type {
            type_ids::HELLO => Ok(Decoded::Message(Self::Hello(payload(frame)?))),
            type_ids::KEYFRAME_REQUEST => {
                Ok(Decoded::Message(Self::KeyframeRequest(payload(frame)?)))
            }
            type_ids::SEAT_ASSIGNED => Ok(Decoded::Message(Self::SeatAssigned(payload(frame)?))),
            type_ids::SEAT_RELEASED => Ok(Decoded::Message(Self::SeatReleased(payload(frame)?))),
            other if frame.must_understand() => Err(DecodeError::UnknownRequired(other)),
            other => Ok(Decoded::SkippedOptional(other)),
        }
    }
}

/// A frame's postcard payload, with nothing left over.
pub(crate) fn payload<T: DeserializeOwned>(frame: &Frame) -> Result<T, DecodeError> {
    let malformed = |reason: String| DecodeError::Payload {
        msg_type: frame.msg_type,
        reason,
    };
    let (value, rest) =
        postcard::take_from_bytes(&frame.payload).map_err(|e| malformed(e.to_string()))?;
    if !rest.is_empty() {
        return Err(malformed(format!("{} trailing bytes", rest.len())));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> ControlMessage {
        ControlMessage::Hello(Hello {
            proto_minor: 0,
            app_version: "0.1.0".into(),
            features: 0,
        })
    }

    #[test]
    fn hello_roundtrips_through_the_envelope() {
        let mut buf = Vec::new();
        hello().encode(&mut buf).unwrap();
        let (frame, _) = envelope::decode(&buf).unwrap().unwrap();
        assert_eq!(
            ControlMessage::from_frame(&frame).unwrap(),
            Decoded::Message(hello())
        );
    }

    #[test]
    fn seat_messages_roundtrip() {
        for message in [
            ControlMessage::SeatAssigned(SeatAssigned {
                seat: SeatId(0),
                seat_epoch: SeatEpoch(3),
                permissions: Permissions::GAMEPAD | Permissions::KEYBOARD,
            }),
            ControlMessage::SeatReleased(SeatReleased {
                seat: SeatId(1),
                seat_epoch: SeatEpoch(u32::MAX),
            }),
        ] {
            let mut buf = Vec::new();
            message.encode(&mut buf).unwrap();
            let (frame, _) = envelope::decode(&buf).unwrap().unwrap();
            assert_eq!(
                ControlMessage::from_frame(&frame).unwrap(),
                Decoded::Message(message)
            );
        }
    }

    #[test]
    fn unknown_optional_type_is_skipped() {
        let frame = Frame {
            msg_type: 0x0042,
            payload: vec![1, 2, 3],
        };
        assert_eq!(
            ControlMessage::from_frame(&frame).unwrap(),
            Decoded::SkippedOptional(0x0042)
        );
    }

    #[test]
    fn unknown_required_type_is_an_error() {
        let frame = Frame {
            msg_type: MUST_UNDERSTAND | 0x0042,
            payload: vec![],
        };
        assert!(matches!(
            ControlMessage::from_frame(&frame),
            Err(DecodeError::UnknownRequired(_))
        ));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut payload = postcard::to_stdvec(&Hello {
            proto_minor: 0,
            app_version: String::new(),
            features: 0,
        })
        .unwrap();
        payload.push(0);
        let frame = Frame {
            msg_type: type_ids::HELLO,
            payload,
        };
        assert!(matches!(
            ControlMessage::from_frame(&frame),
            Err(DecodeError::Payload { .. })
        ));
    }
}
