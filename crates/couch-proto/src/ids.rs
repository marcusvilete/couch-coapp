//! Identifiers and flags that appear on the wire.

use std::fmt;

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

/// A room member: the Ed25519 public key of their install, which is also
/// their iroh endpoint id.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MemberId(pub [u8; 32]);

impl fmt::Debug for MemberId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MemberId({})", short_hex(&self.0))
    }
}

impl MemberId {
    /// Lowercase hex, as the UI and logs show it.
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        unhex(s).map(Self)
    }
}

/// A room: the first 16 bytes of `SHA-256("couch/room-id/v1" || owner ||
/// nonce)`, which ties the room to its owner (`couch_room::room_id`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RoomId(pub [u8; 16]);

impl fmt::Debug for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RoomId({})", short_hex(&self.0))
    }
}

impl RoomId {
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        unhex(s).map(Self)
    }
}

/// Random bytes the owner picks when creating a room; with the owner's id
/// they determine the [`RoomId`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RoomNonce(pub [u8; 16]);

impl fmt::Debug for RoomNonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RoomNonce({})", short_hex(&self.0))
    }
}

/// One invite's secret, made and kept by the room's owner and carried in the
/// invite link. It is mixed into the password check, so guessing the
/// password also takes the link; it never crosses the wire on its own.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteToken(pub [u8; 32]);

impl fmt::Debug for InviteToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InviteToken(..)")
    }
}

/// Names an invite without revealing its token: the first 16 bytes of
/// `SHA-256("couch/invite-tag/v1" || token)` (`couch_room::signing`). A
/// joiner sends it so the owner knows which invite is being used.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InviteTag(pub [u8; 16]);

impl fmt::Debug for InviteTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InviteTag({})", short_hex(&self.0))
    }
}

/// An Ed25519 signature, as two halves because serde's arrays stop at 32.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature(pub [u8; 32], pub [u8; 32]);

impl Signature {
    pub fn from_bytes(bytes: [u8; 64]) -> Self {
        let (r, s) = bytes.split_at(32);
        Self(
            r.try_into().expect("32 bytes"),
            s.try_into().expect("32 bytes"),
        )
    }

    pub fn to_bytes(&self) -> [u8; 64] {
        let mut out = [0; 64];
        out[..32].copy_from_slice(&self.0);
        out[32..].copy_from_slice(&self.1);
        out
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature({})", short_hex(&self.0))
    }
}

/// A session, chosen by its host; unique within the room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionId(pub u64);

/// A controller slot in a session. Single-player sessions have only seat 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SeatId(pub u8);

/// Incremented on every change of a seat's holder. The host drops input
/// tagged with any other epoch, so packets from a previous holder are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SeatEpoch(pub u32);

impl SeatEpoch {
    pub fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

bitflags! {
    /// What a seat holder may send to the host.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct Permissions: u8 {
        const GAMEPAD = 1 << 0;
        const KEYBOARD = 1 << 1;
        const MOUSE = 1 << 2;
        /// Desktop control (M25b, ADR 0010): keyboard and mouse reach the
        /// whole desktop, without the game window in front, and the Windows
        /// key, Alt+Tab and the rest pass. Only ever granted, with the
        /// keyboard-and-mouse seat, to the home PC's unlocked operator.
        const DESKTOP = 1 << 3;
    }
}

impl Permissions {
    /// Guests get the gamepad only. Keyboard and mouse amount to remote control
    /// of the host's PC, so the host grants them explicitly.
    pub const GUEST_DEFAULT: Self = Self::GAMEPAD;
    /// Every kind of input, without desktop control: what a seat with a
    /// controller, keyboard and mouse gives. Never `Permissions::all()`
    /// for that, which includes [`Self::DESKTOP`].
    pub const ALL_INPUT: Self = Self::GAMEPAD.union(Self::KEYBOARD).union(Self::MOUSE);
}

fn short_hex(bytes: &[u8]) -> String {
    bytes.iter().take(5).map(|b| format!("{b:02x}")).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != 2 * N || !s.is_ascii() {
        return None;
    }
    let mut out = [0; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guests_default_to_gamepad_only() {
        assert!(Permissions::GUEST_DEFAULT.contains(Permissions::GAMEPAD));
        assert!(!Permissions::GUEST_DEFAULT.intersects(Permissions::KEYBOARD | Permissions::MOUSE));
    }

    #[test]
    fn seat_epoch_wraps() {
        assert_eq!(SeatEpoch(u32::MAX).next(), SeatEpoch(0));
    }

    #[test]
    fn debug_shows_a_short_prefix() {
        let id = MemberId([0xab; 32]);
        assert_eq!(format!("{id:?}"), "MemberId(ababababab)");
    }

    #[test]
    fn tokens_never_show_in_debug() {
        assert_eq!(format!("{:?}", InviteToken([7; 32])), "InviteToken(..)");
    }

    #[test]
    fn hex_roundtrips_and_rejects_junk() {
        let id = MemberId(std::array::from_fn(|i| i as u8 * 7));
        assert_eq!(MemberId::from_hex(&id.to_hex()), Some(id));
        let room = RoomId([0xc4; 16]);
        assert_eq!(RoomId::from_hex(&room.to_hex()), Some(room));
        for bad in ["", "zz", &"0".repeat(63), &"g".repeat(64), &"é".repeat(32)] {
            assert_eq!(MemberId::from_hex(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn signature_halves_roundtrip() {
        let bytes: [u8; 64] = std::array::from_fn(|i| i as u8);
        assert_eq!(Signature::from_bytes(bytes).to_bytes(), bytes);
        let encoded = postcard::to_stdvec(&Signature::from_bytes(bytes)).unwrap();
        assert_eq!(encoded, bytes, "64 raw bytes on the wire");
    }
}
