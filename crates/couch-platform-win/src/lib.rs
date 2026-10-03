//! Windows backends. Everything that calls Windows APIs lives here: the ViGEm
//! probe now; capture, WASAPI audio, `SendInput`, virtual pads and process
//! watching from M1 to M6.
//!
//! Compiles to nothing on other platforms; their backends get sibling crates.

#![cfg(windows)]

pub mod vigem;
