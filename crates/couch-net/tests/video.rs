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
    video::{HostVideo, ViewerVideo},
};

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
    let mut video = HostVideo::start(host_conn, 7, &datagrams);
    let mut received = ViewerVideo::start(viewer_conn, Arc::new(ProcessClock::new()));

    video.start_epoch(&PARAMS).unwrap();
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
    video.start_epoch(&PARAMS).unwrap();
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
