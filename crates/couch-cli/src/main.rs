//! Developer harness for Couch Co-App.
//!
//! - `probe`: what this machine can do;
//! - `windows`: list capturable windows;
//! - `capture-test`: the capture, encode and decode pipeline without a network;
//! - `host` / `view`: stream between two processes over iroh;
//! - `latency-probe`: the M1 latency measurement on one machine;
//! - `gates`: the rate-control gates (G1, G3 to G5), run and judged;
//! - `pad-test`: a virtual gamepad that moves by itself;
//! - `input-sink`: keyboard and mouse injection checked end to end;
//! - `release-check`: the host releases input whenever a session ends badly;
//! - `fake-game` and `lifecycle-check`: a game swapping its window or
//!   exiting, and a host killed, are noticed in time.

mod audio_probe;
mod capture_test;
mod fake_game;
mod gates;
mod host;
mod host_pad;
mod input_sink;
mod latency_probe;
mod lifecycle_check;
mod net_args;
mod pad_test;
mod probe;
mod release_check;
mod screen_probe;
mod stats;
mod view;

use std::{path::PathBuf, process::ExitCode, time::Duration};

use clap::{Parser, Subcommand, ValueEnum};

use crate::net_args::{CongestionArg, NetArgs};

#[derive(Parser)]
#[command(name = "couch-cli", version, about = "Couch Co-App developer harness")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CodecArg {
    H264,
    Hevc,
}

impl From<CodecArg> for couch_media::Codec {
    fn from(c: CodecArg) -> Self {
        match c {
            CodecArg::H264 => Self::H264,
            CodecArg::Hevc => Self::Hevc,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SourceArg {
    Window,
    Monitor,
    Testpattern,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum AudioArg {
    /// The game for a window, everything but this app for the monitor, test
    /// clicks for the test pattern.
    Auto,
    /// The captured window's process and its children.
    Game,
    /// Everything this PC plays, except this app.
    System,
    /// Test clicks, twice a second.
    Tone,
    Off,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check codecs, networking, gamepads and the virtual gamepad driver.
    Probe {
        /// Seconds to wait for iroh to reach a relay.
        #[arg(long, default_value_t = 20)]
        net_timeout_secs: u64,
    },
    /// List windows that can be captured.
    Windows,
    /// Capture, encode, decode and read the latency clock, without a network.
    CaptureTest {
        /// Window title to capture (substring). Defaults to the latency clock.
        #[arg(long)]
        window: Option<String>,
        /// Capture the primary monitor instead of a window.
        #[arg(long)]
        monitor: bool,
        #[arg(long, default_value_t = 10)]
        secs: u64,
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..=240))]
        fps: u32,
        #[arg(long, default_value_t = 20)]
        bitrate_mbps: u32,
        /// The codec to encode in (M12c: HEVC is NVENC's only).
        #[arg(long, value_enum, default_value = "h264")]
        codec: CodecArg,
        /// Also measure each decoded picture against the captured one (luma
        /// PSNR): picture quality at this bitrate, to compare codecs.
        #[arg(long)]
        psnr: bool,
        /// Also write the encoded stream here (Annex B; `ffplay` plays it).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Stream a window, the monitor or the test pattern to viewers, each with
    /// an encoder of its own. Prints a `TICKET` line to pass to `view`.
    Host {
        #[arg(long, value_enum, default_value = "window")]
        source: SourceArg,
        /// Window title (substring) for `--source window`. Defaults to the latency clock.
        #[arg(long)]
        title: Option<String>,
        /// Frame size for `--source testpattern`.
        #[arg(long, num_args = 2, value_names = ["W", "H"], default_values_t = [1920, 1080])]
        pattern_size: Vec<u32>,
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..=240))]
        fps: u32,
        #[arg(long, default_value_t = 10.0)]
        start_mbps: f64,
        #[arg(long, default_value_t = 1.5)]
        min_mbps: f64,
        #[arg(long, default_value_t = 20.0)]
        max_mbps: f64,
        /// Bitrate ceiling while the path goes through a relay.
        #[arg(long, default_value_t = 2.0)]
        relay_max_mbps: f64,
        /// This PC's upload cap, split between the viewers.
        #[arg(long)]
        upload_mbps: Option<f64>,
        /// Viewers at once; more are turned away.
        #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(1..=7))]
        max_viewers: u8,
        /// Write every rate-controller tick to this CSV file; with several
        /// viewers, the next ones go to numbered files beside it.
        #[arg(long)]
        rc_trace: Option<PathBuf>,
        /// Write every captured frame, and what pacing did with it (sent,
        /// held or released after its wait), to this CSV file (M9e).
        #[arg(long)]
        pacing_trace: Option<PathBuf>,
        /// Let the viewer use keyboard and mouse, not just the gamepad. This
        /// is remote control of this PC while the captured window is in front.
        #[arg(long)]
        allow_kbm: bool,
        /// Virtual pads, one per seat: each viewer gets the first free seat
        /// as it connects (only the first may have keyboard and mouse).
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=4))]
        pads: u8,
        /// Give the virtual pad a real Xbox 360 controller's USB ids. Without
        /// it the pad uses development ids, which a viewer on this machine
        /// skips; some games only accept the real ones.
        #[arg(long)]
        real_pad_ids: bool,
        /// Play seat 0 with this PC's controller N (as `host-pad` lists them,
        /// from 1), hidden from games while streaming; viewers take the next
        /// free seats.
        #[arg(long)]
        host_pad: Option<usize>,
        /// With `--host-pad`: don't hide the controller from games.
        #[arg(long)]
        show_host_pad: bool,
        /// Whose sound to stream.
        #[arg(long, value_enum, default_value = "auto")]
        audio: AudioArg,
        /// Stop the stream after this many seconds and start it again two
        /// seconds later, once, in the same session (M19): viewers stay and
        /// see the second stream.
        #[arg(long)]
        restart_stream_after: Option<u64>,
        /// How loud viewers hear the sound, in percent of the game's (P7):
        /// below 100 quieter, up to 400 boosted (peaks rounded off).
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(0..=400))]
        audio_level: u16,
        #[command(flatten)]
        net: NetArgs,
    },
    /// Connect to a host and show its stream in a viewer window.
    View {
        /// The ticket a `host` printed.
        ticket: String,
        /// Stop after this many seconds and print a `RESULT` line.
        #[arg(long)]
        measure_secs: Option<u64>,
        #[arg(long, num_args = 2, value_names = ["X", "Y"])]
        viewer_pos: Option<Vec<i32>>,
        #[arg(long, num_args = 2, value_names = ["W", "H"])]
        viewer_size: Option<Vec<u32>>,
        /// Start the viewer with its HUD hidden (F3 toggles it).
        #[arg(long)]
        no_hud: bool,
        /// Write every presented frame's timings to this CSV file.
        #[arg(long)]
        timings_csv: Option<PathBuf>,
        /// Start the viewer window again if it crashes, as the app does.
        #[arg(long)]
        restart_viewer: bool,
        #[command(flatten)]
        net: NetArgs,
    },
    /// Capture the screen once and list the latency barcodes on it.
    ScreenScan {
        /// Also write the frame as raw BGRA here.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Measure clock-to-present latency on this machine (gates G1, G1b).
    LatencyProbe {
        #[arg(long, default_value_t = 30)]
        secs: u64,
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..=240))]
        fps: u32,
        #[arg(long, default_value_t = 20.0)]
        max_mbps: f64,
        /// Also read both barcodes from screen captures (G1b).
        #[arg(long)]
        screen: bool,
        /// Also time audio: what the pipeline adds (A1), and the clock's
        /// clicks from the clock to the viewer's playback.
        #[arg(long)]
        audio: bool,
        /// Write every presented frame's timings to this CSV file.
        #[arg(long)]
        timings_csv: Option<PathBuf>,
        /// Write the host's rate-controller ticks to this CSV file.
        #[arg(long)]
        rc_trace: Option<PathBuf>,
        /// The viewer's netem profile, if it should differ from `--netem` (the
        /// host's). A blackout on the host side cuts both directions.
        #[arg(long)]
        netem_viewer: Option<String>,
        /// Two viewers watch (gate G7). Everything then runs on the netem
        /// transport.
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=2))]
        viewers: u8,
        /// With two viewers: a netem profile for the host's traffic to the
        /// first viewer only, e.g. `rate=5mbit`.
        #[arg(long)]
        impair_first: Option<String>,
        /// Load the CPU for the whole run with this many busy threads at
        /// normal priority, as a game would; alone, one per logical CPU.
        #[arg(long, num_args = 0..=1, default_missing_value = "0")]
        cpu_load: Option<usize>,
        /// Confine the whole run to these logical CPUs, e.g. `16-19`: a
        /// weaker PC on this one. With `--cpu-load` alone, the load fills
        /// exactly these.
        #[arg(long)]
        cpus: Option<String>,
        /// Cap the latency clock, which plays the game, at this frame rate
        /// (by default it paints every refresh), e.g. 61 against a 60 fps
        /// stream: a game a little faster than the stream.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=1000))]
        clock_fps: Option<u32>,
        /// Make the latency clock GPU-bound like a demanding game: it draws
        /// this many blended full-size rectangles into a 4K texture each
        /// frame (M9f). The summary shows the clock's own frame rate.
        #[arg(long)]
        gpu_load: Option<u32>,
        /// Write the host's pacing trace to this CSV (see `host --pacing-trace`).
        #[arg(long)]
        pacing_trace: Option<PathBuf>,
        #[command(flatten)]
        net: NetArgs,
    },
    /// Run the rate-control gates (G1, G3, G4, G5) on this machine and judge
    /// them. About 12 minutes, or 4 with `--quick`.
    Gates {
        /// Run G3 for 2 minutes instead of 10.
        #[arg(long)]
        quick: bool,
        /// Run only these gates, e.g. `--only g4,g5`.
        #[arg(long, value_enum, value_delimiter = ',')]
        only: Vec<gates::Gate>,
        /// Where the traces and logs go.
        #[arg(long, default_value = "target/gates")]
        out: PathBuf,
        #[arg(long, value_enum, default_value = "media")]
        congestion: CongestionArg,
    },
    /// Plug a virtual Xbox 360 pad and move it; watch it in joy.cpl.
    PadTest {
        #[arg(long, default_value_t = 30)]
        secs: u64,
        /// Use a real Xbox 360 controller's USB ids.
        #[arg(long)]
        real_pad_ids: bool,
        /// How many pads to plug in.
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=4))]
        count: u8,
    },
    /// List this PC's controllers and HidHide's state; with `--feed N`, hide
    /// controller N and drive a virtual pad from it, as a host on a seat does.
    HostPad {
        /// The controller to feed, as listed (from 1).
        #[arg(long)]
        feed: Option<usize>,
        #[arg(long, default_value_t = 30)]
        secs: u64,
        /// Feed without hiding the controller from other programs.
        #[arg(long)]
        no_hide: bool,
    },
    /// Inject a scripted keyboard and mouse run into a test window and check
    /// it arrived intact.
    InputSink,
    /// Kill, close, freeze and stop host and viewer processes, and check the
    /// host releases the viewer's input each time, in time. About 1 minute.
    ReleaseCheck,
    /// A stand-in game window: swaps its window, closes it or exits when told
    /// on stdin (`swap`, `close`, `exit`) or at the given times.
    FakeGame {
        #[arg(long)]
        swap_after: Option<f64>,
        #[arg(long)]
        close_after: Option<f64>,
        #[arg(long)]
        exit_after: Option<f64>,
    },
    /// A game swapping its window, exiting, closing it; a host killed, and
    /// killed holding a key: check each is noticed in time. About 1 minute.
    LifecycleCheck {
        /// Run only these: swap, exit, close, kill-host, stale-key.
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
    },
    /// For `lifecycle-check`: hold one key (a hex HID usage) through the
    /// host's injector.
    #[command(hide = true)]
    HoldKey { usage: String },
}

fn pair<T: Copy>(v: Option<Vec<T>>) -> Option<(T, T)> {
    v.and_then(|v| Some((*v.first()?, *v.get(1)?)))
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    couch_platform_win::timer::request_1ms_resolution();

    let result = match Cli::parse().command {
        Cmd::Probe { net_timeout_secs } => {
            return probe::run(Duration::from_secs(net_timeout_secs)).await;
        }
        Cmd::Windows => list_windows(),
        Cmd::CaptureTest {
            window,
            monitor,
            secs,
            fps,
            bitrate_mbps,
            codec,
            psnr,
            out,
        } => tokio::task::block_in_place(|| {
            // Capture and codecs block; keep them off the async runtime's threads.
            capture_test::run(capture_test::Args {
                window,
                monitor,
                secs,
                fps,
                bitrate_mbps,
                codec: codec.into(),
                psnr,
                out,
            })
        }),
        Cmd::Host {
            source,
            title,
            pattern_size,
            fps,
            start_mbps,
            min_mbps,
            max_mbps,
            relay_max_mbps,
            upload_mbps,
            max_viewers,
            rc_trace,
            pacing_trace,
            allow_kbm,
            pads,
            real_pad_ids,
            host_pad,
            show_host_pad,
            audio,
            restart_stream_after,
            audio_level,
            net,
        } => {
            use couch_engine::pipeline::{AudioChoice, Source};
            host::run(host::Args {
                source: match source {
                    SourceArg::Window => {
                        Source::WindowTitle(title.unwrap_or_else(host::default_window))
                    }
                    SourceArg::Monitor => Source::PrimaryScreen,
                    // clap guarantees exactly two values.
                    SourceArg::Testpattern => Source::TestPattern {
                        width: pattern_size[0],
                        height: pattern_size[1],
                    },
                },
                fps,
                start_mbps,
                min_mbps,
                max_mbps,
                relay_max_mbps,
                upload_mbps,
                max_viewers: usize::from(max_viewers),
                rc_trace,
                pacing_trace,
                permissions: if allow_kbm {
                    couch_input::Permissions::all()
                } else {
                    couch_input::Permissions::GUEST_DEFAULT
                },
                pads,
                real_pad_ids,
                host_pad,
                hide_host_pad: !show_host_pad,
                audio: match audio {
                    AudioArg::Auto => AudioChoice::Auto,
                    AudioArg::Game => AudioChoice::Game,
                    AudioArg::System => AudioChoice::System,
                    AudioArg::Tone => AudioChoice::Tone,
                    AudioArg::Off => AudioChoice::Off,
                },
                restart_stream_after,
                audio_level,
                net,
            })
            .await
        }
        Cmd::View {
            ticket,
            measure_secs,
            viewer_pos,
            viewer_size,
            no_hud,
            timings_csv,
            restart_viewer,
            net,
        } => {
            view::run(view::Args {
                ticket,
                measure_secs,
                viewer_pos: pair(viewer_pos),
                viewer_size: pair(viewer_size),
                hide_hud: no_hud,
                timings_csv,
                restart_viewer,
                net,
            })
            .await
        }
        Cmd::ScreenScan { out } => {
            tokio::task::block_in_place(|| screen_probe::scan_once(out.as_deref()))
        }
        Cmd::LatencyProbe {
            secs,
            fps,
            max_mbps,
            screen,
            audio,
            timings_csv,
            rc_trace,
            netem_viewer,
            viewers,
            impair_first,
            cpu_load,
            cpus,
            clock_fps,
            gpu_load,
            pacing_trace,
            net,
        } => match cpus.as_deref().map(latency_probe::parse_cpus).transpose() {
            Err(e) => Err(anyhow::anyhow!("--cpus: {e}")),
            Ok(cpus) => latency_probe::run_all(latency_probe::Args {
                secs,
                fps,
                max_mbps,
                screen,
                audio,
                timings_csv,
                rc_trace,
                netem_viewer,
                viewers: usize::from(viewers),
                impair_first,
                log: None,
                cpu_load,
                cpus,
                clock_fps,
                gpu_load,
                pacing_trace,
                // `COUCH_HOST_CODEC` as set, if at all.
                h264: false,
                net,
            })
            .await
            .map(drop),
        },
        Cmd::Gates {
            quick,
            only,
            out,
            congestion,
        } => {
            gates::run(gates::Args {
                only,
                quick,
                out,
                congestion,
            })
            .await
        }
        Cmd::PadTest {
            secs,
            real_pad_ids,
            count,
        } => tokio::task::block_in_place(|| pad_test::run(secs, real_pad_ids, count)),
        Cmd::HostPad {
            feed,
            secs,
            no_hide,
        } => tokio::task::block_in_place(|| host_pad::run(feed, secs, !no_hide)),
        Cmd::InputSink => tokio::task::block_in_place(input_sink::run),
        Cmd::ReleaseCheck => tokio::task::block_in_place(release_check::run),
        Cmd::FakeGame {
            swap_after,
            close_after,
            exit_after,
        } => tokio::task::block_in_place(|| {
            fake_game::run(fake_game::Args {
                swap_after,
                close_after,
                exit_after,
            })
        }),
        Cmd::LifecycleCheck { only } => tokio::task::block_in_place(|| lifecycle_check::run(&only)),
        Cmd::HoldKey { usage } => tokio::task::block_in_place(|| lifecycle_check::hold_key(&usage)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn list_windows() -> anyhow::Result<()> {
    for w in couch_platform_win::capture::list_windows()? {
        println!("{:>6}  {:<28} {}", w.process_id, w.process, w.title);
    }
    for m in couch_platform_win::capture::list_monitors()? {
        let primary = if m.primary { "  (primary)" } else { "" };
        println!("screen  {}x{}  {}{primary}", m.width, m.height, m.name);
    }
    Ok(())
}
