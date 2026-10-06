//! `couch-cli fake-game`: a stand-in game window for lifecycle checks. It
//! swaps its window, closes it, or exits when a line says so on stdin
//! (`swap`, `close`, `exit`) or at the times given, and prints
//! `EVENT fake-<what> <QPC µs>` for each step.

use std::{
    io::BufRead,
    sync::mpsc,
    time::{Duration, Instant},
};

use couch_platform_win::{
    clock,
    fake_game::{self, Action, Event},
};

pub struct Args {
    pub swap_after: Option<f64>,
    pub close_after: Option<f64>,
    pub exit_after: Option<f64>,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let (tx, commands) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            let action = match line.trim() {
                "swap" => Action::Swap,
                "close" => Action::Close,
                "exit" => Action::Exit,
                _ => continue,
            };
            if tx.send(action).is_err() {
                return;
            }
        }
    });
    let started = Instant::now();
    let mut schedule: Vec<(Duration, Action)> = [
        (args.swap_after, Action::Swap),
        (args.close_after, Action::Close),
        (args.exit_after, Action::Exit),
    ]
    .into_iter()
    .filter_map(|(at, action)| Some((Duration::from_secs_f64(at?), action)))
    .collect();
    schedule.sort_by_key(|(at, _)| *at);
    let next = move || {
        if let Ok(action) = commands.try_recv() {
            return Some(action);
        }
        let (at, action) = *schedule.first()?;
        (started.elapsed() >= at).then(|| {
            schedule.remove(0);
            action
        })
    };
    fake_game::run(next, |event| {
        let what = match event {
            Event::Shown => "shown",
            Event::Swapped => "swapped",
            Event::Closed => "closed",
            Event::Exiting => "exiting",
        };
        println!("EVENT fake-{what} {}", clock::now().0);
    })?;
    Ok(())
}
