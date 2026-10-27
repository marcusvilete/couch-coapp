//! couch-viewer: shows a session's stream, plays its audio and captures local
//! input. It never touches the network. The main app feeds it over stdin and
//! reads its replies on stdout (`couch_proto::ipc`), so nothing else may be
//! written to stdout; logs go to stderr.
//!
//! Modes:
//! - no arguments: show the stream arriving on the IPC pipe until it closes;
//! - `--hud-preview STATE [--hud-preview-pointer X Y] [--hud-scale S]`: no
//!   pipe, a made-up session in one of `viewer::Preview::NAMES`, to check the
//!   HUD by eye (`scripts/viewer-shots.ps1` shoots them all);
//! - `--clock [x y] [--clicks] [--fps N] [--gpu-load N]`: the latency clock window (see `clock.rs`);
//! - `--input-sink FILE`: a window that records the input it receives (see
//!   `sink.rs`);
//! - `--probe`: print SDL3 and gamepad information as one JSON line, for
//!   `couch-cli probe`;
//! - `--version`.

#![deny(clippy::print_stdout)]

mod audio;
mod clock;
mod gpu;
mod hud;
mod input;
mod pipe;
mod sink;
mod viewer;
#[cfg(windows)]
mod zero_copy;

use std::io::Write;

use anyhow::{anyhow, bail};
use couch_media::Micros;

/// Now, on the clock frames are stamped with (QPC on Windows).
pub fn now_us() -> Micros {
    #[cfg(windows)]
    {
        couch_platform_win::clock::now()
    }
    #[cfg(not(windows))]
    {
        compile_error!("couch-viewer needs a cross-process clock on this platform")
    }
}

fn main() -> anyhow::Result<()> {
    // Without RUST_LOG (an installed app), our own info lines still reach
    // viewer.log: they say which decoding path and GPU backend ran.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,couch=info")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None
        | Some(
            "--pos" | "--size" | "--no-hud" | "--hide-stats" | "--hud-preview" | "--hud-scale",
        ) => viewer::run(window_options(&args)?),
        Some("--clock") => {
            let coord =
                |i: usize, default| args.get(i).and_then(|v| v.parse().ok()).unwrap_or(default);
            let clicks = args.iter().any(|a| a == "--clicks");
            let fps = match args.iter().position(|a| a == "--fps") {
                Some(i) => Some(
                    args.get(i + 1)
                        .and_then(|v| v.parse().ok())
                        .filter(|&fps: &u32| fps > 0)
                        .ok_or_else(|| anyhow!("--fps needs a frame rate"))?,
                ),
                None => None,
            };
            let gpu_load = match args.iter().position(|a| a == "--gpu-load") {
                Some(i) => args
                    .get(i + 1)
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| anyhow!("--gpu-load needs a number of rectangles"))?,
                None => 0,
            };
            clock::run(coord(1, 80), coord(2, 80), clicks, fps, gpu_load)
        }
        Some("--input-sink") => {
            let path = args
                .get(1)
                .ok_or_else(|| anyhow!("--input-sink needs a file"))?;
            sink::run(std::path::Path::new(path))
        }
        Some("--probe") => probe(),
        Some("--version") => {
            writeln!(std::io::stdout().lock(), "{}", env!("CARGO_PKG_VERSION"))?;
            Ok(())
        }
        Some(other) => bail!("unknown argument {other:?}"),
    }
}

/// `--pos X Y` and `--size W H` for the stream window, `--no-hud`,
/// `--hide-stats`, `--hud-preview STATE`, `--hud-preview-pointer X Y` and
/// `--hud-scale S`.
fn window_options(args: &[String]) -> anyhow::Result<viewer::WindowOptions> {
    let mut options = viewer::WindowOptions::default();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let mut number = || -> anyhow::Result<i64> {
            rest.next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| anyhow!("{flag} needs two numbers"))
        };
        match flag.as_str() {
            "--pos" => options.position = Some((number()? as i32, number()? as i32)),
            "--size" => options.size = (number()? as u32, number()? as u32),
            "--no-hud" => options.hide_hud = true,
            "--hide-stats" => options.hide_stats = true,
            "--hud-preview" => {
                let name = rest.next().map(String::as_str).unwrap_or_default();
                options.preview = Some(viewer::Preview::named(name).ok_or_else(|| {
                    let names: Vec<&str> = viewer::Preview::NAMES.iter().map(|(n, _)| *n).collect();
                    anyhow!("--hud-preview needs one of {}", names.join(", "))
                })?);
            }
            "--hud-preview-pointer" => {
                options.preview_pointer = Some((number()? as f32, number()? as f32));
            }
            "--hud-scale" => {
                options.hud_scale = Some(
                    rest.next()
                        .and_then(|v| v.parse().ok())
                        .filter(|s: &f32| (0.5..=4.0).contains(s))
                        .ok_or_else(|| anyhow!("--hud-scale needs a scale from 0.5 to 4"))?,
                );
            }
            other => bail!("unknown argument {other:?}"),
        }
    }
    Ok(options)
}

fn probe() -> anyhow::Result<()> {
    let sdl = sdl3::init().map_err(|e| anyhow!("SDL init failed: {e}"))?;
    let gamepads = sdl
        .gamepad()
        .map_err(|e| anyhow!("SDL gamepad subsystem failed: {e}"))?;
    let names: Vec<String> = gamepads
        .gamepads()
        .map_err(|e| anyhow!("listing gamepads failed: {e}"))?
        .into_iter()
        .map(|id| {
            let name = gamepads
                .name_for_id(id)
                .unwrap_or_else(|e| format!("<unnamed: {e}>"));
            let ids = (
                gamepads.vendor_for_id(id).unwrap_or(0),
                gamepads.product_for_id(id).unwrap_or(0),
            );
            let path = gamepads.path_for_id(id).ok();
            let skipped = ids == couch_input::DEV_PAD_IDS
                || (ids == couch_input::XBOX_360_IDS && input::may_be_hosts(path.as_deref(), ids));
            if skipped {
                format!(
                    "{name} ({:04x}:{:04x}, maybe a host's virtual pad: skipped)",
                    ids.0, ids.1
                )
            } else {
                format!("{name} ({:04x}:{:04x})", ids.0, ids.1)
            }
        })
        .collect();
    let report = serde_json::json!({
        "sdl_version": sdl3::version::version().to_string(),
        "gamepads": names,
    });
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, &report)?;
    writeln!(out)?;
    Ok(())
}
