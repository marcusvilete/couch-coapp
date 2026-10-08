//! Builds of different wire revisions (`Hello::proto_minor`) are refused
//! right after `Hello` (M10a), and each side learns the other's version, so
//! the app can say who needs to update.

mod common;

use std::time::{Duration, Instant};

use common::bind;
use couch_net::{
    Endpoint, NetError, SecretKey,
    control::{self},
    endpoint::{EndpointOptions, RelayChoice},
};
use couch_proto::{
    PROTOCOL_MINOR,
    control::{ControlMessage, Decoded, Hello},
    envelope,
};

async fn endpoint() -> Endpoint {
    bind(EndpointOptions::new(
        SecretKey::generate(),
        RelayChoice::Disabled,
    ))
    .await
}

/// A `Hello` from another build.
fn other_hello() -> Vec<u8> {
    let mut out = Vec::new();
    ControlMessage::Hello(Hello {
        proto_minor: PROTOCOL_MINOR + 1,
        app_version: "9.9.9".into(),
        features: 0,
    })
    .encode(&mut out)
    .unwrap();
    out
}

/// Reads the first control message off a raw stream.
async fn first_message(recv: &mut iroh::endpoint::RecvStream) -> ControlMessage {
    let mut buf = Vec::new();
    loop {
        if let Some((frame, _)) = envelope::decode(&buf).unwrap() {
            match ControlMessage::from_frame(&frame).unwrap() {
                Decoded::Message(message) => return message,
                other => panic!("expected a message, got {other:?}"),
            }
        }
        let chunk = recv.read_chunk(64 * 1024).await.unwrap().expect("data");
        buf.extend_from_slice(&chunk);
    }
}

#[tokio::test]
async fn an_acceptor_refuses_another_revision_and_tells_its_own() {
    let (host, guest) = (endpoint().await, endpoint().await);
    let addr = host.addr();
    let accept = async {
        let conn = host
            .accept()
            .await
            .unwrap()
            .accept()
            .unwrap()
            .await
            .unwrap();
        let started = Instant::now();
        let result = control::accept(&conn).await;
        (result.map(|_| ()), started.elapsed())
    };
    let dial = async {
        let conn = guest.connect(addr, couch_proto::ALPN).await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        send.write_all(&other_hello()).await.unwrap();
        // The acceptor's own Hello still arrives, so this side can say who
        // is newer; then it closes, as a dialer of either revision does.
        let theirs = first_message(&mut recv).await;
        conn.close(4u32.into(), b"version");
        theirs
    };
    let ((result, waited), theirs) = tokio::join!(accept, dial);
    match result {
        Err(NetError::Incompatible { minor, app_version }) => {
            assert_eq!(minor, PROTOCOL_MINOR + 1);
            assert_eq!(app_version, "9.9.9");
        }
        other => panic!("expected the other revision refused, got {other:?}"),
    }
    let ControlMessage::Hello(hello) = theirs else {
        panic!("expected the acceptor's Hello, got {theirs:?}");
    };
    assert_eq!(hello.proto_minor, PROTOCOL_MINOR);
    assert!(
        waited < Duration::from_secs(2),
        "the acceptor stops waiting once the dialer closes ({waited:?})"
    );
}

/// Anyone can dial and send any text as its version: only what a version
/// looks like is kept for logs and the screen.
#[test]
fn a_peers_version_is_cleaned_before_use() {
    assert_eq!(control::shown_version("0.9.0"), "0.9.0");
    assert_eq!(control::shown_version("1.0.0-beta+3"), "1.0.0-beta+3");
    assert_eq!(
        control::shown_version("0.9\n[ERROR] forged"),
        "0.9ERRORforged"
    );
    assert_eq!(control::shown_version(&"9".repeat(64 * 1024)).len(), 32);
    assert_eq!(control::shown_version("\u{202e}"), "an unknown version");
}

#[tokio::test]
async fn a_dialer_refuses_another_revision() {
    let (host, guest) = (endpoint().await, endpoint().await);
    let addr = host.addr();
    let accept = async {
        let conn = host
            .accept()
            .await
            .unwrap()
            .accept()
            .unwrap()
            .await
            .unwrap();
        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        let _ours = first_message(&mut recv).await;
        send.write_all(&other_hello()).await.unwrap();
        conn
    };
    let dial = async {
        let conn = guest.connect(addr, couch_proto::ALPN).await.unwrap();
        control::open(&conn).await.map(|_| ())
    };
    let (_conn, result) = tokio::join!(accept, dial);
    assert!(
        matches!(
            &result,
            Err(NetError::Incompatible { minor, app_version })
                if *minor == PROTOCOL_MINOR + 1 && app_version == "9.9.9"
        ),
        "{result:?}"
    );
}
