//! `couch-cli gates`: runs M1's rate-control gates (G1, G3, G4, G5) through
//! `latency-probe` and judges them from its traces. Re-run after changing rate
//! control, congestion control or the video transport; the reference numbers
//! are in docs/m1-results.md.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use anyhow::{Context, anyhow, bail};
use clap::ValueEnum;
use couch_net::netem::{Change, NetemConfig};

use crate::{
    latency_probe,
    net_args::{CongestionArg, NetArgs},
    stats::Samples,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Gate {
    G1,
    G3,
    G4,
    G5,
}

#[derive(Debug, Clone)]
pub struct Args {
    /// Run only these; empty runs all.
    pub only: Vec<Gate>,
    /// G3 for 2 minutes instead of 10.
    pub quick: bool,
    pub out: PathBuf,
    pub congestion: CongestionArg,
}

const MAX_MBPS: f64 = 20.0;
/// The baseline is the median latency over this long before the event. Not
/// the whole run before it: latency rises as the bitrate ramps up toward a
/// rate cap.
const BASELINE_US: u64 = 2_000_000;
/// Latency is back when the median of this many frames...
const BACK_FRAMES: usize = 30;
/// ...is within this many ms of the baseline median.
const BACK_SLACK_MS: f64 = 5.0;

struct Spec {
    gate: Gate,
    what: &'static str,
    secs: u64,
    netem: Option<&'static str>,
    netem_viewer: Option<&'static str>,
}

fn specs(quick: bool) -> [Spec; 4] {
    [
        Spec {
            gate: Gate::G1,
            what: "direct, unimpaired",
            secs: 30,
            netem: None,
            netem_viewer: None,
        },
        Spec {
            gate: Gate::G3,
            what: "1% loss, 20 ms each way",
            secs: if quick { 120 } else { 600 },
            netem: Some("delay=20ms,loss=1%"),
            netem_viewer: None,
        },
        Spec {
            gate: Gate::G4,
            what: "2 s blackout at 10 s",
            secs: 30,
            netem: Some("at10s=blackout:2s"),
            netem_viewer: Some("delay=0ms"),
        },
        Spec {
            gate: Gate::G5,
            what: "throttle 25 to 5 Mbit/s at 10 s",
            secs: 30,
            netem: Some("rate=25mbit,at10s=rate:5mbit"),
            netem_viewer: Some("delay=0ms"),
        },
    ]
}

impl Gate {
    fn name(self) -> &'static str {
        match self {
            Gate::G1 => "G1",
            Gate::G3 => "G3",
            Gate::G4 => "G4",
            Gate::G5 => "G5",
        }
    }
}

/// One line of the verdict table. `pass: None` is information only.
struct Check {
    gate: Gate,
    name: &'static str,
    measured: String,
    target: String,
    pass: Option<bool>,
}

#[derive(Default)]
struct Report {
    checks: Vec<Check>,
    /// The controller's actions, for reading a failure.
    actions: String,
    /// G1's p95, the baseline for G3's limit.
    p95: Option<f64>,
}

impl Report {
    fn check(
        &mut self,
        gate: Gate,
        name: &'static str,
        measured: String,
        target: &str,
        pass: bool,
    ) {
        self.checks.push(Check {
            gate,
            name,
            measured,
            target: target.into(),
            pass: Some(pass),
        });
    }

    fn info(&mut self, gate: Gate, name: &'static str, measured: String) {
        self.checks.push(Check {
            gate,
            name,
            measured,
            target: String::new(),
            pass: None,
        });
    }
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    std::fs::create_dir_all(&args.out)
        .with_context(|| format!("creating {}", args.out.display()))?;
    let mut checks = Vec::new();
    let mut baseline_p95 = None;
    for spec in specs(args.quick) {
        if !args.only.is_empty() && !args.only.contains(&spec.gate) {
            continue;
        }
        let gate = spec.gate;
        println!("== {} ({}, {} s)", gate.name(), spec.what, spec.secs);
        let file = |suffix: &str| {
            args.out
                .join(format!("{}{suffix}", gate.name().to_ascii_lowercase()))
        };
        let (view_csv, rc_csv) = (file("-view.csv"), file("-rc.csv"));
        let probe = latency_probe::Args {
            secs: spec.secs,
            fps: 60,
            max_mbps: MAX_MBPS,
            screen: false,
            audio: false,
            timings_csv: Some(view_csv.clone()),
            rc_trace: Some(rc_csv.clone()),
            netem_viewer: spec.netem_viewer.map(Into::into),
            log: Some(file(".log")),
            net: NetArgs {
                relay: "none".into(),
                relay_only: false,
                port: None,
                congestion: args.congestion,
                netem: spec.netem.map(Into::into),
            },
        };
        let report = match latency_probe::run(probe).await {
            Ok(result) => judge(&spec, &result, &view_csv, &rc_csv, baseline_p95),
            Err(e) => Err(e),
        }
        .unwrap_or_else(|e| {
            let mut report = Report::default();
            report.check(gate, "run", format!("{e:#}"), "completes", false);
            report
        });
        for c in &report.checks {
            let verdict = match c.pass {
                Some(true) => "pass",
                Some(false) => "FAIL",
                None => "",
            };
            println!("   {:<28} {:<36} {verdict}", c.name, c.measured);
        }
        if !report.actions.is_empty() {
            println!("   actions: {}", report.actions);
        }
        if gate == Gate::G1 {
            baseline_p95 = report.p95;
        }
        checks.extend(report.checks);
    }

    println!();
    println!(
        "{:<4} {:<28} {:<36} {:<34} verdict",
        "gate", "check", "measured", "target"
    );
    for c in &checks {
        let verdict = match c.pass {
            Some(true) => "PASS",
            Some(false) => "FAIL",
            None => "-",
        };
        println!(
            "{:<4} {:<28} {:<36} {:<34} {verdict}",
            c.gate.name(),
            c.name,
            c.measured,
            c.target
        );
    }
    println!();
    println!(
        "traces and logs: {} (<gate>-rc.csv, <gate>-view.csv, <gate>.log)",
        args.out.display()
    );
    let failed = checks.iter().filter(|c| c.pass == Some(false)).count();
    if failed > 0 {
        bail!("{failed} check(s) failed");
    }
    Ok(())
}

fn judge(
    spec: &Spec,
    result: &serde_json::Value,
    view_csv: &Path,
    rc_csv: &Path,
    baseline_p95: Option<f64>,
) -> anyhow::Result<Report> {
    let gate = spec.gate;
    let frames = read_frames(view_csv)?;
    let ticks = read_ticks(rc_csv)?;
    if frames.is_empty() {
        bail!("no frames were presented");
    }
    let latency = |p: &str| {
        result["clock_to_present_ms"][p]
            .as_f64()
            .with_context(|| format!("no clock_to_present {p} in the result"))
    };
    let (p50, p95, max) = (latency("p50")?, latency("p95")?, latency("max")?);

    let mut report = Report::default();
    let decode_errors = result["decode_errors"].as_u64().unwrap_or(u64::MAX);
    let ended_early = result["ended_early"].as_bool().unwrap_or(true);
    report.check(
        gate,
        "full run, clean decodes",
        format!(
            "{}, {decode_errors} decode errors",
            if ended_early {
                "ended early"
            } else {
                "full run"
            }
        ),
        "full run, 0 decode errors",
        !ended_early && decode_errors == 0,
    );

    match gate {
        Gate::G1 => {
            report.check(
                gate,
                "clock->present p50 / p95",
                format!("{p50:.1} / {p95:.1} ms"),
                "<= 25 / 35 ms",
                p50 <= 25.0 && p95 <= 35.0,
            );
            report.p95 = Some(p95);
            report.actions = actions(&ticks, ticks.first().map_or(0, |t| t.qpc_us));
        }
        Gate::G3 => {
            let rtt = ticks.last().context("empty rc trace")?.min_rtt_us / 1000.0;
            let base = baseline_p95.unwrap_or(35.0);
            let limit = base + 2.0 * rtt + 20.0;
            report.check(
                gate,
                "clock->present p95",
                format!("{p95:.1} ms (p50 {p50:.1}, max {max:.0})"),
                &format!(
                    "<= {limit:.0} ms ({}{base:.1} + 2 x {rtt:.1} + 20)",
                    if baseline_p95.is_some() {
                        ""
                    } else {
                        "no G1: "
                    }
                ),
                p95 <= limit,
            );
            let minutes = per_minute_p95(&frames);
            if let (Some(last), Some(best)) = (
                minutes.last(),
                minutes.iter().copied().min_by(f64::total_cmp),
            ) && minutes.len() >= 2
            {
                let list: Vec<String> = minutes.iter().map(|m| format!("{m:.0}")).collect();
                report.check(
                    gate,
                    "drift: p95 last vs best min",
                    format!("{:+.1} ms ({})", last - best, list.join(" ")),
                    "<= +10 ms",
                    last - best <= 10.0,
                );
            }
            let mean = ticks.iter().map(|t| t.applied_bps).sum::<f64>() / ticks.len() as f64 / 1e6;
            report.check(
                gate,
                "mean bitrate",
                format!("{mean:.1} Mbit/s"),
                &format!("> {:.0} Mbit/s (holds under loss)", 0.75 * MAX_MBPS),
                mean > 0.75 * MAX_MBPS,
            );
            report.info(
                gate,
                "catch-ups",
                count(&ticks, &["catch-up"], 0).to_string(),
            );
            report.actions = actions(&ticks, ticks[0].qpc_us);
        }
        Gate::G4 => {
            let start = netem_start(&ticks)?;
            let (at, change) = scheduled(spec.netem.unwrap_or_default(), start)?;
            let Change::Blackout(length) = change else {
                bail!("G4's profile should schedule a blackout");
            };
            let end = at + length.as_micros() as u64;
            let baseline = latencies(&frames, at.saturating_sub(BASELINE_US), at)
                .percentile(50.0)
                .context("no frames before the outage")?;
            let first = frames
                .iter()
                .find(|f| f.presented_us >= end)
                .context("no frames after the outage")?;
            report.info(
                gate,
                "first frame after the outage",
                format!("{:.0} ms", ms(first.presented_us - end)),
            );
            let back = latency_back(&frames, end, baseline);
            report.check(
                gate,
                "latency back to baseline",
                back.map_or("never".into(), |us| {
                    format!("{:.0} ms (baseline p50 {baseline:.1})", ms(us))
                }),
                "<= 1000 ms after the outage",
                back.is_some_and(|us| us <= 1_000_000),
            );
            let keyframes = count(&ticks, &["resume", "catch-up"], at);
            report.check(
                gate,
                "recovery keyframes",
                keyframes.to_string(),
                "exactly 1 (resume + catch-ups)",
                keyframes == 1,
            );
            report.actions = actions(&ticks, at);
        }
        Gate::G5 => {
            let start = netem_start(&ticks)?;
            let (at, change) = scheduled(spec.netem.unwrap_or_default(), start)?;
            let Change::Rate(Some(link)) = change else {
                bail!("G5's profile should schedule a rate cap");
            };
            let (low, high) = (0.5 * link as f64, 1.2 * link as f64);
            let settle = settled(&ticks, at, low, high);
            report.check(
                gate,
                "bitrate settled",
                settle.map_or("never".into(), |us| format!("{:.2} s", us as f64 / 1e6)),
                &format!(
                    "<= 2 s, then stays {:.1}-{:.1} Mbit/s",
                    low / 1e6,
                    high / 1e6
                ),
                settle.is_some_and(|us| us <= 2_000_000),
            );
            let baseline = latencies(&frames, at.saturating_sub(BASELINE_US), at)
                .percentile(50.0)
                .context("no frames before the throttle")?;
            let back = latency_back(&frames, at, baseline);
            report.check(
                gate,
                "latency back to baseline",
                back.map_or("never".into(), |us| {
                    format!("{:.2} s (baseline p50 {baseline:.1})", us as f64 / 1e6)
                }),
                "<= 2 s after the throttle",
                back.is_some_and(|us| us <= 2_000_000),
            );
            let after = latencies(&frames, at, u64::MAX);
            report.info(
                gate,
                "worst frame after throttle",
                format!("{:.0} ms", after.percentile(100.0).unwrap_or(f64::NAN)),
            );
            report.info(gate, "clock->present p95", format!("{p95:.1} ms"));
            report.actions = actions(&ticks, at);
        }
    }
    Ok(report)
}

fn ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

/// A presented frame from the viewer's `--timings-csv`.
#[derive(Debug, Clone, Copy)]
struct Frame {
    presented_us: u64,
    /// 0 when the barcode wasn't read.
    barcode_us: u64,
}

impl Frame {
    fn latency_ms(&self) -> Option<f64> {
        (self.barcode_us != 0).then(|| ms(self.presented_us.saturating_sub(self.barcode_us)))
    }
}

/// A rate-controller tick from the host's `--rc-trace`.
#[derive(Debug, Clone, Default)]
struct Tick {
    qpc_us: u64,
    applied_bps: f64,
    min_rtt_us: f64,
    netem_us: Option<u64>,
    action: String,
}

/// One of our CSV traces: a header, then plain comma-separated rows.
struct Csv {
    columns: HashMap<String, usize>,
    rows: Vec<Vec<String>>,
}

impl Csv {
    fn parse(text: &str) -> anyhow::Result<Self> {
        let mut lines = text.lines();
        let header: Vec<&str> = lines.next().context("empty trace")?.split(',').collect();
        let rows = lines
            .map(|l| l.split(',').map(str::to_string).collect::<Vec<_>>())
            // A host killed mid-line leaves a short last row.
            .filter(|row| row.len() == header.len())
            .collect();
        let columns = header
            .into_iter()
            .enumerate()
            .map(|(i, c)| (c.to_string(), i))
            .collect();
        Ok(Self { columns, rows })
    }

    fn read(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Column `name` of `row`, `None` when empty.
    fn get<T: FromStr>(&self, row: &[String], name: &str) -> anyhow::Result<Option<T>> {
        let i = *self
            .columns
            .get(name)
            .with_context(|| format!("no {name} column"))?;
        match row[i].as_str() {
            "" => Ok(None),
            v => v
                .parse()
                .map(Some)
                .map_err(|_| anyhow!("bad {name} value {v:?}")),
        }
    }

    fn need<T: FromStr>(&self, row: &[String], name: &str) -> anyhow::Result<T> {
        self.get(row, name)?
            .with_context(|| format!("empty {name} value"))
    }
}

fn read_frames(path: &Path) -> anyhow::Result<Vec<Frame>> {
    let csv = Csv::read(path)?;
    csv.rows
        .iter()
        .map(|row| {
            Ok(Frame {
                presented_us: csv.need(row, "presented_us")?,
                barcode_us: csv.need(row, "barcode_us")?,
            })
        })
        .collect()
}

fn read_ticks(path: &Path) -> anyhow::Result<Vec<Tick>> {
    let csv = Csv::read(path)?;
    let ticks: Vec<Tick> = csv
        .rows
        .iter()
        .map(|row| {
            Ok(Tick {
                qpc_us: csv.need(row, "qpc_us")?,
                applied_bps: csv.need(row, "applied_bps")?,
                min_rtt_us: csv.need(row, "min_rtt_us")?,
                netem_us: csv.get(row, "netem_us")?,
                action: csv.get(row, "action")?.unwrap_or_default(),
            })
        })
        .collect::<anyhow::Result<_>>()?;
    if ticks.is_empty() {
        bail!("{} has no controller ticks", path.display());
    }
    Ok(ticks)
}

/// QPC time the host's netem link started; its schedule counts from there.
fn netem_start(ticks: &[Tick]) -> anyhow::Result<u64> {
    ticks
        .iter()
        .find_map(|t| t.netem_us.map(|n| t.qpc_us.saturating_sub(n)))
        .context("the rc trace has no netem_us times")
}

/// QPC time and kind of the first change scheduled in `profile`.
fn scheduled(profile: &str, netem_start: u64) -> anyhow::Result<(u64, Change)> {
    let config: NetemConfig = profile.parse().map_err(anyhow::Error::msg)?;
    let (at, change) = config
        .schedule
        .first()
        .cloned()
        .context("the profile schedules nothing")?;
    Ok((netem_start + at.as_micros() as u64, change))
}

/// Latencies of the frames presented in `[from, to)`.
fn latencies(frames: &[Frame], from: u64, to: u64) -> Samples {
    let mut samples = Samples::default();
    for f in frames {
        if (from..to).contains(&f.presented_us)
            && let Some(l) = f.latency_ms()
        {
            samples.push(l);
        }
    }
    samples
}

/// Each minute's p95 latency. A last part under 30 s is left out.
fn per_minute_p95(frames: &[Frame]) -> Vec<f64> {
    let (Some(first), Some(last)) = (frames.first(), frames.last()) else {
        return Vec::new();
    };
    let minute = Duration::from_secs(60).as_micros() as u64;
    let mut out = Vec::new();
    let mut from = first.presented_us;
    while last.presented_us >= from + minute / 2 {
        if let Some(p95) = latencies(frames, from, from + minute).percentile(95.0) {
            out.push(p95);
        }
        from += minute;
    }
    out
}

/// How long after `from` latency first comes back: the median of the next
/// `BACK_FRAMES` frames within `BACK_SLACK_MS` of `baseline` ms.
fn latency_back(frames: &[Frame], from: u64, baseline: f64) -> Option<u64> {
    let after: Vec<(u64, f64)> = frames
        .iter()
        .filter(|f| f.presented_us >= from)
        .filter_map(|f| Some((f.presented_us, f.latency_ms()?)))
        .collect();
    after.windows(BACK_FRAMES).find_map(|w| {
        let mut window = Samples::default();
        for &(_, l) in w {
            window.push(l);
        }
        let median = window.percentile(50.0)?;
        (median <= baseline + BACK_SLACK_MS).then(|| w[0].0 - from)
    })
}

/// How long after `from` the applied bitrate enters `[low, high]` bps for
/// good: `None` if it is outside at the end of the trace.
fn settled(ticks: &[Tick], from: u64, low: f64, high: f64) -> Option<u64> {
    let after: Vec<&Tick> = ticks.iter().filter(|t| t.qpc_us >= from).collect();
    let inside = |t: &Tick| (low..=high).contains(&t.applied_bps);
    let first_for_good = match after.iter().rposition(|t| !inside(t)) {
        None => 0,
        Some(i) => i + 1,
    };
    after.get(first_for_good).map(|t| t.qpc_us - from)
}

/// Ticks from `from` on whose action starts with one of `kinds`.
fn count(ticks: &[Tick], kinds: &[&str], from: u64) -> usize {
    ticks
        .iter()
        .filter(|t| t.qpc_us >= from && kinds.iter().any(|k| t.action.starts_with(k)))
        .count()
}

/// The controller's actions, timed from `origin`, bitrates in Mbit/s.
fn actions(ticks: &[Tick], origin: u64) -> String {
    const MAX: usize = 12;
    let all: Vec<String> = ticks
        .iter()
        .filter(|t| !t.action.is_empty())
        .map(|t| {
            let secs = (t.qpc_us as f64 - origin as f64) / 1e6;
            let action = match t.action.split_once(' ') {
                Some((verb, bps)) => match bps.parse::<f64>() {
                    Ok(bps) => format!("{verb} {:.1}", bps / 1e6),
                    Err(_) => t.action.clone(),
                },
                None => t.action.clone(),
            };
            format!("{secs:+.2}s {action}")
        })
        .collect();
    let mut out = all.iter().take(MAX).cloned().collect::<Vec<_>>().join(", ");
    if all.len() > MAX {
        out.push_str(&format!(", ... ({} more)", all.len() - MAX));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames 60 a second from `start_us`, each with `latency(i)` ms.
    fn frames(start_us: u64, n: usize, latency: impl Fn(usize) -> f64) -> Vec<Frame> {
        (0..n)
            .map(|i| {
                let presented_us = start_us + i as u64 * 16_667;
                Frame {
                    presented_us,
                    barcode_us: presented_us - (latency(i) * 1000.0) as u64,
                }
            })
            .collect()
    }

    fn tick(qpc_us: u64, applied_mbps: f64, action: &str) -> Tick {
        Tick {
            qpc_us,
            applied_bps: applied_mbps * 1e6,
            action: action.into(),
            ..Default::default()
        }
    }

    #[test]
    fn csv_skips_a_cut_off_last_row() {
        let csv = Csv::parse("a,b,c\n1,,x\n2,3,y\n4,5").unwrap();
        assert_eq!(csv.rows.len(), 2);
        assert_eq!(csv.get::<u32>(&csv.rows[0], "b").unwrap(), None);
        assert_eq!(csv.need::<u32>(&csv.rows[1], "b").unwrap(), 3);
        assert!(csv.need::<u32>(&csv.rows[0], "nope").is_err());
    }

    #[test]
    fn minutes_drop_a_short_tail() {
        // 152 s: two full minutes and 32 s, which counts.
        let f = frames(1_000_000, 9100, |i| if i < 3600 { 20.0 } else { 40.0 });
        let minutes = per_minute_p95(&f);
        assert_eq!(minutes.len(), 3);
        assert!((minutes[0] - 20.0).abs() < 0.01);
        assert!((minutes[2] - 40.0).abs() < 0.01);
        // 110 s: the 50 s tail counts, 10 s would not.
        assert_eq!(per_minute_p95(&f[..6600]).len(), 2);
        assert_eq!(per_minute_p95(&f[..4200]).len(), 1);
    }

    #[test]
    fn latency_is_back_when_the_median_returns() {
        // 300 ms for the first 20 frames after the event, then 15 ms.
        let f = frames(1_000_000, 200, |i| if i < 20 { 300.0 } else { 15.0 });
        // From frame 5 on, a window holds at least 15 fast frames, so its
        // median (the 15th smallest) is fast.
        assert_eq!(
            latency_back(&f, 1_000_000, 14.0),
            Some(f[5].presented_us - 1_000_000)
        );
        assert_eq!(latency_back(&f, 1_000_000, 5.0), None);
    }

    #[test]
    fn bitrate_settles_when_it_stays_in_the_band() {
        let ticks = [
            tick(0, 20.0, ""),
            tick(1_000, 20.0, ""),
            tick(2_000, 4.0, "catch-up 4000000"),
            tick(3_000, 6.5, "set 6500000"),
            tick(4_000, 5.0, "set 5000000"),
            tick(5_000, 5.5, ""),
        ];
        assert_eq!(settled(&ticks, 1_000, 2.5e6, 6e6), Some(3_000));
        assert_eq!(settled(&ticks[..4], 1_000, 2.5e6, 6e6), None);
        assert_eq!(settled(&ticks[4..], 1_000, 2.5e6, 6e6), Some(3_000));
        assert_eq!(count(&ticks, &["catch-up", "resume"], 0), 1);
    }

    #[test]
    fn actions_read_in_mbit_and_seconds() {
        let ticks = [
            tick(0, 20.0, ""),
            tick(500_000, 20.0, "pause"),
            tick(2_250_000, 10.0, "resume 10000000"),
        ];
        assert_eq!(
            actions(&ticks, 1_000_000),
            "-0.50s pause, +1.25s resume 10.0"
        );
    }

    #[test]
    fn schedules_are_placed_from_the_trace() {
        let mut t = tick(5_000_000, 20.0, "");
        t.netem_us = Some(4_000_000);
        assert_eq!(netem_start(&[tick(0, 1.0, ""), t]).unwrap(), 1_000_000);
        let (at, change) = scheduled("rate=25mbit,at10s=rate:5mbit", 1_000_000).unwrap();
        assert_eq!(at, 11_000_000);
        assert_eq!(change, Change::Rate(Some(5_000_000)));
    }
}
