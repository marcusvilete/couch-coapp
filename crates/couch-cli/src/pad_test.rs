//! `couch-cli pad-test`: plugs a virtual Xbox 360 pad and moves it for a
//! while, printing any rumble a game sends. Watch it in `joy.cpl` (Game
//! Controllers) or any gamepad tester.

use std::time::{Duration, Instant};

use anyhow::Context;
use couch_input::{DEV_PAD_IDS, PadButtons, PadState, VirtualPadFactory};
use couch_platform_win::vigem::VigemPadFactory;

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

pub fn run(secs: u64, real_ids: bool) -> anyhow::Result<()> {
    let ids = (!real_ids).then_some(DEV_PAD_IDS);
    let factory = VigemPadFactory::connect(ids).context("connecting to ViGEmBus")?;
    let mut pad = factory.plug().context("plugging a virtual pad")?;
    println!(
        "virtual Xbox 360 pad plugged in{}; open joy.cpl to watch it. Running {secs} s.",
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
        let state = PadState {
            buttons: BUTTONS[(t * 2.0) as usize % BUTTONS.len()],
            left_trigger: ((t.fract()) * 255.0) as u8,
            right_trigger: ((1.0 - t.fract()) * 255.0) as u8,
            left_stick: (stick(angle.cos()), stick(angle.sin())),
            right_stick: (stick(-angle.cos()), stick(angle.sin())),
        };
        pad.submit(&state)?;
        if let Some(r) = pad.take_rumble() {
            println!("rumble: low {} high {}", r.low_frequency, r.high_frequency);
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    pad.submit(&PadState::NEUTRAL)?;
    println!("done; unplugging");
    Ok(())
}
