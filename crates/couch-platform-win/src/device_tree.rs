//! Where a device sits in Windows' device tree (P9): the device and its
//! parents up to the root, each with its ids, class and driver. A
//! controller's chain tells a physical one (under USB or Bluetooth) from a
//! virtual one (under a virtual bus, such as ViGEmBus's).

use windows::{
    Win32::Devices::{
        DeviceAndDriverInstallation::{
            CM_DRP_CLASSGUID, CM_DRP_DEVICEDESC, CM_DRP_ENUMERATOR_NAME, CM_DRP_HARDWAREID,
            CM_DRP_SERVICE, CM_Get_DevNode_Registry_PropertyW, CM_Get_Device_IDW,
            CM_Get_Device_Interface_PropertyW, CM_Get_Parent, CM_LOCATE_DEVNODE_NORMAL,
            CM_Locate_DevNodeW, CR_BUFFER_SMALL, CR_SUCCESS,
        },
        Properties::{DEVPKEY_Device_InstanceId, DEVPROP_TYPE_STRING, DEVPROPTYPE},
    },
    core::HSTRING,
};

/// The tree is never this deep; a guard against a loop.
const MAX_DEPTH: usize = 32;

/// One device of a chain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Node {
    /// As `HID\VID_045E&PID_028E&IG_00\3&...`.
    pub instance_id: String,
    pub hardware_ids: Vec<String>,
    /// Its setup class, as `{xxxxxxxx-...}`.
    pub class: String,
    /// Its driver's service name, such as `HidUsb` or `ViGEmBus`.
    pub service: String,
    /// What enumerated it: `USB`, `BTHENUM`, `HID`, `ROOT`...
    pub enumerator: String,
    pub description: String,
}

/// The instance id of the device a device interface path belongs to (the
/// `\\?\HID#...#{guid}` paths SDL gives); `None` for anything else.
pub fn instance_for_path(path: &str) -> Option<String> {
    let path = HSTRING::from(path);
    let mut kind = DEVPROPTYPE::default();
    let mut buf = [0u16; 512];
    let mut size = size_of_val(&buf) as u32;
    // SAFETY: `path` is a NUL-terminated wide string that outlives the call;
    // the key is a static; `buf` is writable for `size` bytes.
    let r = unsafe {
        CM_Get_Device_Interface_PropertyW(
            &path,
            &DEVPKEY_Device_InstanceId,
            &mut kind,
            Some(buf.as_mut_ptr().cast()),
            &mut size,
            0,
        )
    };
    if r != CR_SUCCESS || kind != DEVPROP_TYPE_STRING {
        return None;
    }
    let id = wide_str(&buf[..(size as usize / 2).min(buf.len())]);
    (!id.is_empty()).then_some(id)
}

/// The device with `instance_id` and its parents, nearest first, up to the
/// root; empty if no such device is present.
pub fn chain(instance_id: &str) -> Vec<Node> {
    let id = HSTRING::from(instance_id);
    let mut inst = 0u32;
    // SAFETY: `inst` is a valid out-parameter; `id` outlives the call.
    if unsafe { CM_Locate_DevNodeW(&mut inst, &id, CM_LOCATE_DEVNODE_NORMAL) } != CR_SUCCESS {
        return Vec::new();
    }
    let mut out = Vec::new();
    for _ in 0..MAX_DEPTH {
        out.push(node(inst));
        let mut parent = 0u32;
        // SAFETY: `parent` is a valid out-parameter; `inst` came from the
        // configuration manager. The root has no parent, which ends it.
        if unsafe { CM_Get_Parent(&mut parent, inst, 0) } != CR_SUCCESS {
            break;
        }
        inst = parent;
    }
    out
}

fn node(inst: u32) -> Node {
    let mut id = [0u16; 512];
    // SAFETY: `id` is a writable buffer whose length the binding passes.
    let instance_id = if unsafe { CM_Get_Device_IDW(inst, &mut id, 0) } == CR_SUCCESS {
        wide_str(&id)
    } else {
        String::new()
    };
    let first = |property| {
        strings(inst, property)
            .into_iter()
            .next()
            .unwrap_or_default()
    };
    Node {
        instance_id,
        hardware_ids: strings(inst, CM_DRP_HARDWAREID),
        class: first(CM_DRP_CLASSGUID),
        service: first(CM_DRP_SERVICE),
        enumerator: first(CM_DRP_ENUMERATOR_NAME),
        description: first(CM_DRP_DEVICEDESC),
    }
}

/// A device's string or string list property; empty if it has none.
fn strings(inst: u32, property: u32) -> Vec<String> {
    let mut buf = vec![0u8; 2048];
    for _ in 0..2 {
        let mut len = buf.len() as u32;
        // SAFETY: `buf` is writable for `len` bytes; `len` is a valid
        // in/out parameter.
        let r = unsafe {
            CM_Get_DevNode_Registry_PropertyW(
                inst,
                property,
                None,
                Some(buf.as_mut_ptr().cast()),
                &mut len,
                0,
            )
        };
        if r == CR_SUCCESS {
            let wide: Vec<u16> = buf[..(len as usize).min(buf.len())]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            return wide
                .split(|&c| c == 0)
                .filter(|s| !s.is_empty())
                .map(String::from_utf16_lossy)
                .collect();
        }
        // Longer than the buffer: once more, at the size asked for (up to
        // 64 KiB; these are a few hundred bytes).
        if r != CR_BUFFER_SMALL || len as usize <= buf.len() || len > 64 * 1024 {
            break;
        }
        buf = vec![0u8; len as usize];
    }
    Vec::new()
}

fn wide_str(wide: &[u16]) -> String {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controllers;

    /// Every PC has devices; each one's chain ends at the root.
    #[test]
    fn a_present_devices_chain_reaches_the_root() {
        let device = controllers::present().into_iter().next().expect("a device");
        let chain = chain(&device.instance_id);
        assert!(!chain.is_empty());
        assert!(
            chain[0]
                .instance_id
                .eq_ignore_ascii_case(&device.instance_id)
        );
        let root = chain.last().unwrap();
        assert!(
            root.instance_id
                .to_ascii_uppercase()
                .starts_with("HTREE\\ROOT"),
            "{chain:#?}"
        );
    }

    #[test]
    fn unknown_ids_and_paths_give_nothing() {
        assert!(chain("NOT\\A\\DEVICE").is_empty());
        assert_eq!(instance_for_path("XInput#0"), None);
    }
}
