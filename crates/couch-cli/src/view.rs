//! `couch-cli view <ticket>`: connect to a host, show the stream in
//! `couch-viewer` (through `couch_engine::pipeline::watch`, the app's own
//! viewer side), and measure latency. With `--measure-secs` it stops after
//! that long and prints a `RESULT {json}` line for `latency-probe`.

use std::time::Duration;

use anyhow::Context;
use couch_engine::pipeline::{
    ViewerOptions, platform_backend,
    watch::{Watch, WatchEvent, WatchOptions},
};
use couch_net::{NetError, admission, control, datagrams::Datagrams, endpoint, streams::Streams};
use couch_proto::{
    control::{ALIVE_EVERY, Alive, ControlMessage, KeyframeReason, KeyframeRequest, close_code},
    ipc::{AudioStats, FrameTiming, NetStats},
};
use iroh_tickets::endpoint::EndpointTicket;
use tokio::sync::mpsc;

use crate::{net_args::NetArgs, stats::Samples};

#[derive(Debug, Clone)]
pub struct Args {
    pub ticket: String,
    pub measure_secs: Option<u64>,
    pub viewer_pos: Option<(i32, i32)>,
    pub viewer_size: Option<(u32, u32)>,
    pub hide_hud: bool,
    /// Write every presented frame's timings here.
    pub timings_csv: Option<std::path::PathBuf>,
    /// Start the viewer window again if it crashes (`lifecycle-check`).
    pub restart_viewer: bool,
    pub net: NetArgs,
}

#[derive(Default)]
struct Metrics {
    frames: u64,
    clock_to_present: Samples,
    capture_to_present: Samples,
    arrive_to_present: Samples,
    /// Between the capture times of consecutive frames shown: how even the
    /// host's pacing is (M9d), skipped frames included.
    frame_gap: Samples,
    last_capture_us: Option<u64>,
}

impl Metrics {
    fn record(&mut self, t: &FrameTiming) {
        let ms = |later: u64, earlier: u64| later.saturating_sub(earlier) as f64 / 1000.0;
        self.frames += 1;
        if let Some(last) = self.last_capture_us.replace(t.capture_us)
            && t.capture_us > last
        {
            self.frame_gap.push(ms(t.capture_us, last));
        }
        self.capture_to_present
            .push(ms(t.presented_us, t.capture_us));
        self.arrive_to_present
            .push(ms(t.presented_us, t.arrived_us));
        if t.barcode_us != 0 {
            self.clock_to_present.push(ms(t.presented_us, t.barcode_us));
        }
    }
}

#[derive(Default)]
struct Latency {
    all: Metrics,
    /// Since the last per-second line.
    window: Metrics,
    first_present_us: Option<u64>,
    last_present_us: u64,
    csv: Option<std::io::BufWriter<std::fs::File>>,
}

impl Latency {
    fn record(&mut self, t: &FrameTiming) {
        self.all.record(t);
        self.window.record(t);
        self.first_present_us.get_or_insert(t.presented_us);
        self.last_present_us = t.presented_us;
        if let Some(csv) = self.csv.as_mut() {
            use std::io::Write;
            let _ = writeln!(
                csv,
                "{},{},{},{},{},{},{}",
                t.presented_us,
                t.barcode_us,
                t.capture_us,
                t.arrived_us,
                t.decoded_us,
                t.epoch,
                t.seq
            );
        }
    }

    fn fps(&self) -> f64 {
        match self.first_present_us {
            Some(first) if self.last_present_us > first => {
                (self.all.frames - 1) as f64 * 1e6 / (self.last_present_us - first) as f64
            }
            _ => 0.0,
        }
    }
}

fn percentiles(s: &Samples) -> serde_json::Value {
    serde_json::json!({
        "p50": s.percentile(50.0),
        "p95": s.percentile(95.0),
        "max": s.percentile(100.0),
        "n": s.len(),
    })
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    let ticket: EndpointTicket = args.ticket.parse().context("parsing the ticket")?;
    let (options, _netem) = args.net.options()?;
    let endpoint = endpoint::bind(options).await?;
    let conn = endpoint
        .connect(ticket.endpoint_addr().clone(), couch_proto::ALPN)
        .await
        .context("connecting to the host")?;
    let (mut control_tx, mut control_rx, hello) = control::open(&conn).await?;
    println!(
        "connected to {} (app {})",
        conn.remote_id().fmt_short(),
        hello.app_version
    );
    let backend = platform_backend();
    let clock = backend.clock();
    let (seat_tx, mut seat_rx) = mpsc::unbounded_channel();
    let control_conn = conn.clone();
    tokio::spawn(async move {
        loop {
            match control_rx.recv().await {
                Ok(Some(message)) => {
                    if matches!(
                        message,
                        ControlMessage::SeatAssigned(_) | ControlMessage::SeatReleased(_)
                    ) && seat_tx.send(message).is_err()
                    {
                        return;
                    }
                }
                Err(NetError::TimedOut) => {
                    println!("the host went silent");
                    admission::close(&control_conn, close_code::TIMED_OUT);
                    return;
                }
                Ok(None) | Err(_) => return,
            }
        }
    });

    // Subscribed before asking for the stream.
    let datagrams = Datagrams::start(conn.clone());
    let streams = Streams::start(conn.clone());
    let (watch, mut events) = Watch::start(
        conn.clone(),
        &datagrams,
        &streams,
        backend.clone(),
        WatchOptions {
            session: None,
            viewer: ViewerOptions {
                position: args.viewer_pos,
                size: args.viewer_size,
                hide_hud: args.hide_hud,
                hide_bar: false,
                hide_stats: false,
                log: None,
            },
            measure: true,
            // Off by default: `release-check` kills the window on purpose and
            // expects the connection to close.
            restart_viewer: args.restart_viewer,
            // The default keys, at full volume (the audio checks measure it).
            shortcuts: Default::default(),
            volume: Default::default(),
        },
    )?;
    control_tx
        .send(&ControlMessage::KeyframeRequest(KeyframeRequest {
            epoch: 0,
            reason: KeyframeReason::Start,
        }))
        .await?;

    let mut latency = Latency::default();
    if let Some(path) = &args.timings_csv {
        use std::io::Write;
        let mut csv = std::io::BufWriter::new(
            std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?,
        );
        writeln!(
            csv,
            "presented_us,barcode_us,capture_us,arrived_us,decoded_us,epoch,seq"
        )?;
        latency.csv = Some(csv);
    }
    let mut audio_stats = AudioStats::default();

    let deadline = async {
        match args.measure_secs {
            Some(secs) => tokio::time::sleep(Duration::from_secs(secs)).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(deadline);
    let mut report = tokio::time::interval(Duration::from_secs(1));
    let mut alive = tokio::time::interval(ALIVE_EVERY);
    let mut net: Option<NetStats> = None;
    // Received video, four samples a second, for G7.
    let mut receive_mbps = Samples::default();
    // For the soak test (G2): both should stay 0.
    let mut decode_errors = 0u32;
    let mut path_changes = 0u32;
    // Anything but the measurement deadline ending the loop is a failure.
    let mut ended_early = true;

    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(WatchEvent::Timing(t)) => latency.record(&t),
                Some(WatchEvent::AudioStats(stats)) => audio_stats = stats,
                Some(WatchEvent::KeyframeNeeded { epoch }) => {
                    decode_errors += 1;
                    control_tx
                        .send(&ControlMessage::KeyframeRequest(KeyframeRequest {
                            epoch,
                            reason: KeyframeReason::DecoderError,
                        }))
                        .await?;
                }
                Some(WatchEvent::Net(stats)) => {
                    receive_mbps.push(f64::from(stats.receive_bps) / 1e6);
                    if net.is_some_and(|n| n.relayed != stats.relayed) {
                        path_changes += 1;
                        println!("path changed: now {}", if stats.relayed { "RELAY" } else { "direct" });
                    }
                    net = Some(stats);
                }
                Some(WatchEvent::Rumble(r)) => {
                    println!("rumble from the host: seat {} low {} high {}", r.seat, r.low, r.high);
                }
                // `couch-cli host` seats each viewer once, by its connection.
                Some(WatchEvent::Controllers(report)) => {
                    let names: Vec<&str> =
                        report.controllers.iter().map(|c| c.name.as_str()).collect();
                    println!("the viewer has {} controller(s): {names:?}", names.len());
                }
                Some(WatchEvent::Volume(v)) => {
                    println!("stream volume {}%{}", v.percent, if v.muted { ", muted" } else { "" });
                }
                Some(WatchEvent::Window(w)) => {
                    println!("window: full screen {}, bar {}, stats {}", w.fullscreen, w.bar, w.stats);
                }
                Some(WatchEvent::Action(action)) => println!("the viewer asked to {action:?}; no session here"),
                Some(WatchEvent::ViewerRestarted) => {
                    println!("EVENT viewer-restarted {}", clock.now().0);
                    println!("the viewer window was started again");
                }
                Some(WatchEvent::ViewerCrashed) => {
                    println!("EVENT viewer-crashed {}", clock.now().0);
                    println!("the viewer window crashed");
                    break;
                }
                Some(WatchEvent::ViewerClosed) | None => {
                    println!("EVENT viewer-closed {}", clock.now().0);
                    println!("the viewer window closed");
                    break;
                }
                Some(WatchEvent::ConnectionLost { .. }) => {
                    // For `lifecycle-check`, on the host's clock (QPC).
                    println!("EVENT connection-lost {}", clock.now().0);
                    println!("connection closed");
                    break;
                }
            },
            Some(message) = seat_rx.recv() => match message {
                ControlMessage::SeatAssigned(s) => {
                    println!("you control seat {} ({:?})", s.seat.0, s.permissions);
                    watch.seat_assigned(s);
                }
                ControlMessage::SeatReleased(s) => {
                    println!("seat {} released", s.seat.0);
                    watch.seat_released(s);
                }
                _ => {}
            },
            _ = alive.tick() => {
                // A closed connection is reported by the watch, below; an
                // error here must not end the run first.
                let _ = control_tx.send(&ControlMessage::Alive(Alive {})).await;
            }
            _ = report.tick() => {
                let w = std::mem::take(&mut latency.window);
                let p = |s: &Samples, q| s.percentile(q).map_or("   -".into(), |v| format!("{v:4.1}"));
                let a = audio_stats;
                println!(
                    "{:3} fps  clock->present p50 {} p95 {} ms  arrive->present p50 {} ms  {} {:.1} Mbit/s  \
                     audio buffer {:.0}/{:.0} ms  concealed {}  late {}  underruns {}",
                    w.frames,
                    p(&w.clock_to_present, 50.0),
                    p(&w.clock_to_present, 95.0),
                    p(&w.arrive_to_present, 50.0),
                    if net.is_some_and(|n| n.relayed) { "RELAY" } else { "direct" },
                    f64::from(net.map_or(0, |n| n.receive_bps)) / 1e6,
                    f64::from(a.buffered_us) / 1000.0,
                    f64::from(a.target_us) / 1000.0,
                    a.concealed,
                    a.late,
                    a.underruns,
                );
            }
            () = &mut deadline => {
                ended_early = false;
                break;
            }
        }
    }

    let path = couch_net::video::selected_path(&conn);
    if let Some(mut csv) = latency.csv.take() {
        use std::io::Write;
        csv.flush()?;
    }
    println!(
        "clock -> present     {}",
        latency.all.clock_to_present.summary_ms()
    );
    println!(
        "capture -> present   {}",
        latency.all.capture_to_present.summary_ms()
    );
    println!(
        "arrive -> present    {}",
        latency.all.arrive_to_present.summary_ms()
    );
    println!(
        "frame gap            {}",
        latency.all.frame_gap.summary_ms()
    );
    if args.measure_secs.is_some() {
        let a = audio_stats;
        let result = serde_json::json!({
            "audio": {
                "played": a.played,
                "concealed": a.concealed,
                "late": a.late,
                "dropped": a.dropped,
                "underruns": a.underruns,
                "buffered_ms": f64::from(a.buffered_us) / 1000.0,
                "target_ms": f64::from(a.target_us) / 1000.0,
                "capture_to_queue_ms": f64::from(a.capture_to_queue_us) / 1000.0,
                "capture_to_queue_p95_ms": f64::from(a.capture_to_queue_p95_us) / 1000.0,
            },
            "frames": latency.all.frames,
            "fps": latency.fps(),
            "clock_to_present_ms": percentiles(&latency.all.clock_to_present),
            "capture_to_present_ms": percentiles(&latency.all.capture_to_present),
            "arrive_to_present_ms": percentiles(&latency.all.arrive_to_present),
            "frame_gap_ms": percentiles(&latency.all.frame_gap),
            "receive_mbps": percentiles(&receive_mbps),
            "rtt_ms": path.rtt.as_secs_f64() * 1000.0,
            "relayed": path.relayed,
            "lost_packets": path.lost_packets,
            "decode_errors": decode_errors,
            "path_changes": path_changes,
            "ended_early": ended_early,
        });
        println!("RESULT {result}");
    }
    watch.stop().await;
    conn.close(0u32.into(), b"bye");
    endpoint.close().await;
    Ok(())
}
