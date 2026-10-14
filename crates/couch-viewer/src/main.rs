//! couch-viewer: shows a session's stream, plays its audio and captures local
//! input. It never touches the network. The main app feeds it over stdin and
//! reads its replies on stdout (`couch_proto::ipc`), so nothing else may be
//! written to stdout; logs go to stderr.
//!
//! Modes:
//! - no arguments: show the stream arriving on the IPC pipe until it closes;
//! - `--hud-preview watching|asking|controlling`: no pipe, a made-up session,
//!   to check the HUD by eye;
//! - `--clock [x y] [--clicks] [--fps N]`: the latency clock window (see `clock.rs`);
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
use couch_proto::ipc::HudRole;

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
        None | Some("--pos" | "--size" | "--no-hud" | "--hide-stats" | "--hud-preview") => {
            viewer::run(window_options(&args)?)
        }
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
            clock::run(coord(1, 80), coord(2, 80), clicks, fps)
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
/// `--hide-stats` and `--hud-preview ROLE`.
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
                options.preview = Some(match rest.next().map(String::as_str) {
                    Some("watching") => HudRole::Watching,
                    Some("asking") => HudRole::Asking,
                    Some("controlling") => HudRole::Controlling,
                    _ => bail!("--hud-preview needs watching, asking or controlling"),
                });
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
            let skipped = ids == couch_input::DEV_PAD_IDS
                || (ids == couch_input::XBOX_360_IDS && input::host_pads_here());
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
