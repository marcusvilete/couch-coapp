//! `couch-cli remapper-hop` (M22): what a remapper's pad adds before the
//! host's input reaches a seat, beside a physical controller: the remapper
//! writes its pad, Windows' Xbox driver passes it on, and the host's reader
//! (SDL, `couch_gamepad::host_pads`) reads it like any controller.
//!
//! An unmarked ViGEm pad stands in for the remapper's (no remapper needed):
//! it presses and lets go of A at uneven intervals, and each press is timed
//! from the write to the reader's report. What follows (the seat router,
//! our pad, the game) is the same for every controller of the host's.

use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use couch_input::{
    PadButtons, PadState, VirtualPadFactory,
    local::{ControllerChoice, ControllerKind},
};
use couch_platform_win::vigem::VigemPadFactory;

pub fn run(presses: u32) -> anyhow::Result<()> {
    let reader = couch_gamepad::host_pads();
    let remappers = |list: &[couch_input::local::LocalController]| -> Vec<String> {
        list.iter()
            .filter(|c| c.kind == ControllerKind::OtherVirtual)
            .filter_map(|c| c.device.clone())
            .collect()
    };
    let before = remappers(&reader.list().context("listing controllers")?);
    if !before.is_empty() {
        bail!(
            "another program's virtual pad is plugged in ({}): close the remapper first, the reader would take the first one",
            before.join(", ")
        );
    }
    let factory = VigemPadFactory::connect_unmarked(None).context("connecting to ViGEmBus")?;
    let mut stand_in = factory.plug().context("plugging the stand-in")?;
    println!("plugged an unmarked pad, standing in for a remapper's");

    // Listed once its player light is set: a second or so.
    let deadline = Instant::now() + Duration::from_secs(10);
    let found = loop {
        let list = reader.list().context("listing controllers")?;
        if let Some(c) = list.iter().find(|c| {
            c.kind == ControllerKind::OtherVirtual
                && c.device.as_ref().is_some_and(|d| !before.contains(d))
        }) {
            break c.clone();
        }
        if Instant::now() > deadline {
            bail!("the stand-in was never listed as another program's virtual pad");
        }
        thread::sleep(Duration::from_millis(250));
    };
    println!(
        "the reader lists it: {} ({})",
        found.name,
        found.device.as_deref().unwrap_or("?")
    );

    let (tx, rx) = mpsc::channel::<(PadState, Instant)>();
    let _followed = reader
        .follow(
            ControllerChoice {
                ids: found.ids,
                kind: ControllerKind::OtherVirtual,
            },
            Box::new(move |state| {
                let _ = tx.send((state, Instant::now()));
            }),
            Box::new(|device| match device {
                Some(device) => println!("the reader reads {device}"),
                None => println!("the reader reads nothing"),
            }),
        )
        .context("following it")?;
    thread::sleep(Duration::from_millis(500));
    while rx.try_recv().is_ok() {}

    let mut hops = Vec::new();
    let mut lost = 0u32;
    // What the reader reported instead, for a run where presses go missing.
    let mut others: Vec<PadState> = Vec::new();
    let mut gap = 37u64;
    for i in 0..presses {
        // Uneven gaps, 20 to 69 ms, so the writes don't fall in step with
        // the reader's polling.
        gap = (gap * 31 + 17) % 50;
        thread::sleep(Duration::from_millis(20 + gap));
        let state = PadState {
            buttons: if i % 2 == 0 {
                PadButtons::A
            } else {
                PadButtons::empty()
            },
            ..PadState::NEUTRAL
        };
        let sent = Instant::now();
        stand_in.submit(&state).context("writing the stand-in")?;
        let until = sent + Duration::from_millis(500);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                // Buttons only: SDL reads a centred stick as 1 (it turns
                // XInput's Y axis over), not 0.
                Ok((seen, at)) if seen.buttons == state.buttons => {
                    hops.push(at.duration_since(sent));
                    break;
                }
                Ok((seen, _)) => {
                    if others.len() < 8 && !others.contains(&seen) {
                        others.push(seen);
                    }
                }
                Err(_) => {
                    lost += 1;
                    break;
                }
            }
        }
    }

    hops.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let at = |q: f64| hops[((hops.len() - 1) as f64 * q).round() as usize];
    if hops.is_empty() {
        bail!("no press reached the reader ({lost} lost); it reported {others:?}");
    }
    println!(
        "{} presses: write to the reader's report p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms; {lost} not seen within 500 ms",
        hops.len(),
        ms(at(0.5)),
        ms(at(0.95)),
        ms(*hops.last().expect("not empty"))
    );
    Ok(())
}
