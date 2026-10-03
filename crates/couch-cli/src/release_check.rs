//! `couch-cli release-check`: stages the ways a session ends badly and checks
//! that the host releases the viewer's input each time, in time. Windows
//! keeps injected keys down until something lets go of them, so each of these
//! is a key that could stay held.
//!
//! Each scenario starts a host (test pattern, its own process group) and a
//! viewer on this machine, waits for the seat to go active, does one thing,
//! and times the host's `EVENT` line against the moment it did it, both on
//! the QPC clock.

use std::{
    io::{BufRead, BufReader},
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use couch_platform_win::{clock, process};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    KillView,
    KillViewer,
    CloseViewer,
    FreezeViewer,
    StopHost,
}

impl Scenario {
    const ALL: [Self; 5] = [
        Self::KillView,
        Self::KillViewer,
        Self::CloseViewer,
        Self::FreezeViewer,
        Self::StopHost,
    ];

    fn describe(self) -> &'static str {
        match self {
            Self::KillView => "viewer's main process killed",
            Self::KillViewer => "viewer window process killed",
            Self::CloseViewer => "viewer window closed",
            Self::FreezeViewer => "viewer window process frozen",
            Self::StopHost => "host stopped (Ctrl+Break)",
        }
    }

    /// The host events that count as releasing, and the time allowed.
    fn expect(self) -> (&'static [&'static str], Duration) {
        let ms = Duration::from_millis;
        match self {
            // Nothing tells the host: its 300 ms dead-man timer, checked
            // every 10 ms, plus a margin for scheduling.
            Self::KillView => (&["suspended", "gone"], ms(350)),
            // `view` sees its viewer go and closes the connection.
            Self::KillViewer | Self::CloseViewer => (&["gone", "suspended"], ms(350)),
            // `view` stops the heartbeat after 250 ms without the viewer;
            // the last refresh can go out just before that, then the
            // dead-man's 300 ms.
            Self::FreezeViewer => (&["suspended"], ms(600)),
            Self::StopHost => (&["stopped"], ms(500)),
        }
    }
}

enum Line {
    Ticket(String),
    Event(String, u64),
}

/// A host and a viewer on this machine; both are killed when dropped.
struct Session {
    host: Child,
    view: Child,
    lines: mpsc::Receiver<Line>,
}

impl Drop for Session {
    fn drop(&mut self) {
        for viewer in process::child_processes(self.view.id()).unwrap_or_default() {
            let _ = process::terminate(viewer);
        }
        let _ = self.view.kill();
        let _ = self.host.kill();
        let _ = self.view.wait();
        let _ = self.host.wait();
    }
}

impl Session {
    fn start() -> anyhow::Result<Self> {
        let exe = std::env::current_exe()?;
        let mut host = Command::new(&exe)
            .args([
                "host",
                "--source",
                "testpattern",
                "--pattern-size",
                "1280",
                "720",
                "--fps",
                "30",
                "--relay",
                "none",
            ])
            .creation_flags(process::CREATE_NEW_PROCESS_GROUP)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting the host")?;
        let (tx, lines) = mpsc::channel();
        let stdout = host.stdout.take().expect("piped");
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let parsed = if let Some(ticket) = line.strip_prefix("TICKET ") {
                    Line::Ticket(ticket.to_string())
                } else if let Some(event) = line.strip_prefix("EVENT ") {
                    let mut parts = event.split(' ');
                    let (Some(what), Some(at)) = (parts.next(), parts.next()) else {
                        continue;
                    };
                    let Ok(at) = at.parse() else {
                        continue;
                    };
                    Line::Event(what.to_string(), at)
                } else {
                    continue;
                };
                if tx.send(parsed).is_err() {
                    return;
                }
            }
        });
        let ticket = match lines.recv_timeout(Duration::from_secs(15)) {
            Ok(Line::Ticket(t)) => t,
            _ => bail!("the host printed no ticket"),
        };
        let view = Command::new(&exe)
            .args(["view", &ticket, "--relay", "none"])
            .args(["--viewer-pos", "40", "40", "--viewer-size", "640", "360"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("starting the viewer")?;
        let mut session = Self { host, view, lines };
        session
            .wait_for(&["active"], Duration::from_secs(20))
            .context("the seat never went active")?;
        // Let it settle: heartbeats flowing, video running.
        std::thread::sleep(Duration::from_secs(1));
        Ok(session)
    }

    /// The next host event among `names`, with its QPC time.
    fn wait_for(&mut self, names: &[&str], timeout: Duration) -> Option<(String, u64)> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            match self.lines.recv_timeout(left) {
                Ok(Line::Event(what, at)) if names.contains(&what.as_str()) => {
                    return Some((what, at));
                }
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }

    fn viewer_pid(&self) -> anyhow::Result<u32> {
        process::child_processes(self.view.id())?
            .first()
            .copied()
            .context("the viewer window process isn't running")
    }
}

/// Polls until `child` exits or `timeout` passes.
fn exits_within(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

struct Outcome {
    measured: String,
    pass: bool,
}

fn run_one(scenario: Scenario) -> anyhow::Result<Outcome> {
    let mut s = Session::start()?;
    let (events, limit) = scenario.expect();
    let mut notes = Vec::new();
    let acted = clock::now().0;
    // Held until the scenario's end, so a frozen viewer stays frozen.
    let mut frozen = None;
    match scenario {
        Scenario::KillView => s.view.kill()?,
        Scenario::KillViewer => process::terminate(s.viewer_pid()?)?,
        Scenario::CloseViewer => {
            if process::close_windows(s.viewer_pid()?) == 0 {
                bail!("the viewer has no window to close");
            }
        }
        Scenario::FreezeViewer => frozen = Some(process::freeze(s.viewer_pid()?)?),
        Scenario::StopHost => process::ctrl_break(s.host.id())?,
    }
    let released = s.wait_for(events, Duration::from_secs(3));
    let mut pass = match &released {
        Some((_, at)) => Duration::from_micros(at.saturating_sub(acted)) <= limit,
        None => false,
    };
    let measured = match &released {
        Some((what, at)) => format!("{what} after {} ms", at.saturating_sub(acted) / 1000),
        None => "no release within 3 s".into(),
    };
    match scenario {
        Scenario::CloseViewer => {
            let viewer_gone =
                s.viewer_pid().is_err() || exits_within(&mut s.view, Duration::from_secs(2));
            if !viewer_gone {
                notes.push("viewer still running");
                pass = false;
            }
        }
        Scenario::FreezeViewer => {
            drop(frozen.take());
            if s.wait_for(&["active"], Duration::from_secs(2)).is_some() {
                notes.push("active again after thawing");
            } else {
                notes.push("NOT active again after thawing");
                pass = false;
            }
        }
        Scenario::StopHost => {
            if !exits_within(&mut s.host, Duration::from_secs(3)) {
                notes.push("host didn't exit");
                pass = false;
            }
        }
        Scenario::KillView | Scenario::KillViewer => {}
    }
    let measured = if notes.is_empty() {
        measured
    } else {
        format!("{measured}; {}", notes.join("; "))
    };
    Ok(Outcome { measured, pass })
}

pub fn run() -> anyhow::Result<()> {
    println!("Each scenario opens a viewer window for a few seconds; leave them be.");
    let mut failed = 0;
    let mut rows = Vec::new();
    for scenario in Scenario::ALL {
        let (_, limit) = scenario.expect();
        let outcome = run_one(scenario).unwrap_or_else(|e| Outcome {
            measured: format!("error: {e:#}"),
            pass: false,
        });
        failed += usize::from(!outcome.pass);
        println!(
            "  {:<30} {}  {}",
            scenario.describe(),
            if outcome.pass { "pass" } else { "FAIL" },
            outcome.measured
        );
        rows.push((scenario, limit, outcome));
        std::thread::sleep(Duration::from_millis(500));
    }
    println!();
    println!(
        "{:<30} {:<56} {:<10} verdict",
        "scenario", "measured", "target"
    );
    for (scenario, limit, outcome) in &rows {
        println!(
            "{:<30} {:<56} {:<10} {}",
            scenario.describe(),
            outcome.measured,
            format!("<= {} ms", limit.as_millis()),
            if outcome.pass { "PASS" } else { "FAIL" }
        );
    }
    if failed > 0 {
        bail!("{failed} scenario(s) failed");
    }
    Ok(())
}
