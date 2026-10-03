//! Windows backends. Everything that calls Windows APIs lives here: the QPC
//! clock, Windows.Graphics.Capture, `SendInput` and ViGEm virtual pads now;
//! WASAPI audio and process watching from M3 to M6.
//!
//! Compiles to nothing on other platforms; their backends get sibling crates.

#![cfg(windows)]

pub mod capture;
pub mod clock;
pub mod foreground;
pub mod keymap;
pub mod sendinput;
pub mod timer;
pub mod vigem;
