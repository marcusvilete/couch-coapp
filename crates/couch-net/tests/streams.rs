//! One room connection carries what sessions need: video and input in both
//! directions at once, with one stream acceptor and one datagram reader per
//! side; epochs of another session are refused; and watching again on the
//! same connection works.

mod common;

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use common::{bind, connect};
use couch_input::{forward::Outgoing, router::Input};
use couch_media::{Codec, EncodedFrame, Micros, VideoParams, VideoSender, clock::ProcessClock};
use couch_net::{
    Connection, Endpoint, SecretKey,
    datagrams::Datagrams,
    endpoint::{EndpointOptions, RelayChoice},
    input::{InputReceiver, InputSender},
    streams::Streams,
    video::{HostVideo, VideoFrameIn, ViewerVideo},
};
use couch_proto::{
    ids::{SeatEpoch, SeatId},
    input::{InputMessage, KeyEvent, PadDatagram, PadSnapshot},
    media::reset_code,
};

const PARAMS: VideoParams = VideoParams {
    codec: Codec::H264,
    width: 640,
    height: 360,
    fps: 30,
};

fn frame(i: u32) -> EncodedFrame {
    EncodedFrame {
        data: Bytes::from(vec![i as u8; 5_000]),
        keyframe: i == 0,
        captured_at: Micros(u64::from(i)),
        encode_time_us: 500,
    }
}

async fn endpoint() -> Endpoint {
    bind(EndpointOptions::new(
        SecretKey::generate(),
        RelayChoice::Disabled,
    ))
    .await
}

/// Both sides of one connection, each with its one acceptor and reader.
struct Side {
    conn: Connection,
    datagrams: Datagrams,
    streams: Streams,
}

impl Side {
    fn new(conn: Connection) -> Self {
        Self {
            datagrams: Datagrams::start(conn.clone()),
            streams: Streams::start(conn.clone()),
            conn,
        }
    }

    fn watch(&self, session: u64) -> ViewerVideo {
        ViewerVideo::start(
            self.conn.clone(),
            &self.streams,
            Some(session),
            Arc::new(ProcessClock::new()),
        )
    }
}

async fn next_frame(video: &mut ViewerVideo) -> VideoFrameIn {
    tokio::time::timeout(Duration::from_secs(5), video.frames.recv())
        .await
        .expect("a frame arrives")
        .expect("channel open")
}

fn key(usage: u16) -> Outgoing {
    Outgoing::Stream(InputMessage::Key(KeyEvent {
        seat: SeatId(0),
        seat_epoch: SeatEpoch(1),
        usage,
        down: true,
    }))
}

fn pad(seq: u32) -> Outgoing {
    Outgoing::Pad(PadDatagram {
        seat: SeatId(0),
        seat_epoch: SeatEpoch(1),
        seq,
        pad: PadSnapshot::default(),
    })
}

/// A key on the input stream and a pad datagram, in either order.
async fn expect_key_and_pad(inputs: &mut InputReceiver, usage: u16) {
    let (mut key_seen, mut pad_seen) = (false, false);
    while !(key_seen && pad_seen) {
        let input = tokio::time::timeout(Duration::from_secs(5), inputs.inputs.recv())
            .await
            .expect("input arrives")
            .expect("channel open");
        match input {
            Input::Key(k) => {
                assert_eq!(k.usage, usage);
                key_seen = true;
            }
            Input::Pad(_) => pad_seen = true,
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn video_and_input_share_one_connection_both_ways() {
    let (a_end, b_end) = (endpoint().await, endpoint().await);
    let (a_conn, b_conn) = connect(&a_end, &b_end, a_end.addr()).await;
    let (a, b) = (Side::new(a_conn), Side::new(b_conn));

    // A streams session 1 to B while B streams session 2 to A, and each
    // sends input for the other's game.
    let mut a_video = HostVideo::start(
        a.conn.clone(),
        1,
        &a.datagrams,
        std::sync::Arc::new(couch_media::clock::ProcessClock::new()),
    );
    let mut b_video = HostVideo::start(
        b.conn.clone(),
        2,
        &b.datagrams,
        std::sync::Arc::new(couch_media::clock::ProcessClock::new()),
    );
    let mut b_watches = b.watch(1);
    let mut a_watches = a.watch(2);
    let mut a_inputs = InputReceiver::start(&a.datagrams, &a.streams);
    let mut b_inputs = InputReceiver::start(&b.datagrams, &b.streams);
    let a_sends = InputSender::start(a.conn.clone());
    let b_sends = InputSender::start(b.conn.clone());

    a_video.start_epoch(&PARAMS).unwrap();
    b_video.start_epoch(&PARAMS).unwrap();
    for i in 0..3 {
        a_video.send_frame(&frame(i)).unwrap();
        b_video.send_frame(&frame(i)).unwrap();
    }
    b_sends.send(key(0x04));
    b_sends.send(pad(1));
    a_sends.send(key(0x05));
    a_sends.send(pad(1));

    for i in 0..3 {
        assert_eq!(next_frame(&mut b_watches).await.header.seq, i);
        assert_eq!(next_frame(&mut a_watches).await.header.seq, i);
    }
    expect_key_and_pad(&mut a_inputs, 0x04).await;
    expect_key_and_pad(&mut b_inputs, 0x05).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn epochs_of_another_session_are_refused_and_watching_again_works() {
    let (host_end, viewer_end) = (endpoint().await, endpoint().await);
    let (host_conn, viewer_conn) = connect(&host_end, &viewer_end, host_end.addr()).await;
    let (host, viewer) = (Side::new(host_conn), Side::new(viewer_conn));

    // Left over from an earlier session: refused by a viewer of session 7.
    let mut stale = HostVideo::start(
        host.conn.clone(),
        6,
        &host.datagrams,
        std::sync::Arc::new(couch_media::clock::ProcessClock::new()),
    );
    let mut watching = viewer.watch(7);
    stale.start_epoch(&PARAMS).unwrap();
    stale.send_frame(&frame(0)).unwrap();
    let nothing = tokio::time::timeout(Duration::from_millis(500), watching.frames.recv()).await;
    assert!(nothing.is_err(), "no frame of session 6 gets through");
    stale.end(reset_code::SESSION_END);

    let mut current = HostVideo::start(
        host.conn.clone(),
        7,
        &host.datagrams,
        std::sync::Arc::new(couch_media::clock::ProcessClock::new()),
    );
    current.start_epoch(&PARAMS).unwrap();
    current.send_frame(&frame(0)).unwrap();
    let first = next_frame(&mut watching).await;
    assert_eq!((first.epoch, first.header.seq), (1, 0));
    current.end(reset_code::VIEWER_REMOVED);

    // Stop watching, then watch a new session on the same connection.
    drop(watching);
    let mut again = viewer.watch(8);
    let mut next = HostVideo::start(
        host.conn.clone(),
        8,
        &host.datagrams,
        std::sync::Arc::new(couch_media::clock::ProcessClock::new()),
    );
    next.start_epoch(&PARAMS).unwrap();
    for i in 0..2 {
        next.send_frame(&frame(i)).unwrap();
    }
    assert_eq!(next_frame(&mut again).await.header.seq, 0);
    assert_eq!(next_frame(&mut again).await.header.seq, 1);

    // Reports reach the new sender only.
    let mut outstanding = u64::MAX;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        outstanding = next.feedback().outstanding_bytes;
        if outstanding == 0 {
            break;
        }
    }
    assert_eq!(
        outstanding, 0,
        "the new viewer's reports reach the new sender"
    );
}
