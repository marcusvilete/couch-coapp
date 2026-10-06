//! `couch-cli lifecycle-check`: stages how a session's source and host go
//! away, and checks each is noticed in time.
//!
//! - A game that swaps its window: the host follows the new one.
//! - A game that exits: the session ends with "game exited".
//! - A window that closes while its process runs: "source lost", after the
//!   grace for a new window.
//! - A host that is killed: the viewer notices from the missing `Alive`.
//! - A host killed while a key is held: Windows keeps the key down; the next
//!   host start lets go of it.
//!
//! The fake game, host and viewer print `EVENT <what> <QPC µs>` lines; times
//! are taken between them, all on the QPC clock.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use couch_platform_win::{clock, fake_game, process, sendinput};

/// F24: no program uses it, so holding it down harms nothing.
const F24_USAGE: &str = "0x73";
const VK_F24: u16 = 0x87;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Swap,
    Exit,
    Close,
    KillViewer,
    KillHost,
    StaleKey,
}

impl Scenario {
    const ALL: [Self; 6] = [
        Self::Swap,
        Self::Exit,
        Self::Close,
        Self::KillViewer,
        Self::KillHost,
        Self::StaleKey,
    ];

    fn describe(self) -> &'static str {
        match self {
            Self::Swap => "game swaps its window",
            Self::Exit => "game exits",
            Self::Close => "window closes, game runs",
            Self::KillViewer => "stream window killed",
            Self::KillHost => "host killed",
            Self::StaleKey => "host killed holding a key",
        }
    }

    /// For `--only`.
    fn name(self) -> &'static str {
        match self {
            Self::Swap => "swap",
            Self::Exit => "exit",
            Self::Close => "close",
            Self::KillViewer => "kill-viewer",
            Self::KillHost => "kill-host",
            Self::StaleKey => "stale-key",
        }
    }

    fn target(self) -> &'static str {
        match self {
            Self::Swap => "followed <= 1 s",
            Self::Exit => "game exited <= 3 s",
            Self::Close => "source lost 5-6 s",
            Self::KillViewer => "restarted <= 2 s",
            Self::KillHost => "lost <= 5.5 s",
            Self::StaleKey => "released <= 1 s",
        }
    }
}

/// Who printed a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Who {
    Game,
    Host,
    View,
    Helper,
}

enum Line {
    Ticket(String),
    Event(Who, String, u64),
}

/// Processes for one scenario; all killed when dropped.
struct Run {
    children: Vec<Child>,
    game_in: Option<ChildStdin>,
    tx: mpsc::Sender<Line>,
    lines: mpsc::Receiver<Line>,
}

impl Drop for Run {
    fn drop(&mut self) {
        for child in &mut self.children {
            for grandchild in process::child_processes(child.id()).unwrap_or_default() {
                let _ = process::terminate(grandchild);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Run {
    fn new() -> Self {
        let (tx, lines) = mpsc::channel();
        Self {
            children: Vec::new(),
            game_in: None,
            tx,
            lines,
        }
    }

    /// Starts `couch-cli <args>`, reading its `TICKET` and `EVENT` lines.
    fn spawn(&mut self, who: Who, args: &[&str]) -> anyhow::Result<usize> {
        let exe = std::env::current_exe()?;
        let mut child = Command::new(&exe)
            .args(args)
            .stdin(if who == Who::Game {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("starting {who:?}"))?;
        if who == Who::Game {
            self.game_in = child.stdin.take();
        }
        let stdout = child.stdout.take().expect("piped");
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let parsed = if let Some(ticket) = line.strip_prefix("TICKET ") {
                    Line::Ticket(ticket.to_string())
                } else if let Some(event) = line.strip_prefix("EVENT ") {
                    let mut parts = event.split(' ');
                    let (Some(what), Some(Ok(at))) = (parts.next(), parts.next().map(str::parse))
                    else {
                        continue;
                    };
                    Line::Event(who, what.to_string(), at)
                } else {
                    continue;
                };
                if tx.send(parsed).is_err() {
                    return;
                }
            }
        });
        self.children.push(child);
        Ok(self.children.len() - 1)
    }

    fn tell_game(&mut self, what: &str) -> anyhow::Result<()> {
        let game = self.game_in.as_mut().context("no fake game")?;
        writeln!(game, "{what}")?;
        game.flush()?;
        Ok(())
    }

    fn ticket(&self) -> anyhow::Result<String> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .context("no ticket")?;
            if let Ok(Line::Ticket(t)) = self.lines.recv_timeout(left) {
                return Ok(t);
            }
        }
    }

    /// The next event from `who` among `names`, with its time.
    fn wait_for(&self, who: Who, names: &[&str], timeout: Duration) -> Option<(String, u64)> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            match self.lines.recv_timeout(left) {
                Ok(Line::Event(w, what, at)) if w == who && names.contains(&what.as_str()) => {
                    return Some((what, at));
                }
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }

    /// A host streaming `source_args`, and a viewer that restarts its window
    /// as the app does, with the seat active. Returns their indices.
    fn session(&mut self, source_args: &[&str]) -> anyhow::Result<(usize, usize)> {
        let mut args = vec!["host", "--relay", "none", "--audio", "off", "--fps", "30"];
        args.extend_from_slice(source_args);
        let host = self.spawn(Who::Host, &args)?;
        let ticket = self.ticket()?;
        let view = self.spawn(
            Who::View,
            &[
                "view",
                &ticket,
                "--relay",
                "none",
                "--restart-viewer",
                "--viewer-pos",
                "820",
                "120",
                "--viewer-size",
                "480",
                "270",
            ],
        )?;
        self.wait_for(Who::Host, &["active"], Duration::from_secs(20))
            .context("the seat never went active")?;
        std::thread::sleep(Duration::from_secs(1));
        Ok((host, view))
    }

    fn game_session(&mut self) -> anyhow::Result<()> {
        self.spawn(Who::Game, &["fake-game"])?;
        self.wait_for(Who::Game, &["fake-shown"], Duration::from_secs(10))
            .context("the fake game didn't show")?;
        self.session(&["--source", "window", "--title", fake_game::TITLE])?;
        Ok(())
    }
}

fn ms(us: u64) -> u64 {
    us / 1000
}

struct Outcome {
    measured: String,
    pass: bool,
}

fn outcome(pass: bool, measured: String) -> anyhow::Result<Outcome> {
    Ok(Outcome { measured, pass })
}

fn run_one(scenario: Scenario) -> anyhow::Result<Outcome> {
    let mut run = Run::new();
    match scenario {
        Scenario::Swap => {
            run.game_session()?;
            run.tell_game("swap")?;
            let (_, swapped) = run
                .wait_for(Who::Game, &["fake-swapped"], Duration::from_secs(5))
                .context("the fake game didn't swap")?;
            let Some((_, followed)) =
                run.wait_for(Who::Host, &["followed"], Duration::from_secs(6))
            else {
                return outcome(false, "never followed".into());
            };
            let after = followed.saturating_sub(swapped);
            // Still streaming a while later.
            let ended = run.wait_for(
                Who::Host,
                &["source-lost", "game-exited"],
                Duration::from_secs(3),
            );
            let mut measured = format!("followed after {} ms", ms(after));
            if let Some((what, _)) = &ended {
                measured.push_str(&format!("; then {what}"));
            }
            outcome(after <= 1_000_000 && ended.is_none(), measured)
        }
        Scenario::Exit => {
            run.game_session()?;
            run.tell_game("exit")?;
            let (_, exiting) = run
                .wait_for(Who::Game, &["fake-exiting"], Duration::from_secs(5))
                .context("the fake game didn't exit")?;
            match run.wait_for(
                Who::Host,
                &["game-exited", "source-lost"],
                Duration::from_secs(8),
            ) {
                Some((what, at)) => {
                    let after = at.saturating_sub(exiting);
                    outcome(
                        what == "game-exited" && after <= 3_000_000,
                        format!("{what} after {} ms", ms(after)),
                    )
                }
                None => outcome(false, "the session didn't end".into()),
            }
        }
        Scenario::Close => {
            run.game_session()?;
            run.tell_game("close")?;
            let (_, closed) = run
                .wait_for(Who::Game, &["fake-closed"], Duration::from_secs(5))
                .context("the fake game didn't close its window")?;
            match run.wait_for(
                Who::Host,
                &["game-exited", "source-lost"],
                Duration::from_secs(10),
            ) {
                Some((what, at)) => {
                    let after = at.saturating_sub(closed);
                    outcome(
                        what == "source-lost" && (5_000_000..=6_000_000).contains(&after),
                        format!("{what} after {} ms", ms(after)),
                    )
                }
                None => outcome(false, "the session didn't end".into()),
            }
        }
        Scenario::KillViewer => {
            let (_, view) =
                run.session(&["--source", "testpattern", "--pattern-size", "640", "360"])?;
            let window = process::child_processes(run.children[view].id())?
                .first()
                .copied()
                .context("no viewer window process")?;
            let killed = clock::now().0;
            process::terminate(window)?;
            let Some((what, at)) = run.wait_for(
                Who::View,
                &["viewer-restarted", "viewer-crashed", "viewer-closed"],
                Duration::from_secs(5),
            ) else {
                return outcome(false, "nothing happened".into());
            };
            let after = at.saturating_sub(killed);
            // The dead-man timer suspends the seat unless the new window was
            // quicker; its heartbeat then brings the seat back.
            let seat =
                match run.wait_for(Who::Host, &["suspended", "active"], Duration::from_secs(3)) {
                    None => Some("the seat was never suspended".to_string()),
                    Some((what, at)) if what == "active" => Some(format!(
                        "seat active again after {} ms",
                        ms(at.saturating_sub(killed))
                    )),
                    Some(_) => run
                        .wait_for(Who::Host, &["active"], Duration::from_secs(5))
                        .map(|(_, at)| {
                            format!(
                                "seat suspended, then active again after {} ms",
                                ms(at.saturating_sub(killed))
                            )
                        }),
                };
            let measured = format!(
                "{what} after {} ms; {}",
                ms(after),
                seat.as_deref().unwrap_or("seat NOT active again")
            );
            outcome(
                what == "viewer-restarted" && after <= 2_000_000 && seat.is_some(),
                measured,
            )
        }
        Scenario::KillHost => {
            let (host, _) =
                run.session(&["--source", "testpattern", "--pattern-size", "640", "360"])?;
            let killed = clock::now().0;
            run.children[host].kill()?;
            match run.wait_for(Who::View, &["connection-lost"], Duration::from_secs(20)) {
                Some((_, at)) => {
                    let after = at.saturating_sub(killed);
                    outcome(after <= 5_500_000, format!("lost after {} ms", ms(after)))
                }
                None => outcome(false, "the viewer never noticed".into()),
            }
        }
        Scenario::StaleKey => stale_key(&mut run),
    }
}

/// A process holding F24 through the host's injector is killed; the next
/// host start must let go of it.
fn stale_key(run: &mut Run) -> anyhow::Result<Outcome> {
    if sendinput::vk_down(VK_F24) {
        bail!("F24 is already down before the test");
    }
    let helper = run.spawn(Who::Helper, &["hold-key", F24_USAGE])?;
    run.wait_for(Who::Helper, &["held"], Duration::from_secs(10))
        .context("the helper didn't hold the key")?;
    // Its marker is written within about 20 ms.
    std::thread::sleep(Duration::from_millis(300));
    process::terminate(run.children[helper].id())?;
    std::thread::sleep(Duration::from_millis(200));
    let stuck = sendinput::vk_down(VK_F24);
    run.spawn(
        Who::Host,
        &[
            "host",
            "--relay",
            "none",
            "--source",
            "testpattern",
            "--pattern-size",
            "320",
            "180",
        ],
    )?;
    let started = run
        .wait_for(Who::Host, &["started"], Duration::from_secs(15))
        .map(|(_, at)| at);
    let deadline = Instant::now() + Duration::from_secs(1);
    while sendinput::vk_down(VK_F24) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let released_at = clock::now().0;
    let still = sendinput::vk_down(VK_F24);
    if still {
        // Never leave it down, whatever the verdict.
        let _ = couch_input::KbmInjector::key(
            &mut sendinput::SendInputInjector::new(),
            couch_input::HidUsage(0x73),
            false,
        );
    }
    let measured = match (stuck, still, started) {
        (false, _, _) => "F24 wasn't left down after the kill: nothing to show".to_string(),
        (true, true, _) => "still down 1 s after the host started".to_string(),
        (true, false, Some(at)) => format!(
            "left down by the kill; released {} ms after the host started",
            ms(released_at.saturating_sub(at))
        ),
        (true, false, None) => "released, but the host never said it started".to_string(),
    };
    outcome(stuck && !still && started.is_some(), measured)
}

/// Runs every scenario, or only those named (`swap`, `exit`, `close`,
/// `kill-host`, `stale-key`).
pub fn run(only: &[String]) -> anyhow::Result<()> {
    println!(
        "Each scenario opens a fake game and a viewer window for a few seconds; leave them be."
    );
    let mut failed = 0;
    let mut rows = Vec::new();
    let chosen = Scenario::ALL
        .into_iter()
        .filter(|s| only.is_empty() || only.iter().any(|o| o == s.name()));
    for scenario in chosen {
        let outcome = run_one(scenario).unwrap_or_else(|e| Outcome {
            measured: format!("error: {e:#}"),
            pass: false,
        });
        failed += usize::from(!outcome.pass);
        println!(
            "  {:<28} {}  {}",
            scenario.describe(),
            if outcome.pass { "pass" } else { "FAIL" },
            outcome.measured
        );
        rows.push((scenario, outcome));
        std::thread::sleep(Duration::from_millis(500));
    }
    println!();
    println!(
        "{:<28} {:<58} {:<20} verdict",
        "scenario", "measured", "target"
    );
    for (scenario, outcome) in &rows {
        println!(
            "{:<28} {:<58} {:<20} {}",
            scenario.describe(),
            outcome.measured,
            scenario.target(),
            if outcome.pass { "PASS" } else { "FAIL" }
        );
    }
    if failed > 0 {
        bail!("{failed} scenario(s) failed");
    }
    Ok(())
}

/// `couch-cli hold-key <usage>`: presses one key through the host's injector
/// (so it is recorded as held), says so, and lets go after 30 s unless
/// killed first. Only for `lifecycle-check`.
pub fn hold_key(usage: &str) -> anyhow::Result<()> {
    let usage = u16::from_str_radix(usage.trim_start_matches("0x"), 16).context("a hex usage")?;
    let mut injector = sendinput::SendInputInjector::new();
    couch_input::KbmInjector::key(&mut injector, couch_input::HidUsage(usage), true)?;
    println!("EVENT held {}", clock::now().0);
    std::thread::sleep(Duration::from_secs(30));
    couch_input::KbmInjector::key(&mut injector, couch_input::HidUsage(usage), false)?;
    Ok(())
}
