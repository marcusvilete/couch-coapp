//! `couch-cli host`: stream a window, the monitor or the test pattern, with
//! its audio, to up to `--max-viewers` viewers at once, each with an encoder
//! and rate control of its own (`couch_engine::pipeline::host`, the app's
//! own pipeline). The first viewer to connect gets the seat while nobody
//! holds it; the app instead hands the seat out on request.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::Context;
use couch_engine::pipeline::{
    AudioChoice, Source,
    host::{HostEvent, HostOptions, HostStream, NetemElapsed, Rates, ViewerStats},
    platform_backend,
};
use couch_input::{MemberId, Permissions, SeatEpoch, router::SeatStatus};
use couch_media::clock::LATENCY_CLOCK_TITLE;
use couch_net::{Connection, control, datagrams::Datagrams, endpoint, streams::Streams};
use couch_proto::control::{ControlMessage, SeatAssigned, SeatReleased};
use iroh_tickets::endpoint::EndpointTicket;
use tokio::sync::mpsc;

use crate::net_args::{NetArgs, dial_addr};

#[derive(Debug, Clone)]
pub struct Args {
    pub source: Source,
    pub fps: u32,
    pub start_mbps: f64,
    pub min_mbps: f64,
    pub max_mbps: f64,
    pub relay_max_mbps: f64,
    /// The host's upload cap, split between viewers.
    pub upload_mbps: Option<f64>,
    pub max_viewers: usize,
    /// Write every rate-controller tick to CSV files, one per viewer.
    pub rc_trace: Option<PathBuf>,
    /// What the viewer holding the seat may send: the gamepad by default.
    pub permissions: Permissions,
    pub real_pad_ids: bool,
    pub audio: AudioChoice,
    pub net: NetArgs,
}

/// What happens on a viewer's connection, as the main loop hears it.
enum FromViewer {
    Joined {
        member: MemberId,
        conn: Connection,
        control: mpsc::UnboundedSender<ControlMessage>,
    },
    Keyframe(MemberId),
    Left(MemberId),
}

struct Viewer {
    conn: Connection,
    control: mpsc::UnboundedSender<ControlMessage>,
    /// Kept for the connection's lifetime.
    _channels: (Datagrams, Streams),
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    anyhow::ensure!(
        args.min_mbps <= args.max_mbps,
        "--min-mbps ({}) is above --max-mbps ({})",
        args.min_mbps,
        args.max_mbps
    );
    let (options, netem) = args.net.options()?;
    let endpoint = endpoint::bind(options).await?;
    let addr = dial_addr(&endpoint, netem.as_deref(), args.net.uses_relays()).await?;
    println!("TICKET {}", EndpointTicket::new(addr));
    println!("waiting for viewers (Ctrl+C to stop)");

    let mbps = |v: f64| (v * 1e6) as u32;
    let netem_elapsed = netem.map(|link| Arc::new(move || link.elapsed()) as NetemElapsed);
    let (stream, mut events) = HostStream::start(
        platform_backend(),
        HostOptions {
            source: args.source.clone(),
            fps: args.fps,
            rates: Rates {
                min_bps: mbps(args.min_mbps),
                start_bps: mbps(args.start_mbps),
                max_bps: mbps(args.max_mbps),
                relay_max_bps: mbps(args.relay_max_mbps),
            },
            upload_bps: args.upload_mbps.map(mbps),
            audio: args.audio,
            real_pad_ids: args.real_pad_ids,
            session: 1,
            first_epoch: SeatEpoch(0),
            rc_trace: args.rc_trace.clone(),
            netem_elapsed,
        },
    )
    .await;
    if args
        .permissions
        .intersects(Permissions::KEYBOARD | Permissions::MOUSE)
        && !args.source.is_window()
    {
        println!("keyboard and mouse stay off: they need --source window");
    }

    let (from_viewers, mut heard) = mpsc::unbounded_channel();
    tokio::spawn(accept_viewers(endpoint.clone(), from_viewers));

    let mut viewers: HashMap<MemberId, Viewer> = HashMap::new();
    let mut holder: Option<MemberId> = None;
    let started = std::time::Instant::now();
    let since = || started.elapsed().as_secs_f64();
    let stop = stop_requested();
    tokio::pin!(stop);

    let ended = loop {
        tokio::select! {
            Some(message) = heard.recv() => match message {
                FromViewer::Joined { member, conn, control } => {
                    if viewers.len() >= args.max_viewers {
                        println!("viewer {} turned away: {} watching already", short(member), viewers.len());
                        conn.close(0u32.into(), b"full");
                        continue;
                    }
                    println!("viewer {} connected", short(member));
                    let channels = (Datagrams::start(conn.clone()), Streams::start(conn.clone()));
                    stream.add_viewer(member, conn.clone(), &channels.0, &channels.1);
                    if holder.is_none() {
                        holder = Some(member);
                        stream.grant(member, args.permissions);
                    }
                    viewers.insert(member, Viewer { conn, control, _channels: channels });
                }
                FromViewer::Keyframe(member) => stream.keyframe(member),
                FromViewer::Left(member) => {
                    if viewers.remove(&member).is_some() {
                        println!("viewer {} disconnected", short(member));
                        stream.remove_viewer(member);
                    }
                    if holder == Some(member) {
                        holder = None;
                    }
                }
            },
            Some(event) = events.recv() => {
                if let Some(ended) = on_event(event, &viewers, since()) {
                    break ended;
                }
            }
            () = &mut stop => {
                println!("stopping");
                break Ok(());
            }
        }
    };

    // Releases everything first; the events that says so are printed below.
    stream.stop().await;
    while let Ok(event) = events.try_recv() {
        on_event(event, &viewers, since());
    }
    for viewer in viewers.values() {
        viewer.conn.close(0u32.into(), b"bye");
    }
    endpoint.close().await;
    ended
}

/// Prints an event; `Some` when the stream must end.
fn on_event(
    event: HostEvent,
    viewers: &HashMap<MemberId, Viewer>,
    secs: f64,
) -> Option<anyhow::Result<()>> {
    match event {
        HostEvent::Granted {
            member,
            seat,
            epoch,
            permissions,
        } => {
            if let Some(v) = viewers.get(&member) {
                let _ = v.control.send(ControlMessage::SeatAssigned(SeatAssigned {
                    seat,
                    seat_epoch: epoch,
                    permissions,
                }));
            }
        }
        HostEvent::Revoked {
            member,
            seat,
            epoch,
        } => {
            if let Some(v) = viewers.get(&member) {
                let _ = v.control.send(ControlMessage::SeatReleased(SeatReleased {
                    seat,
                    seat_epoch: epoch,
                }));
            }
        }
        HostEvent::Seat { status, at } => {
            let (what, event) = match status {
                SeatStatus::Active => ("active", "active"),
                SeatStatus::Suspended => ("suspended, everything released", "suspended"),
                SeatStatus::Host => ("back with the host", "revoked"),
                SeatStatus::Granted => ("waiting", "waiting"),
            };
            println!("{secs:8.3}s  seat 0 {what}");
            event_line(event, at.0);
        }
        HostEvent::ViewerGone { at, .. } => {
            println!("{secs:8.3}s  viewer gone: input released");
            event_line("gone", at.0);
        }
        HostEvent::Released { at } => {
            println!("{secs:8.3}s  host stopping: input released");
            event_line("stopped", at.0);
        }
        HostEvent::ViewerFailed { member, error } => {
            println!("viewer {}: {error}", short(member));
            if let Some(v) = viewers.get(&member) {
                v.conn.close(0u32.into(), b"failed");
            }
        }
        HostEvent::Stats(stats) => println!("{}", stats_line(&stats, viewers.len() > 1)),
        HostEvent::Rumble(r) => println!(
            "{secs:8.3}s  rumble from the game: low {} high {}",
            r.low_frequency, r.high_frequency
        ),
        HostEvent::SourceClosed => {
            println!("the captured source closed");
            return Some(Ok(()));
        }
        HostEvent::CaptureFailed(e) => return Some(Err(anyhow::anyhow!("capture failed: {e}"))),
        HostEvent::Note(text) => println!("{text}"),
    }
    None
}

fn stats_line(s: &ViewerStats, name_it: bool) -> String {
    let fb = s.feedback;
    let who = if name_it {
        format!("{}  ", short(s.member))
    } else {
        String::new()
    };
    format!(
        "{who}{:5.1} fps  {:5.1} Mbit/s (target {:5.1})  rtt {:5.1} ms  delay+ {:5.1} ms  backlog {:6} B  \
         loss {}  key {}  catch-up {}  pause {}  encode max {:.1} ms  {}  audio {}",
        s.fps,
        s.bps / 1e6,
        f64::from(s.applied_bps) / 1e6,
        f64::from(fb.rtt_us) / 1000.0,
        f64::from(fb.delay_trend_us) / 1000.0,
        fb.outstanding_bytes,
        fb.lost_packets,
        s.keyframes,
        s.catch_ups,
        s.pauses,
        f64::from(s.encode_us_max) / 1000.0,
        if fb.relayed { "RELAY" } else { "direct" },
        s.audio_packets,
    )
}

fn short(member: MemberId) -> String {
    member.to_hex()[..10].to_string()
}

/// `EVENT <what> <QPC µs>`, for `release-check`, which times releases on the
/// same clock. What: `active`, `suspended`, `revoked`, `waiting`, and when a
/// viewer's input ends, `gone` (it disconnected) or `stopped` (the host).
fn event_line(what: &str, qpc_us: u64) {
    println!("EVENT {what} {qpc_us}");
}

/// Accepts viewers, runs each one's control stream, and reports to the main
/// loop.
async fn accept_viewers(endpoint: couch_net::Endpoint, to_main: mpsc::UnboundedSender<FromViewer>) {
    while let Some(incoming) = endpoint.accept().await {
        let to_main = to_main.clone();
        tokio::spawn(async move {
            let conn = match incoming.accept().context("accepting") {
                Ok(connecting) => match connecting.await {
                    Ok(conn) => conn,
                    Err(e) => {
                        println!("connection failed: {e}");
                        return;
                    }
                },
                Err(e) => {
                    println!("connection failed: {e:#}");
                    return;
                }
            };
            let member = MemberId(*conn.remote_id().as_bytes());
            let (mut tx, mut rx, hello) = match control::accept(&conn).await {
                Ok(control) => control,
                Err(e) => {
                    println!("no control stream: {e}");
                    return;
                }
            };
            println!("viewer app {}", hello.app_version);
            let (control, mut outgoing) = mpsc::unbounded_channel::<ControlMessage>();
            tokio::spawn(async move {
                while let Some(message) = outgoing.recv().await {
                    if let Err(e) = tx.send(&message).await {
                        tracing::debug!("seat notice not sent: {e}");
                        return;
                    }
                }
            });
            if to_main
                .send(FromViewer::Joined {
                    member,
                    conn: conn.clone(),
                    control,
                })
                .is_err()
            {
                return;
            }
            loop {
                match rx.recv().await {
                    Ok(Some(ControlMessage::KeyframeRequest(request))) => {
                        tracing::debug!(?request, "keyframe requested");
                        let _ = to_main.send(FromViewer::Keyframe(member));
                    }
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
            let _ = to_main.send(FromViewer::Left(member));
        });
    }
}

/// Ctrl+C, Ctrl+Break, or the console window closing.
async fn stop_requested() {
    use tokio::signal::windows::{ctrl_break, ctrl_close};
    match (ctrl_break(), ctrl_close()) {
        (Ok(mut brk), Ok(mut close)) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = brk.recv() => {}
                _ = close.recv() => {}
            }
        }
        _ => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

/// Default title for `--source window`.
pub fn default_window() -> String {
    LATENCY_CLOCK_TITLE.to_string()
}
