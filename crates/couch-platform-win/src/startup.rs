//! Starting the app when the user signs in to Windows (M25c, the unattended
//! PC): a value under this user's `Run` key, naming the app's exe with
//! `--background`, so it starts to the notification area.
//!
//! Only the user's own hive, and only a value that names this very exe
//! counts as ours: another install's (an app installed for everyone beside
//! one for just this user) is never read as set, nor removed.

use std::{io, path::Path};

use windows::Win32::{
    Foundation::ERROR_FILE_NOT_FOUND,
    System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW},
};
use windows_core::HSTRING;

use crate::links::{check, command_exe, read_value, same_file};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// The argument the app starts with at sign-in: no window, the tray only.
pub const BACKGROUND: &str = "--background";

/// Whether `name` under the user's Run key starts `exe`.
pub fn is_set(name: &str, exe: &Path) -> io::Result<bool> {
    is_set_in(RUN, name, exe)
}

/// Starts `exe` at sign-in, with [`BACKGROUND`], under `name`; or stops,
/// removing the value only if it is ours.
pub fn set(name: &str, exe: &Path, on: bool) -> io::Result<()> {
    set_in(RUN, name, exe, on)
}

/// [`is_set`] under `key` (tests use a scratch key, never the real Run).
fn is_set_in(key: &str, name: &str, exe: &Path) -> io::Result<bool> {
    Ok(read_value(key, Some(name))?
        .and_then(|command| command_exe(&command))
        .is_some_and(|registered| same_file(Path::new(&registered), exe)))
}

/// [`set`] under `key`.
fn set_in(key: &str, name: &str, exe: &Path, on: bool) -> io::Result<()> {
    let subkey = key;
    let key = HSTRING::from(subkey);
    let value = HSTRING::from(name);
    if on {
        let command: Vec<u16> = format!("\"{}\" {BACKGROUND}", exe.display())
            .encode_utf16()
            .chain([0])
            .collect();
        // SAFETY: live NUL-terminated strings, and `command`'s size in bytes.
        return check(unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &key,
                &value,
                REG_SZ.0,
                Some(command.as_ptr().cast()),
                (command.len() * 2) as u32,
            )
        });
    }
    if !is_set_in(subkey, name, exe)? {
        return Ok(());
    }
    // SAFETY: live NUL-terminated wide strings; one value of this user's
    // Run key, which names this exe.
    match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &key, &value) } {
        ERROR_FILE_NOT_FOUND => Ok(()),
        code => check(code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use windows::Win32::System::Registry::RegDeleteTreeW;

    /// A scratch key of the test's own, never the real Run key: a killed
    /// run must not leave anything that starts at sign-in.
    fn scratch(test: &str) -> String {
        format!(r"Software\CouchCoApp\Test\{test}-{}", std::process::id())
    }

    /// Deletes the test's scratch key when dropped.
    struct Cleanup(String);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            // SAFETY: a live NUL-terminated wide string; the test's own key.
            let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(self.0.as_str())) };
        }
    }

    #[test]
    fn this_program_starts_at_sign_in_and_stops() {
        let key = scratch("start");
        let _cleanup = Cleanup(key.clone());
        let exe = std::env::current_exe().unwrap();
        assert!(!is_set_in(&key, "Couch Co-App", &exe).unwrap());
        set_in(&key, "Couch Co-App", &exe, true).unwrap();
        assert!(is_set_in(&key, "Couch Co-App", &exe).unwrap());
        let command = read_value(&key, Some("Couch Co-App")).unwrap().unwrap();
        assert!(command.ends_with(" --background"), "{command}");
        assert!(command.starts_with('"'), "quoted, for paths with spaces");
        set_in(&key, "Couch Co-App", &exe, false).unwrap();
        assert!(!is_set_in(&key, "Couch Co-App", &exe).unwrap());
        assert!(read_value(&key, Some("Couch Co-App")).unwrap().is_none());
    }

    /// Another install's value: not ours, never removed.
    #[test]
    fn another_programs_value_is_left_alone() {
        let key = scratch("other");
        let _cleanup = Cleanup(key.clone());
        let exe = std::env::current_exe().unwrap();
        let other = std::env::temp_dir();
        set_in(&key, "Couch Co-App", &other, true).unwrap();
        assert!(!is_set_in(&key, "Couch Co-App", &exe).unwrap());
        set_in(&key, "Couch Co-App", &exe, false).unwrap();
        assert!(read_value(&key, Some("Couch Co-App")).unwrap().is_some());
    }
}
