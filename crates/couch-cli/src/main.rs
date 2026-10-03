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
//! - `release-check`: the host releases input whenever a session ends badly.

mod audio_probe;
mod capture_test;
mod gates;
mod host;
mod host_audio;
mod host_input;
mod input_sink;
mod ipc_async;
mod latency_probe;
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
        /// Also write the H.264 stream here (Annex B; `ffplay` plays it).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Stream a window, the monitor or the test pattern to a viewer. Prints
    /// a `TICKET` line to pass to `view`.
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
        /// Write every rate-controller tick to this CSV file.
        #[arg(long)]
        rc_trace: Option<PathBuf>,
        /// Let the viewer use keyboard and mouse, not just the gamepad. This
        /// is remote control of this PC while the captured window is in front.
        #[arg(long)]
        allow_kbm: bool,
        /// Give the virtual pad a real Xbox 360 controller's USB ids. Without
        /// it the pad uses development ids, which a viewer on this machine
        /// skips; some games only accept the real ones.
        #[arg(long)]
        real_pad_ids: bool,
        /// Whose sound to stream.
        #[arg(long, value_enum, default_value = "auto")]
        audio: AudioArg,
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
    },
    /// Inject a scripted keyboard and mouse run into a test window and check
    /// it arrived intact.
    InputSink,
    /// Kill, close, freeze and stop host and viewer processes, and check the
    /// host releases the viewer's input each time, in time. About 1 minute.
    ReleaseCheck,
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
            out,
        } => tokio::task::block_in_place(|| {
            // Capture and codecs block; keep them off the async runtime's threads.
            capture_test::run(capture_test::Args {
                window,
                monitor,
                secs,
                fps,
                bitrate_mbps,
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
            rc_trace,
            allow_kbm,
            real_pad_ids,
            audio,
            net,
        } => {
            host::run(host::Args {
                source: match source {
                    SourceArg::Window => {
                        host::Source::Window(title.unwrap_or_else(host::default_window))
                    }
                    SourceArg::Monitor => host::Source::Monitor,
                    // clap guarantees exactly two values.
                    SourceArg::Testpattern => host::Source::TestPattern {
                        width: pattern_size[0],
                        height: pattern_size[1],
                    },
                },
                fps,
                start_mbps,
                min_mbps,
                max_mbps,
                relay_max_mbps,
                rc_trace,
                permissions: if allow_kbm {
                    couch_input::Permissions::all()
                } else {
                    couch_input::Permissions::GUEST_DEFAULT
                },
                real_pad_ids,
                audio: match audio {
                    AudioArg::Auto => host_audio::AudioChoice::Auto,
                    AudioArg::Game => host_audio::AudioChoice::Game,
                    AudioArg::System => host_audio::AudioChoice::System,
                    AudioArg::Tone => host_audio::AudioChoice::Tone,
                    AudioArg::Off => host_audio::AudioChoice::Off,
                },
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
            net,
        } => {
            view::run(view::Args {
                ticket,
                measure_secs,
                viewer_pos: pair(viewer_pos),
                viewer_size: pair(viewer_size),
                hide_hud: no_hud,
                timings_csv,
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
            net,
        } => latency_probe::run(latency_probe::Args {
            secs,
            fps,
            max_mbps,
            screen,
            audio,
            timings_csv,
            rc_trace,
            netem_viewer,
            log: None,
            net,
        })
        .await
        .map(drop),
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
        Cmd::PadTest { secs, real_pad_ids } => {
            tokio::task::block_in_place(|| pad_test::run(secs, real_pad_ids))
        }
        Cmd::InputSink => tokio::task::block_in_place(input_sink::run),
        Cmd::ReleaseCheck => tokio::task::block_in_place(release_check::run),
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
    Ok(())
}
