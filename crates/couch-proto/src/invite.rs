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
    room::{PeerAddr, RelaySecret, valid_name},
};

pub const LINK_PREFIX: &str = "couchco://join#";
pub const MAGIC: [u8; 2] = *b"CC";
/// The newest layout read. Version 1 is the invite of a room its founder
/// owns, version 2 (M13) of a room handed over since: the same fields, then
/// the founder. Version 3 (M26b) carries the code of the owner's relay: the
/// same fields, then the founder if it isn't the owner, then the code. The
/// oldest layout that holds an invite is written.
pub const VERSION: u8 = 3;
/// Longest text [`Invite::parse`] looks at.
pub const MAX_CHARS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Who founded the room, if not its owner now (M13): the room id binds
    /// the founder, and the chain of handovers from it names the owner.
    pub founder: Option<MemberId>,
    /// The token of the relay [`Self::addr`] names (M26b), when the owner
    /// may pass it on and didn't leave it out: the newcomer joins through it,
    /// and keeps it once admitted. Whoever has the link can use the relay.
    pub relay_code: Option<RelaySecret>,
}

/// Version 1's layout, and the start of version 2's: the invite page reads
/// the name at a fixed offset after `owner` and `nonce`.
#[derive(Serialize, Deserialize)]
struct Layout {
    owner: MemberId,
    nonce: RoomNonce,
    name: String,
    token: InviteToken,
    expires: u64,
    addr: PeerAddr,
}

/// Version 2: version 1's fields, then the founder.
#[derive(Serialize, Deserialize)]
struct HandedOver {
    invite: Layout,
    founder: MemberId,
}

/// Version 3: version 1's fields, the founder if not the owner, the code.
#[derive(Serialize, Deserialize)]
struct WithCode {
    invite: Layout,
    founder: Option<MemberId>,
    relay_code: RelaySecret,
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
    /// The room's founder, whom the room id binds.
    pub fn founder(&self) -> MemberId {
        self.founder.unwrap_or(self.owner)
    }

    fn layout(&self) -> Layout {
        Layout {
            owner: self.owner,
            nonce: self.nonce,
            name: self.name.clone(),
            token: self.token,
            expires: self.expires,
            addr: self.addr.clone(),
        }
    }

    fn from_layout(invite: Layout, founder: Option<MemberId>) -> Self {
        Self {
            owner: invite.owner,
            nonce: invite.nonce,
            name: invite.name,
            token: invite.token,
            expires: invite.expires,
            addr: invite.addr,
            founder,
            relay_code: None,
        }
    }

    /// The payload alone: what follows `#` in the link.
    pub fn payload(&self) -> String {
        let founder = self.founder.filter(|f| *f != self.owner);
        if let Some(relay_code) = &self.relay_code {
            let body = postcard::to_stdvec(&WithCode {
                invite: self.layout(),
                founder,
                relay_code: relay_code.clone(),
            });
            return encode(3, &body.expect("postcard into a Vec"));
        }
        let body = match founder {
            None => (1, postcard::to_stdvec(&self.layout())),
            Some(founder) => (
                2,
                postcard::to_stdvec(&HandedOver {
                    invite: self.layout(),
                    founder,
                }),
            ),
        };
        encode(body.0, &body.1.expect("postcard into a Vec"))
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
        let invite = match version {
            1 => Self::from_layout(whole(body)?, None),
            3 => {
                let with: WithCode = whole(body)?;
                // Only one way to write each invite, as for version 2.
                if with.founder == Some(with.invite.owner) || !with.relay_code.is_valid() {
                    return Err(InviteError::Damaged);
                }
                Self {
                    relay_code: Some(with.relay_code),
                    ..Self::from_layout(with.invite, with.founder)
                }
            }
            _ => {
                let handed: HandedOver = whole(body)?;
                // Only one way to write each invite: a founder who owns the
                // room makes version 1.
                if handed.founder == handed.invite.owner {
                    return Err(InviteError::Damaged);
                }
                Self::from_layout(handed.invite, Some(handed.founder))
            }
        };
        if !invite.is_valid() {
            return Err(InviteError::Damaged);
        }
        Ok(invite)
    }

    fn is_valid(&self) -> bool {
        valid_name(&self.name)
            && self.addr.is_valid()
            // A code is for the relay the invite names.
            && (self.relay_code.is_none() || self.addr.relay.is_some())
    }
}

/// `body` decoded, with nothing left over.
fn whole<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, InviteError> {
    match postcard::take_from_bytes(body) {
        Ok((value, [])) => Ok(value),
        _ => Err(InviteError::Damaged),
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
            founder: None,
            relay_code: None,
        }
    }

    /// M26b: an invite carrying its relay's code is version 3, which starts
    /// as version 1 does (the invite page reads the name at the same
    /// offset), founder or not; the code never shows in Debug.
    #[test]
    fn an_invite_with_its_relays_code_is_version_3() {
        let bytes = |p: &str| BASE32_NOPAD.decode(p.to_uppercase().as_bytes()).unwrap();
        let v1 = bytes(&invite().payload());
        for founder in [None, Some(MemberId([9; 32]))] {
            let with = Invite {
                founder,
                relay_code: Some(RelaySecret("s3cret-token".into())),
                ..invite()
            };
            let payload = with.payload();
            assert_eq!(Invite::parse(&payload), Ok(with.clone()));
            let v3 = bytes(&payload);
            assert_eq!(v3[2], 3);
            assert_eq!(v1[3..], v3[3..v1.len()]);
            assert!(!format!("{with:?}").contains("s3cret"));
        }
        // A code with no relay to go with it, or an empty one, is damage;
        // so is the founder written when it owns the room.
        let relayless = Invite {
            relay_code: Some(RelaySecret("s3cret".into())),
            addr: PeerAddr::default(),
            ..invite()
        };
        assert_eq!(
            Invite::parse(&relayless.payload()),
            Err(InviteError::Damaged)
        );
        let empty = Invite {
            relay_code: Some(RelaySecret(String::new())),
            ..invite()
        };
        assert_eq!(Invite::parse(&empty.payload()), Err(InviteError::Damaged));
        let non_canonical = WithCode {
            invite: invite().layout(),
            founder: Some(invite().owner),
            relay_code: RelaySecret("s3cret".into()),
        };
        assert_eq!(
            Invite::parse(&encode(3, &postcard::to_stdvec(&non_canonical).unwrap())),
            Err(InviteError::Damaged)
        );
    }

    /// M13: a room handed over names its founder too, in version 2.
    #[test]
    fn a_handed_over_room_names_its_founder() {
        let handed = Invite {
            founder: Some(MemberId([9; 32])),
            ..invite()
        };
        let payload = handed.payload();
        assert_eq!(Invite::parse(&payload), Ok(handed.clone()));
        assert_eq!(handed.founder(), MemberId([9; 32]));
        assert_eq!(invite().founder(), invite().owner);
        // Version 2 starts as version 1 does, so the invite page finds the
        // name at the same offset.
        let bytes = |p: &str| BASE32_NOPAD.decode(p.to_uppercase().as_bytes()).unwrap();
        let (v1, v2) = (bytes(&invite().payload()), bytes(&payload));
        assert_eq!((v1[2], v2[2]), (1, 2));
        assert_eq!(v1[3..], v2[3..v1.len()]);
        // A founder who owns the room writes version 1, and only that.
        let same = Invite {
            founder: Some(invite().owner),
            ..invite()
        };
        assert_eq!(same.payload(), invite().payload());
        let non_canonical = HandedOver {
            invite: invite().layout(),
            founder: invite().owner,
        };
        assert_eq!(
            Invite::parse(&encode(2, &postcard::to_stdvec(&non_canonical).unwrap())),
            Err(InviteError::Damaged)
        );
        let mut trailing = postcard::to_stdvec(&HandedOver {
            invite: invite().layout(),
            founder: MemberId([9; 32]),
        })
        .unwrap();
        trailing.push(0);
        assert_eq!(
            Invite::parse(&encode(2, &trailing)),
            Err(InviteError::Damaged)
        );
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
        let mut body = postcard::to_stdvec(&invite().layout()).unwrap();
        body.push(0);
        assert_eq!(Invite::parse(&encode(1, &body)), Err(InviteError::Damaged));
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

    /// The same for version 2 (M13), which `check.js` reads too.
    #[test]
    fn the_handed_over_layout_is_pinned() {
        let handed = Invite {
            founder: Some(MemberId([9; 32])),
            ..invite()
        };
        assert_eq!(handed.payload(), PINNED_V2_PAYLOAD);
    }

    const PINNED_V2_PAYLOAD: &str = "inbqeaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaibaeaqcaibaeaqcaibaeaqcaigfg33gyoqsazdbebaw4yieaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeasakjj62ayarw2duoryhgorpf5zgk3dbpexgk6dbnvygyzjomnxw2lrpaaeqscijbeeqscijbeeqscijbeeqscijbeeqscijbeeqscijbeeqs";

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
