//! Room membership on the control stream: signed member records, the owner's
//! signed policy, snapshots and presence. How peers merge them is in
//! docs/protocol.md (Rooms) and `couch_room::room`.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::ids::{MemberId, RoomId, Signature};

/// Display and room names: at most this many bytes of UTF-8.
pub const MAX_NAME_BYTES: usize = 96;
pub const MAX_RELAY_URL_BYTES: usize = 256;
/// Direct addresses kept per member; extra ones are dropped before signing.
pub const MAX_IPS: usize = 8;
/// Members a policy can list, and records a room keeps (one per member). A
/// snapshot with all of them at their largest still fits a 64 KiB control
/// frame. The owner stops admitting at `couch_room::room::MAX_MEMBERS`.
pub const MAX_ROSTER: usize = 64;
pub const MAX_RECORDS: usize = MAX_ROSTER;
pub const MAX_BANNED: usize = 64;

/// Signature domains: a signature over one kind of body can't pass for another.
pub const MEMBER_RECORD_DOMAIN: &[u8] = b"couch/member-record/v1";
pub const POLICY_DOMAIN: &[u8] = b"couch/room-policy/v1";

/// Where a member can be dialed: an iroh relay and direct addresses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerAddr {
    pub relay: Option<String>,
    pub ips: Vec<SocketAddr>,
}

impl PeerAddr {
    pub fn is_valid(&self) -> bool {
        self.relay
            .as_ref()
            .is_none_or(|url| url.len() <= MAX_RELAY_URL_BYTES)
            && self.ips.len() <= MAX_IPS
    }
}

/// What a member says about itself in one room: its name and where to reach
/// it. Only the member writes it (it is signed with the member's key); a
/// higher `version` replaces a lower, within one stay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRecord {
    pub room: RoomId,
    pub id: MemberId,
    /// The stay this record belongs to: the `since` of the member's roster
    /// entry. Records from an earlier stay (before leaving and being let in
    /// again) no longer count.
    pub since: u32,
    pub version: u64,
    pub name: String,
    pub addr: PeerAddr,
    /// The member is leaving the room; the owner takes it off the roster.
    pub left: bool,
}

impl MemberRecord {
    pub fn is_valid(&self) -> bool {
        valid_name(&self.name) && self.addr.is_valid()
    }
}

/// Who is in the room, written only by its owner (signed with the owner's
/// key); a higher `version` replaces a lower. It holds no secret, so it can
/// be shown to anyone it concerns, such as a banned member as proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomPolicy {
    pub room: RoomId,
    pub version: u32,
    pub name: String,
    /// The members, the owner included. Members accept connections only from
    /// ids on it.
    pub roster: Vec<RosterEntry>,
    /// Refused even with a valid invite.
    pub banned: Vec<MemberId>,
}

impl RoomPolicy {
    pub fn is_valid(&self) -> bool {
        valid_name(&self.name) && self.roster.len() <= MAX_ROSTER && self.banned.len() <= MAX_BANNED
    }

    /// The stay `member` is on the roster for, if it is.
    pub fn since(&self, member: MemberId) -> Option<u32> {
        self.roster.iter().find(|e| e.id == member).map(|e| e.since)
    }

    pub fn lists(&self, member: MemberId) -> bool {
        self.since(member).is_some()
    }
}

/// One member on the roster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterEntry {
    pub id: MemberId,
    /// The policy version that put it on the roster. Its records carry it.
    pub since: u32,
}

/// A body and its author's Ed25519 signature over
/// `domain || postcard(body)` (see [`signing_bytes`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signed<T> {
    pub body: T,
    pub sig: Signature,
}

/// The bytes a signature covers. Verifiers re-encode the decoded body, so a
/// body that arrived in any non-canonical encoding fails the check.
pub fn signing_bytes<T: Serialize>(domain: &[u8], body: &T) -> Vec<u8> {
    let mut out = domain.to_vec();
    // Serializing plain structs into a Vec can't fail.
    out.extend(postcard::to_stdvec(body).expect("postcard into a Vec"));
    out
}

/// Everything a member knows about the room: the owner's policy and every
/// member's record. Sent right after a connection is accepted and every 30 s
/// as repair; the owner sends a joiner one once it is admitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSnapshot {
    pub policy: Signed<RoomPolicy>,
    pub members: Vec<Signed<MemberRecord>>,
}

/// The members the sender is connected to right now. Others dial whoever
/// they aren't connected to yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub connected: Vec<MemberId>,
}

/// Sent before closing a connection on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goodbye {
    pub reason: GoodbyeReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoodbyeReason {
    /// The sender left the room for good.
    LeftRoom,
    /// The sender's app is closing; it will dial back when it starts.
    Shutdown,
}

/// A display or room name: not blank, at most [`MAX_NAME_BYTES`], and
/// nothing invisible that could make one name pass for another: no control
/// characters, direction overrides or zero-width characters. The zero-width
/// joiner stays allowed, since emoji sequences need it.
pub fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_NAME_BYTES
        && !name.chars().any(|c| c.is_control() || is_invisible(c))
}

fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'
            | '\u{200C}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv6Addr, SocketAddrV6};

    use super::*;

    fn largest_record(i: u8) -> Signed<MemberRecord> {
        Signed {
            body: MemberRecord {
                room: RoomId([0xff; 16]),
                id: MemberId([i; 32]),
                since: u32::MAX,
                version: u64::MAX,
                name: "\u{10FFFF}".repeat(MAX_NAME_BYTES / 4),
                addr: PeerAddr {
                    relay: Some("r".repeat(MAX_RELAY_URL_BYTES)),
                    ips: vec![
                        SocketAddr::V6(SocketAddrV6::new(
                            Ipv6Addr::from([0xff; 16]),
                            65535,
                            0,
                            0
                        ));
                        MAX_IPS
                    ],
                },
                left: true,
            },
            sig: Signature([0xff; 32], [0xff; 32]),
        }
    }

    /// Records are capped so the repair snapshot always fits one frame.
    #[test]
    fn a_full_snapshot_fits_a_control_frame() {
        let snapshot = RoomSnapshot {
            policy: Signed {
                body: RoomPolicy {
                    room: RoomId([0xff; 16]),
                    version: u32::MAX,
                    name: "x".repeat(MAX_NAME_BYTES),
                    roster: vec![
                        RosterEntry {
                            id: MemberId([0xff; 32]),
                            since: u32::MAX,
                        };
                        MAX_ROSTER
                    ],
                    banned: vec![MemberId([0xff; 32]); MAX_BANNED],
                },
                sig: Signature([0xff; 32], [0xff; 32]),
            },
            members: (0..MAX_RECORDS as u8).map(largest_record).collect(),
        };
        assert!(snapshot.policy.body.is_valid());
        assert!(snapshot.members.iter().all(|r| r.body.is_valid()));
        let len = postcard::to_stdvec(&snapshot).unwrap().len();
        assert!(
            len + 2 <= crate::envelope::MAX_FRAME_LEN,
            "snapshot is {len} bytes"
        );
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_name("Marcus"));
        assert!(valid_name("Zoë 🎮"));
        assert!(
            valid_name("👨\u{200D}👩\u{200D}👧"),
            "emoji sequences use ZWJ"
        );
        for bad in [
            "",
            "   ",
            "a\nb",
            "tab\there",
            &"x".repeat(MAX_NAME_BYTES + 1),
            "Ana\u{202E}",
            "A\u{200B}na",
            "\u{2066}Bia\u{2069}",
            "Caio\u{FEFF}",
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn signing_bytes_start_with_the_domain() {
        let presence = Presence {
            connected: vec![MemberId([1; 32])],
        };
        let bytes = signing_bytes(POLICY_DOMAIN, &presence);
        assert!(bytes.starts_with(POLICY_DOMAIN));
        assert_ne!(bytes, signing_bytes(MEMBER_RECORD_DOMAIN, &presence));
    }
}
