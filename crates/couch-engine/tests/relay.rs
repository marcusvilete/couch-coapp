//! Our relay (M7): the `iroh-relay` server we deploy, in-process, admitting
//! only clients with its token. Endpoints here use the relay alone, no
//! direct paths, so whatever connects went through it. (Here, beside the
//! room and session scenarios over the same relay, so couch-net's own tests
//! don't build the relay server.)

use std::time::Duration;

use couch_net::{
    EndpointAddr, SecretKey, control,
    endpoint::{self, EndpointOptions, RelayChoice, RelayToken},
};
use couch_testkit::relay::TestRelay;

const TOKEN: &str = "couch-test-token";
const WAIT: Duration = Duration::from_secs(10);

async fn relay_only(relay: &TestRelay, token: Option<&str>) -> couch_net::Endpoint {
    let mut options = EndpointOptions::new(
        SecretKey::generate(),
        RelayChoice::Custom {
            urls: vec![relay.url.parse().unwrap()],
            token: token.and_then(RelayToken::new),
        },
    );
    options.relay_only = true;
    endpoint::bind(options).await.expect("bind")
}

#[tokio::test]
async fn members_with_the_token_meet_through_our_relay() {
    let relay = TestRelay::start(Some(TOKEN)).await;
    let host = relay_only(&relay, Some(TOKEN)).await;
    let viewer = relay_only(&relay, Some(TOKEN)).await;
    for endpoint in [&host, &viewer] {
        tokio::time::timeout(WAIT, endpoint.online())
            .await
            .expect("online through our relay");
    }
    // The relay's URL is all the viewer knows of the host, as in an invite.
    let addr = EndpointAddr::new(host.id()).with_relay_url(relay.url.parse().unwrap());
    assert_eq!(addr.ip_addrs().count(), 0);
    let dial = async {
        let conn = viewer
            .connect(addr, couch_proto::ALPN)
            .await
            .expect("connect");
        control::open(&conn).await.expect("viewer handshake");
        conn
    };
    let accept = async {
        let incoming = host.accept().await.expect("incoming");
        let conn = incoming.accept().expect("accept").await.expect("handshake");
        control::accept(&conn).await.expect("host handshake");
        conn
    };
    let (viewer_conn, host_conn) = tokio::time::timeout(WAIT, async { tokio::join!(dial, accept) })
        .await
        .expect("connected through our relay");
    assert_eq!(viewer_conn.remote_id(), host.id());
    assert_eq!(host_conn.remote_id(), viewer.id());
    assert_eq!(relay.refused(), 0);
}

#[tokio::test]
async fn our_relay_turns_away_a_client_without_the_token() {
    let relay = TestRelay::start(Some(TOKEN)).await;
    for token in [None, Some("another-token")] {
        let stranger = relay_only(&relay, token).await;
        assert!(
            tokio::time::timeout(Duration::from_secs(2), stranger.online())
                .await
                .is_err(),
            "{token:?} reached the relay"
        );
        stranger.close().await;
    }
    assert!(relay.refused() >= 2, "refused {}", relay.refused());
}
