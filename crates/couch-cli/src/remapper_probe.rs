//! `couch-cli remapper-probe` (M22's probe): can another program's virtual
//! pad (a remapper's, DS4Windows's say) be hidden from games while we still
//! read it, as the host's controller through a seat would be?
//!
//! It watches every gamepad SDL finds and prints, with times from its start:
//! - each one plugged and unplugged, with its device and origin
//!   (`couch_platform_win::pad_origin`); pads whose origin can't be told yet
//!   are looked at again every second;
//! - for another program's virtual pad: its button changes (still read while
//!   hidden?), and with `--hide`, how long after SDL saw it HidHide hid it
//!   (`hidhide::hide_virtual`), again whenever the remapper plugs it anew;
//! - every 10 s, whether HidHide's blacklist still has what we added (does
//!   the remapper rewrite it?).
//!
//! `--rumble` shakes those pads every 5 s. Everything hidden is shown again
//! when it ends (Ctrl+C included), or when the pad goes. Check from outside with
//! `.\scripts\xinput-watch.ps1` (PowerShell isn't on HidHide's whitelist, so
//! a hidden pad's slot should read as empty there) and `joy.cpl`.

use std::{
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use couch_gamepad::survey::{Seen, WatchControl, watch};
use couch_input::local::ControllerIds;
use couch_platform_win::{
    hidhide::{self, Hidden},
    pad_origin::{self, Origin},
    vigem::VigemPadFactory,
    virtual_pads,
};

/// A pad SDL lists, as the probe follows it.
struct Pad {
    which: u32,
    name: String,
    ids: ControllerIds,
    path: Option<String>,
    seen_at: Instant,
    /// Its origin, once told.
    origin: Option<Origin>,
    device: Option<String>,
    hidden: Option<Hidden>,
}

pub fn run(secs: u64, hide: bool, sticky: bool, rumble: bool, unmarked: u8) -> anyhow::Result<()> {
    let mut stand_ins = Vec::new();
    if unmarked > 0 {
        let factory = VigemPadFactory::connect_unmarked(None).context("connecting to ViGEmBus")?;
        for _ in 0..unmarked {
            stand_ins.push(
                factory
                    .plug_for_probe()
                    .context("plugging an unmarked pad")?,
            );
        }
        println!("plugged {unmarked} unmarked pad(s), standing in for a remapper's");
    }
    println!(
        "watching for {secs} s{}{}; a host's markers here: {}",
        if hide {
            ", hiding other programs' pads"
        } else {
            ""
        },
        if rumble {
            ", rumbling them every 5 s"
        } else {
            ""
        },
        virtual_pads::host_pads_here()
    );
    match hidhide::blacklist() {
        Ok(list) => println!("HidHide's blacklist at the start: {list:?}"),
        Err(e) => println!("HidHide's blacklist: {e}"),
    }

    let start = Instant::now();
    let t = move || format!("[{:>7.3}s]", start.elapsed().as_secs_f64());
    let pads: Arc<Mutex<Vec<Pad>>> = Arc::default();
    let shared = pads.clone();
    let mut last_recheck = Instant::now();
    let mut last_list = Instant::now();
    let mut last_rumble = Instant::now();
    // With `sticky`, the devices of pads hidden that went, still on the
    // blacklist: the same pad plugged again is hidden from its first moment.
    let mut kept_entries: Vec<(String, Hidden)> = Vec::new();
    // Ctrl+C ends the watch, so what was hidden is shown again on the way
    // out rather than at the next start.
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        tokio::runtime::Handle::current().spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                println!("\nCtrl+C: stopping");
                stop.store(true, Ordering::Relaxed);
            }
        });
    }
    watch(Duration::from_secs(secs), stop, move |seen, control| {
        let mut pads = shared.lock().unwrap_or_else(PoisonError::into_inner);
        match seen {
            Seen::Added(which, p) => {
                println!(
                    "{} + #{which} {:?} {:04x}:{:04x} path {}",
                    t(),
                    p.name,
                    p.vendor,
                    p.product,
                    p.path.as_deref().unwrap_or("(none)")
                );
                pads.push(Pad {
                    which,
                    name: p.name.clone(),
                    ids: ControllerIds {
                        vendor: p.vendor,
                        product: p.product,
                    },
                    path: p.path.clone(),
                    seen_at: Instant::now(),
                    origin: None,
                    device: None,
                    hidden: None,
                });
                let pad = pads.last_mut().expect("just pushed");
                tell(pad, hide, &mut kept_entries, control, &t);
            }
            Seen::Removed(which) => {
                if let Some(at) = pads.iter().position(|p| p.which == which) {
                    let mut pad = pads.remove(at);
                    let mut kept = false;
                    if sticky
                        && let Some(device) = pad.device.clone()
                        && let Some(hidden) = pad.hidden.take()
                    {
                        kept_entries.push((device, hidden));
                        kept = true;
                    }
                    println!(
                        "{} - #{which} {:?} gone{}",
                        t(),
                        pad.name,
                        if kept {
                            "; its device stays on the blacklist, for when it comes back"
                        } else if pad.hidden.is_some() {
                            "; showing its device again"
                        } else {
                            ""
                        }
                    );
                    drop(pad);
                }
            }
            Seen::Changed(which, now) => {
                let name = pads.iter().find(|p| p.which == which).map(|p| &p.name);
                println!(
                    "{} . #{which} {:?} buttons {:04x} left stick {},{} triggers {},{}",
                    t(),
                    name.map_or("?", |n| n.as_str()),
                    now.buttons,
                    now.left_x,
                    now.left_y,
                    now.left_trigger,
                    now.right_trigger
                );
            }
            Seen::Tick => {
                if last_recheck.elapsed() >= Duration::from_secs(1) {
                    last_recheck = Instant::now();
                    for pad in pads.iter_mut().filter(|p| p.origin.is_none()) {
                        tell(pad, hide, &mut kept_entries, control, &t);
                    }
                }
                if rumble && last_rumble.elapsed() >= Duration::from_secs(5) {
                    last_rumble = Instant::now();
                    for pad in pads
                        .iter()
                        .filter(|p| p.origin == Some(Origin::OtherVirtual))
                    {
                        println!("{} ~ rumbling #{}", t(), pad.which);
                        control.rumble(pad.which);
                    }
                }
                if hide && last_list.elapsed() >= Duration::from_secs(10) {
                    last_list = Instant::now();
                    report_blacklist(&pads, &t);
                }
            }
        }
    })
    .map_err(anyhow::Error::msg)?;

    let hidden = pads
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .drain(..)
        .filter(|p| p.hidden.is_some())
        .count();
    println!(
        "\ndone; {hidden} pad(s) shown again{}",
        if stand_ins.is_empty() {
            ""
        } else {
            ", the stand-ins unplugged"
        }
    );
    drop(stand_ins);
    match hidhide::blacklist() {
        Ok(list) => println!("HidHide's blacklist at the end: {list:?}"),
        Err(e) => println!("HidHide's blacklist: {e}"),
    }
    Ok(())
}

/// Tells a pad's origin if it can be told now; another program's is opened
/// (to see it still reads while hidden) and, with `hide`, hidden.
fn tell(
    pad: &mut Pad,
    hide: bool,
    kept: &mut Vec<(String, Hidden)>,
    control: &mut WatchControl,
    t: &dyn Fn() -> String,
) {
    let hosts = virtual_pads::host_pads();
    let found = pad_origin::device_of_gamepad(pad.path.as_deref(), pad.ids);
    let origin = match &found {
        Some((_, chain)) => pad_origin::classify(chain, &hosts),
        None => Origin::Unknown,
    };
    if origin == Origin::Unknown && pad.seen_at.elapsed() < Duration::from_secs(10) {
        // Its light may not be set yet: looked at again in a second.
        return;
    }
    pad.origin = Some(origin);
    pad.device = found.map(|(device, _)| device);
    println!(
        "{}   #{} {:?}: {origin:?}, device {}",
        t(),
        pad.which,
        pad.name,
        pad.device.as_deref().unwrap_or("(not found)")
    );
    // A device kept on the blacklist back: as another program's pad, it
    // was hidden all along; as anything else (ours, say), never again.
    let back = pad.device.as_deref().and_then(|device| {
        kept.iter()
            .position(|(d, _)| d.eq_ignore_ascii_case(device))
    });
    if let Some(at) = back {
        let (_, hidden) = kept.remove(at);
        if origin == Origin::OtherVirtual {
            println!(
                "{}   #{} back: its device was kept on the blacklist, so it was hidden from its first moment",
                t(),
                pad.which
            );
            pad.hidden = Some(hidden);
            control.open(pad.which);
            return;
        }
        println!(
            "{}   #{} has a kept device but is {origin:?}: its entry comes off",
            t(),
            pad.which
        );
        drop(hidden);
    }
    if origin != Origin::OtherVirtual {
        return;
    }
    control.open(pad.which);
    if !hide {
        return;
    }
    let Some(device) = pad.device.clone() else {
        return;
    };
    let asked = Instant::now();
    match hidhide::hide_virtual(&device) {
        Ok(hidden) => {
            println!(
                "{}   #{} hidden: {} ms after SDL saw it (the hiding itself {} ms)",
                t(),
                pad.which,
                pad.seen_at.elapsed().as_millis(),
                asked.elapsed().as_millis()
            );
            pad.hidden = Some(hidden);
            println!(
                "\n>>> #{} {:?} is HIDDEN from games now: check it from outside (xinput-watch.ps1, joy.cpl)\n",
                pad.which, pad.name
            );
        }
        Err(e) => println!("{}   #{} not hidden: {e}", t(), pad.which),
    }
}

/// Whether the hidden pads' devices are still on HidHide's blacklist.
fn report_blacklist(pads: &[Pad], t: &dyn Fn() -> String) {
    let list = match hidhide::blacklist() {
        Ok(list) => list,
        Err(e) => {
            println!("{} HidHide's blacklist: {e}", t());
            return;
        }
    };
    for pad in pads.iter().filter(|p| p.hidden.is_some()) {
        let Some(device) = &pad.device else { continue };
        let on = list.iter().any(|d| d.eq_ignore_ascii_case(device));
        println!(
            "{} = #{} {}",
            t(),
            pad.which,
            if on {
                "still on HidHide's blacklist"
            } else {
                "NO LONGER on HidHide's blacklist (someone rewrote it?)"
            }
        );
    }
}
