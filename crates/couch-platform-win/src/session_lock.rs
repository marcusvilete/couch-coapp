//! Whether Windows shows the lock screen (or another secure desktop, such as
//! a UAC prompt) rather than the user's desktop (M25c): nothing gets in
//! then, and a capture shows black or freezes, so a device running this PC
//! from afar is told.
//!
//! The input desktop is the user's `Default` while unlocked. Locked, it is
//! Winlogon's, which a normal process can't open: that counts as locked too.

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, GetUserObjectInformationW,
    OpenInputDesktop, UOI_NAME,
};

/// Whether the input desktop is anything but the user's own.
pub fn locked() -> bool {
    // SAFETY: no pointers; the handle, if any, is closed below.
    let Ok(desktop) =
        (unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) })
    else {
        return true;
    };
    let mut name = [0u16; 64];
    let mut needed = 0u32;
    // SAFETY: `name` is a live buffer whose size in bytes is passed; the
    // desktop handle is ours until closed.
    let read = unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            size_of_val(&name) as u32,
            Some(&mut needed),
        )
    };
    // SAFETY: ours, closed once.
    let _ = unsafe { CloseDesktop(desktop) };
    if read.is_err() {
        return true;
    }
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    !String::from_utf16_lossy(&name[..len]).eq_ignore_ascii_case("Default")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On a PC someone is using: its own desktop. Ignored, since a build
    /// machine may run tests with no interactive desktop at all.
    #[test]
    #[ignore = "needs an interactive, unlocked desktop"]
    fn an_unlocked_pc_isnt_locked() {
        assert!(!locked());
    }
}
