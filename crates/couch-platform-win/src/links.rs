//! A development build's own registration of the app's link scheme (P5).
//!
//! A debug build may register `couchco://` for the current user (Tauri's
//! deep-link plugin writes `HKCU\Software\Classes\couchco`), which outranks
//! an installed app's registration for everyone. [`forget_user_scheme`]
//! takes that back. The plugin's own `unregister` won't do: it deletes the
//! machine-wide key first, which an installer wrote, and without
//! administrator rights it fails there before reaching the user's key.

use std::{ffi::OsString, io, os::windows::ffi::OsStringExt, path::Path};

use windows::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR},
    System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegDeleteTreeW, RegGetValueW},
};
use windows_core::{HSTRING, PCWSTR};

/// Removes the current user's registration of `scheme`, if it opens `exe`.
/// Never touches the machine-wide one, nor the user's registration for
/// another program (an app installed for just this user writes the same
/// key, with its own exe), nor one for an exe that no longer exists (another
/// checkout's deleted build: remove that by hand). Returns whether it
/// removed one.
pub fn forget_user_scheme(scheme: &str, exe: &Path) -> io::Result<bool> {
    // A URL scheme's own characters only: anything else could aim the
    // delete at `Software\Classes` itself or another of its keys.
    let valid = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    if !valid {
        return Err(io::Error::other(format!("not a URL scheme: {scheme:?}")));
    }
    let key = format!("Software\\Classes\\{scheme}");
    let Some(command) = read_default(&format!("{key}\\shell\\open\\command"))? else {
        return Ok(false);
    };
    match command_exe(&command) {
        Some(registered) if same_file(Path::new(&registered), exe) => {}
        _ => return Ok(false),
    }
    // SAFETY: a live NUL-terminated wide string; deletes a key of this
    // user's own hive (HKCU\Software\Classes, not the merged HKCR view),
    // with what it holds.
    match unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key)) } {
        // Gone since we read it: nothing left to do.
        ERROR_FILE_NOT_FOUND => Ok(false),
        code => check(code).map(|()| true),
    }
}

/// The default value of a key under the current user's hive, if the key
/// and its default value exist.
fn read_default(subkey: &str) -> io::Result<Option<String>> {
    read_value(subkey, None)
}

/// A string value of a key under the current user's hive (`None`: its
/// default value), if the key and the value exist.
pub(crate) fn read_value(subkey: &str, value: Option<&str>) -> io::Result<Option<String>> {
    let subkey = HSTRING::from(subkey);
    let name = value.map(HSTRING::from);
    let name = name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
    let mut bytes = 0u32;
    // SAFETY: asks only for the size, into a live local; `name` is null or
    // a live NUL-terminated wide string.
    let sized = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &subkey,
            name,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        )
    };
    if sized == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    check(sized)?;
    let mut text = vec![0u16; (bytes as usize).div_ceil(2)];
    // SAFETY: `text` holds `bytes` bytes, which the call is told; it writes
    // a NUL-terminated string no longer than that.
    check(unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &subkey,
            name,
            RRF_RT_REG_SZ,
            None,
            Some(text.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    })?;
    let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    Ok(Some(
        OsString::from_wide(&text[..len])
            .to_string_lossy()
            .into_owned(),
    ))
}

/// The program in an open command such as `"C:\path\app.exe" "%1"`.
pub(crate) fn command_exe(command: &str) -> Option<String> {
    let rest = command.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Whether two paths name the same file, however each is spelled.
pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

pub(crate) fn check(code: WIN32_ERROR) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code.0 as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_is_read_from_an_open_command() {
        assert_eq!(
            command_exe(r#""C:\dev\app.exe" "%1""#).as_deref(),
            Some(r"C:\dev\app.exe")
        );
        assert_eq!(command_exe(r#"C:\dev\app.exe "%1""#), None);
        assert_eq!(command_exe(r#""unterminated"#), None);
    }

    #[test]
    fn a_scheme_nobody_registered_is_left_alone() {
        let exe = std::env::current_exe().unwrap();
        assert!(!forget_user_scheme("couchco-test-never-registered", &exe).unwrap());
    }

    #[test]
    fn only_url_schemes_are_taken() {
        let exe = std::env::current_exe().unwrap();
        for bad in ["", "..", "couchco\\shell", "9lives", "a b"] {
            assert!(forget_user_scheme(bad, &exe).is_err(), "{bad:?}");
        }
    }

    /// Registers `scheme` for the current user, opening `program`, as the
    /// deep-link plugin does.
    fn register(scheme: &str, program: &Path) {
        use windows::Win32::System::Registry::{REG_SZ, RegSetKeyValueW};
        let command: Vec<u16> = format!("\"{}\" \"%1\"", program.display())
            .encode_utf16()
            .chain([0])
            .collect();
        let key = HSTRING::from(format!("Software\\Classes\\{scheme}\\shell\\open\\command"));
        // SAFETY: live NUL-terminated strings, and `command`'s size in bytes.
        check(unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &key,
                PCWSTR::null(),
                REG_SZ.0,
                Some(command.as_ptr().cast()),
                (command.len() * 2) as u32,
            )
        })
        .unwrap();
    }

    /// Deletes the test's key when dropped, so a failed assert leaves nothing
    /// in the registry.
    struct Cleanup(String);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let key = HSTRING::from(format!("Software\\Classes\\{}", self.0));
            // SAFETY: a live NUL-terminated wide string; the test's own key.
            let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &key) };
        }
    }

    #[test]
    fn only_a_registration_for_this_very_program_goes() {
        let scheme = format!("couchco-test-{}", std::process::id());
        let _cleanup = Cleanup(scheme.clone());
        let exe = std::env::current_exe().unwrap();
        // Another program's (an app installed for just this user): kept.
        let other = std::env::temp_dir();
        register(&scheme, &other);
        assert!(!forget_user_scheme(&scheme, &exe).unwrap());
        assert!(
            read_default(&format!(
                "Software\\Classes\\{scheme}\\shell\\open\\command"
            ))
            .unwrap()
            .is_some()
        );
        // This program's: removed, key and all.
        register(&scheme, &exe);
        assert!(forget_user_scheme(&scheme, &exe).unwrap());
        assert!(
            read_default(&format!(
                "Software\\Classes\\{scheme}\\shell\\open\\command"
            ))
            .unwrap()
            .is_none()
        );
    }
}
