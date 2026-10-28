//! Linux backends (M16, ADR 0011). Everything that calls Linux-specific APIs
//! lives here, as `couch-platform-win` holds Windows': the clock frames are
//! timed on, for now; later D-Bus, evdev, `uinput` and PipeWire.
//!
//! Compiles to nothing on other platforms.

#![cfg(target_os = "linux")]

pub mod clock;
