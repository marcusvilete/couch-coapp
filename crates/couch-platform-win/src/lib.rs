//! Windows backends. Everything that calls Windows APIs lives here: the QPC
//! clock, Windows.Graphics.Capture and the ViGEm probe now; WASAPI audio,
//! `SendInput`, virtual pads and process watching from M2 to M6.
//!
//! Compiles to nothing on other platforms; their backends get sibling crates.

#![cfg(windows)]

pub mod capture;
pub mod clock;
pub mod timer;
pub mod vigem;
