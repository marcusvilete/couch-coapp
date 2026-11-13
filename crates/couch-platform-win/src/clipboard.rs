//! The clipboard.
//! - Files copied to it (M27e): Explorer's Ctrl+C on a file puts a list of
//!   files there (`CF_HDROP`), not the picture, and SDL reads only text and
//!   pictures. The stream window pastes a copied picture file as it takes a
//!   dropped one.
//! - A secret copied to it (M26a: a relay code, which holds its token): kept
//!   out of the clipboard's history and its sync to other devices.

use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};

use windows::{
    Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
                OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
            },
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        },
        UI::Shell::{DragQueryFileW, HDROP},
    },
    core::{Error, w},
};

/// `CF_HDROP` (WinUser.h): a list of files.
const CF_HDROP: u32 = 15;
/// `CF_UNICODETEXT` (WinUser.h): UTF-16 text ending in a NUL.
const CF_UNICODETEXT: u32 = 13;
/// Files read at most: a paste takes the first picture among them.
const MOST: u32 = 64;

/// The files on the clipboard, if it holds a list of them; none otherwise,
/// or if another program holds the clipboard open.
pub fn copied_files() -> Vec<PathBuf> {
    // SAFETY: the clipboard is opened and closed on this thread, around the
    // read; the handle `GetClipboardData` returns stays the clipboard's (we
    // never free it) and is valid until it closes, and a `CF_HDROP` handle is
    // what `DragQueryFileW` reads.
    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP).is_err() || OpenClipboard(None).is_err() {
            return Vec::new();
        }
        let files = match GetClipboardData(CF_HDROP) {
            Ok(handle) if !handle.is_invalid() => read(HDROP(handle.0)),
            _ => Vec::new(),
        };
        let _ = CloseClipboard();
        files
    }
}

/// The paths in a list of files.
///
/// # Safety
/// `drop` is a valid `CF_HDROP` handle for the call's length.
unsafe fn read(drop: HDROP) -> Vec<PathBuf> {
    // SAFETY: as the caller promises; each buffer is as long as the length
    // asked for and its NUL.
    unsafe {
        let count = DragQueryFileW(drop, u32::MAX, None).min(MOST);
        (0..count)
            .filter_map(|i| {
                let len = DragQueryFileW(drop, i, None) as usize;
                if len == 0 {
                    return None;
                }
                let mut buf = vec![0u16; len + 1];
                let got = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
                (got > 0 && got <= len).then(|| PathBuf::from(OsString::from_wide(&buf[..got])))
            })
            .collect()
    }
}

/// Puts `text` on the clipboard as a secret, as password managers do:
/// Windows' clipboard history (Win+V) and its sync to other devices leave it
/// out. The formats that say so go on before the text, and the clipboard is
/// emptied again if any of it fails, so the text is never there without
/// them. Fails if another program holds the clipboard open.
pub fn copy_secret(text: &str) -> windows::core::Result<()> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let text: Vec<u8> = wide.iter().flat_map(|c| c.to_ne_bytes()).collect();
    // SAFETY: the clipboard is opened and closed on this thread around the
    // writes; each format's data is memory of our own handed to `put`.
    unsafe {
        OpenClipboard(None)?;
        let written = (|| {
            EmptyClipboard()?;
            for name in [
                w!("ExcludeClipboardContentFromMonitorProcessing"),
                w!("CanIncludeInClipboardHistory"),
                w!("CanUploadToCloudClipboard"),
            ] {
                let format = RegisterClipboardFormatW(name);
                if format == 0 {
                    return Err(Error::from_thread());
                }
                // A DWORD 0: "no" for the two that read it.
                put(format, &0u32.to_ne_bytes())?;
            }
            put(CF_UNICODETEXT, &text)
        })();
        if written.is_err() {
            let _ = EmptyClipboard();
        }
        let _ = CloseClipboard();
        written
    }
}

/// Hands `bytes` to the open clipboard as `format`: in memory the system
/// owns once it took it, freed here if it didn't.
///
/// # Safety
/// This thread has the clipboard open.
unsafe fn put(format: u32, bytes: &[u8]) -> windows::core::Result<()> {
    // SAFETY: a movable block of `bytes.len()` bytes, locked while written
    // and unlocked before it is handed over; `GlobalUnlock` reports the
    // last unlock as an error (its lock count reached 0), so its result is
    // ignored.
    unsafe {
        let memory: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
        let at = GlobalLock(memory);
        if at.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err(Error::from_thread());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), at.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(memory);
        if let Err(e) = SetClipboardData(format, Some(HANDLE(memory.0))) {
            let _ = GlobalFree(Some(memory));
            return Err(e);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: it replaces what the clipboard holds.
    #[test]
    #[ignore = "writes the clipboard of the user running it"]
    fn a_secret_is_copied_with_the_formats_that_hide_it() {
        copy_secret("couch-relay-1.test").unwrap();
        // SAFETY: opened and closed here; the data's handle is read only
        // while the clipboard is open.
        unsafe {
            OpenClipboard(None).unwrap();
            for name in [
                w!("ExcludeClipboardContentFromMonitorProcessing"),
                w!("CanIncludeInClipboardHistory"),
                w!("CanUploadToCloudClipboard"),
            ] {
                assert!(IsClipboardFormatAvailable(RegisterClipboardFormatW(name)).is_ok());
            }
            assert!(IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok());
            let _ = CloseClipboard();
        }
    }
}
