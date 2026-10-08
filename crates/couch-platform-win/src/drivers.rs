//! The two drivers the app relies on (M10a; Settings → Drivers): whether
//! each is installed, and running the installer the app ships for it.
//!
//! - **ViGEmBus**: virtual Xbox 360 controllers, so guests' controllers
//!   reach the host's game ([`crate::vigem`]).
//! - **HidHide**: hides the host's own controller from games while it plays
//!   through a seat ([`crate::hidhide`]).
//!
//! Their official installers are bundled next to the app, in `drivers\`
//! (`xtask/src/deps.rs`, `DRIVERS`). They need administrator rights, so they
//! are started with the "runas" verb, which shows Windows' prompt; this
//! process stays unelevated.

use std::{ffi::c_void, path::PathBuf};

use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE},
        System::{
            Registry::{HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegOpenKeyExW},
            Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
        },
        UI::{
            Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

use crate::{hidhide, vigem};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Driver {
    ViGEmBus,
    HidHide,
}

impl Driver {
    /// Its Windows service, registered once it is installed.
    fn service(self) -> &'static str {
        match self {
            Self::ViGEmBus => "ViGEmBus",
            Self::HidHide => "HidHide",
        }
    }

    /// The bundled installer's name in `drivers\` (xtask's `staged_as`).
    fn setup(self) -> &'static str {
        match self {
            Self::ViGEmBus => "vigembus-setup.exe",
            Self::HidHide => "hidhide-setup.exe",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Installed and answering.
    Ready,
    /// Registered, but not answering yet: Windows needs a restart (HidHide
    /// after installing), or the driver failed to start.
    NeedsRestart,
    /// HidHide's control device is held by another program, most likely
    /// its configuration window.
    Busy,
    Missing,
}

/// Whether `driver` is installed and working.
pub fn state(driver: Driver) -> State {
    let working = match driver {
        Driver::ViGEmBus => vigem::probe().is_ok(),
        Driver::HidHide => match hidhide::state() {
            hidhide::State::Ready => true,
            hidhide::State::Busy => return State::Busy,
            hidhide::State::NotInstalled => false,
        },
    };
    match (working, registered(driver.service())) {
        (true, _) => State::Ready,
        (false, true) => State::NeedsRestart,
        (false, false) => State::Missing,
    }
}

/// The installer shipped with the app for `driver`, if it is there (a
/// development build has it after `cargo xtask stage-dlls`).
pub fn installer(driver: Driver) -> Option<PathBuf> {
    let path = std::env::current_exe()
        .ok()?
        .parent()?
        .join("drivers")
        .join(driver.setup());
    path.exists().then_some(path)
}

/// Runs the bundled installer for `driver`, with Windows' administrator
/// prompt, and waits for it to finish. Blocks: call it off the async
/// runtime. `Err` says why it didn't run or what it reported.
pub fn install(driver: Driver) -> Result<(), String> {
    let path = installer(driver).ok_or("its installer isn't shipped with this build")?;
    let file = HSTRING::from(path.as_os_str());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` is a live, initialised SHELLEXECUTEINFOW whose strings
    // (`file` and a static) outlive the call.
    if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
        return Err(if e.code() == ERROR_CANCELLED.to_hresult() {
            "you declined Windows' administrator prompt".into()
        } else {
            format!("its installer didn't start: {e}")
        });
    }
    let process: HANDLE = info.hProcess;
    if process.is_invalid() {
        // Windows handed the work to an already running instance.
        return Ok(());
    }
    let mut code = 0u32;
    // SAFETY: `process` is the handle SEE_MASK_NOCLOSEPROCESS gave us; it is
    // waited on, read and closed once.
    unsafe {
        WaitForSingleObject(process, INFINITE);
        let read = GetExitCodeProcess(process, &mut code);
        let _ = CloseHandle(process);
        read.map_err(|e| format!("its installer's result: {e}"))?;
    }
    match code {
        0 => Ok(()),
        // Windows Installer's "done; restart to finish" (also what WiX
        // bundles report).
        3010 | 1641 => Ok(()),
        // The user cancelled its setup.
        1602 => Err("its setup was cancelled".into()),
        other => Err(format!("its installer ended with code {other}")),
    }
}

/// Whether a service named `name` is registered.
fn registered(name: &str) -> bool {
    let key = HSTRING::from(format!(r"SYSTEM\CurrentControlSet\Services\{name}"));
    let mut handle = HKEY(std::ptr::null_mut::<c_void>());
    // SAFETY: `key` is a live wide string and `handle` a valid out-pointer;
    // an opened key is closed below.
    unsafe {
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, &key, None, KEY_READ, &mut handle).is_ok() {
            let _ = RegCloseKey(handle);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing here installs anything; it only reads.
    #[test]
    fn states_can_be_read_and_unknown_services_are_missing() {
        let _ = state(Driver::ViGEmBus);
        let _ = state(Driver::HidHide);
        assert!(!registered("CouchCoAppNoSuchService"));
        // Every PC has this one.
        assert!(registered("Tcpip"));
    }
}
