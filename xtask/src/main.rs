//! Repository automation, run as `cargo xtask <command>`:
//!
//! - `fetch-deps`: download FFmpeg and SDL3 into third_party/ (pinned URLs,
//!   SHA-256 checked);
//! - `stage-dlls`: put their DLLs next to the binaries in target/, so builds
//!   run without PATH changes;
//! - `gen-ffmpeg-bindings`: regenerate the committed FFmpeg FFI bindings
//!   (after bumping FFmpeg; needs LLVM);
//! - `doctor`: check that this machine can build and run Couch Co-App;
//! - `release-config`: create the private `release.toml` (outside the
//!   repository) with a fresh relay token;
//! - `relay-tool`: build the official relay server for testing here.

mod deps;
mod doctor;
mod ffmpeg_bindings;
mod relay;

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the repository root")
        .to_path_buf()
}

fn main() -> ExitCode {
    let root = repo_root();
    let command = std::env::args().nth(1);
    let result = match command.as_deref() {
        Some("fetch-deps") => deps::fetch(&root),
        Some("stage-dlls") => deps::stage_dlls(&root),
        Some("gen-ffmpeg-bindings") => ffmpeg_bindings::generate(&root),
        Some("doctor") => return doctor::run(&root),
        Some("release-config") => relay::release_config(&root),
        Some("relay-tool") => relay::relay_tool(&root),
        _ => {
            eprintln!(
                "usage: cargo xtask <fetch-deps | stage-dlls | gen-ffmpeg-bindings | doctor | release-config | relay-tool>"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
