//! Linux backends (M16, ADR 0011 and 0012). Everything that calls
//! Linux-specific APIs lives here, as `couch-platform-win` holds Windows':
//! the clock frames are timed on, keeping the stream window's stdout for its
//! pipe, guests' virtual controllers through `uinput`, the desktop's portals
//! (the host's keys, screen sharing), its notifications (the host's
//! notices), and PipeWire (the shared picture, the PC's sound).
//!
//! Compiles to nothing on other platforms.

#![cfg(target_os = "linux")]

pub mod clock;
pub mod global_keys;
pub mod notices;
pub mod pads;
mod portal;
pub mod screencast;
pub mod sound;
pub mod stdio;
