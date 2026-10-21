//! `couch-cli host-pad`: the host's own controller, as a session plays it
//! through a seat (P2).
//!
//! Without `--feed` it lists this PC's controllers (never our own virtual
//! pads; other programs' since M22, named so) and says whether HidHide can
//! hide them. With `--feed N` it plugs a virtual Xbox 360 pad with real ids,
//! hides controller N from every other program (unless `--no-hide`), and
//! drives the pad from the controller for `--secs` through a seat router
//! (as the host's own holder, as in the app), passing the game's rumble
//! back. Watch with `scripts\xinput-watch.ps1` and `joy.cpl`: after
//! reconnecting the controller once, only the virtual pad should be left,
//! moving with it.

use std::{
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use couch_input::{
    HidUsage, InputError, KbmInjector, MemberId, MouseButton, PadState, Permissions, SeatId,
    VirtualPadFactory,
    local::{ControllerChoice, ControllerKind},
    router::{self, Input, SeatRouter},
};
use couch_platform_win::{hidhide, vigem::VigemPadFactory};
use couch_proto::input::PadDatagram;

pub fn run(feed: Option<usize>, secs: u64, hide: bool) -> anyhow::Result<()> {
    let restored = hidhide::restore_stale();
    if restored > 0 {
        println!("showed {restored} controller(s) a crashed host had left hidden");
    }
    println!("HidHide: {:?}", hidhide::state());
    let pads = couch_gamepad::host_pads();
    let list = pads.list().context("listing controllers")?;
    if list.is_empty() {
        println!("no controllers (our virtual pads are never listed)");
    }
    for (i, c) in list.iter().enumerate() {
        println!(
            "{}: {} ({:04x}:{:04x}){}",
            i + 1,
            c.name,
            c.ids.vendor,
            c.ids.product,
            match &c.device {
                Some(device) => format!(", another program's virtual pad: {device}"),
                None => String::new(),
            }
        );
    }
    let Some(n) = feed else {
        return Ok(());
    };
    let Some(chosen) = n.checked_sub(1).and_then(|i| list.get(i)) else {
        bail!("there is no controller {n}");
    };

    // Declared before our pad, so however this ends (an error included),
    // the controller is shown again only after our pad is unplugged.
    let mut hidden: Option<hidhide::Hidden> = None;
    // Our pad first: hiding a remapper's pad by its device (M22) while it
    // is gone would hide whichever pad takes that id next, ours included.
    let factory = VigemPadFactory::connect(None).context("connecting to ViGEmBus")?;
    let mut virtual_pads = vec![Some(factory.plug().context("plugging a virtual pad")?)];
    if hide {
        // A remapper's pad by its device, kept hidden to the end even while
        // the remapper plugs it again.
        hidden = Some(
            match (chosen.kind, &chosen.device) {
                (ControllerKind::OtherVirtual, Some(device)) => hidhide::hide_virtual(device),
                _ => hidhide::hide(chosen.ids),
            }
            .context("hiding it")?,
        );
        println!(
            "{} is hidden from other programs. Reconnect it once (unplug and plug it back, or turn it off and on; for a remapper's pad, the controller behind it) so programs that had it open let go.",
            chosen.name
        );
    }
    // Through a seat router, as the app does: the host's own holder on seat
    // 0, with the pad only (so the Guide button stays out too).
    let host = MemberId([0; 32]);
    let mut router = SeatRouter::new(1);
    let granted = router.grant(SEAT, host, Permissions::GAMEPAD, Instant::now());
    router::apply(&granted, &mut NoKbm, &mut virtual_pads)?;
    let epoch = router.seat(SEAT).map(|s| s.0).context("the seat")?;
    let latest = Arc::new(Mutex::new((PadState::NEUTRAL, 0u64)));
    let sink = latest.clone();
    let followed = pads
        .follow(
            ControllerChoice {
                ids: chosen.ids,
                kind: chosen.kind,
            },
            Box::new(move |state| {
                let mut slot = sink.lock().unwrap_or_else(PoisonError::into_inner);
                slot.0 = state;
                slot.1 += 1;
            }),
            Box::new(|device| {
                if let Some(device) = device {
                    println!("reading {device}");
                }
            }),
        )
        .context("following it")?;
    println!("feeding a virtual Xbox 360 pad from it for {secs} s");

    let started = Instant::now();
    let mut last = (PadState::NEUTRAL, 0);
    let mut reports = 0u64;
    let mut changes = 0u64;
    let mut seq = 0u32;
    while started.elapsed() < Duration::from_secs(secs) {
        let now = *latest.lock().unwrap_or_else(PoisonError::into_inner);
        let mut effects = router.tick(Instant::now());
        if now.1 != last.1 {
            reports += now.1 - last.1;
            if now.0 != last.0 {
                changes += 1;
            }
            last = now;
            seq += 1;
            let datagram = PadDatagram {
                seat: SEAT,
                seat_epoch: epoch,
                seq,
                pad: now.0.into(),
            };
            effects.extend(router.handle(host, Input::Pad(datagram), Instant::now()));
        }
        router::apply(&effects, &mut NoKbm, &mut virtual_pads)?;
        if let Some(Some(pad)) = virtual_pads.first_mut()
            && let Some(r) = pad.take_rumble()
        {
            println!("rumble low {} high {}", r.low_frequency, r.high_frequency);
            followed.rumble(r);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let released = router.revoke_all();
    router::apply(&released, &mut NoKbm, &mut virtual_pads)?;
    println!(
        "done: {reports} reports ({:.0}/s), {changes} changes; unplugging{}",
        reports as f64 / started.elapsed().as_secs_f64(),
        if hidden.is_some() {
            " and showing the controller again"
        } else {
            ""
        }
    );
    drop(followed);
    drop(virtual_pads);
    drop(hidden);
    Ok(())
}

const SEAT: SeatId = SeatId(0);

/// The router's keyboard and mouse go nowhere: the seat has the pad only.
struct NoKbm;

impl KbmInjector for NoKbm {
    fn key(&mut self, _: HidUsage, _: bool) -> Result<(), InputError> {
        Ok(())
    }
    fn mouse_move(&mut self, _: i32, _: i32) -> Result<(), InputError> {
        Ok(())
    }
    fn mouse_button(&mut self, _: MouseButton, _: bool) -> Result<(), InputError> {
        Ok(())
    }
    fn wheel(&mut self, _: i32, _: i32) -> Result<(), InputError> {
        Ok(())
    }
}
