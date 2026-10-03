//! `couch-cli host`: stream a window, the monitor or the test pattern to one
//! viewer at a time, with rate control.

use std::{
    fs::File,
    io::{LineWriter, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicIsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use couch_codec_ffmpeg::FfmpegEncoder;
use couch_input::Permissions;
use couch_media::{
    CaptureEvent, Capturer, Codec, EncoderConfig, EncoderInput, VideoEncoder, VideoParams,
    VideoSender,
    clock::LATENCY_CLOCK_TITLE,
    ratecontrol::{RateAction, RateConfig, RateController},
    testpattern::TestPattern,
};
use couch_net::{
    Connection, control, datagrams::Datagrams, endpoint, netem::NetemTransport, video::HostVideo,
};
use couch_platform_win::foreground::WindowId;
use couch_platform_win::{
    capture::{CaptureOptions, CaptureSource, WgcCapturer},
    clock::QpcClock,
};
use couch_proto::control::ControlMessage;
use iroh_tickets::endpoint::EndpointTicket;
use tokio::{sync::watch, task::JoinHandle};

use crate::{
    host_input,
    net_args::{NetArgs, dial_addr},
};

#[derive(Debug, Clone)]
pub enum Source {
    Window(String),
    Monitor,
    TestPattern { width: u32, height: u32 },
}

#[derive(Debug, Clone)]
pub struct Args {
    pub source: Source,
    pub fps: u32,
    pub start_mbps: f64,
    pub min_mbps: f64,
    pub max_mbps: f64,
    pub relay_max_mbps: f64,
    /// Write every rate-controller tick (feedback and action) to this CSV file.
    pub rc_trace: Option<PathBuf>,
    /// What the viewer may send: the gamepad by default.
    pub permissions: Permissions,
    pub real_pad_ids: bool,
    pub net: NetArgs,
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
    println!("waiting for a viewer (Ctrl+C to stop)");

    // Stopping must release whatever a viewer holds: Windows keeps injected
    // keys down after the process that pressed them exits.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let inputs: Mutex<Vec<JoinHandle<()>>> = Mutex::default();
    let serving = async {
        while let Some(incoming) = endpoint.accept().await {
            let conn = match incoming.accept().context("accepting")?.await {
                Ok(conn) => conn,
                Err(e) => {
                    println!("connection failed: {e}");
                    continue;
                }
            };
            println!("viewer {} connected", conn.remote_id().fmt_short());
            if let Err(e) = serve(
                conn.clone(),
                &args,
                netem.clone(),
                shutdown_rx.clone(),
                &inputs,
            )
            .await
            {
                println!("session ended: {e:#}");
            }
            conn.close(0u32.into(), b"bye");
        }
        Ok(())
    };
    tokio::select! {
        result = serving => result,
        () = stop_requested() => {
            println!("stopping");
            shutdown_tx.send_replace(true);
            let handles = std::mem::take(&mut *inputs.lock().unwrap());
            for handle in handles {
                let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
            }
            Ok(())
        }
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

async fn serve(
    conn: Connection,
    args: &Args,
    netem: Option<Arc<NetemTransport>>,
    shutdown: watch::Receiver<bool>,
    inputs: &Mutex<Vec<JoinHandle<()>>>,
) -> anyhow::Result<()> {
    let (control_tx, mut control_rx, hello) = control::accept(&conn).await?;
    println!("viewer app {}", hello.app_version);
    let datagrams = Datagrams::start(conn.clone());
    // Set by the capture thread once it has the window.
    let game_window = Arc::new(AtomicIsize::new(0));
    let handle = host_input::spawn(
        conn.clone(),
        &datagrams,
        control_tx,
        host_input::Options {
            permissions: args.permissions,
            real_pad_ids: args.real_pad_ids,
            game_window: game_window.clone(),
            window_source: matches!(args.source, Source::Window(_)),
        },
        shutdown,
    );
    inputs.lock().unwrap().push(handle);

    let keyframe = Arc::new(AtomicBool::new(true));
    let stop = Arc::new(AtomicBool::new(false));
    {
        let keyframe = keyframe.clone();
        tokio::spawn(async move {
            while let Ok(Some(message)) = control_rx.recv().await {
                if let ControlMessage::KeyframeRequest(request) = message {
                    tracing::debug!(?request, "keyframe requested");
                    keyframe.store(true, Ordering::SeqCst);
                }
            }
        });
    }

    let video = HostVideo::start(conn.clone(), 1, &datagrams);
    let media = {
        let (args, keyframe, stop) = (args.clone(), keyframe.clone(), stop.clone());
        tokio::task::spawn_blocking(move || {
            media_loop(
                &args,
                video,
                &keyframe,
                &stop,
                netem.as_deref(),
                &game_window,
            )
        })
    };
    tokio::select! {
        result = media => result.context("media thread")?,
        error = conn.closed() => {
            stop.store(true, Ordering::SeqCst);
            println!("viewer disconnected: {error}");
            Ok(())
        }
    }
}

/// The capturer, and the captured window if the source is one.
fn open_capturer(args: &Args) -> anyhow::Result<(Box<dyn Capturer>, Option<WindowId>)> {
    let options = CaptureOptions {
        min_interval: Duration::from_secs(1) / args.fps,
        cursor: true,
    };
    Ok(match &args.source {
        Source::Window(title) => {
            let capturer = WgcCapturer::start(&CaptureSource::WindowTitle(title.clone()), options)?;
            let window = capturer.window();
            (Box::new(capturer), window)
        }
        Source::Monitor => (
            Box::new(WgcCapturer::start(&CaptureSource::PrimaryMonitor, options)?),
            None,
        ),
        &Source::TestPattern { width, height } => (
            Box::new(TestPattern::new(
                width,
                height,
                args.fps,
                Arc::new(QpcClock),
            )),
            None,
        ),
    })
}

fn media_loop(
    args: &Args,
    mut video: HostVideo,
    keyframe: &AtomicBool,
    stop: &AtomicBool,
    netem: Option<&NetemTransport>,
    game_window: &AtomicIsize,
) -> anyhow::Result<()> {
    let (mut capturer, window) = open_capturer(args)?;
    if let Some(window) = window {
        game_window.store(window.0, Ordering::Release);
    }
    let mbps = |v: f64| (v * 1e6) as u32;
    let mut encoder: Option<FfmpegEncoder> = None;
    let mut controller: Option<RateController> = None;
    let mut params: Option<VideoParams> = None;
    let mut force_keyframe = true;
    let mut last_tick = Instant::now();
    let mut stats = PeriodStats::new();
    let mut trace = args.rc_trace.as_deref().map(RcTrace::create).transpose()?;

    while !stop.load(Ordering::SeqCst) {
        if let (Some(c), Some(enc), Some(p)) = (controller.as_mut(), encoder.as_mut(), params)
            && last_tick.elapsed() >= Duration::from_millis(50)
        {
            last_tick = Instant::now();
            let fb = video.feedback();
            let action = c.tick(last_tick, &fb);
            if let Some(trace) = trace.as_mut() {
                trace.write(&fb, c, action, netem.and_then(NetemTransport::elapsed))?;
            }
            match action {
                RateAction::Keep => {}
                RateAction::SetBitrate(bps) => enc.set_bitrate(bps)?,
                RateAction::CatchUp { bitrate } => {
                    enc.set_bitrate(bitrate)?;
                    video.start_epoch(&p)?;
                    force_keyframe = true;
                    stats.catch_ups += 1;
                }
                RateAction::Pause => {
                    // Drop what is queued; nothing more until reports return.
                    video.start_epoch(&p)?;
                    stats.pauses += 1;
                    println!("viewer silent: paused");
                }
                RateAction::Resume { bitrate } => {
                    enc.set_bitrate(bitrate)?;
                    force_keyframe = true;
                    println!(
                        "viewer back: resuming at {:.1} Mbit/s",
                        f64::from(bitrate) / 1e6
                    );
                }
            }
            stats.last_feedback = Some(fb);
        }

        let frame = match capturer.next_event(Duration::from_millis(10))? {
            Some(CaptureEvent::Frame(frame)) => frame.cropped_to_even(),
            Some(CaptureEvent::SourceClosed) => {
                println!("the captured source closed");
                return Ok(());
            }
            None => continue,
        };

        // Nothing goes out while the viewer is silent, not even a new size:
        // the first frame after resuming reopens the encoder if it changed.
        if controller.as_ref().is_some_and(RateController::is_paused) {
            continue;
        }

        let size_changed =
            params.is_none_or(|p| (p.width, p.height) != (frame.width, frame.height));
        if size_changed {
            let mut rate = RateConfig {
                min_bps: mbps(args.min_mbps),
                max_bps: mbps(args.max_mbps),
                relay_max_bps: mbps(args.relay_max_mbps),
                start_bps: controller
                    .as_ref()
                    .map_or(mbps(args.start_mbps), |c| c.applied_bps()),
                keyframe_costly: false,
            };
            // The encoder must start where the controller does.
            let start = rate.clamped_start();
            let enc = FfmpegEncoder::open_best(EncoderConfig {
                codec: Codec::H264,
                width: frame.width,
                height: frame.height,
                fps: args.fps,
                bitrate_bps: start,
            })?;
            println!(
                "encoding {}x{} with {} at {:.1} Mbit/s",
                frame.width,
                frame.height,
                enc.caps().name,
                f64::from(start) / 1e6
            );
            rate.keyframe_costly = enc.caps().bitrate_change_costs_keyframe;
            controller = Some(RateController::new(rate));
            let p = VideoParams {
                codec: Codec::H264,
                width: frame.width,
                height: frame.height,
                fps: args.fps,
            };
            video.start_epoch(&p)?;
            params = Some(p);
            encoder = Some(enc);
            force_keyframe = true;
        }

        let enc = encoder.as_mut().expect("opened above");
        let key = std::mem::take(&mut force_keyframe) | keyframe.swap(false, Ordering::SeqCst);
        for unit in enc.encode(EncoderInput::Cpu(&frame), key)? {
            stats.bytes += unit.data.len() as u64;
            stats.keyframes += u32::from(unit.keyframe);
            stats.encode_us_max = stats.encode_us_max.max(unit.encode_time_us);
            video.send_frame(&unit)?;
        }
        stats.frames += 1;

        if stats.started.elapsed() >= Duration::from_secs(1) {
            let applied = controller.as_ref().map_or(0, |c| c.applied_bps());
            println!("{}", stats.line(applied));
            stats = PeriodStats::new();
        }
    }
    Ok(())
}

struct PeriodStats {
    started: Instant,
    frames: u32,
    bytes: u64,
    keyframes: u32,
    catch_ups: u32,
    pauses: u32,
    encode_us_max: u32,
    last_feedback: Option<couch_media::FeedbackSnapshot>,
}

impl PeriodStats {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            frames: 0,
            bytes: 0,
            keyframes: 0,
            catch_ups: 0,
            pauses: 0,
            encode_us_max: 0,
            last_feedback: None,
        }
    }

    fn line(&self, applied_bps: u32) -> String {
        let secs = self.started.elapsed().as_secs_f64();
        let fb = self.last_feedback.unwrap_or_default();
        format!(
            "{:5.1} fps  {:5.1} Mbit/s (target {:5.1})  rtt {:5.1} ms  delay+ {:5.1} ms  backlog {:6} B  \
             loss {}  key {}  catch-up {}  pause {}  {}",
            f64::from(self.frames) / secs,
            self.bytes as f64 * 8.0 / secs / 1e6,
            f64::from(applied_bps) / 1e6,
            f64::from(fb.rtt_us) / 1000.0,
            f64::from(fb.delay_trend_us) / 1000.0,
            fb.outstanding_bytes,
            fb.lost_packets,
            self.keyframes,
            self.catch_ups,
            self.pauses,
            if fb.relayed { "RELAY" } else { "direct" },
        )
    }
}

/// `--rc-trace`: one CSV row per controller tick. Times are QPC microseconds,
/// like the viewer's `--timings-csv`, so the two files line up. `netem_us` is
/// the time since the netem link started (empty without `--netem`), which
/// places its scheduled changes. Written line by line: `latency-probe` ends
/// the host by killing it.
struct RcTrace(LineWriter<File>);

impl RcTrace {
    fn create(path: &std::path::Path) -> anyhow::Result<Self> {
        let mut w = LineWriter::new(
            File::create(path).with_context(|| format!("creating {}", path.display()))?,
        );
        writeln!(
            w,
            "qpc_us,applied_bps,target_bps,delivery_bps,send_bps,outstanding_bytes,rtt_us,min_rtt_us,delay_trend_us,\
             sent_packets,lost_packets,congestion_events,cwnd_bytes,report_age_ms,relayed,netem_us,action"
        )?;
        Ok(Self(w))
    }

    fn write(
        &mut self,
        fb: &couch_media::FeedbackSnapshot,
        c: &RateController,
        action: RateAction,
        netem: Option<Duration>,
    ) -> anyhow::Result<()> {
        use couch_media::clock::Clock;
        let action = match action {
            RateAction::Keep => String::new(),
            RateAction::SetBitrate(bps) => format!("set {bps}"),
            RateAction::CatchUp { bitrate } => format!("catch-up {bitrate}"),
            RateAction::Pause => "pause".into(),
            RateAction::Resume { bitrate } => format!("resume {bitrate}"),
        };
        let netem = netem.map_or(String::new(), |d| d.as_micros().to_string());
        writeln!(
            self.0,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            QpcClock.now().0,
            c.applied_bps(),
            c.target_bps(),
            fb.delivery_rate_bps,
            fb.send_rate_bps,
            fb.outstanding_bytes,
            fb.rtt_us,
            fb.min_rtt_us,
            fb.delay_trend_us,
            fb.sent_packets,
            fb.lost_packets,
            fb.congestion_events,
            fb.cwnd_bytes,
            fb.report_age_ms,
            u8::from(fb.relayed),
            netem,
            action
        )?;
        Ok(())
    }
}

/// Default title for `--source window`.
pub fn default_window() -> String {
    LATENCY_CLOCK_TITLE.to_string()
}
