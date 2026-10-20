//! `couch-cli pad-test`: plugs virtual Xbox 360 pads (one by default) and
//! moves them for a while, printing any rumble a game sends. Watch them in
//! `joy.cpl` (Game Controllers), `scripts\xinput-watch.ps1` or any gamepad
//! tester: pad N presses its buttons N steps ahead of the first, so each
//! slot shows which pad it is. Also prints how many game controllers
//! Windows lists (`couch_platform_win::controllers`), before and after.

use std::time::{Duration, Instant};

use anyhow::Context;
use couch_input::{DEV_PAD_IDS, PadButtons, PadState, VirtualPadFactory};
use couch_platform_win::{controllers, vigem::VigemPadFactory};

/// Each button in turn, the Guide button left out.
const BUTTONS: [PadButtons; 14] = [
    PadButtons::A,
    PadButtons::B,
    PadButtons::X,
    PadButtons::Y,
    PadButtons::LEFT_SHOULDER,
    PadButtons::RIGHT_SHOULDER,
    PadButtons::BACK,
    PadButtons::START,
    PadButtons::LEFT_THUMB,
    PadButtons::RIGHT_THUMB,
    PadButtons::DPAD_UP,
    PadButtons::DPAD_RIGHT,
    PadButtons::DPAD_DOWN,
    PadButtons::DPAD_LEFT,
];

pub fn run(secs: u64, real_ids: bool, count: u8) -> anyhow::Result<()> {
    let before = controllers::count_all();
    let ids = (!real_ids).then_some(DEV_PAD_IDS);
    let factory = VigemPadFactory::connect(ids).context("connecting to ViGEmBus")?;
    let mut pads = (0..count.max(1))
        .map(|_| factory.plug().context("plugging a virtual pad"))
        .collect::<anyhow::Result<Vec<_>>>()?;
    // Windows lists a new pad shortly after it is plugged in.
    std::thread::sleep(Duration::from_secs(1));
    println!(
        "game controllers Windows lists: {before} before, {} with ours ({} not counting ours, as the app's warning does)",
        controllers::count_all(),
        controllers::count()
    );
    println!(
        "{} virtual Xbox 360 pad(s) plugged in{}; open joy.cpl to watch. Running {secs} s.",
        pads.len(),
        if real_ids {
            String::new()
        } else {
            format!(" as {:04x}:{:04x}", DEV_PAD_IDS.0, DEV_PAD_IDS.1)
        }
    );

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(secs) {
        let t = started.elapsed().as_secs_f64();
        let angle = t * std::f64::consts::TAU / 2.0;
        let stick = |a: f64| (a * 30_000.0) as i16;
        let step = (t * 2.0) as usize;
        let state = |i: usize| PadState {
            buttons: BUTTONS[(step + i) % BUTTONS.len()],
            left_trigger: ((t.fract()) * 255.0) as u8,
            right_trigger: ((1.0 - t.fract()) * 255.0) as u8,
            left_stick: (stick(angle.cos()), stick(angle.sin())),
            right_stick: (stick(-angle.cos()), stick(angle.sin())),
        };
        for (i, pad) in pads.iter_mut().enumerate() {
            pad.submit(&state(i))?;
            if let Some(r) = pad.take_rumble() {
                println!(
                    "pad {}: rumble low {} high {}",
                    i + 1,
                    r.low_frequency,
                    r.high_frequency
                );
            }
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    for pad in &mut pads {
        pad.submit(&PadState::NEUTRAL)?;
    }
    println!("done; unplugging");
    Ok(())
}
