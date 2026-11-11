//! Files copied to the clipboard (M27e): Explorer's Ctrl+C on a file puts a
//! list of files there (`CF_HDROP`), not the picture, and SDL reads only
//! text and pictures. The stream window pastes a copied picture file as it
//! takes a dropped one.

use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};

use windows::Win32::{
    System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    },
    UI::Shell::{DragQueryFileW, HDROP},
};

/// `CF_HDROP` (WinUser.h): a list of files.
const CF_HDROP: u32 = 15;
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
