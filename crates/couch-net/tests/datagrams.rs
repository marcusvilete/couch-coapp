//! The M1 check that video doesn't hold audio up: datagrams (audio) sent while
//! a 1 MB keyframe is in flight on a stream gain at most 2 ms of delay. noq
//! puts datagrams in each packet ahead of stream data, so a datagram waits at
//! most for the next packet the congestion window and the pacer allow.
//!
//! Timing-sensitive, so ignored by default. Run it optimized:
//! `cargo test --release -p couch-net --features netem --test datagrams -- --ignored --nocapture`

mod common;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use common::{bind, connect};
use couch_net::{
    SecretKey,
    endpoint::{EndpointOptions, RelayChoice},
};

/// About one Opus packet: 10 ms at 96 kbit/s plus our header.
const AUDIO_BYTES: usize = 160;
const KEYFRAME_BYTES: usize = 1 << 20;

/// Datagram phases, in the first payload byte.
const WARM_UP: u8 = 0;
const IDLE: u8 = 1;
const KEYFRAME: u8 = 2;
const DONE: u8 = 3;

/// Phase, then the send time in microseconds since `start`. Both endpoints
/// live in this process, so they share the clock.
fn datagram(start: Instant, phase: u8) -> Bytes {
    let mut d = vec![0u8; AUDIO_BYTES];
    d[0] = phase;
    d[1..9].copy_from_slice(&(start.elapsed().as_micros() as u64).to_be_bytes());
    d.into()
}

/// Waits without the coarse OS timer (15.6 ms by default on Windows).
async fn pause(d: Duration) {
    let until = Instant::now() + d;
    while Instant::now() < until {
        tokio::task::yield_now().await;
    }
}

fn percentile(v: &mut [u64], p: f64) -> u64 {
    v.sort_unstable();
    v[((v.len() - 1) as f64 * p / 100.0).round() as usize]
}

struct Measurement {
    idle_p50: u64,
    p50: u64,
    p95: u64,
    max: u64,
    n: usize,
    keyframe_took: Duration,
}

impl std::fmt::Display for Measurement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "audio alone p50 {} µs; during a {} KB keyframe ({:.1} ms) p50 {} p95 {} max {} µs (n={})",
            self.idle_p50,
            KEYFRAME_BYTES / 1024,
            self.keyframe_took.as_secs_f64() * 1000.0,
            self.p50,
            self.p95,
            self.max,
            self.n
        )
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "timing-sensitive: run optimized with --ignored"]
async fn audio_datagrams_overtake_a_keyframe() {
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
    let addr = host.addr();
    let m = measure(&host, &viewer, addr).await;
    println!("{m}");
    assert!(
        m.p95 <= m.idle_p50 + 2_000,
        "audio gained more than 2 ms behind the keyframe"
    );
}

/// Not a pass/fail check: shows what a slower link adds. Audio shares the
/// link's queue with the keyframe, so it waits for what is queued ahead of it.
#[cfg(feature = "netem")]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "timing-sensitive: run optimized with --ignored"]
async fn audio_behind_a_keyframe_on_a_25_mbit_link() {
    use couch_net::netem::{self, NetemTransport};

    let host_link = NetemTransport::new(
        "127.0.0.1:0".parse().unwrap(),
        "rate=25mbit".parse().unwrap(),
    );
    let viewer_link =
        NetemTransport::new("127.0.0.1:0".parse().unwrap(), "delay=0ms".parse().unwrap());
    let mut host_opts = EndpointOptions::new(SecretKey::generate(), RelayChoice::Disabled);
    host_opts.netem = Some(host_link.clone());
    let mut viewer_opts = EndpointOptions::new(SecretKey::generate(), RelayChoice::Disabled);
    viewer_opts.netem = Some(viewer_link);
    let host = bind(host_opts).await;
    let viewer = bind(viewer_opts).await;
    let addr = netem::endpoint_addr(host.id(), host_link.local_addr().expect("bound"));
    println!("{}", measure(&host, &viewer, addr).await);
}

async fn measure(
    host: &couch_net::Endpoint,
    viewer: &couch_net::Endpoint,
    addr: couch_net::EndpointAddr,
) -> Measurement {
    let (host_conn, viewer_conn) = connect(host, viewer, addr).await;
    let start = Instant::now();

    // Viewer: drain video streams, and note when the keyframe is all in.
    let keyframe_in = Arc::new(AtomicBool::new(false));
    {
        let conn = viewer_conn.clone();
        let keyframe_in = keyframe_in.clone();
        tokio::spawn(async move {
            while let Ok(mut recv) = conn.accept_uni().await {
                let keyframe_in = keyframe_in.clone();
                tokio::spawn(async move {
                    let data = recv.read_to_end(usize::MAX).await.expect("stream");
                    if data.len() == KEYFRAME_BYTES {
                        keyframe_in.store(true, Ordering::SeqCst);
                    }
                });
            }
        });
    }
    // Viewer: each datagram's one-way delay, by phase.
    let delays = {
        let conn = viewer_conn.clone();
        tokio::spawn(async move {
            let mut delays: [Vec<u64>; 3] = Default::default();
            while let Ok(d) = conn.read_datagram().await {
                let sent = u64::from_be_bytes(d[1..9].try_into().expect("8 bytes"));
                let delay = (start.elapsed().as_micros() as u64).saturating_sub(sent);
                match d[0] {
                    DONE => break,
                    phase => delays[usize::from(phase)].push(delay),
                }
            }
            delays
        })
    };

    // Warm up like a running stream: a second of 10 Mbit/s video at 60 fps,
    // with audio every 10 ms.
    let mut stream = host_conn.open_uni().await.expect("stream");
    let frame = vec![7u8; 20_000];
    let warm_up_until = Instant::now() + Duration::from_secs(1);
    let mut next_frame = Instant::now();
    while Instant::now() < warm_up_until {
        if Instant::now() >= next_frame {
            stream.write_all(&frame).await.expect("write");
            next_frame += Duration::from_micros(16_667);
        }
        host_conn
            .send_datagram(datagram(start, WARM_UP))
            .expect("datagram");
        pause(Duration::from_millis(10)).await;
    }
    stream.finish().expect("finish");

    // Audio alone.
    for _ in 0..50 {
        host_conn
            .send_datagram(datagram(start, IDLE))
            .expect("datagram");
        pause(Duration::from_millis(10)).await;
    }

    // Audio, every millisecond, while a 1 MB keyframe goes out.
    let keyframe_started = Instant::now();
    {
        let conn = host_conn.clone();
        tokio::spawn(async move {
            let mut stream = conn.open_uni().await.expect("stream");
            stream
                .write_all(&vec![1u8; KEYFRAME_BYTES])
                .await
                .expect("write");
            stream.finish().expect("finish");
        });
    }
    while !keyframe_in.load(Ordering::SeqCst) {
        assert!(
            keyframe_started.elapsed() < Duration::from_secs(10),
            "the keyframe never arrived"
        );
        host_conn
            .send_datagram(datagram(start, KEYFRAME))
            .expect("datagram");
        pause(Duration::from_millis(1)).await;
    }
    let keyframe_took = keyframe_started.elapsed();
    pause(Duration::from_millis(50)).await;
    host_conn
        .send_datagram(datagram(start, DONE))
        .expect("datagram");

    let [_, mut idle, mut during] = tokio::time::timeout(Duration::from_secs(5), delays)
        .await
        .expect("datagrams end")
        .expect("reader");
    assert!(
        idle.len() >= 40 && during.len() >= 5,
        "idle {} during {}",
        idle.len(),
        during.len()
    );
    Measurement {
        idle_p50: percentile(&mut idle, 50.0),
        p50: percentile(&mut during, 50.0),
        p95: percentile(&mut during, 95.0),
        max: percentile(&mut during, 100.0),
        n: during.len(),
        keyframe_took,
    }
}
