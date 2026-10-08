//! Invites: `couchco://join#<payload>`, where the payload is unpadded
//! lowercase base32 of [`MAGIC`], a version byte and a postcard [`Invite`].
//! The magic bytes keep other text that happens to be valid base32 from
//! passing as an invite from a newer version. Pasting the payload alone
//! works too. The part after `#` never reaches a web server, which matters
//! once invites also go out as `https` links (M7).
//!
//! Only the room's owner makes invites, and only the owner admits with them.
//! Each holds its own token, which the owner keeps until the invite expires
//! or is revoked: whoever has the link and the password can join while it
//! lasts. Treat the link like the password's other half.

use data_encoding::BASE32_NOPAD;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ids::{InviteToken, MemberId, RoomNonce},
    room::{PeerAddr, valid_name},
};

pub const LINK_PREFIX: &str = "couchco://join#";
pub const MAGIC: [u8; 2] = *b"CC";
pub const VERSION: u8 = 1;
/// Longest text [`Invite::parse`] looks at.
pub const MAX_CHARS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    /// The room's owner, the only member who admits newcomers.
    pub owner: MemberId,
    pub nonce: RoomNonce,
    /// The room's name when the invite was made, to show before joining.
    pub name: String,
    pub token: InviteToken,
    /// When the owner stops accepting it, in seconds since 1970 (UTC). For
    /// a clear message; the owner's own record of it is what counts.
    pub expires: u64,
    /// Where to reach the owner: its relay, or direct addresses on a LAN.
    pub addr: PeerAddr,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum InviteError {
    #[error("that isn't an invite")]
    NotAnInvite,
    #[error("this invite is from a newer version of Couch Co-App")]
    NewerVersion,
    #[error("this invite is damaged; ask for a new one")]
    Damaged,
}

impl Invite {
    /// The payload alone: what follows `#` in the link.
    pub fn payload(&self) -> String {
        encode(
            VERSION,
            &postcard::to_stdvec(self).expect("postcard into a Vec"),
        )
    }

    pub fn to_link(&self) -> String {
        format!("{LINK_PREFIX}{}", self.payload())
    }

    /// Reads a link (anything ending in `#<payload>`) or a bare payload.
    /// Whitespace inside is ignored, since chat apps wrap long lines.
    pub fn parse(text: &str) -> Result<Self, InviteError> {
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_CHARS {
            return Err(InviteError::NotAnInvite);
        }
        let payload = text.rsplit_once('#').map_or(text, |(_, after)| after);
        let payload: String = payload
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| c.to_ascii_uppercase())
            .collect();
        let bytes = BASE32_NOPAD
            .decode(payload.as_bytes())
            .map_err(|_| InviteError::NotAnInvite)?;
        let Some(([m0, m1, version], body)) = bytes.split_first_chunk::<3>() else {
            return Err(InviteError::NotAnInvite);
        };
        if [*m0, *m1] != MAGIC || *version == 0 {
            return Err(InviteError::NotAnInvite);
        }
        if *version > VERSION {
            return Err(InviteError::NewerVersion);
        }
        let (invite, rest): (Invite, _) =
            postcard::take_from_bytes(body).map_err(|_| InviteError::Damaged)?;
        if !rest.is_empty() || !invite.is_valid() {
            return Err(InviteError::Damaged);
        }
        Ok(invite)
    }

    fn is_valid(&self) -> bool {
        valid_name(&self.name) && self.addr.is_valid()
    }
}

fn encode(version: u8, body: &[u8]) -> String {
    let mut bytes = MAGIC.to_vec();
    bytes.push(version);
    bytes.extend_from_slice(body);
    BASE32_NOPAD.encode(&bytes).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use crate::room::MAX_RELAY_URL_BYTES;

    use super::*;

    fn invite() -> Invite {
        Invite {
            owner: MemberId([1; 32]),
            nonce: RoomNonce([2; 16]),
            name: "Sofá da Ana".into(),
            token: InviteToken([4; 32]),
            expires: 1_800_000_000,
            addr: PeerAddr {
                relay: Some("https://relay.example.com./".into()),
                ips: vec![],
            },
        }
    }

    #[test]
    fn link_and_bare_payload_roundtrip() {
        let link = invite().to_link();
        assert!(link.starts_with(LINK_PREFIX));
        assert_eq!(Invite::parse(&link), Ok(invite()));
        assert_eq!(Invite::parse(&invite().payload()), Ok(invite()));
    }

    #[test]
    fn pasted_text_is_forgiving() {
        let payload = invite().payload();
        let (a, b) = payload.split_at(40);
        let wrapped = format!("  https://couch.example/j#{}\n{} \n", a.to_uppercase(), b);
        assert_eq!(Invite::parse(&wrapped), Ok(invite()));
    }

    #[test]
    fn garbage_is_rejected_not_misread() {
        assert_eq!(Invite::parse(""), Err(InviteError::NotAnInvite));
        // Valid base32, so only the magic bytes tell it apart.
        assert_eq!(Invite::parse("hello there"), Err(InviteError::NotAnInvite));
        assert_eq!(
            Invite::parse(&BASE32_NOPAD.encode(b"\x02 some other format")),
            Err(InviteError::NotAnInvite)
        );
        assert_eq!(
            Invite::parse(&"a".repeat(MAX_CHARS + 1)),
            Err(InviteError::NotAnInvite)
        );
        let mut damaged = invite().payload();
        damaged.truncate(damaged.len() - 10);
        assert!(Invite::parse(&damaged).is_err());
    }

    #[test]
    fn newer_versions_say_so() {
        let text = encode(VERSION + 1, b"whatever comes next");
        assert_eq!(Invite::parse(&text), Err(InviteError::NewerVersion));
    }

    #[test]
    fn trailing_bytes_and_bad_fields_are_rejected() {
        let mut body = postcard::to_stdvec(&invite()).unwrap();
        body.push(0);
        assert_eq!(
            Invite::parse(&encode(VERSION, &body)),
            Err(InviteError::Damaged)
        );
        let mut nameless = invite();
        nameless.name = " ".into();
        assert_eq!(
            Invite::parse(&nameless.payload()),
            Err(InviteError::Damaged)
        );
        let mut long_relay = invite();
        long_relay.addr.relay = Some("r".repeat(MAX_RELAY_URL_BYTES + 1));
        assert_eq!(
            Invite::parse(&long_relay.payload()),
            Err(InviteError::Damaged)
        );
    }

    /// The invite page (`deploy/invite-page/j/invite.js`) reads the room's
    /// name out of this layout, and released invites must keep parsing:
    /// a change here breaks both. `node deploy/invite-page/check.js` reads
    /// this same payload.
    #[test]
    fn the_layout_is_pinned() {
        assert_eq!(invite().payload(), PINNED_PAYLOAD);
    }

    const PINNED_PAYLOAD: &str = "inbqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaibaeaqcaibaeaqcaibaeaqcaigfg33gyoqsazdbebaw4yieaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeasakjj62ayarw2duoryhgorpf5zgk3dbpexgk6dbnvygyzjomnxw2lrpaa";

    #[test]
    fn a_typical_invite_stays_short() {
        assert!(invite().to_link().len() < 250, "{}", invite().to_link());
    }

    #[test]
    fn the_token_never_shows_in_debug() {
        assert!(!format!("{:?}", invite()).contains("[4, 4"));
    }
}
