//! `couch-cli latency-probe`: the M1 measurement on one machine. Starts the
//! latency clock window, a host capturing it, and a viewer showing the stream
//! next to it; collects the viewer's numbers and checks them against the gates.
//! With `--audio`, the clock's clicks are timed too (M3, `audio_probe`).
//!
//! With `--viewers 2`, a second viewer watches too (gate G7), and
//! `--impair-first` impairs only the host's traffic to the first one. Every
//! process then runs on the netem transport, each viewer on a port of its
//! own, so the host can tell their traffic apart.
//!
//! With `--cpu-load`, busy threads load every core for the whole run, as a
//! game would, and report how much work they got done (the game's share);
//! `--cpus` confines the whole run to a few CPUs, a weaker PC on this one.
//! M9d measured thread and process priorities with them, and dropped both
//! (docs/m9d-results.md).

use std::{
    fs::File,
    io::{LineWriter, Write},
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, bail};
use couch_media::clock::{LATENCY_CLOCK_TITLE, latency_clock_size};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

use crate::{
    audio_probe::{AudioProbe, AudioResult},
    net_args::NetArgs,
    screen_probe::ScreenProbe,
};

#[derive(Debug, Clone)]
pub struct Args {
    pub secs: u64,
    pub fps: u32,
    pub max_mbps: f64,
    /// Also measure from screen captures (gate G1b).
    pub screen: bool,
    /// Also measure audio latency (gate A1, and click to playback).
    pub audio: bool,
    pub timings_csv: Option<PathBuf>,
    pub rc_trace: Option<PathBuf>,
    /// The viewer's netem profile, when it should differ from the host's.
    pub netem_viewer: Option<String>,
    /// 1, or 2 for G7.
    pub viewers: usize,
    /// With two viewers: a netem profile for the host's traffic to the
    /// first viewer only.
    pub impair_first: Option<String>,
    /// Write the host's and viewer's output and the report here instead of
    /// to the console.
    pub log: Option<PathBuf>,
    /// Busy threads at normal priority for the whole run, a stand-in for a
    /// game using every core (M9d); 0 means one per logical CPU.
    pub cpu_load: Option<usize>,
    /// Confine the whole run (this process and all it starts) to these
    /// logical CPUs: a weaker PC on this one (M9d).
    pub cpus: Option<Vec<u32>>,
    /// Cap the latency clock (the stand-in game) at this frame rate; by
    /// default it paints every refresh.
    pub clock_fps: Option<u32>,
    pub net: NetArgs,
}

/// Iterations each load thread runs between looks at the stop flag.
const LOAD_BATCH: u64 = 10_000;

/// Threads spinning at normal priority until dropped, counting their work:
/// the stand-in game's share of the CPU.
struct CpuLoad {
    stop: Arc<AtomicBool>,
    work: Arc<AtomicU64>,
    started: std::time::Instant,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl CpuLoad {
    fn start(threads: usize) -> anyhow::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let work = Arc::new(AtomicU64::new(0));
        let threads = (0..threads)
            .map(|i| {
                let (stop, work) = (stop.clone(), work.clone());
                std::thread::Builder::new()
                    .name(format!("cpu-load-{i}"))
                    .spawn(move || {
                        let mut x = 0u64;
                        while !stop.load(Ordering::Relaxed) {
                            for _ in 0..LOAD_BATCH {
                                x = std::hint::black_box(x.wrapping_mul(6364136223846793005) + 1);
                            }
                            work.fetch_add(LOAD_BATCH, Ordering::Relaxed);
                        }
                    })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            stop,
            work,
            started: std::time::Instant::now(),
            threads,
        })
    }

    /// The load's work so far, in millions of iterations a second.
    fn rate(&self) -> f64 {
        self.work.load(Ordering::Relaxed) as f64 / self.started.elapsed().as_secs_f64() / 1e6
    }
}

impl Drop for CpuLoad {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// `16-19` or `0,2,4-7`: logical CPU numbers.
pub fn parse_cpus(list: &str) -> Result<Vec<u32>, String> {
    let mut cpus = Vec::new();
    for part in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let number = |s: &str| {
            s.trim()
                .parse::<u32>()
                .map_err(|_| format!("{s:?} isn't a CPU number"))
        };
        match part.split_once('-') {
            Some((first, last)) => {
                let (first, last) = (number(first)?, number(last)?);
                if first > last {
                    return Err(format!("{part}: backwards"));
                }
                cpus.extend(first..=last);
            }
            None => cpus.push(number(part)?),
        }
    }
    if cpus.is_empty() {
        return Err("no CPUs".into());
    }
    Ok(cpus)
}

/// Clock window position; the viewers go below it at the same size.
const CLOCK_POS: (i32, i32) = (40, 40);
const VIEWER_POS: [(i32, i32); 2] = [(40, 300), (40, 520)];
/// With two viewers, their netem ports, so the host can impair one.
const VIEWER_PORTS: [u16; 2] = [51101, 51102];
/// netem without impairment: the transport alone.
const UNIMPAIRED: &str = "delay=0ms";

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

/// Runs the probe and returns the (first) viewer's `RESULT` (see `view`).
pub async fn run(args: Args) -> anyhow::Result<serde_json::Value> {
    Ok(run_all(args).await?.swap_remove(0))
}

/// Runs the probe and returns every viewer's `RESULT`, in order.
pub async fn run_all(mut args: Args) -> anyhow::Result<Vec<serde_json::Value>> {
    anyhow::ensure!(
        (1..=2).contains(&args.viewers),
        "one or two viewers, not {}",
        args.viewers
    );
    let two = args.viewers == 2;
    if two {
        anyhow::ensure!(
            !args.audio && !args.screen,
            "--audio and --screen measure one viewer"
        );
        args.net.netem = Some(match &args.impair_first {
            Some(profile) => format!("{profile},to={}", VIEWER_PORTS[0]),
            None => UNIMPAIRED.into(),
        });
    }
    let echo = match &args.log {
        None => Echo::Stdout,
        Some(path) => Echo::File(Arc::new(Mutex::new(LineWriter::new(
            File::create(path).with_context(|| format!("creating {}", path.display()))?,
        )))),
    };
    let exe = std::env::current_exe()?;
    let viewer_exe = exe.with_file_name(format!("couch-viewer{}", std::env::consts::EXE_SUFFIX));
    if let Some(cpus) = &args.cpus {
        couch_platform_win::process::confine_to_cpus(cpus)
            .with_context(|| format!("confining the run to CPUs {cpus:?}"))?;
        echo.line(&format!("confined to CPUs {cpus:?}"));
    }
    let load = match args.cpu_load {
        Some(n) => {
            let n = match (n, &args.cpus) {
                (0, Some(cpus)) => cpus.len(),
                (0, None) => std::thread::available_parallelism().map_or(1, usize::from),
                (n, _) => n,
            };
            echo.line(&format!("CPU load: {n} busy threads at normal priority"));
            Some(CpuLoad::start(n)?)
        }
        None => None,
    };

    let mut clock = Command::new(&viewer_exe);
    clock.args([
        "--clock",
        &CLOCK_POS.0.to_string(),
        &CLOCK_POS.1.to_string(),
    ]);
    if args.audio {
        clock.arg("--clicks");
    }
    if let Some(fps) = args.clock_fps {
        clock.args(["--fps", &fps.to_string()]);
    }
    let mut clock = clock
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
    let (ticket, mut connected) = read_ticket(&mut host, echo.clone()).await?;

    // The viewer window shows the clock's stream at its own size, so the copy
    // of the barcode on screen is pixel for pixel like the original.
    let (clock_w, clock_h) = latency_clock_size();
    let mut views = Vec::new();
    for i in 0..args.viewers {
        let mut view_net = args.net.clone();
        if let Some(profile) = &args.netem_viewer {
            view_net.netem = Some(profile.clone());
        }
        if two {
            view_net.netem = Some(UNIMPAIRED.into());
            view_net.port = Some(VIEWER_PORTS[i]);
        }
        let mut view = Command::new(&exe);
        view.args(["view", &ticket, "--measure-secs", &args.secs.to_string()])
            .args([
                "--viewer-pos",
                &VIEWER_POS[i].0.to_string(),
                &VIEWER_POS[i].1.to_string(),
            ])
            .args(["--viewer-size", &clock_w.to_string(), &clock_h.to_string()])
            .arg("--no-hud")
            .args(view_net.to_args());
        if let Some(port) = view_net.port {
            view.args(["--port", &port.to_string()]);
        }
        if let Some(csv) = &args.timings_csv {
            view.arg("--timings-csv").arg(numbered(csv, i));
        }
        let view = view
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("starting the viewer")?;
        views.push(view);
        if two && i == 0 {
            // The first to connect gets the host's first trace file.
            tokio::time::timeout(Duration::from_secs(15), connected.recv())
                .await
                .context("the first viewer never connected")?;
        }
    }

    let audio = if args.audio {
        let (Some(clock_pid), Some(view_pid)) = (clock.id(), views[0].id()) else {
            bail!("the clock or the viewer already exited");
        };
        Some(AudioProbe::start(clock_pid, view_pid).context("starting the audio probe")?)
    } else {
        None
    };
    let screen = if args.screen {
        tokio::time::sleep(Duration::from_secs(2)).await;
        Some(ScreenProbe::start().context("starting the screen probe")?)
    } else {
        None
    };

    let readers: Vec<_> = views
        .into_iter()
        .enumerate()
        .map(|(i, mut view)| {
            let echo = echo.clone();
            let prefix = if i == 0 {
                "[view]".to_string()
            } else {
                format!("[view {}]", i + 1)
            };
            tokio::spawn(async move {
                let mut lines = BufReader::new(view.stdout.take().expect("piped")).lines();
                let mut result = None;
                while let Some(line) = lines.next_line().await? {
                    match line.strip_prefix("RESULT ") {
                        Some(json) => {
                            result = Some(serde_json::from_str::<serde_json::Value>(json)?);
                        }
                        None => echo.line(&format!("{prefix} {line}")),
                    }
                }
                let _ = view.wait().await;
                Ok::<_, anyhow::Error>(result)
            })
        })
        .collect();
    let mut results = Vec::new();
    for reader in readers {
        results.push(reader.await.context("reading a viewer")??);
    }
    // What the stand-in game got done meanwhile: a stream that takes from
    // it shows here (M9d).
    let load_rate = load.as_ref().map(CpuLoad::rate);
    drop(load);
    let screen = screen.map(ScreenProbe::finish);
    let audio = audio
        .map(|probe| tokio::task::block_in_place(|| probe.finish()))
        .transpose()?;
    // Wait for both to exit, so a probe right after this one can't find the
    // old clock window.
    let _ = host.kill().await;
    let _ = clock.kill().await;
    let Some(results) = results.into_iter().collect::<Option<Vec<_>>>() else {
        bail!("a viewer exited without a result");
    };
    let g1 = args.net.netem.is_none() && !args.net.relay_only;
    for (i, result) in results.iter().enumerate() {
        if two {
            echo.line(&format!("viewer {}:", i + 1));
        }
        for line in report(result, g1) {
            echo.line(&line);
        }
    }
    if let Some(rate) = load_rate {
        echo.line(&format!(
            "cpu_load_work            {rate:.0} M iterations/s (the stand-in game's share)"
        ));
    }
    let result = &results[0];
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
    if let Some(audio) = audio {
        for line in audio_report(&audio, result, g1) {
            echo.line(&line);
        }
    }
    Ok(results)
}

/// `path` for the first viewer; `name-2.csv` for the second (as the host
/// numbers its traces).
fn numbered(path: &std::path::Path, i: usize) -> PathBuf {
    if i == 0 {
        return path.to_path_buf();
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path
        .extension()
        .map_or(String::new(), |e| format!(".{}", e.to_string_lossy()));
    path.with_file_name(format!("{stem}-{}{ext}", i + 1))
}

/// The audio lines: what our pipeline adds (judged as A1 when the run was
/// unimpaired), the jitter buffer's counts, and for information the whole
/// click-to-playback time and the offset from video. The last two include
/// Windows' own playback path, 25 to 40 ms on a typical device, which a game
/// played locally pays too (docs/m3-results.md).
fn audio_report(audio: &AudioResult, result: &serde_json::Value, judge: bool) -> Vec<String> {
    let a = &result["audio"];
    let count = |k: &str| a[k].as_u64().unwrap_or(0);
    let total = (count("played") + count("concealed")).max(1);
    let ours_p95 = a["capture_to_queue_p95_ms"].as_f64().filter(|&v| v > 0.0);
    let mut lines = vec![
        format!(
            "audio capture -> handed to the device  p50 {:.1} ms  p95 {}  buffer {:.0} ms (target {:.0})",
            a["capture_to_queue_ms"].as_f64().unwrap_or(0.0),
            ours_p95.map_or("-".into(), |v| format!("{v:.0} ms")),
            a["buffered_ms"].as_f64().unwrap_or(0.0),
            a["target_ms"].as_f64().unwrap_or(0.0),
        ),
        format!(
            "audio click -> playback  {}  ({} clicks)",
            audio.latency.summary_ms(),
            audio.clicks
        ),
        format!(
            "audio concealed {} of {} ({:.2}%)  late {}  dropped {}  underruns {}",
            count("concealed"),
            total,
            count("concealed") as f64 * 100.0 / total as f64,
            count("late"),
            count("dropped"),
            count("underruns"),
        ),
    ];
    let video_p50 = result["clock_to_present_ms"]["p50"].as_f64();
    let offset = audio
        .latency
        .percentile(50.0)
        .zip(video_p50)
        .map(|(a, v)| a - v);
    if let Some(offset) = offset {
        lines.push(format!("audio behind video by {offset:.1} ms (medians)"));
    }
    if judge {
        let a1 = match ours_p95 {
            // The histogram reports a bucket's top: p95 <= 25 ms is "25".
            Some(p95) if p95 <= 25.0 => "PASS",
            Some(_) => "FAIL",
            None => "NO DATA",
        };
        lines.push(format!(
            "A1 (capture -> handed to the device: p95 <= 25 ms): {a1}"
        ));
    }
    lines
}

/// Reads the host's stdout until it prints its ticket, then keeps echoing it.
/// The receiver hears of each viewer that connects.
async fn read_ticket(
    host: &mut Child,
    echo: Echo,
) -> anyhow::Result<(String, tokio::sync::mpsc::UnboundedReceiver<()>)> {
    let (connected_tx, connected) = tokio::sync::mpsc::unbounded_channel();
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
            if line.starts_with("viewer ") && line.ends_with(" connected") {
                let _ = connected_tx.send(());
            }
            echo.line(&format!("[host] {line}"));
        }
    });
    Ok((ticket, connected))
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
        "frame_gap_ms",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_lists_take_numbers_and_ranges() {
        assert_eq!(parse_cpus("16-19"), Ok(vec![16, 17, 18, 19]));
        assert_eq!(parse_cpus("0, 2,4-5"), Ok(vec![0, 2, 4, 5]));
        assert!(parse_cpus("").is_err());
        assert!(parse_cpus("5-3").is_err());
        assert!(parse_cpus("a").is_err());
    }
}
