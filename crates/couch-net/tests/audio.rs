//! Audio end to end on loopback: datagrams from the host arrive intact at
//! the viewer's audio subscriber, with video's datagram traffic alongside.

mod common;

use std::time::Duration;

use common::{bind, connect};
use couch_net::{
    SecretKey,
    audio::{receive_audio, send_audio},
    datagrams::Datagrams,
    endpoint::{EndpointOptions, RelayChoice},
    video::{HostVideo, ViewerVideo},
};
use couch_proto::media::AudioDatagram;

#[tokio::test(flavor = "multi_thread")]
async fn audio_datagrams_reach_the_viewer() {
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

    // Receiver reports flow viewer to host meanwhile; neither side may steal
    // the other's datagrams.
    let host_datagrams = Datagrams::start(host_conn.clone());
    let _video = HostVideo::start(host_conn.clone(), 1, &host_datagrams);
    let viewer_datagrams = Datagrams::start(viewer_conn.clone());
    let _viewer_video = ViewerVideo::start(
        viewer_conn.clone(),
        std::sync::Arc::new(couch_media::clock::ProcessClock::new()),
    );
    let mut audio = receive_audio(&viewer_datagrams);

    let sent: Vec<AudioDatagram> = (0..50u32)
        .map(|seq| AudioDatagram {
            seq,
            capture_us: 1_000_000 + u64::from(seq) * 10_000,
            // About one 96 kbit/s Opus packet.
            packet: vec![seq as u8; 120],
        })
        .collect();
    for packet in &sent {
        assert!(send_audio(&host_conn, packet));
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    let mut received = Vec::new();
    while received.len() < sent.len() {
        let packet = tokio::time::timeout(Duration::from_secs(5), audio.recv())
            .await
            .expect("audio arrives")
            .expect("channel open");
        received.push(packet);
    }
    // Loopback loses nothing; datagrams may in principle reorder.
    received.sort_by_key(|p| p.seq);
    assert_eq!(received, sent);
}
