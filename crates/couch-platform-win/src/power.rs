//! Keeping the PC awake while remote play is on (M25c, the unattended PC):
//! a power request, which `powercfg /requests` lists with its reason.
//!
//! A request is a handle, not tied to the thread that made it (unlike
//! `SetThreadExecutionState`), and closing it clears what it asked for, so a
//! crash can't leave the PC kept awake.

use std::io;

use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        Power::{
            PowerCreateRequest, PowerRequestDisplayRequired, PowerRequestSystemRequired,
            PowerSetRequest,
        },
        Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0},
    },
};
use windows_core::PWSTR;

/// `POWER_REQUEST_CONTEXT_VERSION` (`minwinbase.h`).
const CONTEXT_VERSION: u32 = 0;

/// Keeps the system from sleeping, and with `display` the screen on too,
/// until dropped.
pub struct Awake {
    request: HANDLE,
}

// SAFETY: a power request handle may be used and closed from any thread.
unsafe impl Send for Awake {}

impl Awake {
    pub fn new(reason: &str, display: bool) -> io::Result<Self> {
        let mut text: Vec<u16> = reason.encode_utf16().chain([0]).collect();
        let context = REASON_CONTEXT {
            Version: CONTEXT_VERSION,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 {
                SimpleReasonString: PWSTR(text.as_mut_ptr()),
            },
        };
        // SAFETY: `context` and the string it points at live through the
        // call, which copies the string.
        let request = unsafe { PowerCreateRequest(&context) }.map_err(io::Error::other)?;
        let awake = Self { request };
        // SAFETY: a request handle we own (closed by `Drop` on any error).
        unsafe {
            PowerSetRequest(awake.request, PowerRequestSystemRequired).map_err(io::Error::other)?;
            if display {
                PowerSetRequest(awake.request, PowerRequestDisplayRequired)
                    .map_err(io::Error::other)?;
            }
        }
        Ok(awake)
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        // SAFETY: our own handle, closed once: Windows clears its requests.
        let _ = unsafe { CloseHandle(self.request) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asks and lets go; `powercfg /requests` would list it meanwhile.
    #[test]
    fn a_request_is_made_and_dropped() {
        let awake = Awake::new("Couch Co-App test", true).unwrap();
        drop(awake);
    }
}
