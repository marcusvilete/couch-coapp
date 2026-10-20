//! Where a gamepad comes from (P9): a physical controller, one of a Couch
//! Co-App host's virtual pads on this PC, or another program's virtual pad.
//!
//! Ids can't say: our pads must carry a real Xbox 360 controller's (games
//! need them), and so do a remapper's. The device tree can. A gamepad SDL
//! lists is found in it by its device path, or for an Xbox-style pad, which
//! SDL names only `XInput#N`, by the driver's player light for slot N
//! (`xinput_slots`). Its chain of parents then says:
//! - under ViGEmBus: a virtual pad, ours if its serial (the last part of its
//!   instance id) is in a running host's marker (`virtual_pads`), or while a
//!   host is still plugging one;
//! - under a USB root hub or Bluetooth: physical;
//! - under another device Windows enumerated in software (`ROOT`, `SWD`):
//!   another program's virtual pad;
//! - otherwise, or if the device can't be found or doesn't carry the ids
//!   SDL gave: unknown. Callers then fall back to the rule before P9: a pad
//!   with our pads' ids counts as ours (`ControllerIds::could_be_ours`).

use couch_input::local::ControllerIds;

use crate::{
    controllers,
    device_tree::{self, Node},
    virtual_pads::{self, HostPads},
    xinput_slots,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Physical,
    /// One of a Couch Co-App host's virtual pads on this PC.
    Ours,
    /// Another program's virtual pad (a remapper's, say).
    OtherVirtual,
    Unknown,
}

/// The origin of the gamepad SDL lists with `path` (its `SDL_GetGamepadPath`)
/// and `ids`.
pub fn of_gamepad(path: Option<&str>, ids: ControllerIds) -> Origin {
    of_gamepad_given(path, ids, &virtual_pads::host_pads())
}

/// [`of_gamepad`], with the hosts' pads already read.
pub fn of_gamepad_given(path: Option<&str>, ids: ControllerIds, hosts: &HostPads) -> Origin {
    let Some(instance) = path.and_then(device_for) else {
        return Origin::Unknown;
    };
    let chain = device_tree::chain(&instance);
    // SDL's slot and the driver's light must agree on which pad this is.
    if !chain
        .iter()
        .any(|n| controllers::names_ids(&n.hardware_ids, ids))
    {
        return Origin::Unknown;
    }
    classify(&chain, hosts)
}

/// The origin of the device with `instance_id`, any ViGEm pad counting as
/// ours whoever plugged it: for deciding what may be hidden.
pub fn of_device(instance_id: &str) -> Origin {
    let any_host = HostPads {
        plugging: true,
        ..HostPads::default()
    };
    classify(&device_tree::chain(instance_id), &any_host)
}

/// Whether a controller of `origin` with `ids` may be the host's own
/// controller: listed as one, followed, hidden. A physical one may; a
/// virtual pad never; one whose origin can't be told only without our pads'
/// ids, the rule before P9.
pub fn may_be_the_hosts(origin: Origin, ids: ControllerIds) -> bool {
    match origin {
        Origin::Physical => true,
        Origin::Unknown => !ids.could_be_ours(),
        Origin::Ours | Origin::OtherVirtual => false,
    }
}

/// The device of an SDL path: an XInput slot's by the driver's player
/// lights (exactly one must show it), or a device interface path's own.
fn device_for(path: &str) -> Option<String> {
    let Some(slot) = path.strip_prefix("XInput#") else {
        return device_tree::instance_for_path(path);
    };
    let slot: u8 = slot.parse().ok()?;
    let pads = xinput_slots::xusb_pads();
    // A pad whose light can't be read here might be the one on that slot,
    // and its slot would look free for another to claim.
    if pads
        .iter()
        .any(|pad| pad.error.is_some() || pad.lights.iter().any(Option::is_none))
    {
        return None;
    }
    let mut showing = pads.into_iter().filter(|pad| {
        pad.lights
            .iter()
            .flatten()
            .any(|&light| xinput_slots::slot_for_light(light) == Some(slot))
    });
    let pad = showing.next()?;
    if showing.next().is_some() {
        return None;
    }
    pad.instance_id
}

/// A device's origin from its chain (nearest first), given the hosts' pads.
pub fn classify(chain: &[Node], hosts: &HostPads) -> Origin {
    if let Some(bus) = chain.iter().position(is_vigem_bus) {
        return match serial_under(chain, bus) {
            Some(serial) if !hosts.plugging && !hosts.serials.contains(&serial) => {
                Origin::OtherVirtual
            }
            // Ours, one being plugged that may be it, or no serial to tell.
            _ => Origin::Ours,
        };
    }
    for node in chain {
        let enumerator = node.enumerator.to_ascii_uppercase();
        if matches!(enumerator.as_str(), "BTHENUM" | "BTHLEDEVICE" | "BTHLE")
            || node
                .instance_id
                .to_ascii_uppercase()
                .starts_with("USB\\ROOT_HUB")
        {
            return Origin::Physical;
        }
        if matches!(enumerator.as_str(), "ROOT" | "SWD") {
            return Origin::OtherVirtual;
        }
        // Past the buses a controller can hang from, nearer the root: every
        // chain ends under ROOT, which says nothing of the device.
        if matches!(enumerator.as_str(), "PCI" | "ACPI" | "ACPI_HAL" | "HTREE") {
            break;
        }
    }
    Origin::Unknown
}

/// ViGEmBus's own node: by its driver, or by its hardware id should the
/// driver's name be unreadable.
fn is_vigem_bus(node: &Node) -> bool {
    node.service.eq_ignore_ascii_case("ViGEmBus")
        || node
            .hardware_ids
            .iter()
            .any(|id| id.eq_ignore_ascii_case(r"Nefarius\ViGEmBus\Gen1"))
}

/// The serial of a ViGEm pad in the chain: the last part of the instance id
/// of the device right under the bus.
pub fn vigem_serial(chain: &[Node]) -> Option<u32> {
    serial_under(chain, chain.iter().position(is_vigem_bus)?)
}

fn serial_under(chain: &[Node], bus: usize) -> Option<u32> {
    let pad = chain.get(bus.checked_sub(1)?)?;
    pad.instance_id.rsplit('\\').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn node(instance_id: &str, enumerator: &str, service: &str) -> Node {
        Node {
            instance_id: instance_id.into(),
            enumerator: enumerator.into(),
            service: service.into(),
            ..Node::default()
        }
    }

    /// The chains P9's probe recorded on the dev PC, trimmed to what counts.
    fn dualsense_bluetooth() -> Vec<Node> {
        vec![
            node(
                r"HID\{00001124-...}_VID&0002054C_PID&0CE6\8&39D32BA7&2&0000",
                "HID",
                "",
            ),
            node(
                r"BTHENUM\{00001124-...}_VID&0002054C_PID&0CE6\7&3449",
                "BTHENUM",
                "HidBth",
            ),
            node(r"BTH\MS_BTHBRB\6&35E9DD16&1&1", "BTH", "BthEnum"),
            node(r"USB\VID_0A12&PID_0001\5&2CF64626&0&5", "USB", "BTHUSB"),
            node(r"USB\ROOT_HUB30\4&6970116&0&0", "USB", "USBHUB3"),
            node(r"PCI\VEN_8086&DEV_7AE0\3&11583659&0&A0", "PCI", "USBXHCI"),
            node(r"ROOT\ACPI_HAL\0000", "ROOT", r"\Driver\ACPI_HAL"),
            node(r"HTREE\ROOT\0", "HTREE", ""),
        ]
    }

    fn gamesir_usb() -> Vec<Node> {
        vec![
            node(
                r"USB\VID_3537&PID_1040&MI_00\7&9849E94&0&0000",
                "USB",
                "xusb22",
            ),
            node(r"USB\VID_3537&PID_1040\6&1D73F1DC&0&3", "USB", "usbccgp"),
            node(r"USB\ROOT_HUB30\5&8CFB068&0&0", "USB", "USBHUB3"),
            node(r"PCI\VEN_1B21&DEV_2142\4&394F6BFA&0&00E0", "PCI", "USBXHCI"),
            node(r"ROOT\ACPI_HAL\0000", "ROOT", r"\Driver\ACPI_HAL"),
            node(r"HTREE\ROOT\0", "HTREE", ""),
        ]
    }

    fn vigem_pad(serial: u32) -> Vec<Node> {
        vec![
            node(r"HID\VID_045E&PID_028E&IG_01\3&18276F9D&0&0000", "HID", ""),
            node(
                r"USB\VID_045E&PID_028E&IG_01\2&24AF3C9A&1&01",
                "USB",
                "HidUsb",
            ),
            node(&format!(r"USB\VID_045E&PID_028E\{serial}"), "USB", "xusb22"),
            node(r"ROOT\SYSTEM\0003", "ROOT", "ViGEmBus"),
            node(r"HTREE\ROOT\0", "HTREE", ""),
        ]
    }

    fn hosts(serials: &[u32], plugging: bool) -> HostPads {
        HostPads {
            serials: serials.iter().copied().collect::<BTreeSet<_>>(),
            plugging,
        }
    }

    #[test]
    fn physical_controllers_by_their_bus() {
        let none = hosts(&[], false);
        assert_eq!(classify(&dualsense_bluetooth(), &none), Origin::Physical);
        assert_eq!(classify(&gamesir_usb(), &none), Origin::Physical);
        // From any node of it, the XInput collection's or the USB node's.
        assert_eq!(classify(&gamesir_usb()[1..], &none), Origin::Physical);
    }

    #[test]
    fn a_vigem_pad_is_ours_only_by_a_hosts_serial() {
        assert_eq!(
            classify(&vigem_pad(51), &hosts(&[51, 52], false)),
            Origin::Ours
        );
        assert_eq!(
            classify(&vigem_pad(36), &hosts(&[51, 52], false)),
            Origin::OtherVirtual,
            "a remapper's, on the same bus"
        );
        assert_eq!(
            classify(&vigem_pad(36), &hosts(&[], false)),
            Origin::OtherVirtual
        );
    }

    /// Doubt makes a ViGEm pad ours: a host still plugging one, or no serial
    /// to read.
    #[test]
    fn doubt_makes_a_vigem_pad_ours() {
        assert_eq!(classify(&vigem_pad(36), &hosts(&[51], true)), Origin::Ours);
        let mut odd = vigem_pad(36);
        odd[2].instance_id = r"USB\VID_045E&PID_028E\NOT-A-NUMBER".into();
        assert_eq!(classify(&odd, &hosts(&[], false)), Origin::Ours);
        // The bus at the very start of a chain has no pad under it.
        assert_eq!(
            classify(&vigem_pad(36)[3..], &hosts(&[], false)),
            Origin::Ours
        );
    }

    #[test]
    fn other_software_buses_are_virtual_and_the_rest_unknown() {
        let none = hosts(&[], false);
        let soft = [
            node(r"HID\VID_054C&PID_05C4\1&2", "HID", ""),
            node(r"ROOT\SOMEBUS\0000", "ROOT", "SomeRemapperBus"),
            node(r"HTREE\ROOT\0", "HTREE", ""),
        ];
        assert_eq!(classify(&soft, &none), Origin::OtherVirtual);
        let swd = [node(r"SWD\SOMETHING\1", "SWD", "x")];
        assert_eq!(classify(&swd, &none), Origin::OtherVirtual);
        // Under PCI with no bus we know: the ROOT above it says nothing.
        let odd = [
            node(r"HID\VID_1234&PID_5678\1", "HID", ""),
            node(r"PCI\VEN_1234\2", "PCI", "x"),
            node(r"ROOT\ACPI_HAL\0000", "ROOT", r"\Driver\ACPI_HAL"),
        ];
        assert_eq!(classify(&odd, &none), Origin::Unknown);
        assert_eq!(classify(&[], &none), Origin::Unknown);
    }

    /// The bus is known by its hardware id too: with its driver's name
    /// unreadable, a host's pad must never pass for another program's.
    #[test]
    fn vigem_is_known_by_its_hardware_id_too() {
        let mut chain = vigem_pad(51);
        chain[3].service.clear();
        chain[3].hardware_ids = vec![r"Nefarius\ViGEmBus\Gen1".into()];
        assert_eq!(classify(&chain, &hosts(&[51], false)), Origin::Ours);
        assert_eq!(vigem_serial(&chain), Some(51));
        assert_eq!(vigem_serial(&gamesir_usb()), None);
    }

    #[test]
    fn only_a_physical_controller_or_an_unknown_one_without_our_ids_is_the_hosts() {
        let ours = ControllerIds {
            vendor: 0x045E,
            product: 0x028E,
        };
        let dualsense = ControllerIds {
            vendor: 0x054C,
            product: 0x0CE6,
        };
        assert!(
            may_be_the_hosts(Origin::Physical, ours),
            "a real Xbox 360 pad"
        );
        assert!(may_be_the_hosts(Origin::Unknown, dualsense));
        assert!(!may_be_the_hosts(Origin::Unknown, ours));
        for virtual_pad in [Origin::Ours, Origin::OtherVirtual] {
            assert!(!may_be_the_hosts(virtual_pad, dualsense));
        }
    }

    #[test]
    fn unknown_paths_are_unknown() {
        let ids = ControllerIds {
            vendor: 0x045E,
            product: 0x028E,
        };
        assert_eq!(of_gamepad(None, ids), Origin::Unknown);
        assert_eq!(of_gamepad(Some("nothing"), ids), Origin::Unknown);
        // No pad shows slot 7's light.
        assert_eq!(of_gamepad(Some("XInput#7"), ids), Origin::Unknown);
    }
}
