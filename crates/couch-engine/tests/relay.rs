//! Our relay (M7): the `iroh-relay` server we deploy, in-process, admitting
//! only clients with its token. Endpoints here use the relay alone, no
//! direct paths, so whatever connects went through it. (Here, beside the
//! room and session scenarios over the same relay, so couch-net's own tests
//! don't build the relay server.)

use std::time::Duration;

use couch_net::{
    EndpointAddr, SecretKey, control,
    endpoint::{self, EndpointOptions, Relay, RelayChoice, RelayToken},
};
use couch_testkit::relay::TestRelay;

const TOKEN: &str = "couch-test-token";
const WAIT: Duration = Duration::from_secs(10);

async fn relay_only(relay: &TestRelay, token: Option<&str>) -> couch_net::Endpoint {
    let mut options = EndpointOptions::new(
        SecretKey::generate(),
        RelayChoice::ours(vec![Relay::new(
            relay.url.parse().unwrap(),
            token.and_then(RelayToken::new),
        )]),
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

/// M26a's first tests (ADR 0014): relays registered one by one, each with
/// its own token, driven through iroh's relay map directly. What they show
/// decides how registering works: a relay added while running (the M26b
/// invite flow needs it), a new code for a refusing relay, the home relay
/// removed.
mod registered {
    use std::{sync::Arc, time::Duration};

    use couch_testkit::relay::TestRelay;
    use iroh::{
        Endpoint, EndpointAddr, RelayConfig, RelayMode, RelayUrl, Watcher, endpoint::presets,
    };
    use tokio::time::timeout;

    const WAIT: Duration = Duration::from_secs(10);
    /// How long a dial that shouldn't connect is given.
    const NO: Duration = Duration::from_secs(5);

    fn url(relay: &TestRelay) -> RelayUrl {
        relay.url.parse().unwrap()
    }

    fn entry(relay: &TestRelay, token: Option<&str>) -> RelayConfig {
        let config = RelayConfig::new(url(relay), None);
        match token {
            Some(token) => config.with_auth_token(token),
            None => config,
        }
    }

    /// An endpoint on these relays alone: no direct paths, so whatever
    /// connects went through one of them.
    async fn holding(relays: Vec<RelayConfig>) -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Custom(relays.into_iter().collect()))
            .clear_ip_transports()
            .alpns(vec![couch_proto::ALPN.to_vec()])
            .bind()
            .await
            .expect("bind")
    }

    async fn online(endpoints: &[&Endpoint]) {
        for endpoint in endpoints {
            timeout(WAIT, endpoint.online())
                .await
                .expect("online through its relay");
        }
    }

    /// `from` dials `to` at `at`, and a stream goes across: whether that
    /// happened within `wait`.
    async fn meet(from: &Endpoint, to: &Endpoint, at: &TestRelay, wait: Duration) -> bool {
        let addr = EndpointAddr::new(to.id()).with_relay_url(url(at));
        let dial = async {
            let conn = from.connect(addr, couch_proto::ALPN).await.ok()?;
            let mut send = conn.open_uni().await.ok()?;
            send.write_all(b"hello").await.ok()?;
            send.finish().ok()?;
            Some(conn)
        };
        // Every incoming connection, each on its own: one abandoned by an
        // earlier dial that timed out may still arrive first.
        let accept = async {
            let mut tries = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    incoming = to.accept() => {
                        let incoming = incoming?;
                        tries.spawn(async move {
                            let conn = incoming.accept().ok()?.await.ok()?;
                            let mut recv = conn.accept_uni().await.ok()?;
                            let got = recv.read_to_end(16).await.ok()?;
                            (got == b"hello").then_some(conn)
                        });
                    }
                    Some(done) = tries.join_next() => {
                        if let Ok(Some(conn)) = done {
                            return Some(conn);
                        }
                    }
                }
            }
        };
        let met = timeout(wait, async {
            let (dialed, accepted) = tokio::join!(dial, accept);
            dialed.zip(accepted)
        })
        .await;
        matches!(met, Ok(Some(_)))
    }

    /// Waits until `endpoint`'s address names `relay` as its home.
    async fn home_becomes(endpoint: &Endpoint, relay: &TestRelay) {
        let want = url(relay);
        let mut addr = endpoint.watch_addr();
        timeout(WAIT, async {
            loop {
                if addr.get().relay_urls().any(|u| *u == want) {
                    return;
                }
                addr.updated().await.expect("the endpoint is running");
            }
        })
        .await
        .expect("the home relay moved");
    }

    #[tokio::test]
    async fn each_registered_relay_gets_its_own_token() {
        let one = TestRelay::start(Some("one")).await;
        let two = TestRelay::start(Some("two")).await;
        let both = holding(vec![entry(&one, Some("one")), entry(&two, Some("two"))]).await;
        let on_one = holding(vec![entry(&one, Some("one"))]).await;
        let on_two = holding(vec![entry(&two, Some("two"))]).await;
        online(&[&both, &on_one, &on_two]).await;

        assert!(meet(&both, &on_one, &one, WAIT).await, "both → one");
        assert!(meet(&both, &on_two, &two, WAIT).await, "both → two");
        assert_eq!((one.refused(), two.refused()), (0, 0), "each got its own");

        // Neither of the other two holds the other's relay: no way across,
        // and the relay turned the stranger away (which a checker sees).
        assert!(!meet(&on_one, &on_two, &two, NO).await);
        assert!(two.refused() >= 1, "relay two refused relay one's member");
    }

    #[tokio::test]
    async fn a_member_at_an_open_relay_outside_our_list_is_reached() {
        let ours = TestRelay::start(Some("one")).await;
        let open = TestRelay::start(None).await;
        let us = holding(vec![entry(&ours, Some("one"))]).await;
        let them = holding(vec![entry(&open, None)]).await;
        online(&[&us, &them]).await;
        assert!(meet(&us, &them, &open, WAIT).await);
        assert_eq!(ours.refused(), 0);
    }

    #[tokio::test]
    async fn a_refused_code_is_reported_by_the_home_relay_status() {
        let relay = TestRelay::start(Some("right")).await;
        let us = holding(vec![entry(&relay, Some("wrong"))]).await;
        let mut status = us.home_relay_status();
        let reason = timeout(WAIT, async {
            loop {
                if let Some(reason) = status
                    .get()
                    .iter()
                    .find_map(|s| s.auth_denied_reason().map(str::to_string))
                {
                    return reason;
                }
                status.updated().await.expect("the endpoint is running");
            }
        })
        .await
        .expect("the refusal is reported");
        assert!(
            reason.contains("Couch Co-App"),
            "the relay's reason: {reason}"
        );
    }

    #[tokio::test]
    async fn a_relay_added_while_running_carries_a_dial() {
        let one = TestRelay::start(Some("one")).await;
        let two = TestRelay::start(Some("two")).await;
        let us = holding(vec![entry(&one, Some("one"))]).await;
        let them = holding(vec![entry(&two, Some("two"))]).await;
        online(&[&us, &them]).await;

        // First without its code, as a newcomer whose link names a relay it
        // doesn't hold yet: refused.
        assert!(!meet(&us, &them, &two, NO).await);
        let refused = two.refused();
        assert!(refused >= 1);

        let previous = us
            .insert_relay(url(&two), Arc::new(entry(&two, Some("two"))))
            .await;
        assert!(previous.is_none());
        assert!(
            meet(&us, &them, &two, WAIT).await,
            "the added relay's code is used"
        );
        assert_eq!(two.refused(), refused, "nothing refused after adding it");
    }

    #[tokio::test]
    async fn a_relay_added_before_its_first_dial_carries_it() {
        let one = TestRelay::start(Some("one")).await;
        let two = TestRelay::start(Some("two")).await;
        let us = holding(vec![entry(&one, Some("one"))]).await;
        let them = holding(vec![entry(&two, Some("two"))]).await;
        online(&[&us, &them]).await;

        us.insert_relay(url(&two), Arc::new(entry(&two, Some("two"))))
            .await;
        assert!(
            meet(&us, &them, &two, WAIT).await,
            "the added relay's code is used"
        );
        assert_eq!(two.refused(), 0);
    }

    #[tokio::test]
    async fn a_new_code_for_a_refusing_relay_applies_while_running() {
        let relay = TestRelay::start(Some("new")).await;
        let us = holding(vec![entry(&relay, Some("old"))]).await;
        let them = holding(vec![entry(&relay, Some("new"))]).await;
        online(&[&them]).await;
        assert!(
            timeout(NO, us.online()).await.is_err(),
            "the old code is refused"
        );

        let previous = us
            .insert_relay(url(&relay), Arc::new(entry(&relay, Some("new"))))
            .await;
        assert!(previous.is_some_and(|p| p.auth_token.as_deref() == Some("old")));
        timeout(WAIT, us.online())
            .await
            .expect("online with the new code");
        assert!(meet(&them, &us, &relay, WAIT).await);
    }

    #[tokio::test]
    async fn removing_the_home_relay_moves_home_to_another() {
        let one = TestRelay::start(Some("one")).await;
        let two = TestRelay::start(Some("two")).await;
        let us = holding(vec![entry(&one, Some("one")), entry(&two, Some("two"))]).await;
        online(&[&us]).await;
        let home = us
            .addr()
            .relay_urls()
            .next()
            .cloned()
            .expect("a home relay");
        let ((gone, _), (stays, token)) = if home == url(&one) {
            ((&one, "one"), (&two, "two"))
        } else {
            ((&two, "two"), (&one, "one"))
        };

        assert!(us.remove_relay(&url(gone)).await.is_some());
        home_becomes(&us, stays).await;
        let them = holding(vec![entry(stays, Some(token))]).await;
        online(&[&them]).await;
        assert!(
            meet(&them, &us, stays, WAIT).await,
            "reached at the new home"
        );
    }

    #[tokio::test]
    async fn a_removed_relay_gets_no_token() {
        let one = TestRelay::start(Some("one")).await;
        let two = TestRelay::start(Some("two")).await;
        let us = holding(vec![entry(&one, Some("one")), entry(&two, Some("two"))]).await;
        let them = holding(vec![entry(&two, Some("two"))]).await;
        online(&[&us, &them]).await;

        assert!(us.remove_relay(&url(&two)).await.is_some());
        home_becomes(&us, &one).await;
        let refused = two.refused();
        assert!(
            !meet(&us, &them, &two, NO).await,
            "dialed there without a code"
        );
        assert!(two.refused() > refused, "relay two refused the dial");
    }
}

/// A relay code is checked before it is saved (M26a): the relay lets us in
/// with its token, refuses another (in its own words), or isn't there.
#[tokio::test]
async fn a_relay_check_tells_accepted_refused_and_unreachable_apart() {
    use couch_net::endpoint::{RelayCheck, check_relay};

    let relay = TestRelay::start(Some(TOKEN)).await;
    let url: couch_net::RelayUrl = relay.url.parse().unwrap();
    let with = |token: Option<&str>| Relay::new(url.clone(), token.and_then(RelayToken::new));
    assert_eq!(
        check_relay(&with(Some(TOKEN)), WAIT).await.unwrap(),
        RelayCheck::Accepted
    );
    let RelayCheck::Refused(reason) = check_relay(&with(Some("old-token")), WAIT).await.unwrap()
    else {
        panic!("refused");
    };
    assert!(reason.contains("Couch Co-App"), "{reason}");
    assert!(matches!(
        check_relay(&with(None), WAIT).await.unwrap(),
        RelayCheck::Refused(_)
    ));
    let open = TestRelay::start(None).await;
    let open = Relay::new(open.url.parse().unwrap(), None);
    assert_eq!(
        check_relay(&open, WAIT).await.unwrap(),
        RelayCheck::Accepted
    );

    // Nothing listens there any more.
    let gone = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let nowhere = Relay::new(format!("http://127.0.0.1:{gone}").parse().unwrap(), None);
    assert!(matches!(
        check_relay(&nowhere, Duration::from_secs(3)).await.unwrap(),
        RelayCheck::Unreachable(_)
    ));
}

/// M26a: what the engine tells of our own connection, from real relays.
/// Our relay is home; a second one with a wrong code is caught by the
/// background check and becomes the trouble the shell shows; a wrong code
/// for the home relay itself is caught as iroh reports it.
#[tokio::test]
async fn the_network_view_tells_each_relay_s_state() {
    use couch_engine::{
        Command, Engine, EngineOptions, NetworkTimings, NetworkTrouble, NetworkView, Profile,
        RelayRules, RelayStateView, Relays, Response, secrets::Unprotected,
    };

    let ours = TestRelay::start(Some(TOKEN)).await;
    let other = TestRelay::start(Some("other-token")).await;
    let dir = tempfile::tempdir().unwrap();
    let options = EngineOptions {
        relays: Relays::Profile(RelayRules {
            built_in: None,
            test_build: true,
            public_relays: false,
        }),
        relay_only: true,
        secret_store: std::sync::Arc::new(Unprotected),
        network: NetworkTimings {
            check_timeout: Duration::from_secs(3),
            trouble_after: Duration::from_millis(300),
            first_check: Duration::from_millis(200),
            check_every: Duration::from_secs(1),
            recheck_after: Duration::from_millis(500),
        },
        ..EngineOptions::development()
    };
    let engine = Engine::start(Profile::new("net", dir.path()).unwrap(), options)
        .await
        .unwrap();
    let add = |url: &str, token: &str, unchecked| Command::AddRelay {
        url: url.into(),
        token: Some(token.into()),
        personal: false,
        unchecked,
    };
    let view = |r: Response| match r {
        Response::Network(v) => v,
        other => panic!("the network view: {other:?}"),
    };
    let until = |what: &'static str, check: fn(&NetworkView) -> bool| {
        let engine = &engine;
        async move {
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    let now = view(engine.handle(Command::GetNetwork).await.unwrap());
                    if check(&now) {
                        return now;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        }
    };

    view(engine.handle(add(&ours.url, TOKEN, false)).await.unwrap());
    let home = until("our relay home", |v| {
        v.relays
            .first()
            .is_some_and(|r| r.state == RelayStateView::Home)
    })
    .await;
    assert_eq!(home.trouble, None);
    assert!(home.home_relay.is_some());

    // A second relay, its code wrong (saved anyway): the check catches it.
    view(engine.handle(add(&other.url, "wrong", true)).await.unwrap());
    // Set aside, whatever iroh first made its home: ours is home.
    until("the second relay refused, ours home", |v| {
        matches!(v.relays[1].state, RelayStateView::Refused { .. })
            && v.relays[0].state == RelayStateView::Home
    })
    .await;
    until("the trouble said", |v| {
        matches!(v.trouble, Some(NetworkTrouble::RelayRefused { .. }))
    })
    .await;
    // Its right code: the trouble goes.
    view(
        engine
            .handle(add(&other.url, "other-token", false))
            .await
            .unwrap(),
    );
    until("no trouble", |v| v.trouble.is_none()).await;

    // Our home relay given a wrong code: iroh reports the refusal, it is
    // set aside, and the other relay becomes home.
    view(
        engine
            .handle(add(&ours.url, "rotated-away", true))
            .await
            .unwrap(),
    );
    until("ours refused, the other home", |v| {
        matches!(v.relays[0].state, RelayStateView::Refused { .. })
            && v.relays[1].state == RelayStateView::Home
    })
    .await;
    engine.shutdown().await;
}
