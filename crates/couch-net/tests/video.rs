//! Two real endpoints on loopback: the control handshake, epoch streams and
//! receiver reports end to end.

mod common;

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use common::{bind, connect};
use couch_media::{Codec, EncodedFrame, Micros, VideoParams, VideoSender, clock::ProcessClock};
use couch_net::{
    SecretKey,
    datagrams::Datagrams,
    endpoint::{EndpointOptions, RelayChoice},
    streams::Streams,
    video::{HostVideo, ViewerVideo},
};
use couch_proto::media::FrameRef;

const PARAMS: VideoParams = VideoParams {
    codec: Codec::H264,
    width: 1280,
    height: 720,
    fps: 60,
};

fn frame(i: u32, keyframe: bool) -> EncodedFrame {
    EncodedFrame {
        data: Bytes::from(vec![i as u8; 20_000 + i as usize]),
        keyframe,
        captured_at: Micros(u64::from(i)),
        encode_time_us: 1_000,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn frames_arrive_in_order_and_reports_clear_the_backlog() {
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

    let datagrams = Datagrams::start(host_conn.clone());
    let mut video = HostVideo::start(
        host_conn,
        7,
        &datagrams,
        std::sync::Arc::new(couch_media::clock::ProcessClock::new()),
    );
    let streams = Streams::start(viewer_conn.clone());
    let mut received = ViewerVideo::start(
        viewer_conn,
        &streams,
        Some(7),
        Arc::new(ProcessClock::new()),
    );

    video.start_epoch(&PARAMS, None).unwrap();
    for i in 0..20 {
        video.send_frame(&frame(i, i == 0)).unwrap();
    }
    let mut first_epoch = Vec::new();
    while first_epoch.len() < 20 {
        let f = tokio::time::timeout(Duration::from_secs(5), received.frames.recv())
            .await
            .expect("frames keep coming")
            .expect("channel open");
        first_epoch.push(f);
    }
    assert!(first_epoch.iter().all(|f| f.epoch == 1));
    let seqs: Vec<u32> = first_epoch.iter().map(|f| f.header.seq).collect();
    assert_eq!(seqs, (0..20).collect::<Vec<_>>(), "in order, none missing");
    assert_eq!(first_epoch[5].data.len(), 20_005, "payload intact");
    assert!(first_epoch[0].header.keyframe);

    // A new epoch starts at seq 0 and the old stream is abandoned.
    video.start_epoch(&PARAMS, None).unwrap();
    for i in 0..5 {
        video.send_frame(&frame(i, i == 0)).unwrap();
    }
    let mut second_epoch = Vec::new();
    while second_epoch.len() < 5 {
        let f = tokio::time::timeout(Duration::from_secs(5), received.frames.recv())
            .await
            .expect("frames keep coming")
            .expect("channel open");
        if f.epoch == 2 {
            second_epoch.push(f.header.seq);
        }
    }
    assert_eq!(second_epoch, vec![0, 1, 2, 3, 4]);

    // Receiver reports eventually confirm everything in the epoch arrived.
    let mut outstanding = u64::MAX;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        outstanding = video.feedback().outstanding_bytes;
        if outstanding == 0 {
            break;
        }
    }
    assert_eq!(outstanding, 0, "reports confirm the epoch was delivered");
    let fb = video.feedback();
    assert!(fb.rtt_us > 0 && !fb.relayed, "{fb:?}");
}

/// M12d-2: the host learns which frames the viewer received whole, and an
/// epoch continuing from one of them gets there after every frame of the old
/// epoch up to it, naming it.
#[tokio::test(flavor = "multi_thread")]
async fn a_continuing_epoch_follows_the_frame_it_continues_from() {
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
    let datagrams = Datagrams::start(host_conn.clone());
    let mut video = HostVideo::start(host_conn, 7, &datagrams, Arc::new(ProcessClock::new()));
    let streams = Streams::start(viewer_conn.clone());
    let mut received = ViewerVideo::start(
        viewer_conn,
        &streams,
        Some(7),
        Arc::new(ProcessClock::new()),
    );

    assert_eq!(video.next_frame(), None, "no epoch yet");
    video.start_epoch(&PARAMS, None).unwrap();
    assert_eq!(video.next_frame(), Some(FrameRef { epoch: 1, seq: 0 }));
    for i in 0..10 {
        video.send_frame(&frame(i, i == 0)).unwrap();
    }
    // The reports say all ten arrived.
    let mut got = None;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        got = video.received();
        if got == Some(FrameRef { epoch: 1, seq: 9 }) {
            break;
        }
    }
    assert_eq!(got, Some(FrameRef { epoch: 1, seq: 9 }));

    // Five more, received too, then an epoch continuing from the third of
    // them (a viewer that fell behind decoding names an older frame than
    // the newest it received).
    for i in 10..15 {
        video.send_frame(&frame(i, false)).unwrap();
    }
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if video.received() == Some(FrameRef { epoch: 1, seq: 14 }) {
            break;
        }
    }
    assert_eq!(video.received(), Some(FrameRef { epoch: 1, seq: 14 }));
    let from = FrameRef { epoch: 1, seq: 12 };
    video.start_epoch(&PARAMS, Some(from)).unwrap();
    // Until the viewer reports the new epoch, what it holds is the old one's.
    assert!(video.received().is_some_and(|r| r.epoch == 1));
    for i in 0..5 {
        video.send_frame(&frame(100 + i, false)).unwrap();
    }

    let mut order = Vec::new();
    while order.last().is_none_or(|&(e, s, _)| (e, s) != (2, 4)) {
        let f = tokio::time::timeout(Duration::from_secs(5), received.frames.recv())
            .await
            .expect("frames keep coming")
            .expect("channel open");
        order.push((f.epoch, f.header.seq, f.continues));
    }
    let first_new = order.iter().position(|&(e, ..)| e == 2).unwrap();
    assert_eq!(order[first_new].2, Some(from), "it names what it continues");
    assert!(
        order[first_new + 1..]
            .iter()
            .all(|&(e, _, c)| e == 2 && c.is_none()),
        "nothing of the old epoch after it: {order:?}"
    );
    let old: Vec<u32> = order[..first_new].iter().map(|&(_, s, _)| s).collect();
    assert!(
        old.len() >= 13 && old[..13] == (0..13).collect::<Vec<_>>()[..],
        "every frame up to the one it continues from, in order: {old:?}"
    );
    assert!(received.wanted(1, 12) && !received.wanted(1, 13));

    // The viewer's reports of the new epoch move what it holds on.
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if video.received() == Some(FrameRef { epoch: 2, seq: 4 }) {
            return;
        }
    }
    panic!("received stayed at {:?}", video.received());
}

#[cfg(feature = "netem")]
#[tokio::test(flavor = "multi_thread")]
async fn netem_delay_shows_up_in_the_path_rtt() {
    use couch_net::netem::{self, NetemConfig, NetemTransport};

    let config: NetemConfig = "delay=30ms".parse().unwrap();
    let host_link = NetemTransport::new("127.0.0.1:0".parse().unwrap(), config.clone());
    let viewer_link = NetemTransport::new("127.0.0.1:0".parse().unwrap(), config);
    let mut host_opts = EndpointOptions::new(SecretKey::generate(), RelayChoice::Disabled);
    host_opts.netem = Some(host_link.clone());
    let mut viewer_opts = EndpointOptions::new(SecretKey::generate(), RelayChoice::Disabled);
    viewer_opts.netem = Some(viewer_link);
    let host = bind(host_opts).await;
    let viewer = bind(viewer_opts).await;

    let addr = netem::endpoint_addr(host.id(), host_link.local_addr().expect("bound"));
    let (host_conn, _viewer_conn) = connect(&host, &viewer, addr).await;
    // Let a few packets go back and forth to settle the estimate.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let rtt = couch_net::video::selected_path(&host_conn).rtt;
    assert!(
        rtt >= Duration::from_millis(55) && rtt < Duration::from_millis(120),
        "30 ms each way should give ~60 ms RTT, got {rtt:?}"
    );
}
