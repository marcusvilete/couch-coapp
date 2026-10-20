//! Which XInput slot each Xbox-style pad has (P9). SDL names those pads only
//! by slot (`XInput#N`), so the slot is how a gamepad SDL lists is found in
//! Windows' device tree (`device_tree`).
//!
//! The Xbox 360 driver (xusb22) gives every such pad, ours and other
//! programs' virtual ones included, a device interface, and answers which
//! player light it shows: the light XInput set for its slot. Those requests
//! are undocumented; their layouts here are as community projects describe
//! them, and P9's probe checks them against ViGEm's own word for our pads.

use windows::{
    Win32::{
        Devices::DeviceAndDriverInstallation::{
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW,
            CM_Get_Device_Interface_ListW, CR_BUFFER_SMALL, CR_SUCCESS,
        },
        Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE},
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
        System::IO::DeviceIoControl,
    },
    core::{GUID, HSTRING, PCWSTR},
};

use crate::device_tree;

/// The Xbox 360 driver's device interface (`GUID_DEVINTERFACE_XUSB`).
const XUSB_INTERFACE: GUID = GUID::from_u128(0xEC87F1E3_C13B_4100_B5F7_8B84D54260CB);
/// `IOCTL_XUSB_GET_INFORMATION`: the driver's version, how many pads the
/// interface carries (a wireless receiver has four), its ids.
const GET_INFORMATION: u32 = 0x8000_6000;
/// `IOCTL_XUSB_GET_LED_STATE`: in `{ version: u16, index: u8 }`, out
/// `{ version: u16, led: u8 }`.
const GET_LED_STATE: u32 = 0x8000_E008;

/// One Xbox driver interface and what it answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XusbPad {
    pub path: String,
    /// The device it belongs to, for `device_tree::chain`.
    pub instance_id: Option<String>,
    /// `GET_INFORMATION`'s answer as it came, for the probe to show.
    pub information: Vec<u8>,
    /// The version the light was read with, if it was.
    pub version: Option<u16>,
    /// Each pad's player light on this interface, in order; `None` where
    /// it couldn't be read.
    pub lights: Vec<Option<u8>>,
    /// Why it couldn't be opened, if it couldn't.
    pub error: Option<String>,
}

/// The XInput slot a player light means: lights 2 to 5 flash and 6 to 9
/// stay on for slots 0 to 3; anything else is no slot.
pub fn slot_for_light(light: u8) -> Option<u8> {
    match light {
        2..=5 => Some(light - 2),
        6..=9 => Some(light - 6),
        _ => None,
    }
}

/// Every Xbox driver interface present, with its device and lights.
pub fn xusb_pads() -> Vec<XusbPad> {
    interface_paths(&XUSB_INTERFACE)
        .into_iter()
        .map(|path| {
            let mut pad = XusbPad {
                instance_id: device_tree::instance_for_path(&path),
                ..XusbPad::default()
            };
            match Device::open(&path) {
                Ok(device) => device.read(&mut pad),
                Err(e) => pad.error = Some(e.to_string()),
            }
            pad.path = path;
            pad
        })
        .collect()
}

struct Device(HANDLE);

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateFileW` and is closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

impl Device {
    fn open(path: &str) -> windows::core::Result<Self> {
        let path = HSTRING::from(path);
        // SAFETY: `path` is a NUL-terminated wide string that outlives the
        // call; no security attributes or template. XInput opens these the
        // same way; opening changes nothing on the pad.
        let handle = unsafe {
            CreateFileW(
                &path,
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }?;
        Ok(Self(handle))
    }

    fn ioctl(&self, code: u32, input: &[u8], output: &mut [u8]) -> windows::core::Result<usize> {
        let mut returned = 0u32;
        // SAFETY: the buffers are valid for the lengths passed, and the call
        // is synchronous (no OVERLAPPED), so they outlive it.
        unsafe {
            DeviceIoControl(
                self.0,
                code,
                (!input.is_empty()).then_some(input.as_ptr().cast()),
                input.len() as u32,
                (!output.is_empty()).then_some(output.as_mut_ptr().cast()),
                output.len() as u32,
                Some(&mut returned),
                None,
            )
        }?;
        Ok((returned as usize).min(output.len()))
    }

    fn read(&self, pad: &mut XusbPad) {
        let mut info = [0u8; 64];
        let count = match self.ioctl(GET_INFORMATION, &[], &mut info) {
            Ok(n) => {
                pad.information = info[..n].to_vec();
                // Its version, then how many pads it carries.
                if n >= 3 { info[2] } else { 1 }
            }
            Err(e) => {
                pad.error = Some(format!("information: {e}"));
                1
            }
        };
        // 1.1 first: the driver reports 1.3 on the dev PC, but answers the
        // light request only at 1.1. Then the version it reports, then 1.0.
        let told = (pad.information.len() >= 2)
            .then(|| u16::from_le_bytes([pad.information[0], pad.information[1]]));
        let mut versions = vec![0x0101];
        for v in told.into_iter().chain([0x0100]) {
            if !versions.contains(&v) {
                versions.push(v);
            }
        }
        for index in 0..count.clamp(1, 4) {
            let mut light = None;
            for &version in &versions {
                let [lo, hi] = version.to_le_bytes();
                let mut out = [0u8; 3];
                if let Ok(3) = self.ioctl(GET_LED_STATE, &[lo, hi, index], &mut out) {
                    light = Some(out[2]);
                    pad.version = Some(version);
                    break;
                }
            }
            pad.lights.push(light);
        }
    }
}

/// The paths of the device interfaces of a class present now.
fn interface_paths(class: &GUID) -> Vec<String> {
    for _ in 0..3 {
        let mut len = 0u32;
        // SAFETY: `len` is a valid out-parameter; `class` outlives the call.
        let r = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut len,
                class,
                PCWSTR::null(),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if r != CR_SUCCESS || len == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        // SAFETY: `buf` is a writable buffer whose length the binding passes.
        let r = unsafe {
            CM_Get_Device_Interface_ListW(
                class,
                PCWSTR::null(),
                &mut buf,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if r == CR_SUCCESS {
            return buf
                .split(|&c| c == 0)
                .filter(|s| !s.is_empty())
                .map(String::from_utf16_lossy)
                .collect();
        }
        // A pad came between the two calls: ask the size again.
        if r != CR_BUFFER_SMALL {
            return Vec::new();
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lights_name_slots() {
        assert_eq!(slot_for_light(6), Some(0));
        assert_eq!(slot_for_light(9), Some(3));
        assert_eq!(slot_for_light(2), Some(0));
        assert_eq!(slot_for_light(5), Some(3));
        for none in [0, 1, 10, 13, 255] {
            assert_eq!(slot_for_light(none), None);
        }
    }

    /// With or without pads plugged in, listing works and doesn't hang.
    #[test]
    fn listing_xbox_pads_works() {
        let pads = xusb_pads();
        assert!(pads.len() < 64, "{pads:#?}");
    }
}
