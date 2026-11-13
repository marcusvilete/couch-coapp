//! A secret copied to the clipboard (M26a: a relay code, which holds its
//! token), marked for clipboard managers to keep out of their history
//! (`x-kde-passwordManagerHint: secret`, which KDE's Klipper honours).
//!
//! On Linux the program that copied serves the clipboard until something
//! else is copied, so the one clipboard of the process stays alive with it.

use std::sync::{Mutex, PoisonError};

use arboard::{Clipboard, SetExtLinux};

static CLIPBOARD: Mutex<Option<Clipboard>> = Mutex::new(None);

/// Puts `text` on the clipboard as a secret. Fails without a display (no
/// X11 or Wayland server to serve it), or where the desktop refuses.
pub fn copy_secret(text: &str) -> Result<(), String> {
    let mut held = CLIPBOARD.lock().unwrap_or_else(PoisonError::into_inner);
    if held.is_none() {
        *held = Some(Clipboard::new().map_err(|e| e.to_string())?);
    }
    held.as_mut()
        .expect("made above")
        .set()
        .exclude_from_history()
        .text(text)
        .map_err(|e| e.to_string())
}
