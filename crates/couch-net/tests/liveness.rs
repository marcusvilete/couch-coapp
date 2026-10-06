//! `Alive` on the control stream: a peer that sends it and then goes silent
//! is timed out; one that keeps sending it, or never sent it (an older
//! build), is not.

mod common;

use std::time::{Duration, Instant};

use common::bind;
use couch_net::{
    Endpoint, NetError, SecretKey,
    control::{self, ControlReceiver, ControlSender},
    endpoint::{EndpointOptions, RelayChoice},
};
use couch_proto::{
    control::{Alive, ControlMessage, KeyframeReason, KeyframeRequest},
    room::{Goodbye, GoodbyeReason},
};

const LIMIT: Duration = Duration::from_millis(300);

async fn endpoint() -> Endpoint {
    bind(EndpointOptions::new(
        SecretKey::generate(),
        RelayChoice::Disabled,
    ))
    .await
}

/// The dialer's sender and the acceptor's receiver, with a short limit.
async fn pair() -> (
    ControlSender,
    ControlReceiver,
    (Endpoint, Endpoint, Vec<couch_net::Connection>),
) {
    let (host, viewer) = (endpoint().await, endpoint().await);
    let addr = host.addr();
    let dial = async {
        let conn = viewer.connect(addr, couch_proto::ALPN).await.unwrap();
        let (tx, _rx, _) = control::open(&conn).await.unwrap();
        (conn, tx)
    };
    let accept = async {
        let conn = host
            .accept()
            .await
            .unwrap()
            .accept()
            .unwrap()
            .await
            .unwrap();
        let (_tx, rx, _) = control::accept(&conn).await.unwrap();
        (conn, rx)
    };
    let ((dialed, tx), (accepted, mut rx)) = tokio::join!(dial, accept);
    rx.set_silent_limit(LIMIT);
    (tx, rx, (host, viewer, vec![dialed, accepted]))
}

fn alive() -> ControlMessage {
    ControlMessage::Alive(Alive {})
}

#[tokio::test]
async fn a_peer_that_goes_silent_is_timed_out() {
    let (mut tx, mut rx, _keep) = pair().await;
    tx.send(&alive()).await.unwrap();
    let started = Instant::now();
    let result = rx.recv().await;
    assert!(matches!(result, Err(NetError::TimedOut)), "{result:?}");
    let waited = started.elapsed();
    assert!(
        waited >= LIMIT - Duration::from_millis(20) && waited < LIMIT * 3,
        "timed out after {waited:?}"
    );
}

#[tokio::test]
async fn a_peer_that_keeps_saying_alive_is_not() {
    let (mut tx, mut rx, _keep) = pair().await;
    let talk = async {
        for _ in 0..10 {
            tx.send(&alive()).await.unwrap();
            tokio::time::sleep(LIMIT / 3).await;
        }
        let ask = ControlMessage::KeyframeRequest(KeyframeRequest {
            epoch: 4,
            reason: KeyframeReason::Start,
        });
        tx.send(&ask).await.unwrap();
        tx
    };
    let (received, _tx) = tokio::join!(rx.recv(), talk);
    // `Alive` itself is never handed up; the next real message is.
    assert!(matches!(
        received,
        Ok(Some(ControlMessage::KeyframeRequest(KeyframeRequest {
            epoch: 4,
            ..
        })))
    ));
}

#[tokio::test]
async fn a_peer_that_never_said_alive_is_never_timed_out() {
    let (mut tx, mut rx, _keep) = pair().await;
    let quiet = tokio::time::timeout(LIMIT * 3, rx.recv()).await;
    assert!(quiet.is_err(), "nothing should end the wait: {quiet:?}");
    let bye = ControlMessage::Goodbye(Goodbye {
        reason: GoodbyeReason::LeftRoom,
    });
    tx.send(&bye).await.unwrap();
    assert_eq!(rx.recv().await.unwrap(), Some(bye));
}
