//! Linux backends (M16, ADR 0011 and 0012). Everything that calls
//! Linux-specific APIs lives here, as `couch-platform-win` holds Windows':
//! the clock frames are timed on, keeping the stream window's stdout for its
//! pipe, guests' virtual controllers and the injected keyboard and mouse
//! through `uinput`, the desktop's portals (the host's keys, screen
//! sharing), its notifications (the host's notices), which window is in
//! front (a KWin script, or our GNOME extension), PipeWire (the shared
//! picture, the PC's sound), and what a home
//! PC run from afar needs: staying awake (logind and the inhibit portal),
//! its lock screen (logind), and starting at sign-in (XDG autostart).
//!
//! Compiles to nothing on other platforms.

#![cfg(target_os = "linux")]

pub mod autostart;
pub mod awake;
pub mod chime;
pub mod clipboard;
pub mod clock;
mod cursor;
pub mod folder;
pub mod front;
pub mod global_keys;
pub mod gpu;
pub mod kbm;
pub mod launcher;
pub mod notices;
pub mod pads;
mod portal;
pub mod screencast;
pub mod session_lock;
pub mod sound;
pub mod stdio;
