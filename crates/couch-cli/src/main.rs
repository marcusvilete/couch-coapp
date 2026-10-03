//! Developer harness for Couch Co-App.
//!
//! `probe` checks that this machine can run the app. The M1 spike commands
//! (host, view, clock, latency-probe) are added next to it.

mod probe;

use std::{process::ExitCode, time::Duration};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "couch-cli", version, about = "Couch Co-App developer harness")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check codecs, networking, gamepads and the virtual gamepad driver.
    Probe {
        /// Seconds to wait for iroh to reach a relay.
        #[arg(long, default_value_t = 20)]
        net_timeout_secs: u64,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    match Cli::parse().command {
        Cmd::Probe { net_timeout_secs } => probe::run(Duration::from_secs(net_timeout_secs)).await,
    }
}
