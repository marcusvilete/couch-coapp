//! Typed control messages. Each has a fixed type id and a postcard payload.
//!
//! A payload may change in place, with the wire revision bumped
//! ([`crate::PROTOCOL_MINOR`], docs/protocol.md). Payloads with trailing
//! bytes are rejected, so a change that forgets the bump fails loudly.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

use crate::{
    admission::{AdmitConfirm, AdmitReply, JoinStart, MemberHello},
    envelope::{self, EnvelopeError, Frame, MUST_UNDERSTAND},
    ids::{Permissions, SeatEpoch, SeatId},
    remote::{
        RemoteCommand, RemoteLock, RemoteOffer, RemoteReply, RemoteState, RemoteUnlock,
        RemoteUnlockReply,
    },
    room::{
        Goodbye, HandoverAnswer, HandoverOffer, HandoverWithdraw, MemberRecord, OwnedPolicy,
        Presence, RoomSnapshot, Signed,
    },
    session::{
        ControlCancel, ControlDeclined, ControlRequest, GameFocus, SessionAnnounce, SessionEnded,
        SessionJoin, SessionJoinReply, SessionLeave, SessionStatus, ViewerControllers,
    },
};

/// Type ids for control messages.
pub mod type_ids {
    use super::MUST_UNDERSTAND;

    pub const HELLO: u16 = MUST_UNDERSTAND | 0x0001;
    pub const GOODBYE: u16 = MUST_UNDERSTAND | 0x0002;
    /// Optional: a peer that doesn't know it is simply never timed out.
    pub const ALIVE: u16 = 0x0003;
    pub const KEYFRAME_REQUEST: u16 = MUST_UNDERSTAND | 0x0101;
    pub const SEAT_ASSIGNED: u16 = MUST_UNDERSTAND | 0x0201;
    pub const SEAT_RELEASED: u16 = MUST_UNDERSTAND | 0x0202;
    pub const CONTROL_REQUEST: u16 = MUST_UNDERSTAND | 0x0203;
    pub const CONTROL_CANCEL: u16 = MUST_UNDERSTAND | 0x0204;
    pub const CONTROL_DECLINED: u16 = MUST_UNDERSTAND | 0x0205;
    /// Optional: a member that doesn't know it keeps choosing its
    /// controllers itself (P12).
    pub const SEAT_CONTROLLER: u16 = 0x0206;
    pub const JOIN_START: u16 = MUST_UNDERSTAND | 0x0401;
    pub const MEMBER_HELLO: u16 = MUST_UNDERSTAND | 0x0402;
    pub const ADMIT_REPLY: u16 = MUST_UNDERSTAND | 0x0403;
    pub const ADMIT_CONFIRM: u16 = MUST_UNDERSTAND | 0x0404;
    pub const ROOM_SNAPSHOT: u16 = MUST_UNDERSTAND | 0x0501;
    pub const MEMBER_UPDATE: u16 = MUST_UNDERSTAND | 0x0502;
    pub const POLICY_UPDATE: u16 = MUST_UNDERSTAND | 0x0503;
    /// Optional: a peer that ignores presence still works, just repairs slower.
    pub const PRESENCE: u16 = 0x0504;
    /// Handing a room over (M13): a peer that doesn't know them can't take
    /// part in the room's ownership.
    pub const HANDOVER_OFFER: u16 = MUST_UNDERSTAND | 0x0505;
    pub const HANDOVER_ANSWER: u16 = MUST_UNDERSTAND | 0x0506;
    pub const HANDOVER_WITHDRAW: u16 = MUST_UNDERSTAND | 0x0507;
    /// The session directory is optional: a peer that doesn't know sessions
    /// just doesn't show them. The rest only follow an announcement.
    pub const SESSION_ANNOUNCE: u16 = 0x0601;
    pub const SESSION_STATUS: u16 = 0x0602;
    pub const SESSION_ENDED: u16 = 0x0603;
    pub const SESSION_JOIN: u16 = MUST_UNDERSTAND | 0x0604;
    pub const SESSION_JOIN_REPLY: u16 = MUST_UNDERSTAND | 0x0605;
    pub const SESSION_LEAVE: u16 = MUST_UNDERSTAND | 0x0606;
    /// Optional: without it, players only miss the "not in front" note.
    pub const GAME_FOCUS: u16 = 0x0607;
    /// Optional (M18): without it, a viewer counts as having no controller
    /// to spare, and every seat it gets moves it.
    pub const VIEWER_CONTROLLERS: u16 = 0x0608;
    /// Remote play (M25): only between a home PC and a device it trusts.
    /// The offer is optional: a device that doesn't know it just never
    /// unlocks.
    pub const REMOTE_OFFER: u16 = 0x0701;
    pub const REMOTE_UNLOCK: u16 = MUST_UNDERSTAND | 0x0702;
    pub const REMOTE_UNLOCK_REPLY: u16 = MUST_UNDERSTAND | 0x0703;
    pub const REMOTE_COMMAND: u16 = MUST_UNDERSTAND | 0x0704;
    pub const REMOTE_REPLY: u16 = MUST_UNDERSTAND | 0x0705;
    pub const REMOTE_STATE: u16 = MUST_UNDERSTAND | 0x0706;
    pub const REMOTE_LOCK: u16 = MUST_UNDERSTAND | 0x0707;
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
    /// Nothing heard for [`SILENT_LIMIT`](crate::control::SILENT_LIMIT) from
    /// a peer that sends `Alive`.
    pub const TIMED_OUT: u32 = 12;
}

/// Each side sends [`Alive`] after saying nothing for this long.
pub const ALIVE_EVERY: std::time::Duration = std::time::Duration::from_secs(1);
/// A peer that has sent `Alive` and then says nothing for this long is taken
/// for gone: its connection is closed with `TIMED_OUT`.
pub const SILENT_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Host to a member: you now hold this seat. Input must carry `seat_epoch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatAssigned {
    pub seat: SeatId,
    pub seat_epoch: SeatEpoch,
    pub permissions: Permissions,
    /// Which of the member's controllers the host chose for it, by its slot
    /// in the member's `ControllerReport` (P12); `None` lets the member
    /// choose. The member follows it while that controller is connected.
    pub controller: Option<u8>,
}

/// Host to a member, optional: drive this seat you hold with your
/// controller `controller` (P12, by its slot), swapping with the seat it
/// drove, if any. Ignored for a seat the member doesn't hold (a release
/// came first) or a controller it doesn't have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatController {
    pub seat: SeatId,
    pub controller: u8,
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

/// "Still here": each side sends it after a second of saying nothing, so a
/// peer silent for 5 s can be taken for gone (docs/protocol.md, Liveness).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alive {}

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
    Alive(Alive),
    KeyframeRequest(KeyframeRequest),
    SeatAssigned(SeatAssigned),
    SeatReleased(SeatReleased),
    SeatController(SeatController),
    JoinStart(JoinStart),
    MemberHello(Box<MemberHello>),
    AdmitReply(AdmitReply),
    AdmitConfirm(AdmitConfirm),
    RoomSnapshot(Box<RoomSnapshot>),
    MemberUpdate(Box<Signed<MemberRecord>>),
    PolicyUpdate(Box<OwnedPolicy>),
    Presence(Presence),
    HandoverOffer(HandoverOffer),
    HandoverAnswer(HandoverAnswer),
    HandoverWithdraw(HandoverWithdraw),
    SessionAnnounce(SessionAnnounce),
    SessionStatus(SessionStatus),
    SessionEnded(SessionEnded),
    SessionJoin(SessionJoin),
    SessionJoinReply(SessionJoinReply),
    SessionLeave(SessionLeave),
    ControlRequest(ControlRequest),
    ControlCancel(ControlCancel),
    ControlDeclined(ControlDeclined),
    GameFocus(GameFocus),
    ViewerControllers(ViewerControllers),
    RemoteOffer(RemoteOffer),
    RemoteUnlock(RemoteUnlock),
    RemoteUnlockReply(RemoteUnlockReply),
    RemoteCommand(RemoteCommand),
    RemoteReply(Box<RemoteReply>),
    RemoteState(Box<RemoteState>),
    RemoteLock(RemoteLock),
}

impl ControlMessage {
    /// Whether this is remote play's (M25), between a home PC and a device
    /// it trusts.
    pub fn is_remote(&self) -> bool {
        matches!(
            self,
            Self::RemoteOffer(_)
                | Self::RemoteUnlock(_)
                | Self::RemoteUnlockReply(_)
                | Self::RemoteCommand(_)
                | Self::RemoteReply(_)
                | Self::RemoteState(_)
                | Self::RemoteLock(_)
        )
    }

    /// Whether this is about a session (its directory, joining, control),
    /// which the session's host and its viewers handle, rather than the room.
    pub fn is_session(&self) -> bool {
        matches!(
            self,
            Self::SessionAnnounce(_)
                | Self::SessionStatus(_)
                | Self::SessionEnded(_)
                | Self::SessionJoin(_)
                | Self::SessionJoinReply(_)
                | Self::SessionLeave(_)
                | Self::ControlRequest(_)
                | Self::ControlCancel(_)
                | Self::ControlDeclined(_)
                | Self::GameFocus(_)
                | Self::ViewerControllers(_)
                | Self::SeatAssigned(_)
                | Self::SeatReleased(_)
                | Self::SeatController(_)
                | Self::KeyframeRequest(_)
        )
    }
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
            Self::Alive(_) => type_ids::ALIVE,
            Self::KeyframeRequest(_) => type_ids::KEYFRAME_REQUEST,
            Self::SeatAssigned(_) => type_ids::SEAT_ASSIGNED,
            Self::SeatReleased(_) => type_ids::SEAT_RELEASED,
            Self::SeatController(_) => type_ids::SEAT_CONTROLLER,
            Self::JoinStart(_) => type_ids::JOIN_START,
            Self::MemberHello(_) => type_ids::MEMBER_HELLO,
            Self::AdmitReply(_) => type_ids::ADMIT_REPLY,
            Self::AdmitConfirm(_) => type_ids::ADMIT_CONFIRM,
            Self::RoomSnapshot(_) => type_ids::ROOM_SNAPSHOT,
            Self::MemberUpdate(_) => type_ids::MEMBER_UPDATE,
            Self::PolicyUpdate(_) => type_ids::POLICY_UPDATE,
            Self::Presence(_) => type_ids::PRESENCE,
            Self::HandoverOffer(_) => type_ids::HANDOVER_OFFER,
            Self::HandoverAnswer(_) => type_ids::HANDOVER_ANSWER,
            Self::HandoverWithdraw(_) => type_ids::HANDOVER_WITHDRAW,
            Self::SessionAnnounce(_) => type_ids::SESSION_ANNOUNCE,
            Self::SessionStatus(_) => type_ids::SESSION_STATUS,
            Self::SessionEnded(_) => type_ids::SESSION_ENDED,
            Self::SessionJoin(_) => type_ids::SESSION_JOIN,
            Self::SessionJoinReply(_) => type_ids::SESSION_JOIN_REPLY,
            Self::SessionLeave(_) => type_ids::SESSION_LEAVE,
            Self::ControlRequest(_) => type_ids::CONTROL_REQUEST,
            Self::ControlCancel(_) => type_ids::CONTROL_CANCEL,
            Self::ControlDeclined(_) => type_ids::CONTROL_DECLINED,
            Self::GameFocus(_) => type_ids::GAME_FOCUS,
            Self::ViewerControllers(_) => type_ids::VIEWER_CONTROLLERS,
            Self::RemoteOffer(_) => type_ids::REMOTE_OFFER,
            Self::RemoteUnlock(_) => type_ids::REMOTE_UNLOCK,
            Self::RemoteUnlockReply(_) => type_ids::REMOTE_UNLOCK_REPLY,
            Self::RemoteCommand(_) => type_ids::REMOTE_COMMAND,
            Self::RemoteReply(_) => type_ids::REMOTE_REPLY,
            Self::RemoteState(_) => type_ids::REMOTE_STATE,
            Self::RemoteLock(_) => type_ids::REMOTE_LOCK,
        }
    }

    /// Appends this message, framed, to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        let payload = match self {
            Self::Hello(m) => postcard::to_stdvec(m)?,
            Self::Goodbye(m) => postcard::to_stdvec(m)?,
            Self::Alive(m) => postcard::to_stdvec(m)?,
            Self::KeyframeRequest(m) => postcard::to_stdvec(m)?,
            Self::SeatAssigned(m) => postcard::to_stdvec(m)?,
            Self::SeatReleased(m) => postcard::to_stdvec(m)?,
            Self::SeatController(m) => postcard::to_stdvec(m)?,
            Self::JoinStart(m) => postcard::to_stdvec(m)?,
            Self::MemberHello(m) => postcard::to_stdvec(m)?,
            Self::AdmitReply(m) => postcard::to_stdvec(m)?,
            Self::AdmitConfirm(m) => postcard::to_stdvec(m)?,
            Self::RoomSnapshot(m) => postcard::to_stdvec(m)?,
            Self::MemberUpdate(m) => postcard::to_stdvec(m)?,
            Self::PolicyUpdate(m) => postcard::to_stdvec(m)?,
            Self::Presence(m) => postcard::to_stdvec(m)?,
            Self::HandoverOffer(m) => postcard::to_stdvec(m)?,
            Self::HandoverAnswer(m) => postcard::to_stdvec(m)?,
            Self::HandoverWithdraw(m) => postcard::to_stdvec(m)?,
            Self::SessionAnnounce(m) => postcard::to_stdvec(m)?,
            Self::SessionStatus(m) => postcard::to_stdvec(m)?,
            Self::SessionEnded(m) => postcard::to_stdvec(m)?,
            Self::SessionJoin(m) => postcard::to_stdvec(m)?,
            Self::SessionJoinReply(m) => postcard::to_stdvec(m)?,
            Self::SessionLeave(m) => postcard::to_stdvec(m)?,
            Self::ControlRequest(m) => postcard::to_stdvec(m)?,
            Self::ControlCancel(m) => postcard::to_stdvec(m)?,
            Self::ControlDeclined(m) => postcard::to_stdvec(m)?,
            Self::GameFocus(m) => postcard::to_stdvec(m)?,
            Self::ViewerControllers(m) => postcard::to_stdvec(m)?,
            Self::RemoteOffer(m) => postcard::to_stdvec(m)?,
            Self::RemoteUnlock(m) => postcard::to_stdvec(m)?,
            Self::RemoteUnlockReply(m) => postcard::to_stdvec(m)?,
            Self::RemoteCommand(m) => postcard::to_stdvec(m)?,
            Self::RemoteReply(m) => postcard::to_stdvec(m)?,
            Self::RemoteState(m) => postcard::to_stdvec(m)?,
            Self::RemoteLock(m) => postcard::to_stdvec(m)?,
        };
        envelope::encode(self.type_id(), &payload, out)?;
        Ok(())
    }

    /// Interprets a frame produced by [`envelope::decode`].
    pub fn from_frame(frame: &Frame) -> Result<Decoded, DecodeError> {
        let message = match frame.msg_type {
            type_ids::HELLO => Self::Hello(payload(frame)?),
            type_ids::GOODBYE => Self::Goodbye(payload(frame)?),
            type_ids::ALIVE => Self::Alive(payload(frame)?),
            type_ids::KEYFRAME_REQUEST => Self::KeyframeRequest(payload(frame)?),
            type_ids::SEAT_ASSIGNED => Self::SeatAssigned(payload(frame)?),
            type_ids::SEAT_RELEASED => Self::SeatReleased(payload(frame)?),
            type_ids::SEAT_CONTROLLER => Self::SeatController(payload(frame)?),
            type_ids::JOIN_START => Self::JoinStart(payload(frame)?),
            type_ids::MEMBER_HELLO => Self::MemberHello(payload(frame)?),
            type_ids::ADMIT_REPLY => Self::AdmitReply(payload(frame)?),
            type_ids::ADMIT_CONFIRM => Self::AdmitConfirm(payload(frame)?),
            type_ids::ROOM_SNAPSHOT => Self::RoomSnapshot(payload(frame)?),
            type_ids::MEMBER_UPDATE => Self::MemberUpdate(payload(frame)?),
            type_ids::POLICY_UPDATE => Self::PolicyUpdate(payload(frame)?),
            type_ids::PRESENCE => Self::Presence(payload(frame)?),
            type_ids::HANDOVER_OFFER => Self::HandoverOffer(payload(frame)?),
            type_ids::HANDOVER_ANSWER => Self::HandoverAnswer(payload(frame)?),
            type_ids::HANDOVER_WITHDRAW => Self::HandoverWithdraw(payload(frame)?),
            type_ids::SESSION_ANNOUNCE => Self::SessionAnnounce(payload(frame)?),
            type_ids::SESSION_STATUS => Self::SessionStatus(payload(frame)?),
            type_ids::SESSION_ENDED => Self::SessionEnded(payload(frame)?),
            type_ids::SESSION_JOIN => Self::SessionJoin(payload(frame)?),
            type_ids::SESSION_JOIN_REPLY => Self::SessionJoinReply(payload(frame)?),
            type_ids::SESSION_LEAVE => Self::SessionLeave(payload(frame)?),
            type_ids::CONTROL_REQUEST => Self::ControlRequest(payload(frame)?),
            type_ids::CONTROL_CANCEL => Self::ControlCancel(payload(frame)?),
            type_ids::CONTROL_DECLINED => Self::ControlDeclined(payload(frame)?),
            type_ids::GAME_FOCUS => Self::GameFocus(payload(frame)?),
            type_ids::VIEWER_CONTROLLERS => Self::ViewerControllers(payload(frame)?),
            type_ids::REMOTE_OFFER => Self::RemoteOffer(payload(frame)?),
            type_ids::REMOTE_UNLOCK => Self::RemoteUnlock(payload(frame)?),
            type_ids::REMOTE_UNLOCK_REPLY => Self::RemoteUnlockReply(payload(frame)?),
            type_ids::REMOTE_COMMAND => Self::RemoteCommand(payload(frame)?),
            type_ids::REMOTE_REPLY => Self::RemoteReply(payload(frame)?),
            type_ids::REMOTE_STATE => Self::RemoteState(payload(frame)?),
            type_ids::REMOTE_LOCK => Self::RemoteLock(payload(frame)?),
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
                controller: None,
            }),
            ControlMessage::SeatAssigned(SeatAssigned {
                seat: SeatId(2),
                seat_epoch: SeatEpoch(4),
                permissions: Permissions::GAMEPAD,
                controller: Some(3),
            }),
            ControlMessage::SeatReleased(SeatReleased {
                seat: SeatId(1),
                seat_epoch: SeatEpoch(u32::MAX),
            }),
            ControlMessage::SeatController(SeatController {
                seat: SeatId(1),
                controller: 7,
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
    fn alive_is_optional_and_empty() {
        let mut buf = Vec::new();
        ControlMessage::Alive(Alive {}).encode(&mut buf).unwrap();
        let (frame, _) = envelope::decode(&buf).unwrap().unwrap();
        assert!(!frame.must_understand());
        assert!(frame.payload.is_empty());
        roundtrip(ControlMessage::Alive(Alive {}));
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
        let policy = OwnedPolicy {
            policy: Signed {
                body: crate::room::RoomPolicy {
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
            },
            handovers: vec![Signed {
                body: crate::room::Handover {
                    room: RoomId([3; 16]),
                    from: MemberId([5; 32]),
                    to: MemberId([4; 32]),
                    after: 1,
                },
                sig,
            }],
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
            ControlMessage::HandoverOffer(HandoverOffer {
                offer: u64::MAX,
                version: 7,
            }),
            ControlMessage::HandoverAnswer(HandoverAnswer {
                offer: 1,
                accept: true,
            }),
            ControlMessage::HandoverWithdraw(HandoverWithdraw { offer: 2 }),
        ] {
            assert!(!message.is_session());
            roundtrip(message);
        }
    }

    #[test]
    fn session_messages_roundtrip() {
        use crate::{
            ids::{MemberId, SessionId},
            session::{
                ControllerReport, EndReason, GuestController, JoinResult, SeatDriver, SeatKind,
            },
        };
        let session = SessionId(0x0123_4567_89ab_cdef);
        for message in [
            ControlMessage::SessionAnnounce(SessionAnnounce {
                session,
                title: "Celeste".into(),
                seats: vec![SeatKind::Controller, SeatKind::KeyboardMouse],
                max_viewers: 7,
            }),
            ControlMessage::SessionStatus(SessionStatus {
                session,
                viewers: vec![MemberId([1; 32]), MemberId([2; 32])],
                holders: vec![None, Some(MemberId([2; 32]))],
                host_seats: vec![SeatId(0)],
                host_on_seats: true,
                control_paused: true,
                stream: crate::session::StreamState::Stopped(EndReason::SourceLost),
            }),
            ControlMessage::SessionEnded(SessionEnded {
                session,
                reason: EndReason::SourceLost,
            }),
            ControlMessage::SessionJoin(SessionJoin {
                session,
                codecs: crate::codec::DECODES_H264 | crate::codec::DECODES_HEVC,
            }),
            ControlMessage::SessionJoinReply(SessionJoinReply {
                session,
                result: JoinResult::Full,
            }),
            ControlMessage::SessionLeave(SessionLeave { session }),
            ControlMessage::ControlRequest(ControlRequest {
                session,
                seat: SeatId(1),
                controller: None,
            }),
            ControlMessage::ControlRequest(ControlRequest {
                session,
                seat: SeatId(2),
                controller: Some(3),
            }),
            ControlMessage::ControlCancel(ControlCancel {
                session,
                what: crate::session::Cancel::Seat(None),
            }),
            ControlMessage::ControlCancel(ControlCancel {
                session,
                what: crate::session::Cancel::Seat(Some(SeatId(2))),
            }),
            ControlMessage::ControlDeclined(ControlDeclined { session }),
            ControlMessage::GameFocus(GameFocus {
                session,
                in_front: false,
            }),
            ControlMessage::ViewerControllers(ViewerControllers {
                session,
                report: ControllerReport::default(),
            }),
            ControlMessage::ViewerControllers(ViewerControllers {
                session,
                report: ControllerReport {
                    controllers: vec![
                        GuestController {
                            slot: 0,
                            name: "DualSense Wireless Controller".into(),
                        },
                        GuestController {
                            slot: 2,
                            name: "Xbox Controller".into(),
                        },
                    ],
                    seats: vec![
                        SeatDriver {
                            seat: SeatId(0),
                            controller: Some(2),
                        },
                        SeatDriver {
                            seat: SeatId(1),
                            controller: None,
                        },
                    ],
                },
            }),
        ] {
            assert!(message.is_session());
            roundtrip(message);
        }
    }

    #[test]
    fn remote_messages_roundtrip() {
        use crate::{
            ids::{MemberId, SessionId},
            remote::*,
            session::{ControllerReport, StreamState},
        };
        for message in [
            ControlMessage::RemoteOffer(RemoteOffer {
                wait_secs: Some(60),
            }),
            ControlMessage::RemoteUnlock(RemoteUnlock {
                pin: Pin("246810".into()),
            }),
            ControlMessage::RemoteUnlockReply(RemoteUnlockReply {
                result: UnlockResult::WrongPin { wait_secs: 0 },
            }),
            ControlMessage::RemoteCommand(RemoteCommand {
                id: 7,
                op: RemoteOp::StartStream {
                    source: RemoteSource::Window(-3),
                    name: "Celeste".into(),
                    fps: 60,
                    max_mbps: 20,
                    upload_mbps: None,
                },
            }),
            ControlMessage::RemoteCommand(RemoteCommand {
                id: 8,
                op: RemoteOp::PassControl {
                    member: MemberId([4; 32]),
                    seat: SeatId(1),
                    controller: Some(0),
                },
            }),
            ControlMessage::RemoteReply(Box::new(RemoteReply {
                id: 7,
                result: RemoteResult::Failed("that window is gone".into()),
            })),
            ControlMessage::RemoteState(Box::new(RemoteState {
                session: Some(RemoteSession {
                    session: SessionId(9),
                    starting: true,
                    stream: StreamState::Waiting,
                    streaming: None,
                    requests: vec![RemoteRequest {
                        member: MemberId([5; 32]),
                        seat: SeatId(0),
                        controller: None,
                    }],
                    guests: vec![RemoteGuest {
                        member: MemberId([5; 32]),
                        report: ControllerReport::default(),
                    }],
                    sound_level: 140,
                    pads_setting_up: vec![SeatId(2)],
                    pads_setting_up_secs: 42,
                    pads_missing: Vec::new(),
                    host_controllers: Vec::new(),
                    host_homes: Vec::new(),
                    panic_key: true,
                }),
            })),
            ControlMessage::RemoteLock(RemoteLock {
                reason: LockReason::Panic,
            }),
        ] {
            assert!(message.is_remote() && !message.is_session());
            roundtrip(message);
        }
        assert_eq!(type_ids::REMOTE_OFFER & MUST_UNDERSTAND, 0);
    }

    /// The fullest state and source list fit a control frame.
    #[test]
    fn the_largest_remote_messages_fit_a_frame() {
        use crate::{
            ids::{MemberId, SessionId},
            remote::*,
            session::{
                ControllerReport, GuestController, MAX_GUEST_CONTROLLERS, MAX_SEATS, SeatDriver,
                StreamState,
            },
        };
        let text = "\u{1F3AE}".repeat(MAX_REMOTE_TEXT / 4);
        let report = ControllerReport {
            controllers: (0..MAX_GUEST_CONTROLLERS as u8)
                .map(|slot| GuestController {
                    slot,
                    name: crate::session::controller_name(&"W".repeat(200)),
                })
                .collect(),
            seats: (0..MAX_GUEST_CONTROLLERS as u8)
                .map(|s| SeatDriver {
                    seat: SeatId(s),
                    controller: Some(s),
                })
                .collect(),
        };
        let state = RemoteState {
            session: Some(RemoteSession {
                session: SessionId(u64::MAX),
                starting: true,
                stream: StreamState::Live,
                streaming: Some(RemoteStream {
                    name: text.clone(),
                    fps: u32::MAX,
                    max_mbps: u32::MAX,
                    upload_mbps: Some(u32::MAX),
                }),
                requests: (0..MAX_REMOTE_LISTED)
                    .map(|i| RemoteRequest {
                        member: MemberId([i as u8; 32]),
                        seat: SeatId(4),
                        controller: Some(3),
                    })
                    .collect(),
                guests: (0..MAX_REMOTE_LISTED)
                    .map(|i| RemoteGuest {
                        member: MemberId([i as u8; 32]),
                        report: report.clone(),
                    })
                    .collect(),
                sound_level: u16::MAX,
                pads_setting_up: (0..MAX_SEATS as u8).map(SeatId).collect(),
                pads_setting_up_secs: u32::MAX,
                pads_missing: (0..MAX_SEATS as u8).map(SeatId).collect(),
                host_controllers: vec![text.clone(); 4],
                host_homes: (0..4).map(SeatId).collect(),
                panic_key: true,
            }),
        };
        let sources = RemoteSources {
            windows: (0..MAX_REMOTE_WINDOWS)
                .map(|_| RemoteWindow {
                    id: i64::MIN,
                    title: text.clone(),
                    app: text.clone(),
                    elevated: true,
                    refresh_hz: Some(u32::MAX),
                })
                .collect(),
            screens: (0..MAX_REMOTE_SCREENS)
                .map(|_| RemoteScreen {
                    id: i64::MIN,
                    name: text.clone(),
                    width: u32::MAX,
                    height: u32::MAX,
                    primary: true,
                    refresh_hz: Some(u32::MAX),
                })
                .collect(),
            virtual_pads: true,
        };
        assert!(state.clone().checked().is_some() && sources.is_valid());
        for message in [
            ControlMessage::RemoteState(Box::new(state)),
            ControlMessage::RemoteReply(Box::new(RemoteReply {
                id: u32::MAX,
                result: RemoteResult::Sources(sources),
            })),
        ] {
            let mut buf = Vec::new();
            message.encode(&mut buf).unwrap();
            assert!(buf.len() < envelope::MAX_FRAME_LEN, "{} bytes", buf.len());
        }
    }

    /// The directory is optional, so a peer that doesn't know sessions only
    /// misses them; what follows an announcement must be understood.
    #[test]
    fn only_the_session_directory_is_optional() {
        for id in [
            type_ids::SESSION_ANNOUNCE,
            type_ids::SESSION_STATUS,
            type_ids::SESSION_ENDED,
            type_ids::GAME_FOCUS,
            type_ids::VIEWER_CONTROLLERS,
            type_ids::SEAT_CONTROLLER,
        ] {
            assert_eq!(id & MUST_UNDERSTAND, 0, "{id:#06x}");
        }
        for id in [
            type_ids::SESSION_JOIN,
            type_ids::SESSION_JOIN_REPLY,
            type_ids::SESSION_LEAVE,
            type_ids::CONTROL_REQUEST,
            type_ids::CONTROL_CANCEL,
            type_ids::CONTROL_DECLINED,
        ] {
            assert_ne!(id & MUST_UNDERSTAND, 0, "{id:#06x}");
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
            type_ids::HANDOVER_OFFER,
            type_ids::HANDOVER_ANSWER,
            type_ids::HANDOVER_WITHDRAW,
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
