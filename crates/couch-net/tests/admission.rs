//! Admission over real iroh connections on loopback.

use std::{sync::Arc, time::Duration};

use couch_net::{
    Connection, Endpoint, SecretKey,
    admission::{self, AcceptTerms, AdmissionError, JoinTerms, PasswordKey, Request},
    control,
    endpoint::{self, EndpointOptions, Gate, RelayChoice},
};
use couch_proto::{
    control::{ControlMessage, close_code},
    ids::{InviteTag, InviteToken, MemberId, RoomId, Signature},
    room::{OwnedPolicy, RoomPolicy, RoomSnapshot, RosterEntry, Signed},
};
use iroh::endpoint::ConnectionError;

const ROOM: RoomId = RoomId([7; 16]);
const TOKEN: InviteToken = InviteToken([1; 32]);
const TAG: InviteTag = InviteTag([2; 16]);
const CONNECT: Duration = Duration::from_secs(5);

async fn bind(gate: Option<Gate>) -> Endpoint {
    let mut options = EndpointOptions::new(SecretKey::generate(), RelayChoice::Disabled);
    options.gate = gate;
    endpoint::bind(options).await.expect("bind")
}

fn id(endpoint: &Endpoint) -> MemberId {
    MemberId(*endpoint.id().as_bytes())
}

fn policy(roster: Vec<MemberId>, banned: Vec<MemberId>) -> OwnedPolicy {
    OwnedPolicy::founders(Signed {
        body: RoomPolicy {
            room: ROOM,
            version: 3,
            name: "Couch".into(),
            roster: roster
                .into_iter()
                .map(|id| RosterEntry { id, since: 0 })
                .collect(),
            banned,
        },
        sig: Signature([2; 32], [3; 32]),
    })
}

/// A newcomer joining an owner: the password check on both ends.
async fn join(
    newcomer_pw: &str,
    owner_pw: &str,
) -> (Result<(), AdmissionError>, Result<(), AdmissionError>) {
    let (newcomer, owner) = (bind(None).await, bind(None).await);
    let (newcomer_id, owner_id) = (id(&newcomer), id(&owner));
    let addr = owner.addr();
    let newcomer_key = PasswordKey::derive(ROOM, newcomer_pw);
    let owner_key = PasswordKey::derive(ROOM, owner_pw);
    let dial = async {
        let (conn, mut tx, mut rx) = admission::connect(&newcomer, addr, CONNECT)
            .await
            .expect("connect");
        let terms = JoinTerms {
            room: ROOM,
            token: &TOKEN,
            invite: TAG,
            password: &newcomer_key,
            me: newcomer_id,
            owner: owner_id,
        };
        (admission::join(&conn, &mut tx, &mut rx, terms).await, conn)
    };
    let accept = async {
        let incoming = owner.accept().await.expect("incoming");
        let conn = incoming.accept().expect("accept").await.expect("handshake");
        let (mut tx, mut rx, _) = control::accept(&conn).await.expect("hello");
        let result = async {
            let Request::Join(start) = admission::read_request(&conn, &mut rx).await? else {
                panic!("expected a join");
            };
            assert_eq!(start.invite, TAG);
            let terms = AcceptTerms {
                token: &TOKEN,
                password: &owner_key,
                me: owner_id,
            };
            admission::accept_join(&conn, &mut tx, &mut rx, &start, terms).await
        }
        .await;
        (result, conn)
    };
    let ((joined, a), (accepted, b)): ((_, Connection), (_, Connection)) =
        tokio::join!(dial, accept);
    a.close(0u32.into(), b"done");
    b.close(0u32.into(), b"done");
    newcomer.close().await;
    owner.close().await;
    (joined, accepted)
}

#[tokio::test]
async fn the_right_password_admits_both_sides() {
    let (joined, accepted) = join("hunter2", "hunter2").await;
    assert_eq!(joined, Ok(()));
    assert_eq!(accepted, Ok(()));
}

#[tokio::test]
async fn a_wrong_password_fails_on_both_sides() {
    let (joined, accepted) = join("hunter3", "hunter2").await;
    assert_eq!(joined, Err(AdmissionError::WrongPassword));
    // The owner learns it from the newcomer's close, and counts it.
    assert_eq!(
        accepted,
        Err(AdmissionError::Refused(close_code::ADMISSION_FAILED))
    );
}

/// One member's `MemberHello` at another, which answers with `snapshot`,
/// or refuses showing `proof`.
async fn hello(
    snapshot: Option<RoomSnapshot>,
    proof: Option<OwnedPolicy>,
) -> Result<RoomSnapshot, AdmissionError> {
    let (a, b) = (bind(None).await, bind(None).await);
    let a_id = id(&a);
    let addr = b.addr();
    let dial = async {
        let (conn, mut tx, mut rx) = admission::connect(&a, addr, CONNECT)
            .await
            .expect("connect");
        let ours = policy(vec![a_id], vec![]);
        let result = admission::hello_member(&conn, &mut tx, &mut rx, ROOM, ours).await;
        (result, conn)
    };
    let accept = async {
        let incoming = b.accept().await.expect("incoming");
        let conn = incoming.accept().expect("accept").await.expect("handshake");
        let (mut tx, mut rx, _) = control::accept(&conn).await.expect("hello");
        let Request::Member(hello) = admission::read_request(&conn, &mut rx).await.unwrap() else {
            panic!("expected a member hello");
        };
        assert_eq!(hello.room, ROOM);
        assert!(hello.policy.policy.body.lists(a_id));
        if let Some(snapshot) = snapshot {
            tx.send(&ControlMessage::RoomSnapshot(Box::new(snapshot)))
                .await
                .unwrap();
        }
        if let Some(proof) = proof {
            admission::refuse_with(&conn, &mut tx, proof, close_code::NOT_MEMBER).await;
        }
        conn
    };
    let ((result, conn_a), conn_b) = tokio::join!(dial, accept);
    conn_a.close(0u32.into(), b"done");
    conn_b.close(0u32.into(), b"done");
    a.close().await;
    b.close().await;
    result
}

#[tokio::test]
async fn a_member_hello_is_answered_with_the_snapshot() {
    let snapshot = RoomSnapshot {
        policy: policy(vec![MemberId([1; 32])], vec![]),
        members: vec![],
    };
    assert_eq!(hello(Some(snapshot.clone()), None).await, Ok(snapshot));
}

/// A refused member is shown the owner's policy before the close, so it can
/// check for itself instead of trusting a close code.
#[tokio::test]
async fn a_refused_member_is_shown_the_policy() {
    let proof = policy(vec![], vec![MemberId([5; 32])]);
    assert_eq!(
        hello(None, Some(proof.clone())).await,
        Err(AdmissionError::Shown(Box::new(proof)))
    );
}

#[tokio::test]
async fn the_gate_refuses_before_anything_else_runs() {
    let gate = Gate(Arc::new(|_| Some(close_code::RATE_LIMITED)));
    let (a, b) = (bind(None).await, bind(Some(gate)).await);
    let addr = b.addr();
    let accepting = tokio::spawn(async move {
        // The refused connection never comes out of accept.
        if let Some(incoming) = b.accept().await
            && let Ok(accepting) = incoming.accept()
        {
            assert!(accepting.await.is_err(), "a gated connection was accepted");
        }
        b
    });
    let result = a.connect(addr, couch_proto::ALPN).await;
    let refused = match result {
        Err(_) => true,
        Ok(conn) => {
            let reason = conn.closed().await;
            matches!(reason, ConnectionError::ApplicationClosed(ref close)
                if close.error_code == close_code::RATE_LIMITED.into())
        }
    };
    assert!(refused, "the dialer saw the rate-limit code");
    accepting.await.unwrap().close().await;
    a.close().await;
}
