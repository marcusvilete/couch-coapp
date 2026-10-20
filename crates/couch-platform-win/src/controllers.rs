//! Game controllers plugged into this PC, counted for Go live's warning: in
//! a multiplayer game, the host's own controller can take a player's place.
//! Also every present device with its ids, class and container ([`present`]),
//! which `crate::hidhide` uses to find all of one controller's nodes.
//!
//! Counts present HID collections that are joysticks or gamepads (usage page
//! 1, usage 4 or 5). XInput controllers show one too, and so do virtual pads;
//! a Couch Co-App host's own are left out (P9, `crate::pad_origin`), another
//! program's count, since games see them. A controller with several such
//! collections counts more than once: good enough for a warning.
//!
//! The usage is in a collection's hardware ids for an XInput pad's "HID-
//! compatible game controller", which has no compatible ids at all (seen
//! with ViGEm's Xbox 360 pad); other drivers may put it in the compatible
//! ids. Both lists are read.

use couch_input::local::ControllerIds;
use windows::{
    Win32::Devices::DeviceAndDriverInstallation::{
        DIGCF_ALLCLASSES, DIGCF_PRESENT, HDEVINFO, SETUP_DI_REGISTRY_PROPERTY, SP_DEVINFO_DATA,
        SPDRP_BASE_CONTAINERID, SPDRP_CLASSGUID, SPDRP_COMPATIBLEIDS, SPDRP_HARDWAREID,
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
        SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceRegistryPropertyW,
    },
    core::w,
};

use crate::{
    device_tree, pad_origin,
    virtual_pads::{self, HostPads},
};

/// The ids of a HID joystick and a HID gamepad.
const IDS: [&str; 2] = ["HID_DEVICE_UP:0001_U:0004", "HID_DEVICE_UP:0001_U:0005"];

/// How many game controllers games can see: every one plugged in but the
/// virtual pads of Couch Co-App hosts on this PC (P9); 0 if they can't be
/// listed.
pub fn count() -> usize {
    count_leaving_out(Some(&virtual_pads::host_pads()))
}

/// Every game controller plugged in, the hosts' pads included (for
/// `couch-cli pad-test`, to see its pads appear).
pub fn count_all() -> usize {
    count_leaving_out(None)
}

fn count_leaving_out(hosts: Option<&HostPads>) -> usize {
    // SAFETY: no class GUID, a static enumerator string and no window; the
    // returned set is destroyed by the guard.
    let Ok(set) =
        (unsafe { SetupDiGetClassDevsW(None, w!("HID"), None, DIGCF_ALLCLASSES | DIGCF_PRESENT) })
    else {
        return 0;
    };
    let set = DevInfo(set);
    let mut count = 0;
    for index in 0.. {
        let mut data = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: `data` is a valid out-parameter with its size set; the set
        // lives until the guard drops.
        if unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut data) }.is_err() {
            break;
        }
        let game = [SPDRP_HARDWAREID, SPDRP_COMPATIBLEIDS]
            .into_iter()
            .any(|property| {
                ids(&set, &data, property)
                    .iter()
                    .any(|id| IDS.iter().any(|want| id.eq_ignore_ascii_case(want)))
            });
        let hosts_pad = hosts.is_some_and(|h| h.any() && is_hosts(&set, &data, h));
        if game && !hosts_pad {
            count += 1;
        }
    }
    count
}

/// Whether a device is one of the hosts' virtual pads.
fn is_hosts(set: &DevInfo, data: &SP_DEVINFO_DATA, hosts: &HostPads) -> bool {
    instance_id(set, data).is_some_and(|id| {
        pad_origin::classify(&device_tree::chain(&id), hosts) == pad_origin::Origin::Ours
    })
}

fn instance_id(set: &DevInfo, data: &SP_DEVINFO_DATA) -> Option<String> {
    let mut id = [0u16; 512];
    let mut needed = 0u32;
    // SAFETY: `id` is a writable buffer whose length the binding passes;
    // `data` came from enumerating this set. Instance ids are at most 200
    // characters (MAX_DEVICE_ID_LEN).
    unsafe { SetupDiGetDeviceInstanceIdW(set.0, data, Some(&mut id), Some(&mut needed)) }.ok()?;
    Some(String::from_utf16_lossy(
        &id[..id.iter().position(|&c| c == 0).unwrap_or(0)],
    ))
}

/// A device plugged in now, as [`present`] lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Device {
    /// As `SetupDiGetDeviceInstanceId` gives it, e.g. `HID\VID_054C&PID_0CE6&MI_03\7&...`.
    pub instance_id: String,
    pub hardware_ids: Vec<String>,
    /// Its setup class, as `{xxxxxxxx-...}`.
    pub class: String,
    /// The physical device it belongs to: every node of one controller (its
    /// USB or Bluetooth node, its HID children, its XInput node) shares it.
    pub container: Option<String>,
}

/// Every device plugged in now, of any class; empty if they can't be listed.
pub(crate) fn present() -> Vec<Device> {
    // SAFETY: no class GUID or enumerator, and no window; the returned set
    // is destroyed by the guard.
    let Ok(set) =
        (unsafe { SetupDiGetClassDevsW(None, None, None, DIGCF_ALLCLASSES | DIGCF_PRESENT) })
    else {
        return Vec::new();
    };
    let set = DevInfo(set);
    let mut devices = Vec::new();
    for index in 0.. {
        let mut data = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: as in `count`.
        if unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut data) }.is_err() {
            break;
        }
        let Some(instance_id) = instance_id(&set, &data) else {
            continue;
        };
        let first = |property| ids(&set, &data, property).into_iter().next();
        devices.push(Device {
            instance_id,
            hardware_ids: ids(&set, &data, SPDRP_HARDWAREID),
            class: first(SPDRP_CLASSGUID).unwrap_or_default(),
            container: first(SPDRP_BASE_CONTAINERID),
        });
    }
    devices
}

/// A device's string or string list property (a `REG_SZ` or `REG_MULTI_SZ`,
/// such as its hardware or compatible ids); empty if it has none.
fn ids(set: &DevInfo, data: &SP_DEVINFO_DATA, property: SETUP_DI_REGISTRY_PROPERTY) -> Vec<String> {
    let mut buf = vec![0u8; 2048];
    let mut needed = 0u32;
    // A list longer than the buffer: once more, at the size asked for (up
    // to 64 KiB; id lists are a few hundred bytes).
    for _ in 0..2 {
        // SAFETY: `buf` is a writable buffer whose length the binding
        // passes; `data` came from enumerating this set.
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                set.0,
                data,
                property,
                None,
                Some(&mut buf),
                Some(&mut needed),
            )
        };
        if ok.is_ok() {
            break;
        }
        let wanted = needed as usize;
        if wanted <= buf.len() || wanted > 64 * 1024 {
            return Vec::new();
        }
        buf = vec![0u8; wanted];
        needed = 0;
    }
    if needed == 0 {
        return Vec::new();
    }
    let len = (needed as usize).min(buf.len()) / 2;
    let wide: Vec<u16> = buf[..len * 2]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    wide.split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// Whether hardware ids name `ids`: `VID_054C&PID_0CE6` (USB), or
/// `VID&0002054C_PID&0CE6` (Bluetooth, with a prefix before the vendor).
pub(crate) fn names_ids(hardware_ids: &[String], ids: ControllerIds) -> bool {
    hardware_ids.iter().any(|id| {
        let id = id.to_ascii_uppercase();
        hex_after(&id, "VID") == Some(ids.vendor) && hex_after(&id, "PID") == Some(ids.product)
    })
}

/// The 16-bit hex value after `key` and a `_` or `&`: the last four digits
/// of a run of up to eight.
fn hex_after(id: &str, key: &str) -> Option<u16> {
    let at = id.find(key)? + key.len();
    let rest = id.get(at..)?;
    let rest = rest.strip_prefix('_').or_else(|| rest.strip_prefix('&'))?;
    let digits: String = rest
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .take(8)
        .collect();
    if digits.len() < 4 {
        return None;
    }
    u16::from_str_radix(&digits[digits.len() - 4..], 16).ok()
}

struct DevInfo(HDEVINFO);

impl Drop for DevInfo {
    fn drop(&mut self) {
        // SAFETY: the set came from `SetupDiGetClassDevsW` and is destroyed
        // once.
        let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever is plugged in, listing works and doesn't hang.
    #[test]
    fn counting_controllers_works() {
        let n = count();
        assert!(n < 64, "{n}");
    }

    #[test]
    fn ids_are_read_from_usb_and_bluetooth_forms() {
        let dualsense = ControllerIds {
            vendor: 0x054C,
            product: 0x0CE6,
        };
        let bt = [r"HID\{00001124-0000-1000-8000-00805f9b34fb}_VID&0002054c_PID&0ce6".into()];
        assert!(names_ids(&bt, dualsense));
        assert!(names_ids(
            &[r"USB\VID_054C&PID_0CE6&REV_0100".into()],
            dualsense
        ));
        assert!(!names_ids(&[r"USB\VID_045E&PID_028E".into()], dualsense));
        assert_eq!(hex_after("USB\\VID_054C&PID_0CE6", "VID"), Some(0x054C));
        assert_eq!(hex_after("HID\\VID_12", "VID"), None);
    }

    /// Every PC has devices with instance ids and classes.
    #[test]
    fn listing_devices_works() {
        let devices = present();
        assert!(!devices.is_empty());
        assert!(devices.iter().all(|d| !d.instance_id.is_empty()));
        assert!(devices.iter().any(|d| d.class.starts_with('{')));
        assert!(devices.iter().any(|d| d.container.is_some()));
    }
}
