//! Wi-Fi "media streaming mode" while streaming or watching (M9a).
//!
//! Windows' WLAN service scans for networks in the background now and then,
//! which shows up as periodic packet loss and jitter on a Wi-Fi link. Media
//! streaming mode asks the adapter to stop that while it's on. Sunshine does
//! the same while streaming (`docs/research/sunshine-moonlight.md`, item 4);
//! this is our own code, on the documented `WlanSetInterface` API.
//!
//! The mode lives with our WLAN client handle: closing it (dropping
//! [`MediaStreaming`]) undoes it, and so does our process ending, crash
//! included, since Windows closes the handle then. So nothing can be left
//! behind.
//!
//! It does nothing, without failing, on a PC without Wi-Fi (no WLAN service or
//! no connected adapter), or where Windows refuses the call; the log says
//! which.

use std::ffi::c_void;

use windows::Win32::{
    Foundation::{ERROR_SUCCESS, HANDLE},
    NetworkManagement::WiFi::{
        WLAN_INTERFACE_INFO, WLAN_INTERFACE_INFO_LIST, WlanCloseHandle, WlanEnumInterfaces,
        WlanFreeMemory, WlanOpenHandle, WlanSetInterface, wlan_interface_state_connected,
        wlan_intf_opcode_media_streaming_mode,
    },
};

/// Media streaming mode, on for every connected Wi-Fi adapter while this
/// lives.
pub struct MediaStreaming {
    handle: HANDLE,
    adapters: usize,
}

// The handle is an opaque WLAN client handle, usable from any thread.
unsafe impl Send for MediaStreaming {}

impl MediaStreaming {
    /// Turns the mode on for every connected Wi-Fi adapter. `None` (logged)
    /// when there is nothing to turn on, or Windows refused.
    pub fn enable() -> Option<Self> {
        let mut negotiated = 0u32;
        let mut handle = HANDLE::default();
        // SAFETY: both out-pointers point at live locals; version 2 is the
        // Vista-and-later client API.
        let opened = unsafe { WlanOpenHandle(2, None, &mut negotiated, &mut handle) };
        if opened != ERROR_SUCCESS.0 {
            tracing::info!(
                "Wi-Fi media streaming mode: no WLAN service (code {opened}), so no Wi-Fi to tune"
            );
            return None;
        }
        // From here on, `Drop` closes the handle on every path.
        let mut mode = Self {
            handle,
            adapters: 0,
        };

        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        // SAFETY: the handle is open; `list` receives memory WLAN allocates,
        // freed below with `WlanFreeMemory`.
        let listed = unsafe { WlanEnumInterfaces(mode.handle, None, &mut list) };
        if listed != ERROR_SUCCESS.0 || list.is_null() {
            tracing::info!("Wi-Fi media streaming mode: no adapters listed (code {listed})");
            return None;
        }
        // SAFETY: on success `list` points at a WLAN_INTERFACE_INFO_LIST whose
        // `InterfaceInfo` array holds `dwNumberOfItems` entries (a flexible
        // array declared with length 1), valid until `WlanFreeMemory`.
        let interfaces: &[WLAN_INTERFACE_INFO] = unsafe {
            std::slice::from_raw_parts(
                (*list).InterfaceInfo.as_ptr(),
                (*list).dwNumberOfItems as usize,
            )
        };
        let mut refused = Vec::new();
        for interface in interfaces {
            if interface.isState != wlan_interface_state_connected {
                continue;
            }
            let on: i32 = 1; // a Win32 BOOL
            // SAFETY: the handle is open, the GUID comes from the list (still
            // alive), and the data is a BOOL of the size we pass.
            let set = unsafe {
                WlanSetInterface(
                    mode.handle,
                    &interface.InterfaceGuid,
                    wlan_intf_opcode_media_streaming_mode,
                    size_of::<i32>() as u32,
                    &on as *const i32 as *const c_void,
                    None,
                )
            };
            if set == ERROR_SUCCESS.0 {
                mode.adapters += 1;
            } else {
                refused.push(set);
            }
        }
        // SAFETY: `list` came from WlanEnumInterfaces and isn't used after this.
        unsafe { WlanFreeMemory(list as *const c_void) };

        if !refused.is_empty() {
            tracing::info!(
                "Wi-Fi media streaming mode: refused for {} adapter(s), codes {refused:?}",
                refused.len()
            );
        }
        if mode.adapters == 0 {
            tracing::info!("Wi-Fi media streaming mode: no connected Wi-Fi adapter to tune");
            return None;
        }
        tracing::info!(
            "Wi-Fi media streaming mode on for {} adapter(s)",
            mode.adapters
        );
        Some(mode)
    }

    /// How many adapters it turned on.
    pub fn adapters(&self) -> usize {
        self.adapters
    }
}

impl Drop for MediaStreaming {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by WlanOpenHandle and is closed only
        // here. Closing it ends the mode.
        unsafe { WlanCloseHandle(self.handle, None) };
        if self.adapters > 0 {
            tracing::info!("Wi-Fi media streaming mode off");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On any PC: turning it on and off again never fails or panics. On a
    /// PC without Wi-Fi (both development PCs) it is simply `None`.
    #[test]
    fn enabling_and_dropping_is_harmless() {
        if let Some(mode) = MediaStreaming::enable() {
            assert!(mode.adapters() > 0);
        }
    }
}
