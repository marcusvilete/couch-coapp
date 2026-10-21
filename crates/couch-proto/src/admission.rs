//! Admission messages: what a connection between room members exchanges,
//! after `Hello`, before it carries anything else (docs/protocol.md,
//! Admission).
//!
//! - A newcomer joins only through the room's owner, with an invite and the
//!   password: `JoinStart`, `AdmitReply`, `AdmitConfirm`. The cryptography
//!   is in `couch_net::admission`.
//! - Members connect to each other with `MemberHello`: the owner's signed
//!   member list decides, and iroh has already authenticated the ids.

use serde::{Deserialize, Serialize};

use crate::{
    ids::{InviteTag, RoomId},
    room::OwnedPolicy,
};

/// A SPAKE2 message: a side byte (`b'A'` from the dialer, `b'B'` from the
/// acceptor) and a compressed Edwards25519 point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PakeMessage(pub u8, pub [u8; 32]);

impl PakeMessage {
    pub const LEN: usize = 33;

    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        let (&side, point) = bytes.split_first()?;
        Some(Self(side, point.try_into().ok()?))
    }

    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut out = [0; Self::LEN];
        out[0] = self.0;
        out[1..].copy_from_slice(&self.1);
        out
    }
}

/// Newcomer to owner, first after `Hello`: which room and invite, and the
/// newcomer's SPAKE2 message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinStart {
    pub room: RoomId,
    pub invite: InviteTag,
    pub pake: PakeMessage,
}

/// Owner to newcomer: its SPAKE2 message and key confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmitReply {
    pub pake: PakeMessage,
    pub confirm: [u8; 32],
}

/// Newcomer to owner, once the owner's confirmation checked out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmitConfirm {
    pub confirm: [u8; 32],
}

/// Member to member, first after `Hello`: which room, and the newest owner's
/// policy the dialer has (with the handovers that make its signer the owner,
/// M13), so an acceptor that missed a change (a member who joined meanwhile)
/// can catch up before deciding. The acceptor answers with its
/// `RoomSnapshot`, or refuses: with the policy that shows why
/// (`PolicyUpdate`), then a close.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberHello {
    pub room: RoomId,
    pub policy: OwnedPolicy,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pake_message_is_33_bytes_on_the_wire() {
        let message = PakeMessage(b'A', [7; 32]);
        assert_eq!(
            postcard::to_stdvec(&message).unwrap().len(),
            PakeMessage::LEN
        );
        assert_eq!(PakeMessage::from_slice(&message.to_bytes()), Some(message));
        assert_eq!(PakeMessage::from_slice(&[b'A'; 32]), None);
        assert_eq!(PakeMessage::from_slice(&[]), None);
    }
}
