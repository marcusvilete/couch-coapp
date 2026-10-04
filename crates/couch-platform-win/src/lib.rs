//! Windows backends. Everything that calls Windows APIs lives here: the QPC
//! clock, Windows.Graphics.Capture, WASAPI loopback audio, `SendInput` and
//! ViGEm virtual pads, DPAPI for secrets on disk now; process watching in M6.
//!
//! Compiles to nothing on other platforms; their backends get sibling crates.

#![cfg(windows)]

pub mod audio;
pub mod capture;
pub mod clock;
pub mod dpapi;
pub mod foreground;
pub mod keymap;
pub mod process;
pub mod sendinput;
pub mod timer;
pub mod vigem;
