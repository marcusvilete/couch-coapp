//! `couch-cli input-sink`: injects a scripted keyboard and mouse run with the
//! host's `SendInput` injector into `couch-viewer --input-sink`, and checks
//! that the window received exactly that. Proves the scancode table and the
//! injector on one machine, without a second PC.
//!
//! Injection goes to whatever window has the foreground, so it stops at once
//! if the sink window loses it.

use std::{
    path::PathBuf,
    process::{Child, Command},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use couch_input::{HidUsage, KbmInjector, MouseButton};
use couch_platform_win::{
    foreground::foreground_process, keymap::key_code, sendinput::SendInputInjector,
};

/// Left out of the run: the Windows keys (Start menu), Print Screen (the
/// snipping tool), the lock keys (they would stay toggled), and non-US #,
/// which shares its scancode with \ and reads back as \.
const SKIPPED: [u16; 7] = [0xE3, 0xE7, 0x46, 0x39, 0x47, 0x53, 0x32];
/// Last, in case one of them puts the window into menu mode: F10 and Alt.
const LAST: [u16; 7] = [0x43, 0xE0, 0xE1, 0xE2, 0xE4, 0xE5, 0xE6];

const MOVES: [(i32, i32); 4] = [(10, 0), (0, -7), (-3, 4), (25, 25)];
const WHEEL: [(i32, i32); 3] = [(0, 120), (0, -240), (120, 0)];

struct Sink {
    child: Child,
    file: PathBuf,
}

impl Drop for Sink {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.file);
    }
}

pub fn run() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let viewer = exe.with_file_name(format!("couch-viewer{}", std::env::consts::EXE_SUFFIX));
    let file = std::env::temp_dir().join(format!("couch-input-sink-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let child = Command::new(&viewer)
        .arg("--input-sink")
        .arg(&file)
        .spawn()
        .with_context(|| format!("starting {}", viewer.display()))?;
    let sink = Sink { child, file };

    let deadline = Instant::now() + Duration::from_secs(10);
    while !read(&sink.file).lines().any(|l| l == "ready") {
        if Instant::now() > deadline {
            bail!("the sink window never got the focus; click it and try again");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(200));

    let keys: Vec<u16> = (0..=0xFF)
        .filter(|u| key_code(*u).is_some() && !SKIPPED.contains(u) && !LAST.contains(u))
        .chain(LAST)
        .collect();
    let pid = sink.child.id();
    let mut kbm = SendInputInjector::new();
    // Presses only while the sink is in front; releases always, so losing
    // the focus mid-press can't leave a key held on this machine.
    let mut step =
        |release: bool,
         f: &mut dyn FnMut(&mut SendInputInjector) -> Result<(), couch_input::InputError>|
         -> anyhow::Result<()> {
            if !release && foreground_process() != Some(pid) {
                bail!("the sink window lost the foreground; stopped injecting");
            }
            f(&mut kbm)?;
            std::thread::sleep(Duration::from_millis(3));
            Ok(())
        };
    let mut expected = Vec::new();
    for &usage in &keys {
        step(false, &mut |k| k.key(HidUsage(usage), true))?;
        step(true, &mut |k| k.key(HidUsage(usage), false))?;
        expected.push(format!("key {usage:#04x} down"));
        expected.push(format!("key {usage:#04x} up"));
    }
    for button in MouseButton::ALL {
        step(false, &mut |k| k.mouse_button(button, true))?;
        step(true, &mut |k| k.mouse_button(button, false))?;
        expected.push(format!("button {} down", button.code()));
        expected.push(format!("button {} up", button.code()));
    }
    for (dx, dy) in MOVES {
        step(false, &mut |k| k.mouse_move(dx, dy))?;
    }
    for (dx, dy) in WHEEL {
        step(false, &mut |k| k.wheel(dx, dy))?;
    }
    std::thread::sleep(Duration::from_millis(500));

    let text = read(&sink.file);
    let mut got = Vec::new();
    let (mut moved, mut wheeled) = ((0.0, 0.0), (0.0, 0.0));
    for line in text.lines() {
        let parts: Vec<&str> = line.split(' ').collect();
        let pair = || -> (f64, f64) {
            (
                parts.get(1).and_then(|v| v.parse().ok()).unwrap_or(0.0),
                parts.get(2).and_then(|v| v.parse().ok()).unwrap_or(0.0),
            )
        };
        match parts[0] {
            "motion" => {
                let (x, y) = pair();
                moved = (moved.0 + x, moved.1 + y);
            }
            "wheel" => {
                let (x, y) = pair();
                wheeled = (wheeled.0 + x, wheeled.1 + y);
            }
            "key" | "button" => got.push(line.to_string()),
            _ => {}
        }
    }

    let mut problems = Vec::new();
    for (i, want) in expected.iter().enumerate() {
        match got.get(i) {
            Some(g) if g == want => {}
            Some(g) => problems.push(format!("event {i}: expected {want:?}, got {g:?}")),
            None => problems.push(format!("event {i}: expected {want:?}, got nothing")),
        }
    }
    if got.len() > expected.len() {
        problems.push(format!(
            "{} unexpected extra events",
            got.len() - expected.len()
        ));
    }
    let want_move = MOVES.iter().fold((0.0, 0.0), |a, m| {
        (a.0 + f64::from(m.0), a.1 + f64::from(m.1))
    });
    if moved != want_move {
        problems.push(format!("mouse moved {moved:?}, expected {want_move:?}"));
    }
    // SDL reports the wheel with up and right positive, like Windows.
    let want_wheel = WHEEL.iter().fold((0.0, 0.0), |a, w| {
        (a.0 + f64::from(w.0), a.1 + f64::from(w.1))
    });
    if wheeled != want_wheel {
        problems.push(format!("wheel moved {wheeled:?}, expected {want_wheel:?}"));
    }

    println!(
        "injected {} keys, {} mouse buttons, {} moves, {} wheel turns",
        keys.len(),
        MouseButton::ALL.len(),
        MOVES.len(),
        WHEEL.len()
    );
    if problems.is_empty() {
        println!("input-sink: PASS (every event arrived, in order)");
        Ok(())
    } else {
        for p in problems.iter().take(30) {
            println!("  {p}");
        }
        bail!("input-sink: FAIL ({} problems)", problems.len())
    }
}

fn read(path: &PathBuf) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}
