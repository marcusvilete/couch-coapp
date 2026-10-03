//! Input end to end on loopback: the forwarder's output reaches the host's
//! router through the input stream and datagrams, and rumble comes back.

mod common;

use std::time::{Duration, Instant};

use common::{bind, connect};
use couch_input::{
    HidUsage, MemberId, MouseButton, PadButtons, PadState, Permissions,
    forward::Forwarder,
    router::{Effect, SeatRouter},
};
use couch_net::{
    SecretKey,
    datagrams::Datagrams,
    endpoint::{EndpointOptions, RelayChoice},
    input::{InputReceiver, InputSender, send_rumble},
    video::{HostVideo, ViewerVideo},
};
use couch_proto::{
    ids::SeatId,
    input::{PadSnapshot, RumbleDatagram},
    ipc::LocalInput,
    media::{datagram, decode_payload},
};

const SEAT: SeatId = SeatId(0);

#[tokio::test(flavor = "multi_thread")]
async fn local_input_reaches_the_router_and_rumble_comes_back() {
    let host = bind(EndpointOptions::new(
        SecretKey::generate(),
        RelayChoice::Disabled,
    ))
    .await;
    let viewer = bind(EndpointOptions::new(
        SecretKey::generate(),
        RelayChoice::Disabled,
    ))
    .await;
    let (host_conn, viewer_conn) = connect(&host, &viewer, host.addr()).await;
    let member = MemberId(*viewer.id().as_bytes());

    // Video runs alongside, as it would, and must not swallow input.
    let host_datagrams = Datagrams::start(host_conn.clone());
    let _video = HostVideo::start(host_conn.clone(), 1, &host_datagrams);
    let mut inputs = InputReceiver::start(host_conn.clone(), &host_datagrams);
    let viewer_datagrams = Datagrams::start(viewer_conn.clone());
    let _viewer_video = ViewerVideo::start(
        viewer_conn.clone(),
        std::sync::Arc::new(couch_media::clock::ProcessClock::new()),
    );
    let mut rumbles = viewer_datagrams.subscribe(datagram::RUMBLE);
    let sender = InputSender::start(viewer_conn.clone());

    let now = Instant::now();
    let mut router = SeatRouter::new(1);
    router.set_foreground(true);
    router.set_pointer_inside(true);
    let Some(&Effect::Granted {
        epoch, permissions, ..
    }) = router.grant(SEAT, member, Permissions::all(), now).last()
    else {
        panic!("no grant");
    };

    let mut forwarder = Forwarder::new();
    let mut out = forwarder.seat_assigned(SEAT, epoch, permissions, now);
    for input in [
        LocalInput::Pad(PadSnapshot {
            buttons: PadButtons::A.bits(),
            ..Default::default()
        }),
        LocalInput::Key {
            usage: 0x1A,
            down: true,
        },
        LocalInput::MouseMotion { dx: 7, dy: -2 },
        LocalInput::MouseButton {
            button: 1,
            down: true,
        },
    ] {
        out.extend(forwarder.local(input, now));
    }
    for message in out {
        sender.send(message);
    }

    let mut effects = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while effects.len() < 4 {
        let input = tokio::time::timeout_at(deadline, inputs.inputs.recv())
            .await
            .expect("input arrives")
            .expect("channel open");
        effects.extend(router.handle(member, input, Instant::now()));
    }
    // Datagrams and the stream may interleave; each effect arrives once.
    for expected in [
        Effect::Pad(
            SEAT,
            PadState {
                buttons: PadButtons::A,
                ..PadState::NEUTRAL
            },
        ),
        Effect::Key(HidUsage(0x1A), true),
        Effect::MouseMove(7, -2),
        Effect::MouseButton(MouseButton::Left, true),
    ] {
        assert_eq!(
            effects.iter().filter(|e| **e == expected).count(),
            1,
            "{expected:?} in {effects:?}"
        );
    }

    send_rumble(
        &host_conn,
        RumbleDatagram {
            seat: SEAT,
            low: 200,
            high: 10,
        },
    );
    let payload = tokio::time::timeout(Duration::from_secs(5), rumbles.recv())
        .await
        .expect("rumble arrives")
        .expect("channel open");
    assert_eq!(
        decode_payload::<RumbleDatagram>(&payload),
        Some(RumbleDatagram {
            seat: SEAT,
            low: 200,
            high: 10
        })
    );
}
