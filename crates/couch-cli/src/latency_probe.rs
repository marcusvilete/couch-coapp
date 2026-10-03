//! `couch-cli latency-probe`: the M1 measurement on one machine. Starts the
//! latency clock window, a host capturing it, and a viewer showing the stream
//! next to it; collects the viewer's numbers and checks them against the gates.

use std::{
    fs::File,
    io::{LineWriter, Write},
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, bail};
use couch_media::clock::{LATENCY_CLOCK_TITLE, latency_clock_size};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

use crate::{net_args::NetArgs, screen_probe::ScreenProbe};

#[derive(Debug, Clone)]
pub struct Args {
    pub secs: u64,
    pub fps: u32,
    pub max_mbps: f64,
    /// Also measure from screen captures (gate G1b).
    pub screen: bool,
    pub timings_csv: Option<PathBuf>,
    pub rc_trace: Option<PathBuf>,
    /// The viewer's netem profile, when it should differ from the host's.
    pub netem_viewer: Option<String>,
    /// Write the host's and viewer's output and the report here instead of
    /// to the console.
    pub log: Option<PathBuf>,
    pub net: NetArgs,
}

/// Clock window position; the viewer goes right below it at the same size.
const CLOCK_POS: (i32, i32) = (40, 40);
const VIEWER_POS: (i32, i32) = (40, 300);

/// Where the children's output goes: the console, or a log file.
#[derive(Clone)]
enum Echo {
    Stdout,
    File(Arc<Mutex<LineWriter<File>>>),
}

impl Echo {
    fn line(&self, line: &str) {
        match self {
            Echo::Stdout => println!("{line}"),
            Echo::File(file) => {
                let _ = writeln!(file.lock().unwrap(), "{line}");
            }
        }
    }
}

/// Runs the probe and returns the viewer's `RESULT` (see `view`).
pub async fn run(args: Args) -> anyhow::Result<serde_json::Value> {
    let echo = match &args.log {
        None => Echo::Stdout,
        Some(path) => Echo::File(Arc::new(Mutex::new(LineWriter::new(
            File::create(path).with_context(|| format!("creating {}", path.display()))?,
        )))),
    };
    let exe = std::env::current_exe()?;
    let viewer_exe = exe.with_file_name(format!("couch-viewer{}", std::env::consts::EXE_SUFFIX));

    let mut clock = Command::new(&viewer_exe)
        .args([
            "--clock",
            &CLOCK_POS.0.to_string(),
            &CLOCK_POS.1.to_string(),
        ])
        .kill_on_drop(true)
        .spawn()
        .context("starting the latency clock")?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let mut host = Command::new(&exe);
    host.args(["host", "--source", "window", "--title", LATENCY_CLOCK_TITLE])
        .args([
            "--fps",
            &args.fps.to_string(),
            "--max-mbps",
            &args.max_mbps.to_string(),
        ])
        .args(args.net.to_args());
    if let Some(trace) = &args.rc_trace {
        host.arg("--rc-trace").arg(trace);
    }
    let mut host = host
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting the host")?;
    let ticket = read_ticket(&mut host, echo.clone()).await?;

    // The viewer window shows the clock's stream at its own size, so the copy
    // of the barcode on screen is pixel for pixel like the original.
    let (clock_w, clock_h) = latency_clock_size();
    let mut view_net = args.net.clone();
    if let Some(profile) = &args.netem_viewer {
        view_net.netem = Some(profile.clone());
    }
    let mut view = Command::new(&exe);
    view.args(["view", &ticket, "--measure-secs", &args.secs.to_string()])
        .args([
            "--viewer-pos",
            &VIEWER_POS.0.to_string(),
            &VIEWER_POS.1.to_string(),
        ])
        .args(["--viewer-size", &clock_w.to_string(), &clock_h.to_string()])
        .arg("--no-hud")
        .args(view_net.to_args());
    if let Some(csv) = &args.timings_csv {
        view.arg("--timings-csv").arg(csv);
    }
    let mut view = view
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting the viewer")?;

    let screen = if args.screen {
        tokio::time::sleep(Duration::from_secs(2)).await;
        Some(ScreenProbe::start().context("starting the screen probe")?)
    } else {
        None
    };

    let mut lines = BufReader::new(view.stdout.take().expect("piped")).lines();
    let mut result = None;
    while let Some(line) = lines.next_line().await? {
        match line.strip_prefix("RESULT ") {
            Some(json) => result = Some(serde_json::from_str::<serde_json::Value>(json)?),
            None => echo.line(&format!("[view] {line}")),
        }
    }
    let _ = view.wait().await;
    let screen = screen.map(ScreenProbe::finish);
    // Wait for both to exit, so a probe right after this one can't find the
    // old clock window.
    let _ = host.kill().await;
    let _ = clock.kill().await;
    let Some(result) = result else {
        bail!("the viewer exited without a result");
    };
    let g1 = args.net.netem.is_none() && !args.net.relay_only;
    for line in report(&result, g1) {
        echo.line(&line);
    }
    if let Some(screen) = screen {
        echo.line(&format!(
            "screen (clock -> viewer on screen)  {}",
            screen.summary_ms()
        ));
        let verdict = match screen.percentile(50.0) {
            Some(p50) if p50 <= 50.0 => "PASS",
            Some(_) => "FAIL",
            None => "NO DATA",
        };
        echo.line(&format!("G1b (screen: median <= 50 ms): {verdict}"));
    }
    Ok(result)
}

/// Reads the host's stdout until it prints its ticket, then keeps echoing it.
async fn read_ticket(host: &mut Child, echo: Echo) -> anyhow::Result<String> {
    let mut lines = BufReader::new(host.stdout.take().expect("piped")).lines();
    let ticket = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(line) = lines.next_line().await? {
            if let Some(ticket) = line.strip_prefix("TICKET ") {
                return Ok::<_, anyhow::Error>(ticket.to_string());
            }
            echo.line(&format!("[host] {line}"));
        }
        bail!("the host exited before printing a ticket")
    })
    .await
    .context("waiting for the host's ticket")??;
    tokio::spawn(async move {
        while let Ok(Some(line)) = lines.next_line().await {
            echo.line(&format!("[host] {line}"));
        }
    });
    Ok(ticket)
}

/// The summary lines, with the G1 verdict when the run measured G1 (direct,
/// unimpaired).
fn report(result: &serde_json::Value, g1: bool) -> Vec<String> {
    let get = |metric: &str, p: &str| result[metric][p].as_f64();
    let mut lines = vec![
        String::new(),
        format!(
            "frames {}  fps {:.1}  rtt {:.1} ms  relayed {}  lost packets {}",
            result["frames"],
            result["fps"].as_f64().unwrap_or(0.0),
            result["rtt_ms"].as_f64().unwrap_or(0.0),
            result["relayed"],
            result["lost_packets"]
        ),
        format!(
            "decode errors {}  path changes {}  ended early {}",
            result["decode_errors"], result["path_changes"], result["ended_early"]
        ),
    ];
    for metric in [
        "clock_to_present_ms",
        "capture_to_present_ms",
        "arrive_to_present_ms",
    ] {
        lines.push(format!(
            "{metric:<24} p50 {:>6.1}  p95 {:>6.1}  max {:>6.1}",
            get(metric, "p50").unwrap_or(f64::NAN),
            get(metric, "p95").unwrap_or(f64::NAN),
            get(metric, "max").unwrap_or(f64::NAN)
        ));
    }
    if g1 {
        let (p50, p95) = (
            get("clock_to_present_ms", "p50"),
            get("clock_to_present_ms", "p95"),
        );
        let verdict = match (p50, p95) {
            (Some(p50), Some(p95)) if p50 <= 25.0 && p95 <= 35.0 => "PASS",
            (Some(_), Some(_)) => "FAIL",
            _ => "NO DATA",
        };
        lines.push(format!(
            "G1 (clock -> present: p50 <= 25 ms, p95 <= 35 ms): {verdict}"
        ));
    }
    lines
}
