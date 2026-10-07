//! Windows backends. Everything that calls Windows APIs lives here: the QPC
//! clock, Windows.Graphics.Capture, WASAPI loopback audio, `SendInput` and
//! ViGEm virtual pads, HidHide (hiding the host's own controller), DPAPI for
//! secrets on disk, the host's panic-key hook, and watching the game's
//! process.
//!
//! Compiles to nothing on other platforms; their backends get sibling crates.

#![cfg(windows)]

pub mod audio;
pub mod capture;
pub mod clock;
pub mod controllers;
pub mod dpapi;
pub mod fake_game;
pub mod foreground;
pub mod game;
pub mod held_input;
pub mod hidhide;
pub mod keymap;
pub(crate) mod markers;
pub mod panic_key;
pub mod process;
pub mod sendinput;
pub mod timer;
pub mod vigem;
pub mod virtual_pads;
