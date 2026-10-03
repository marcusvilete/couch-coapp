//! `couch-cli view <ticket>`: connect to a host, show the stream in
//! `couch-viewer`, and measure latency. With `--measure-secs` it stops after
//! that long and prints a `RESULT {json}` line for `latency-probe`.

use std::{
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::Context;
use couch_input::{Permissions, forward::Forwarder};
use couch_net::{control, datagrams::Datagrams, endpoint, input::InputSender, video::ViewerVideo};
use couch_platform_win::clock::QpcClock;
use couch_proto::{
    control::{ControlMessage, KeyframeReason, KeyframeRequest},
    input::RumbleDatagram,
    ipc::{
        self, AudioPacket, AudioStats, FrameTiming, LocalInput, NetStats, RumbleState, SeatState,
        VideoFrameHeader,
    },
    media::{datagram, decode_payload},
};
use iroh_tickets::endpoint::EndpointTicket;
use tokio::{process::Command, sync::mpsc};

use crate::{ipc_async, net_args::NetArgs, stats::Samples};

#[derive(Debug, Clone)]
pub struct Args {
    pub ticket: String,
    pub measure_secs: Option<u64>,
    pub viewer_pos: Option<(i32, i32)>,
    pub viewer_size: Option<(u32, u32)>,
    pub hide_hud: bool,
    /// Write every presented frame's timings here.
    pub timings_csv: Option<std::path::PathBuf>,
    pub net: NetArgs,
}

#[derive(Default)]
struct Metrics {
    frames: u64,
    clock_to_present: Samples,
    capture_to_present: Samples,
    arrive_to_present: Samples,
}

impl Metrics {
    fn record(&mut self, t: &FrameTiming) {
        let ms = |later: u64, earlier: u64| later.saturating_sub(earlier) as f64 / 1000.0;
        self.frames += 1;
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
    let (seat_tx, mut seat_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok(Some(message)) = control_rx.recv().await {
            if matches!(
                message,
                ControlMessage::SeatAssigned(_) | ControlMessage::SeatReleased(_)
            ) && seat_tx.send(message).is_err()
            {
                return;
            }
        }
    });
    let datagrams = Datagrams::start(conn.clone());
    let mut rumbles = datagrams.subscribe(datagram::RUMBLE);
    let mut audio = couch_net::audio::receive_audio(&datagrams);
    let input_out = InputSender::start(conn.clone());
    let mut forwarder = Forwarder::new();
    control_tx
        .send(&ControlMessage::KeyframeRequest(KeyframeRequest {
            epoch: 0,
            reason: KeyframeReason::Start,
        }))
        .await?;

    let mut video = ViewerVideo::start(conn.clone(), Arc::new(QpcClock));

    let viewer_exe = std::env::current_exe()?
        .with_file_name(format!("couch-viewer{}", std::env::consts::EXE_SUFFIX));
    let mut command = Command::new(&viewer_exe);
    if let Some((x, y)) = args.viewer_pos {
        command.args(["--pos", &x.to_string(), &y.to_string()]);
    }
    if let Some((w, h)) = args.viewer_size {
        command.args(["--size", &w.to_string(), &h.to_string()]);
    }
    if args.hide_hud {
        command.arg("--no-hud");
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {}", viewer_exe.display()))?;
    let mut viewer_in = child.stdin.take().expect("piped");
    let mut viewer_out = child.stdout.take().expect("piped");

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
    let latency = Arc::new(Mutex::new(latency));
    let audio_stats = Arc::new(Mutex::new(AudioStats::default()));
    let (keyframe_tx, mut keyframe_rx) = mpsc::unbounded_channel();
    let (local_tx, mut local_rx) = mpsc::unbounded_channel::<LocalInput>();
    {
        let latency = latency.clone();
        let audio_stats = audio_stats.clone();
        tokio::spawn(async move {
            while let Ok(Some(frame)) = ipc_async::read_frame(&mut viewer_out).await {
                match frame.kind {
                    ipc::kinds::FRAME_TIMING => {
                        if let Some(t) = FrameTiming::read(&frame.payload) {
                            latency.lock().unwrap().record(&t);
                        }
                    }
                    ipc::kinds::AUDIO_STATS => {
                        if let Some(stats) = ipc::from_payload(&frame.payload) {
                            *audio_stats.lock().unwrap() = stats;
                        }
                    }
                    ipc::kinds::KEYFRAME_REQUEST => {
                        let _ = keyframe_tx.send(());
                    }
                    ipc::kinds::INPUT => {
                        if let Some(input) = ipc::from_payload(&frame.payload) {
                            let _ = local_tx.send(input);
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    let mut scratch = Vec::new();
    let mut payload = Vec::new();
    let hello_payload = ipc::IPC_VERSION.to_be_bytes();
    ipc_async::write_frame(
        &mut viewer_in,
        ipc::kinds::HELLO,
        &hello_payload,
        &mut scratch,
    )
    .await?;

    let deadline = async {
        match args.measure_secs {
            Some(secs) => tokio::time::sleep(Duration::from_secs(secs)).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(deadline);
    let mut report = tokio::time::interval(Duration::from_secs(1));
    let mut net_tick = tokio::time::interval(Duration::from_millis(250));
    let mut input_tick = tokio::time::interval(Duration::from_millis(10));
    input_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut net_since = Instant::now();
    let mut received_bytes = 0u64;
    let mut net: Option<NetStats> = None;
    // For the soak test (G2): both should stay 0.
    let mut decode_errors = 0u32;
    let mut path_changes = 0u32;
    // Anything but the measurement deadline ending the loop is a failure.
    let mut ended_early = true;

    loop {
        tokio::select! {
            frame = video.frames.recv() => {
                let Some(frame) = frame else { break };
                received_bytes += frame.data.len() as u64;
                if frame.epoch < video.latest_epoch() {
                    continue;
                }
                payload.clear();
                VideoFrameHeader {
                    epoch: frame.epoch,
                    seq: frame.header.seq,
                    keyframe: frame.header.keyframe,
                    capture_us: frame.header.capture_us,
                    encode_us: frame.header.encode_us,
                    arrived_us: frame.arrived.0,
                }
                .write(&frame.data, &mut payload);
                if ipc_async::write_frame(&mut viewer_in, ipc::kinds::VIDEO_FRAME, &payload, &mut scratch)
                    .await
                    .is_err()
                {
                    println!("the viewer window closed");
                    break;
                }
            }
            Some(packet) = audio.recv() => {
                payload = ipc::to_payload(&AudioPacket {
                    seq: packet.seq,
                    capture_us: packet.capture_us,
                    arrived_us: couch_platform_win::clock::now().0,
                    packet: packet.packet,
                });
                if ipc_async::write_frame(&mut viewer_in, ipc::kinds::AUDIO_PACKET, &payload, &mut scratch)
                    .await
                    .is_err()
                {
                    println!("the viewer window closed");
                    break;
                }
            }
            Some(local) = local_rx.recv() => {
                for out in forwarder.local(local, Instant::now()) {
                    input_out.send(out);
                }
            }
            _ = input_tick.tick() => {
                for out in forwarder.tick(Instant::now()) {
                    input_out.send(out);
                }
            }
            Some(message) = seat_rx.recv() => {
                match message {
                    ControlMessage::SeatAssigned(s) => {
                        println!("you control seat {} ({:?})", s.seat.0, s.permissions);
                        for out in forwarder.seat_assigned(s.seat, s.seat_epoch, s.permissions, Instant::now()) {
                            input_out.send(out);
                        }
                    }
                    ControlMessage::SeatReleased(s) => {
                        println!("seat {} went back to the host", s.seat.0);
                        forwarder.seat_released(s.seat, s.seat_epoch);
                    }
                    _ => continue,
                }
                let state = SeatState {
                    holding: forwarder.holding().is_some(),
                    permissions: forwarder.holding().map_or(Permissions::empty(), |h| h.1),
                };
                payload = ipc::to_payload(&state);
                if ipc_async::write_frame(&mut viewer_in, ipc::kinds::SEAT, &payload, &mut scratch)
                    .await
                    .is_err()
                {
                    println!("the viewer window closed");
                    break;
                }
            }
            Some(rumble) = rumbles.recv() => {
                let Some(r) = decode_payload::<RumbleDatagram>(&rumble) else {
                    continue;
                };
                println!("rumble from the host: low {} high {}", r.low, r.high);
                payload = ipc::to_payload(&RumbleState { low: r.low, high: r.high });
                if ipc_async::write_frame(&mut viewer_in, ipc::kinds::RUMBLE, &payload, &mut scratch)
                    .await
                    .is_err()
                {
                    println!("the viewer window closed");
                    break;
                }
            }
            Some(()) = keyframe_rx.recv() => {
                decode_errors += 1;
                video.note_decode_error();
                control_tx
                    .send(&ControlMessage::KeyframeRequest(KeyframeRequest {
                        epoch: video.latest_epoch(),
                        reason: KeyframeReason::DecoderError,
                    }))
                    .await?;
            }
            _ = net_tick.tick() => {
                let path = couch_net::video::selected_path(&conn);
                let secs = net_since.elapsed().as_secs_f64().max(1e-3);
                net_since = Instant::now();
                let stats = NetStats {
                    rtt_us: path.rtt.as_micros().min(u128::from(u32::MAX)) as u32,
                    relayed: path.relayed,
                    receive_bps: (std::mem::take(&mut received_bytes) as f64 * 8.0 / secs) as u32,
                };
                if net.is_some_and(|n| n.relayed != stats.relayed) {
                    path_changes += 1;
                    println!("path changed: now {}", if stats.relayed { "RELAY" } else { "direct" });
                }
                net = Some(stats);
                payload.clear();
                stats.write(&mut payload);
                if ipc_async::write_frame(&mut viewer_in, ipc::kinds::NET_STATS, &payload, &mut scratch)
                    .await
                    .is_err()
                {
                    println!("the viewer window closed");
                    break;
                }
            }
            _ = report.tick() => {
                let mut l = latency.lock().unwrap();
                let w = std::mem::take(&mut l.window);
                let p = |s: &Samples, q| s.percentile(q).map_or("   -".into(), |v| format!("{v:4.1}"));
                let a = *audio_stats.lock().unwrap();
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
            status = child.wait() => {
                println!("the viewer exited: {status:?}");
                break;
            }
            error = conn.closed() => {
                println!("connection closed: {error}");
                break;
            }
        }
    }

    let path = couch_net::video::selected_path(&conn);
    {
        let mut l = latency.lock().unwrap();
        if let Some(mut csv) = l.csv.take() {
            use std::io::Write;
            csv.flush()?;
        }
        println!(
            "clock -> present     {}",
            l.all.clock_to_present.summary_ms()
        );
        println!(
            "capture -> present   {}",
            l.all.capture_to_present.summary_ms()
        );
        println!(
            "arrive -> present    {}",
            l.all.arrive_to_present.summary_ms()
        );
        if args.measure_secs.is_some() {
            let a = *audio_stats.lock().unwrap();
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
                "frames": l.all.frames,
                "fps": l.fps(),
                "clock_to_present_ms": percentiles(&l.all.clock_to_present),
                "capture_to_present_ms": percentiles(&l.all.capture_to_present),
                "arrive_to_present_ms": percentiles(&l.all.arrive_to_present),
                "rtt_ms": path.rtt.as_secs_f64() * 1000.0,
                "relayed": path.relayed,
                "lost_packets": path.lost_packets,
                "decode_errors": decode_errors,
                "path_changes": path_changes,
                "ended_early": ended_early,
            });
            println!("RESULT {result}");
        }
    }
    let _ = ipc_async::write_frame(&mut viewer_in, ipc::kinds::SHUTDOWN, &[], &mut scratch).await;
    conn.close(0u32.into(), b"bye");
    endpoint.close().await;
    Ok(())
}
