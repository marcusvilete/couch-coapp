//! Admission: what a connection between room members exchanges, after
//! `Hello`, before it carries anything else (docs/protocol.md, Admission).
//!
//! **A newcomer joins only through the room's owner,** with an invite and the
//! password: asymmetric SPAKE2 over Ed25519, the newcomer as side A, the
//! owner as side B. Each side proves it derived the same key with an HMAC
//! over the transcript, keyed from the SPAKE2 key salted with keying material
//! exported from this QUIC connection, so a confirmation can't be relayed
//! onto another connection. One run lets a wrong guesser test exactly one
//! password, and only online; the owner limits how often
//! (`couch_room::limits`) and retires an invite after a few wrong passwords.
//!
//! **Members meet each other with `MemberHello`.** iroh has authenticated
//! both ids; the acceptor checks the dialer against the owner's signed member
//! list, after adopting the dialer's copy if it is newer.
//!
//! **Keys.**
//! - Password key = HKDF-SHA256(salt = room id, ikm = NFKC(trimmed password),
//!   info = "couch/pw/v1"). Only the owner keeps it; nobody keeps the
//!   password.
//! - SPAKE2 password = HKDF-SHA256(salt = the invite's token, ikm = password
//!   key, info = "couch/pake-pw/v2" || room id). A guess is worthless
//!   without the invite.
//!
//! This composition is ours. It needs an outside review before any public
//! release (docs/architecture.md).

use std::{fmt, time::Duration};

use couch_proto::{
    admission::{AdmitConfirm, AdmitReply, JoinStart, MemberHello, PakeMessage},
    control::{ControlMessage, close_code},
    ids::{InviteTag, InviteToken, MemberId, RoomId},
    room::{RoomPolicy, RoomSnapshot, Signed},
};
use hkdf::Hkdf;
use hmac::{Hmac, KeyInit, Mac};
use iroh::{
    Endpoint, EndpointAddr,
    endpoint::{ConnectError, ConnectingError, Connection, ConnectionError, VarInt},
};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use unicode_normalization::UnicodeNormalization;

use crate::control::{ControlReceiver, ControlSender};

/// Each exchange after `Hello`, from its first message to the last.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// TLS exporter label (RFC 5705); the room id is the context.
const EXPORTER_LABEL: &[u8] = b"couch-coapp pake v1";

/// The password, stretched for one room. What the owner keeps.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PasswordKey(pub [u8; 32]);

impl fmt::Debug for PasswordKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PasswordKey(..)")
    }
}

impl PasswordKey {
    /// Leading and trailing whitespace is dropped (chat apps add it), and
    /// Unicode is normalized so the same text typed on different keyboards
    /// matches.
    pub fn derive(room: RoomId, password: &str) -> Self {
        let normalized: String = password.trim().nfkc().collect();
        let mut out = [0; 32];
        Hkdf::<Sha256>::new(Some(&room.0), normalized.as_bytes())
            .expand(b"couch/pw/v1", &mut out)
            .expect("32 bytes is a valid HKDF-SHA256 length");
        Self(out)
    }
}

/// Why a connection didn't become a room connection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    #[error("wrong password")]
    WrongPassword,
    #[error("refused by the other member ({})", close_name(*.0))]
    Refused(u32),
    /// Refused, with the owner's signed policy as the reason (we are banned,
    /// or no longer on the roster). The room checks it; a peer's word alone
    /// proves nothing.
    #[error("refused, with the owner's policy as the reason")]
    Shown(Box<Signed<RoomPolicy>>),
    #[error("the exchange timed out")]
    Timeout,
    #[error("no answer from the other member")]
    Unreachable,
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("connection: {0}")]
    Connection(String),
}

/// A close code's name, for messages.
pub fn close_name(code: u32) -> &'static str {
    match code {
        close_code::NORMAL => "normal",
        close_code::ADMISSION_FAILED => "admission failed",
        close_code::BANNED => "banned",
        close_code::DUPLICATE => "duplicate",
        close_code::VERSION => "version",
        close_code::RATE_LIMITED => "rate limited",
        close_code::ROOM_FULL => "room full",
        close_code::LEFT_ROOM => "left the room",
        close_code::INVITE_EXPIRED => "invite expired",
        close_code::UNKNOWN_ROOM => "unknown room",
        close_code::PROTOCOL => "protocol error",
        close_code::NOT_MEMBER => "not a member",
        _ => "unknown",
    }
}

fn pake_password(room: RoomId, token: &InviteToken, key: &PasswordKey) -> [u8; 32] {
    let mut out = [0; 32];
    Hkdf::<Sha256>::new(Some(&token.0), &key.0)
        .expand_multi_info(&[b"couch/pake-pw/v2", &room.0], &mut out)
        .expect("32 bytes is a valid HKDF-SHA256 length");
    out
}

fn identity(room: RoomId, member: MemberId) -> Identity {
    let mut bytes = b"couch/v1".to_vec();
    bytes.extend_from_slice(&room.0);
    bytes.extend_from_slice(&member.0);
    Identity::new(&bytes)
}

/// Everything both sides agree on, hashed once.
fn transcript(
    room: RoomId,
    invite: InviteTag,
    newcomer: MemberId,
    owner: MemberId,
    msg_a: &PakeMessage,
    msg_b: &PakeMessage,
) -> [u8; 32] {
    Sha256::new()
        .chain_update(b"couch/join/v1")
        .chain_update(room.0)
        .chain_update(invite.0)
        .chain_update(newcomer.0)
        .chain_update(owner.0)
        .chain_update(msg_a.to_bytes())
        .chain_update(msg_b.to_bytes())
        .finalize()
        .into()
}

/// (k_A, k_B): the newcomer's and the owner's confirmation keys.
fn confirmation_keys(spake_key: &[u8], exported: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let mut okm = [0; 64];
    Hkdf::<Sha256>::new(Some(exported), spake_key)
        .expand(b"couch/confirm/v1", &mut okm)
        .expect("64 bytes is a valid HKDF-SHA256 length");
    let (a, b) = okm.split_at(32);
    (a.try_into().expect("32"), b.try_into().expect("32"))
}

fn mac(key: &[u8; 32], transcript: &[u8; 32]) -> [u8; 32] {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(transcript);
    mac.finalize().into_bytes().into()
}

/// Constant time.
fn mac_matches(key: &[u8; 32], transcript: &[u8; 32], tag: &[u8; 32]) -> bool {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(transcript);
    mac.verify_slice(tag).is_ok()
}

/// The newcomer's half of the password check, without I/O.
pub struct Newcomer {
    spake: Spake2<Ed25519Group>,
    room: RoomId,
    invite: InviteTag,
    me: MemberId,
    owner: MemberId,
    msg_a: PakeMessage,
}

impl Newcomer {
    pub fn start(
        room: RoomId,
        invite: InviteTag,
        token: &InviteToken,
        password: &PasswordKey,
        me: MemberId,
        owner: MemberId,
    ) -> (Self, JoinStart) {
        let pw = Password::new(pake_password(room, token, password));
        let (spake, msg) =
            Spake2::<Ed25519Group>::start_a(&pw, &identity(room, me), &identity(room, owner));
        let msg_a = PakeMessage::from_slice(&msg).expect("SPAKE2 Ed25519 messages are 33 bytes");
        let start = JoinStart {
            room,
            invite,
            pake: msg_a,
        };
        (
            Self {
                spake,
                room,
                invite,
                me,
                owner,
                msg_a,
            },
            start,
        )
    }

    /// Checks the owner's confirmation; returns ours to send.
    pub fn finish(
        self,
        reply: &AdmitReply,
        exported: &[u8; 32],
    ) -> Result<AdmitConfirm, AdmissionError> {
        let key = self
            .spake
            .finish(&reply.pake.to_bytes())
            .map_err(|e| AdmissionError::Protocol(format!("SPAKE2: {e}")))?;
        let transcript = transcript(
            self.room,
            self.invite,
            self.me,
            self.owner,
            &self.msg_a,
            &reply.pake,
        );
        let (k_a, k_b) = confirmation_keys(&key, exported);
        if !mac_matches(&k_b, &transcript, &reply.confirm) {
            return Err(AdmissionError::WrongPassword);
        }
        Ok(AdmitConfirm {
            confirm: mac(&k_a, &transcript),
        })
    }
}

/// The owner's half, without I/O, after it sent its reply.
pub struct Owner {
    k_a: [u8; 32],
    transcript: [u8; 32],
}

impl Owner {
    pub fn respond(
        start: &JoinStart,
        token: &InviteToken,
        password: &PasswordKey,
        newcomer: MemberId,
        me: MemberId,
        exported: &[u8; 32],
    ) -> Result<(Self, AdmitReply), AdmissionError> {
        let pw = Password::new(pake_password(start.room, token, password));
        let (spake, msg) = Spake2::<Ed25519Group>::start_b(
            &pw,
            &identity(start.room, newcomer),
            &identity(start.room, me),
        );
        let msg_b = PakeMessage::from_slice(&msg).expect("SPAKE2 Ed25519 messages are 33 bytes");
        let key = spake
            .finish(&start.pake.to_bytes())
            .map_err(|e| AdmissionError::Protocol(format!("SPAKE2: {e}")))?;
        let transcript = transcript(start.room, start.invite, newcomer, me, &start.pake, &msg_b);
        let (k_a, k_b) = confirmation_keys(&key, exported);
        let reply = AdmitReply {
            pake: msg_b,
            confirm: mac(&k_b, &transcript),
        };
        Ok((Self { k_a, transcript }, reply))
    }

    pub fn verify(self, confirm: &AdmitConfirm) -> Result<(), AdmissionError> {
        if mac_matches(&self.k_a, &self.transcript, &confirm.confirm) {
            Ok(())
        } else {
            Err(AdmissionError::WrongPassword)
        }
    }
}

/// Keying material only this connection's two ends can compute.
pub fn exported_key(conn: &Connection, room: RoomId) -> Result<[u8; 32], AdmissionError> {
    let mut out = [0; 32];
    conn.export_keying_material(&mut out, EXPORTER_LABEL, &room.0)
        .map_err(|e| AdmissionError::Connection(format!("keying material: {e:?}")))?;
    Ok(out)
}

pub fn close(conn: &Connection, code: u32) {
    conn.close(VarInt::from_u32(code), close_name(code).as_bytes());
}

/// The next message, or the reason the connection ended.
async fn next(
    conn: &Connection,
    rx: &mut ControlReceiver,
) -> Result<ControlMessage, AdmissionError> {
    match rx.recv().await {
        Ok(Some(message)) => Ok(message),
        Ok(None) => Err(closed(conn)),
        Err(crate::NetError::Protocol(e)) => {
            close(conn, close_code::PROTOCOL);
            Err(AdmissionError::Protocol(e))
        }
        Err(crate::NetError::Version(kind)) => {
            close(conn, close_code::VERSION);
            Err(AdmissionError::Protocol(format!(
                "unknown message type {kind:#06x} that must be understood"
            )))
        }
        Err(_) => Err(closed(conn)),
    }
}

/// Why the connection is closed, as far as admission is concerned.
pub fn closed(conn: &Connection) -> AdmissionError {
    match conn.close_reason() {
        Some(ConnectionError::ApplicationClosed(close)) => AdmissionError::Refused(
            u32::try_from(close.error_code.into_inner()).unwrap_or(u32::MAX),
        ),
        Some(other) => AdmissionError::Connection(other.to_string()),
        None => AdmissionError::Connection("stream closed".into()),
    }
}

fn unexpected(conn: &Connection, what: &str, got: &ControlMessage) -> AdmissionError {
    close(conn, close_code::PROTOCOL);
    AdmissionError::Protocol(format!("expected {what}, got {:#06x}", got.type_id()))
}

/// Connects to a member and exchanges `Hello`, each with its timeout. A
/// peer that refused us during the handshake (its gate) reads as
/// [`AdmissionError::Refused`].
pub async fn connect(
    endpoint: &Endpoint,
    target: EndpointAddr,
    connect_timeout: Duration,
) -> Result<(Connection, ControlSender, ControlReceiver), AdmissionError> {
    let conn =
        match tokio::time::timeout(connect_timeout, endpoint.connect(target, couch_proto::ALPN))
            .await
        {
            Err(_) => return Err(AdmissionError::Unreachable),
            Ok(Err(e)) => return Err(connect_error(&e)),
            Ok(Ok(conn)) => conn,
        };
    match tokio::time::timeout(TIMEOUT, crate::control::open(&conn)).await {
        Ok(Ok((tx, rx, _hello))) => Ok((conn, tx, rx)),
        Ok(Err(_)) => Err(closed(&conn)),
        Err(_) => {
            close(&conn, close_code::NORMAL);
            Err(AdmissionError::Timeout)
        }
    }
}

fn connect_error(e: &ConnectError) -> AdmissionError {
    let closed_by_peer = match e {
        ConnectError::Connection { source, .. }
        | ConnectError::Connecting {
            source: ConnectingError::ConnectionError { source, .. },
            ..
        } => match source {
            ConnectionError::ApplicationClosed(close) => Some(close.error_code),
            _ => None,
        },
        _ => None,
    };
    match closed_by_peer {
        Some(code) => AdmissionError::Refused(u32::try_from(code.into_inner()).unwrap_or(u32::MAX)),
        None => AdmissionError::Connection(e.to_string()),
    }
}

/// What a newcomer brings to the owner.
pub struct JoinTerms<'a> {
    pub room: RoomId,
    pub token: &'a InviteToken,
    pub invite: InviteTag,
    pub password: &'a PasswordKey,
    pub me: MemberId,
    pub owner: MemberId,
}

/// Newcomer side, after [`connect`] to the owner: the password check. On
/// failure the connection is closed (by us or by the owner).
pub async fn join(
    conn: &Connection,
    tx: &mut ControlSender,
    rx: &mut ControlReceiver,
    terms: JoinTerms<'_>,
) -> Result<(), AdmissionError> {
    let (newcomer, start) = Newcomer::start(
        terms.room,
        terms.invite,
        terms.token,
        terms.password,
        terms.me,
        terms.owner,
    );
    tx.send(&ControlMessage::JoinStart(start))
        .await
        .map_err(|_| closed(conn))?;
    match next(conn, rx).await? {
        ControlMessage::AdmitReply(reply) => {
            let exported = exported_key(conn, terms.room)?;
            match newcomer.finish(&reply, &exported) {
                Ok(confirm) => tx
                    .send(&ControlMessage::AdmitConfirm(confirm))
                    .await
                    .map_err(|_| closed(conn)),
                Err(e) => {
                    let code = match e {
                        AdmissionError::WrongPassword => close_code::ADMISSION_FAILED,
                        _ => close_code::PROTOCOL,
                    };
                    close(conn, code);
                    Err(e)
                }
            }
        }
        ControlMessage::PolicyUpdate(policy) => {
            close(conn, close_code::NORMAL);
            Err(AdmissionError::Shown(policy))
        }
        other => Err(unexpected(conn, "AdmitReply", &other)),
    }
}

/// Member side, after [`connect`] to another member: introduces us with our
/// copy of the owner's policy and returns the snapshot the acceptor sends
/// when it lets us in.
pub async fn hello_member(
    conn: &Connection,
    tx: &mut ControlSender,
    rx: &mut ControlReceiver,
    room: RoomId,
    policy: Signed<RoomPolicy>,
) -> Result<RoomSnapshot, AdmissionError> {
    tx.send(&ControlMessage::MemberHello(Box::new(MemberHello {
        room,
        policy,
    })))
    .await
    .map_err(|_| closed(conn))?;
    loop {
        match next(conn, rx).await? {
            ControlMessage::RoomSnapshot(snapshot) => return Ok(*snapshot),
            // The acceptor may already speak about the room (presence).
            ControlMessage::Presence(_) => continue,
            ControlMessage::PolicyUpdate(policy) => {
                close(conn, close_code::NORMAL);
                return Err(AdmissionError::Shown(policy));
            }
            other => return Err(unexpected(conn, "RoomSnapshot", &other)),
        }
    }
}

/// What a connection's first message after `Hello` asks for.
pub enum Request {
    Join(JoinStart),
    Member(Box<MemberHello>),
}

/// Acceptor side, right after `control::accept`.
pub async fn read_request(
    conn: &Connection,
    rx: &mut ControlReceiver,
) -> Result<Request, AdmissionError> {
    match next(conn, rx).await? {
        ControlMessage::JoinStart(start) => Ok(Request::Join(start)),
        ControlMessage::MemberHello(hello) => Ok(Request::Member(hello)),
        other => Err(unexpected(conn, "JoinStart or MemberHello", &other)),
    }
}

/// What the owner brings to a newcomer's password check.
pub struct AcceptTerms<'a> {
    pub token: &'a InviteToken,
    pub password: &'a PasswordKey,
    pub me: MemberId,
}

/// Owner side: runs the password check for `start`. On failure the
/// connection is closed with `ADMISSION_FAILED` (or by the newcomer).
pub async fn accept_join(
    conn: &Connection,
    tx: &mut ControlSender,
    rx: &mut ControlReceiver,
    start: &JoinStart,
    terms: AcceptTerms<'_>,
) -> Result<(), AdmissionError> {
    let newcomer = MemberId(*conn.remote_id().as_bytes());
    let exported = exported_key(conn, start.room)?;
    let (owner, reply) = match Owner::respond(
        start,
        terms.token,
        terms.password,
        newcomer,
        terms.me,
        &exported,
    ) {
        Ok(ok) => ok,
        Err(e) => {
            close(conn, close_code::PROTOCOL);
            return Err(e);
        }
    };
    tx.send(&ControlMessage::AdmitReply(reply))
        .await
        .map_err(|_| closed(conn))?;
    match next(conn, rx).await? {
        ControlMessage::AdmitConfirm(confirm) => match owner.verify(&confirm) {
            Ok(()) => Ok(()),
            Err(e) => {
                close(conn, close_code::ADMISSION_FAILED);
                Err(e)
            }
        },
        other => Err(unexpected(conn, "AdmitConfirm", &other)),
    }
}

/// Acceptor: refuses with `code`, first showing the dialer the owner's
/// signed policy (it holds nothing secret) so it can see why for itself.
pub async fn refuse_with(
    conn: &Connection,
    tx: &mut ControlSender,
    policy: Signed<RoomPolicy>,
    code: u32,
) {
    let message = ControlMessage::PolicyUpdate(Box::new(policy));
    if tx.send(&message).await.is_ok() {
        tx.finish(Duration::from_secs(1)).await;
    }
    close(conn, code);
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM: RoomId = RoomId([1; 16]);
    const TOKEN: InviteToken = InviteToken([2; 32]);
    const TAG: InviteTag = InviteTag([9; 16]);
    const A: MemberId = MemberId([0xaa; 32]);
    const B: MemberId = MemberId([0xbb; 32]);
    const EXPORTED: [u8; 32] = [3; 32];

    fn run(
        newcomer_pw: &str,
        owner_pw: &str,
        owner_token: InviteToken,
        owner_exported: [u8; 32],
    ) -> (Result<AdmitConfirm, AdmissionError>, Owner) {
        let (newcomer, start) = Newcomer::start(
            ROOM,
            TAG,
            &TOKEN,
            &PasswordKey::derive(ROOM, newcomer_pw),
            A,
            B,
        );
        let (owner, reply) = Owner::respond(
            &start,
            &owner_token,
            &PasswordKey::derive(ROOM, owner_pw),
            A,
            B,
            &owner_exported,
        )
        .unwrap();
        (newcomer.finish(&reply, &EXPORTED), owner)
    }

    #[test]
    fn same_password_admits_both_ways() {
        let (confirm, owner) = run("pizza 123", "pizza 123", TOKEN, EXPORTED);
        owner.verify(&confirm.unwrap()).unwrap();
    }

    #[test]
    fn wrong_password_is_caught_by_the_newcomer() {
        let (confirm, _) = run("pizza 123", "pizza 124", TOKEN, EXPORTED);
        assert_eq!(confirm.unwrap_err(), AdmissionError::WrongPassword);
    }

    #[test]
    fn the_invite_token_is_part_of_the_password() {
        let (confirm, _) = run("pizza", "pizza", InviteToken([9; 32]), EXPORTED);
        assert_eq!(confirm.unwrap_err(), AdmissionError::WrongPassword);
    }

    /// A confirmation from one connection is useless on another.
    #[test]
    fn confirmations_are_bound_to_the_connection() {
        let (confirm, _) = run("pizza", "pizza", TOKEN, [4; 32]);
        assert_eq!(confirm.unwrap_err(), AdmissionError::WrongPassword);
    }

    #[test]
    fn a_forged_newcomer_confirmation_fails() {
        let (_, owner) = run("pizza", "pizza", TOKEN, EXPORTED);
        assert_eq!(
            owner.verify(&AdmitConfirm { confirm: [0; 32] }),
            Err(AdmissionError::WrongPassword)
        );
    }

    /// The owner binds the identity it saw from TLS: a newcomer claiming
    /// another id derives a different key.
    #[test]
    fn identities_are_bound() {
        let pw = PasswordKey::derive(ROOM, "pizza");
        let (newcomer, start) = Newcomer::start(ROOM, TAG, &TOKEN, &pw, A, B);
        let impostor = MemberId([0xcc; 32]);
        let (_, reply) = Owner::respond(&start, &TOKEN, &pw, impostor, B, &EXPORTED).unwrap();
        assert_eq!(
            newcomer.finish(&reply, &EXPORTED).unwrap_err(),
            AdmissionError::WrongPassword
        );
    }

    #[test]
    fn the_invite_named_is_bound() {
        let pw = PasswordKey::derive(ROOM, "pizza");
        let (newcomer, mut start) = Newcomer::start(ROOM, TAG, &TOKEN, &pw, A, B);
        start.invite = InviteTag([8; 16]);
        let (_, reply) = Owner::respond(&start, &TOKEN, &pw, A, B, &EXPORTED).unwrap();
        assert_eq!(
            newcomer.finish(&reply, &EXPORTED).unwrap_err(),
            AdmissionError::WrongPassword
        );
    }

    #[test]
    fn malformed_pake_messages_fail() {
        let pw = PasswordKey::derive(ROOM, "pizza");
        let (_, mut start) = Newcomer::start(ROOM, TAG, &TOKEN, &pw, A, B);
        start.pake.0 = b'B'; // claims to be the owner's side
        assert!(matches!(
            Owner::respond(&start, &TOKEN, &pw, A, B, &EXPORTED),
            Err(AdmissionError::Protocol(_))
        ));
        let (newcomer, start) = Newcomer::start(ROOM, TAG, &TOKEN, &pw, A, B);
        let (_, mut reply) = Owner::respond(&start, &TOKEN, &pw, A, B, &EXPORTED).unwrap();
        // Some other point (or none): either way it fails, never admits.
        reply.pake.1 = [0xff; 32];
        assert!(newcomer.finish(&reply, &EXPORTED).is_err());
    }

    #[test]
    fn passwords_are_normalized_and_trimmed() {
        // "é" precomposed and decomposed, with stray whitespace.
        assert_eq!(
            PasswordKey::derive(ROOM, "caf\u{e9}"),
            PasswordKey::derive(ROOM, " cafe\u{301}\n")
        );
        assert_ne!(
            PasswordKey::derive(ROOM, "cafe"),
            PasswordKey::derive(ROOM, "Cafe")
        );
        assert_ne!(
            PasswordKey::derive(ROOM, "cafe"),
            PasswordKey::derive(RoomId([2; 16]), "cafe"),
            "the same password differs per room"
        );
    }

    #[test]
    fn keys_never_show_in_debug() {
        assert_eq!(
            format!("{:?}", PasswordKey::derive(ROOM, "x")),
            "PasswordKey(..)"
        );
    }
}
