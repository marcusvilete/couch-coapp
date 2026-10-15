//! Windows backends. Everything that calls Windows APIs lives here: the QPC
//! clock, Windows.Graphics.Capture, WASAPI loopback audio, `SendInput` and
//! ViGEm virtual pads, HidHide (hiding the host's own controller), DPAPI for
//! secrets on disk, the host's keys hook (the panic key, answering
//! requests), and watching the game's process.
//!
//! Compiles to nothing on other platforms; their backends get sibling crates.

#![cfg(windows)]

pub mod audio;
pub mod capture;
pub mod clock;
pub mod controllers;
pub mod dpapi;
pub mod drivers;
pub mod fake_game;
pub mod foreground;
pub mod game;
pub mod gpu_frames;
pub mod held_input;
pub mod hidhide;
pub mod host_keys;
pub mod keymap;
pub mod links;
pub(crate) mod markers;
pub mod monitor;
pub mod process;
pub mod sendinput;
pub mod timer;
pub mod top_windows;
pub mod vigem;
pub mod virtual_pads;
pub mod wgc;
pub mod wifi;
