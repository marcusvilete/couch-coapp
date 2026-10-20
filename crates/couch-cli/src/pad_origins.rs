//! `couch-cli pad-origins` (P9's probe): every gamepad SDL finds, with SDL's
//! path for it, where that device sits in Windows' device tree, and the
//! origin the app gives it (`couch_platform_win::pad_origin`): physical, or
//! a virtual pad and whose. Optionally plugs our own virtual pads first
//! (`--ours`, marked as a host's are) and pads that leave no marker
//! (`--unmarked`), standing in for another program's. SDL names Xbox-style
//! pads only by XInput slot, so it also lists each Xbox driver interface
//! with its player light (`xinput_slots`), beside ViGEm's own word for the
//! pads it plugged.
//!
//! Beyond plugging those, nothing is opened, hidden or moved.

use std::time::Duration;

use anyhow::Context;
use couch_gamepad::survey::survey;
use couch_input::local::ControllerIds;
use couch_platform_win::{
    device_tree::{self, Node},
    pad_origin,
    vigem::VigemPadFactory,
    virtual_pads, xinput_slots,
};

pub fn run(ours: u8, unmarked: u8, settle: u64, hold: u64) -> anyhow::Result<()> {
    let mut pads = Vec::new();
    if ours > 0 {
        let factory = VigemPadFactory::connect(None).context("connecting to ViGEmBus")?;
        for _ in 0..ours {
            let pad = factory
                .plug_for_probe()
                .context("plugging one of our pads")?;
            pads.push(("ours", pad));
        }
    }
    if unmarked > 0 {
        let factory = VigemPadFactory::connect_unmarked(None).context("connecting to ViGEmBus")?;
        for _ in 0..unmarked {
            let pad = factory
                .plug_for_probe()
                .context("plugging an unmarked pad")?;
            pads.push(("unmarked", pad));
        }
    }
    println!(
        "plugged {ours} of ours (marked) and {unmarked} unmarked; a host's markers here: {}",
        virtual_pads::host_pads_here()
    );

    let listed = survey(Duration::from_secs(settle)).map_err(anyhow::Error::msg)?;
    println!("\n== SDL's gamepads: {}", listed.len());
    for (i, pad) in listed.iter().enumerate() {
        println!(
            "\n{}. {:?} {:04x}:{:04x} (kind {}, real {})",
            i + 1,
            pad.name,
            pad.vendor,
            pad.product,
            pad.kind,
            pad.real_kind
        );
        println!("   SDL path: {}", pad.path.as_deref().unwrap_or("(none)"));
        if let Some(id) = pad.path.as_deref().and_then(device_tree::instance_for_path) {
            print_chain(&device_tree::chain(&id));
        }
        let ids = ControllerIds {
            vendor: pad.vendor,
            product: pad.product,
        };
        // The app's own rule, slot lookup included.
        println!(
            "   origin: {:?}",
            pad_origin::of_gamepad(pad.path.as_deref(), ids)
        );
    }

    // ViGEm's own word on the pads plugged here: the ground truth for the
    // driver's lights below.
    println!("\n== The pads plugged here, by ViGEm");
    let mut serials = Vec::new();
    for (which, pad) in &mut pads {
        println!(
            "{which}: serial {}, user index {:?}, light {:?}",
            pad.serial(),
            pad.user_index(),
            pad.light()
        );
        serials.push(pad.serial());
    }

    let hosts = virtual_pads::host_pads();
    let xusb = xinput_slots::xusb_pads();
    println!("\n== Xbox driver interfaces: {}", xusb.len());
    for pad in &xusb {
        println!("\n{}", pad.path);
        if let Some(e) = &pad.error {
            println!("   error: {e}");
        }
        println!(
            "   information {:02x?}, light read with version {:?}: {:?}",
            pad.information, pad.version, pad.lights
        );
        let Some(id) = &pad.instance_id else {
            println!("   no device for that path");
            continue;
        };
        let chain = device_tree::chain(id);
        print_chain(&chain);
        let origin = pad_origin::classify(&chain, &hosts);
        let whose = match pad_origin::vigem_serial(&chain) {
            Some(s) if serials.contains(&s) => format!(", serial {s}: plugged by this probe"),
            Some(s) => format!(", serial {s}"),
            None => String::new(),
        };
        let slots: Vec<_> = pad
            .lights
            .iter()
            .flatten()
            .filter_map(|&light| xinput_slots::slot_for_light(light))
            .collect();
        println!("   origin: {origin:?}{whose}; XInput slots by its light: {slots:?}");
    }

    if hold > 0 {
        println!("\nholding the pads for {hold} s");
        std::thread::sleep(Duration::from_secs(hold));
    }
    let plugged = !pads.is_empty();
    drop(pads);
    println!(
        "\ndone{}",
        if plugged { "; unplugged the pads" } else { "" }
    );
    Ok(())
}

fn print_chain(chain: &[Node]) {
    for (depth, node) in chain.iter().enumerate() {
        println!(
            "   {}{}  [enum {}, service {}, class {}] {:?} hw {}",
            "  ".repeat(depth),
            node.instance_id,
            or_dash(&node.enumerator),
            or_dash(&node.service),
            or_dash(&node.class),
            node.description,
            node.hardware_ids.first().map_or("-", String::as_str),
        );
    }
}

fn or_dash(s: &str) -> &str {
    if s.is_empty() { "-" } else { s }
}
