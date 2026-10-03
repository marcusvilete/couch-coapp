//! couch-viewer: shows a session's stream, plays its audio and captures local
//! input. It never touches the network. The main app feeds it over stdin and
//! reads its replies on stdout (`couch_proto::ipc`), so nothing else may be
//! written to stdout; logs go to stderr.
//!
//! Modes:
//! - no arguments: serve the IPC pipe until the parent closes it (M0 drains it;
//!   decode and render arrive in M1);
//! - `--probe`: print SDL3 and gamepad information as one JSON line, for
//!   `couch-cli probe`;
//! - `--version`.

#![deny(clippy::print_stdout)]

use std::io::Write;

use anyhow::{Context, anyhow, bail};
use couch_proto::ipc;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => serve_pipe(),
        Some("--probe") => probe(),
        Some("--version") => {
            writeln!(std::io::stdout().lock(), "{}", env!("CARGO_PKG_VERSION"))?;
            Ok(())
        }
        Some(other) => bail!("unknown argument {other:?}"),
    }
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
            gamepads
                .name_for_id(id)
                .unwrap_or_else(|e| format!("<unnamed: {e}>"))
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

fn serve_pipe() -> anyhow::Result<()> {
    let mut stdin = std::io::stdin().lock();
    let mut frames = 0u64;
    while let Some(frame) = ipc::read_frame(&mut stdin).context("reading the IPC pipe")? {
        frames += 1;
        if frame.kind == ipc::kinds::SHUTDOWN {
            break;
        }
    }
    tracing::info!(frames, "IPC pipe closed");
    Ok(())
}
