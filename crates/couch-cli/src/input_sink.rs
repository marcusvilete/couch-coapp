//! `couch-cli input-sink`: plays a remote guest holding the seat. A scripted
//! keyboard and mouse run goes through the host's seat router and its
//! `SendInput` injector into `couch-viewer --input-sink`, which stands for
//! the game; the check is that the window received exactly that. Then the
//! combinations a guest may never send (the Windows key, Alt+Tab, Alt+Esc,
//! Ctrl+Esc, Ctrl+Shift+Esc, Alt+F4) must not arrive. Proves the scancode
//! table, the injector and the router's filter on one machine, without a
//! second PC.
//!
//! Injection goes to whatever window has the foreground, so it stops at once
//! if the sink window loses it.

use std::{
    path::PathBuf,
    process::{Child, Command},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use couch_input::{
    MemberId, MouseButton, Permissions, SeatEpoch, SeatId,
    router::{self, Effect, Input, SeatRouter},
};
use couch_platform_win::{
    foreground::foreground_process, keymap::key_code, sendinput::SendInputInjector,
};
use couch_proto::input::{KeyEvent, MouseButtonEvent, MouseDatagram, MouseTotals};

const SEAT: SeatId = SeatId(0);
const GUEST: MemberId = MemberId([7; 32]);

/// Left out of the run: the Windows keys (Start menu), Print Screen (the
/// snipping tool), the lock keys (they would stay toggled), and non-US #,
/// which shares its scancode with \ and reads back as \.
const SKIPPED: [u16; 7] = [0xE3, 0xE7, 0x46, 0x39, 0x47, 0x53, 0x32];
/// Last, in case one of them puts the window into menu mode: F10 and Alt.
const LAST: [u16; 7] = [0x43, 0xE0, 0xE1, 0xE2, 0xE4, 0xE5, 0xE6];

/// What a guest may never send (`couch_input::filter`), each pressed in
/// order and let go in reverse. If the Windows key got through, the Start
/// menu would take the foreground and stop the run.
const BLOCKED: [&[u16]; 6] = [
    &[0xE3],             // Windows
    &[0xE2, 0x2B],       // Alt+Tab
    &[0xE2, 0x29],       // Alt+Esc
    &[0xE0, 0x29],       // Ctrl+Esc
    &[0xE0, 0xE1, 0x29], // Ctrl+Shift+Esc
    &[0xE2, 0x3D],       // Alt+F4
];
/// Ctrl, Shift and Alt: in the blocked combinations, these may arrive.
const MODIFIERS: [u16; 6] = [0xE0, 0xE1, 0xE2, 0xE4, 0xE5, 0xE6];

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
    let mut guest = Guest::new(sink.child.id());
    let mut expected = Vec::new();
    for &usage in &keys {
        guest.key(usage, true)?;
        guest.key(usage, false)?;
        expected.push(format!("key {usage:#04x} down"));
        expected.push(format!("key {usage:#04x} up"));
    }
    for button in MouseButton::ALL {
        guest.button(button, true)?;
        guest.button(button, false)?;
        expected.push(format!("button {} down", button.code()));
        expected.push(format!("button {} up", button.code()));
    }
    for (dx, dy) in MOVES {
        guest.mouse(dx, dy, 0, 0)?;
    }
    for (dx, dy) in WHEEL {
        guest.mouse(0, 0, dx, dy)?;
    }
    std::thread::sleep(Duration::from_millis(500));
    let text = read(&sink.file);

    // Last, as one of them could put the window into menu mode: the
    // combinations a guest may never send. Only their modifiers may arrive.
    let before = text.lines().count();
    for combo in BLOCKED {
        for &usage in combo {
            guest.key(usage, true)?;
        }
        for &usage in combo.iter().rev() {
            guest.key(usage, false)?;
        }
    }
    std::thread::sleep(Duration::from_millis(500));
    let leaked: Vec<String> = read(&sink.file)
        .lines()
        .skip(before)
        .filter(|line| {
            line.strip_prefix("key ")
                .and_then(|rest| rest.split(' ').next())
                .and_then(|code| u16::from_str_radix(code.trim_start_matches("0x"), 16).ok())
                .is_some_and(|usage| !MODIFIERS.contains(&usage))
        })
        .map(str::to_string)
        .collect();

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
    for line in &leaked {
        problems.push(format!("a blocked combination got through: {line}"));
    }

    println!(
        "injected {} keys, {} mouse buttons, {} moves, {} wheel turns through the seat router; \
         {} blocked combinations held back",
        keys.len(),
        MouseButton::ALL.len(),
        MOVES.len(),
        WHEEL.len(),
        BLOCKED.len()
    );
    if problems.is_empty() {
        println!("input-sink: PASS (every event arrived, in order; nothing blocked did)");
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

/// A remote guest holding the seat: its input goes through the host's seat
/// router into the injector, as in a session, with the sink window as the
/// game.
struct Guest {
    router: SeatRouter,
    kbm: SendInputInjector,
    epoch: SeatEpoch,
    totals: MouseTotals,
    sink: u32,
}

impl Guest {
    fn new(sink: u32) -> Self {
        let mut router = SeatRouter::new(1);
        let mut epoch = SeatEpoch(0);
        for effect in router.grant(SEAT, GUEST, Permissions::all(), Instant::now()) {
            if let Effect::Granted { epoch: e, .. } = effect {
                epoch = e;
            }
        }
        Self {
            router,
            kbm: SendInputInjector::new(),
            epoch,
            totals: MouseTotals::default(),
            sink,
        }
    }

    /// Hands `input` to the router while the sink is in front. If it isn't,
    /// the router lets go of everything (as it would for a game), and the run
    /// stops.
    fn send(&mut self, input: Input) -> anyhow::Result<()> {
        let front = foreground_process() == Some(self.sink);
        // The sink takes clicks wherever the pointer is.
        self.router.set_pointer_inside(true);
        let lost = self.router.set_foreground(front);
        router::apply(&lost, &mut self.kbm, &mut [])?;
        if !front {
            bail!("the sink window lost the foreground; stopped injecting");
        }
        let effects = self.router.handle(GUEST, input, Instant::now());
        router::apply(&effects, &mut self.kbm, &mut [])?;
        std::thread::sleep(Duration::from_millis(3));
        Ok(())
    }

    fn key(&mut self, usage: u16, down: bool) -> anyhow::Result<()> {
        self.send(Input::Key(KeyEvent {
            seat: SEAT,
            seat_epoch: self.epoch,
            usage,
            down,
        }))
    }

    fn button(&mut self, button: MouseButton, down: bool) -> anyhow::Result<()> {
        self.send(Input::MouseButton(MouseButtonEvent {
            seat: SEAT,
            seat_epoch: self.epoch,
            button: button.code(),
            down,
            totals: self.totals,
        }))
    }

    fn mouse(&mut self, dx: i32, dy: i32, wheel_x: i32, wheel_y: i32) -> anyhow::Result<()> {
        let t = &mut self.totals;
        t.version = t.version.wrapping_add(1);
        t.x = t.x.wrapping_add(dx);
        t.y = t.y.wrapping_add(dy);
        t.wheel_x = t.wheel_x.wrapping_add(wheel_x);
        t.wheel_y = t.wheel_y.wrapping_add(wheel_y);
        self.send(Input::Mouse(MouseDatagram {
            seat: SEAT,
            seat_epoch: self.epoch,
            totals: self.totals,
        }))
    }
}

impl Drop for Guest {
    /// Whatever happened, nothing stays held.
    fn drop(&mut self) {
        let released = self.router.revoke_all();
        let _ = router::apply(&released, &mut self.kbm, &mut []);
    }
}
