//! Room ids and the signatures on member records and policies.
//!
//! A member's id is its Ed25519 public key (the iroh endpoint id), so records
//! are verified against the id they describe and policies against the owner.

use couch_proto::{
    ids::{InviteTag, InviteToken, MemberId, RoomId, RoomNonce, Signature},
    room::{MEMBER_RECORD_DOMAIN, MemberRecord, POLICY_DOMAIN, RoomPolicy, Signed, signing_bytes},
};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

/// The room id an owner and nonce stand for. Anyone can recompute it, so a
/// room can't be passed off as having a different owner.
pub fn room_id(owner: MemberId, nonce: RoomNonce) -> RoomId {
    let hash: [u8; 32] = Sha256::new()
        .chain_update(b"couch/room-id/v1")
        .chain_update(owner.0)
        .chain_update(nonce.0)
        .finalize()
        .into();
    RoomId(hash[..16].try_into().expect("16 of 32 bytes"))
}

/// How a joiner names its invite to the owner without sending the token.
pub fn invite_tag(token: &InviteToken) -> InviteTag {
    let hash: [u8; 32] = Sha256::new()
        .chain_update(b"couch/invite-tag/v1")
        .chain_update(token.0)
        .finalize()
        .into();
    InviteTag(hash[..16].try_into().expect("16 of 32 bytes"))
}

/// The member id that goes with a signing key.
pub fn member_id(key: &SigningKey) -> MemberId {
    MemberId(key.verifying_key().to_bytes())
}

pub fn sign_record(key: &SigningKey, record: MemberRecord) -> Signed<MemberRecord> {
    let sig = key.sign(&signing_bytes(MEMBER_RECORD_DOMAIN, &record));
    Signed {
        body: record,
        sig: Signature::from_bytes(sig.to_bytes()),
    }
}

pub fn sign_policy(key: &SigningKey, policy: RoomPolicy) -> Signed<RoomPolicy> {
    let sig = key.sign(&signing_bytes(POLICY_DOMAIN, &policy));
    Signed {
        body: policy,
        sig: Signature::from_bytes(sig.to_bytes()),
    }
}

/// Signed by the member the record describes.
pub fn record_is_authentic(record: &Signed<MemberRecord>) -> bool {
    verify(
        record.body.id,
        &signing_bytes(MEMBER_RECORD_DOMAIN, &record.body),
        &record.sig,
    )
}

/// Signed by `owner`.
pub fn policy_is_authentic(policy: &Signed<RoomPolicy>, owner: MemberId) -> bool {
    verify(
        owner,
        &signing_bytes(POLICY_DOMAIN, &policy.body),
        &policy.sig,
    )
}

fn verify(signer: MemberId, message: &[u8], sig: &Signature) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(&signer.0) else {
        return false;
    };
    // Strict: rejects small-order keys and non-canonical signatures, as iroh does.
    key.verify_strict(
        message,
        &ed25519_dalek::Signature::from_bytes(&sig.to_bytes()),
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use couch_proto::room::{PeerAddr, RosterEntry};

    use super::*;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn record(key: &SigningKey) -> MemberRecord {
        MemberRecord {
            room: RoomId([1; 16]),
            id: member_id(key),
            since: 0,
            version: 1,
            name: "Ana".into(),
            addr: PeerAddr::default(),
            left: false,
        }
    }

    #[test]
    fn room_id_depends_on_owner_and_nonce() {
        let a = room_id(MemberId([1; 32]), RoomNonce([2; 16]));
        assert_eq!(a, room_id(MemberId([1; 32]), RoomNonce([2; 16])));
        assert_ne!(a, room_id(MemberId([3; 32]), RoomNonce([2; 16])));
        assert_ne!(a, room_id(MemberId([1; 32]), RoomNonce([4; 16])));
    }

    #[test]
    fn records_verify_only_as_signed_by_their_member() {
        let ana = key(1);
        let signed = sign_record(&ana, record(&ana));
        assert!(record_is_authentic(&signed));

        // Someone else signing Ana's record.
        let forged = sign_record(&key(2), record(&ana));
        assert!(!record_is_authentic(&forged));

        // Any change to the body breaks it.
        let mut renamed = signed.clone();
        renamed.body.name = "Eve".into();
        assert!(!record_is_authentic(&renamed));
        let mut moved = signed;
        moved.body.room = RoomId([9; 16]);
        assert!(!record_is_authentic(&moved));
    }

    #[test]
    fn policies_verify_only_against_the_owner() {
        let owner = key(1);
        let policy = sign_policy(
            &owner,
            RoomPolicy {
                room: RoomId([1; 16]),
                version: 1,
                name: "Couch".into(),
                roster: vec![RosterEntry {
                    id: member_id(&owner),
                    since: 0,
                }],
                banned: vec![],
            },
        );
        assert!(policy_is_authentic(&policy, member_id(&owner)));
        assert!(!policy_is_authentic(&policy, member_id(&key(2))));
    }

    /// A record signature can't be replayed as a policy signature or back.
    #[test]
    fn domains_separate_record_and_policy_signatures() {
        let ana = key(1);
        let signed = sign_record(&ana, record(&ana));
        let bytes = signing_bytes(POLICY_DOMAIN, &signed.body);
        assert!(!verify(member_id(&ana), &bytes, &signed.sig));
    }

    #[test]
    fn invite_tags_are_distinct_and_stable() {
        let a = invite_tag(&InviteToken([1; 32]));
        assert_eq!(a, invite_tag(&InviteToken([1; 32])));
        assert_ne!(a, invite_tag(&InviteToken([2; 32])));
        assert_ne!(a.0, [1; 16], "never the token itself");
    }

    #[test]
    fn an_invalid_public_key_fails_closed() {
        let ana = key(1);
        let mut signed = sign_record(&ana, record(&ana));
        signed.body.id = MemberId([0; 32]);
        assert!(!record_is_authentic(&signed));
    }
}
