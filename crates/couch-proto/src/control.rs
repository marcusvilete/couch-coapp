//! Typed control messages. Each has a fixed type id and a postcard payload.
//!
//! Released payloads are frozen: extend the protocol by adding message types,
//! never by changing an existing payload (docs/protocol.md). Payloads with
//! trailing bytes are rejected for the same reason.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

use crate::{
    admission::{AdmitConfirm, AdmitReply, JoinStart, MemberHello},
    envelope::{self, EnvelopeError, Frame, MUST_UNDERSTAND},
    ids::{Permissions, SeatEpoch, SeatId},
    room::{Goodbye, MemberRecord, Presence, RoomPolicy, RoomSnapshot, Signed},
};

/// Type ids for control messages.
pub mod type_ids {
    use super::MUST_UNDERSTAND;

    pub const HELLO: u16 = MUST_UNDERSTAND | 0x0001;
    pub const GOODBYE: u16 = MUST_UNDERSTAND | 0x0002;
    pub const KEYFRAME_REQUEST: u16 = MUST_UNDERSTAND | 0x0101;
    pub const SEAT_ASSIGNED: u16 = MUST_UNDERSTAND | 0x0201;
    pub const SEAT_RELEASED: u16 = MUST_UNDERSTAND | 0x0202;
    pub const JOIN_START: u16 = MUST_UNDERSTAND | 0x0401;
    pub const MEMBER_HELLO: u16 = MUST_UNDERSTAND | 0x0402;
    pub const ADMIT_REPLY: u16 = MUST_UNDERSTAND | 0x0403;
    pub const ADMIT_CONFIRM: u16 = MUST_UNDERSTAND | 0x0404;
    pub const ROOM_SNAPSHOT: u16 = MUST_UNDERSTAND | 0x0501;
    pub const MEMBER_UPDATE: u16 = MUST_UNDERSTAND | 0x0502;
    pub const POLICY_UPDATE: u16 = MUST_UNDERSTAND | 0x0503;
    /// Optional: a peer that ignores presence still works, just repairs slower.
    pub const PRESENCE: u16 = 0x0504;
}

/// QUIC application close codes for connections between members.
pub mod close_code {
    pub const NORMAL: u32 = 0;
    /// The password check failed: wrong password, or a confirmation that
    /// didn't arrive in time.
    pub const ADMISSION_FAILED: u32 = 1;
    pub const BANNED: u32 = 2;
    /// A second connection between the same members in the same room; the
    /// other one is kept.
    pub const DUPLICATE: u32 = 3;
    pub const VERSION: u32 = 4;
    pub const RATE_LIMITED: u32 = 5;
    pub const ROOM_FULL: u32 = 6;
    pub const LEFT_ROOM: u32 = 7;
    /// The invite expired, was revoked, or took too many wrong passwords.
    pub const INVITE_EXPIRED: u32 = 8;
    /// The acceptor isn't in that room (any more).
    pub const UNKNOWN_ROOM: u32 = 9;
    /// A malformed or unexpected message.
    pub const PROTOCOL: u32 = 10;
    /// Not on the owner's member list (any more).
    pub const NOT_MEMBER: u32 = 11;
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
    Goodbye(Goodbye),
    KeyframeRequest(KeyframeRequest),
    SeatAssigned(SeatAssigned),
    SeatReleased(SeatReleased),
    JoinStart(JoinStart),
    MemberHello(Box<MemberHello>),
    AdmitReply(AdmitReply),
    AdmitConfirm(AdmitConfirm),
    RoomSnapshot(Box<RoomSnapshot>),
    MemberUpdate(Box<Signed<MemberRecord>>),
    PolicyUpdate(Box<Signed<RoomPolicy>>),
    Presence(Presence),
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
            Self::Goodbye(_) => type_ids::GOODBYE,
            Self::KeyframeRequest(_) => type_ids::KEYFRAME_REQUEST,
            Self::SeatAssigned(_) => type_ids::SEAT_ASSIGNED,
            Self::SeatReleased(_) => type_ids::SEAT_RELEASED,
            Self::JoinStart(_) => type_ids::JOIN_START,
            Self::MemberHello(_) => type_ids::MEMBER_HELLO,
            Self::AdmitReply(_) => type_ids::ADMIT_REPLY,
            Self::AdmitConfirm(_) => type_ids::ADMIT_CONFIRM,
            Self::RoomSnapshot(_) => type_ids::ROOM_SNAPSHOT,
            Self::MemberUpdate(_) => type_ids::MEMBER_UPDATE,
            Self::PolicyUpdate(_) => type_ids::POLICY_UPDATE,
            Self::Presence(_) => type_ids::PRESENCE,
        }
    }

    /// Appends this message, framed, to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        let payload = match self {
            Self::Hello(m) => postcard::to_stdvec(m)?,
            Self::Goodbye(m) => postcard::to_stdvec(m)?,
            Self::KeyframeRequest(m) => postcard::to_stdvec(m)?,
            Self::SeatAssigned(m) => postcard::to_stdvec(m)?,
            Self::SeatReleased(m) => postcard::to_stdvec(m)?,
            Self::JoinStart(m) => postcard::to_stdvec(m)?,
            Self::MemberHello(m) => postcard::to_stdvec(m)?,
            Self::AdmitReply(m) => postcard::to_stdvec(m)?,
            Self::AdmitConfirm(m) => postcard::to_stdvec(m)?,
            Self::RoomSnapshot(m) => postcard::to_stdvec(m)?,
            Self::MemberUpdate(m) => postcard::to_stdvec(m)?,
            Self::PolicyUpdate(m) => postcard::to_stdvec(m)?,
            Self::Presence(m) => postcard::to_stdvec(m)?,
        };
        envelope::encode(self.type_id(), &payload, out)?;
        Ok(())
    }

    /// Interprets a frame produced by [`envelope::decode`].
    pub fn from_frame(frame: &Frame) -> Result<Decoded, DecodeError> {
        let message = match frame.msg_type {
            type_ids::HELLO => Self::Hello(payload(frame)?),
            type_ids::GOODBYE => Self::Goodbye(payload(frame)?),
            type_ids::KEYFRAME_REQUEST => Self::KeyframeRequest(payload(frame)?),
            type_ids::SEAT_ASSIGNED => Self::SeatAssigned(payload(frame)?),
            type_ids::SEAT_RELEASED => Self::SeatReleased(payload(frame)?),
            type_ids::JOIN_START => Self::JoinStart(payload(frame)?),
            type_ids::MEMBER_HELLO => Self::MemberHello(payload(frame)?),
            type_ids::ADMIT_REPLY => Self::AdmitReply(payload(frame)?),
            type_ids::ADMIT_CONFIRM => Self::AdmitConfirm(payload(frame)?),
            type_ids::ROOM_SNAPSHOT => Self::RoomSnapshot(payload(frame)?),
            type_ids::MEMBER_UPDATE => Self::MemberUpdate(payload(frame)?),
            type_ids::POLICY_UPDATE => Self::PolicyUpdate(payload(frame)?),
            type_ids::PRESENCE => Self::Presence(payload(frame)?),
            other if frame.must_understand() => return Err(DecodeError::UnknownRequired(other)),
            other => return Ok(Decoded::SkippedOptional(other)),
        };
        Ok(Decoded::Message(message))
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

    fn roundtrip(message: ControlMessage) {
        let mut buf = Vec::new();
        message.encode(&mut buf).unwrap();
        let (frame, used) = envelope::decode(&buf).unwrap().unwrap();
        assert_eq!(used, buf.len());
        assert_eq!(
            ControlMessage::from_frame(&frame).unwrap(),
            Decoded::Message(message)
        );
    }

    #[test]
    fn admission_and_room_messages_roundtrip() {
        use crate::{
            admission::PakeMessage,
            ids::{InviteTag, MemberId, RoomId, Signature},
            room::{GoodbyeReason, PeerAddr},
        };
        let sig = Signature([1; 32], [2; 32]);
        let record = Signed {
            body: MemberRecord {
                room: RoomId([3; 16]),
                id: MemberId([4; 32]),
                since: 1,
                version: 9,
                name: "Bia".into(),
                addr: PeerAddr {
                    relay: Some("https://relay.example./".into()),
                    ips: vec!["10.0.0.2:51000".parse().unwrap()],
                },
                left: false,
            },
            sig,
        };
        let policy = Signed {
            body: RoomPolicy {
                room: RoomId([3; 16]),
                version: 2,
                name: "Couch".into(),
                roster: vec![crate::room::RosterEntry {
                    id: MemberId([4; 32]),
                    since: 1,
                }],
                banned: vec![MemberId([6; 32])],
            },
            sig,
        };
        for message in [
            ControlMessage::Goodbye(Goodbye {
                reason: GoodbyeReason::LeftRoom,
            }),
            ControlMessage::JoinStart(JoinStart {
                room: RoomId([3; 16]),
                invite: InviteTag([12; 16]),
                pake: PakeMessage(b'A', [7; 32]),
            }),
            ControlMessage::MemberHello(Box::new(MemberHello {
                room: RoomId([3; 16]),
                policy: policy.clone(),
            })),
            ControlMessage::AdmitReply(AdmitReply {
                pake: PakeMessage(b'B', [8; 32]),
                confirm: [9; 32],
            }),
            ControlMessage::AdmitConfirm(AdmitConfirm { confirm: [10; 32] }),
            ControlMessage::RoomSnapshot(Box::new(RoomSnapshot {
                policy: policy.clone(),
                members: vec![record.clone(), record.clone()],
            })),
            ControlMessage::MemberUpdate(Box::new(record)),
            ControlMessage::PolicyUpdate(Box::new(policy)),
            ControlMessage::Presence(Presence {
                connected: vec![MemberId([11; 32])],
            }),
        ] {
            roundtrip(message);
        }
    }

    #[test]
    fn room_messages_must_be_understood_except_presence() {
        for id in [
            type_ids::GOODBYE,
            type_ids::JOIN_START,
            type_ids::MEMBER_HELLO,
            type_ids::ADMIT_REPLY,
            type_ids::ADMIT_CONFIRM,
            type_ids::ROOM_SNAPSHOT,
            type_ids::MEMBER_UPDATE,
            type_ids::POLICY_UPDATE,
        ] {
            assert_ne!(id & MUST_UNDERSTAND, 0, "{id:#06x}");
        }
        assert_eq!(type_ids::PRESENCE & MUST_UNDERSTAND, 0);
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
