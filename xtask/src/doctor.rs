//! `cargo xtask doctor`: is this machine ready to build and run Couch Co-App?

use std::{path::Path, process::Command, process::ExitCode};

use crate::{deps, ffmpeg_bindings};

#[derive(Default)]
struct Checks {
    failures: usize,
}

impl Checks {
    /// A check that must pass.
    fn require(&mut self, label: &str, result: Result<String, String>) {
        match result {
            Ok(detail) => println!("[ ok ] {label:<22} {detail}"),
            Err(detail) => {
                self.failures += 1;
                println!("[FAIL] {label:<22} {detail}");
            }
        }
    }

    /// A check that is only needed by a later step or milestone.
    fn recommend(&mut self, label: &str, result: Result<String, String>) {
        match result {
            Ok(detail) => println!("[ ok ] {label:<22} {detail}"),
            Err(detail) => println!("[WARN] {label:<22} {detail}"),
        }
    }
}

pub fn run(root: &Path) -> ExitCode {
    let mut checks = Checks::default();

    checks.require("Rust toolchain", rust_toolchain(root));
    checks.recommend("libclang", libclang());
    // The `opus` crate builds libopus from source with it.
    checks.require(
        "CMake",
        output_of("cmake", &["--version"])
            .map(|v| v.lines().next().unwrap_or_default().to_string())
            .map_err(|e| format!("{e} (winget install Kitware.CMake)")),
    );
    checks.require("FFmpeg bindings", ffmpeg_bindings(root));
    for dep in deps::DEPS {
        let result = if deps::is_installed(root, dep) {
            Ok(format!("third_party/{}", dep.dir))
        } else {
            Err("missing or not the pinned version; run `cargo xtask fetch-deps`".into())
        };
        checks.require(dep.name.split(' ').next().unwrap_or(dep.name), result);
    }
    for driver in deps::DRIVERS {
        let result = if deps::driver_fetched(root, driver) {
            Ok(format!("third_party/drivers/{}", driver.staged_as))
        } else {
            Err("missing or not the pinned version; run `cargo xtask fetch-deps` (the installer bundles it)".into())
        };
        checks.recommend(
            &format!(
                "{} installer",
                driver.name.split(' ').next().unwrap_or(driver.name)
            ),
            result,
        );
    }
    checks.require("FFmpeg license", ffmpeg_license(root));
    checks.recommend("Staged DLLs", staged_dlls(root));
    checks.require("cargo-deny", output_of("cargo", &["deny", "--version"]));
    checks.require("Node.js", output_of("node", &["--version"]));
    checks.require("npm", npm_version());
    checks.recommend("Front-end packages", front_end_packages(root));
    #[cfg(windows)]
    {
        checks.require("WebView2 runtime", webview2());
        checks.recommend("ViGEmBus driver", vigem_service());
    }

    println!();
    if checks.failures == 0 {
        println!("All required checks passed.");
        ExitCode::SUCCESS
    } else {
        println!("{} required check(s) failed.", checks.failures);
        ExitCode::FAILURE
    }
}

fn rust_toolchain(root: &Path) -> Result<String, String> {
    let file =
        std::fs::read_to_string(root.join("rust-toolchain.toml")).map_err(|e| e.to_string())?;
    let pinned = file
        .lines()
        .find_map(|l| l.trim().strip_prefix("channel = "))
        .map(|v| v.trim_matches('"').to_string())
        .ok_or("no channel in rust-toolchain.toml")?;
    let version = output_of("rustc", &["--version"])?;
    if version.contains(&pinned) {
        Ok(version)
    } else {
        Err(format!("{version}, but rust-toolchain.toml pins {pinned}"))
    }
}

/// Only `cargo xtask gen-ffmpeg-bindings` needs libclang; normal builds don't.
fn libclang() -> Result<String, String> {
    let dir = std::env::var("LIBCLANG_PATH").map_err(|_| {
        "LIBCLANG_PATH is not set (it comes from .cargo/config.toml when run through cargo)"
            .to_string()
    })?;
    let dll = Path::new(&dir).join("libclang.dll");
    if dll.exists() {
        Ok(format!("{} (for gen-ffmpeg-bindings)", dll.display()))
    } else {
        Err(format!(
            "{} not found; only needed to regenerate FFmpeg bindings (winget install LLVM.LLVM)",
            dll.display()
        ))
    }
}

/// The committed bindings must have been generated from the pinned FFmpeg.
fn ffmpeg_bindings(root: &Path) -> Result<String, String> {
    let ffmpeg = deps::DEPS
        .iter()
        .find(|d| d.dir == "ffmpeg")
        .expect("FFmpeg is pinned in deps.rs");
    let file = root.join(ffmpeg_bindings::BINDING_FILE);
    let text = std::fs::read_to_string(&file).map_err(|_| {
        format!(
            "{} missing; run `cargo xtask gen-ffmpeg-bindings`",
            file.display()
        )
    })?;
    if text.contains(&ffmpeg_bindings::provenance(ffmpeg.sha256)) {
        Ok(format!("match the pinned {}", ffmpeg.name))
    } else {
        Err("generated from a different FFmpeg; run `cargo xtask gen-ffmpeg-bindings`".into())
    }
}

fn ffmpeg_license(root: &Path) -> Result<String, String> {
    let exe = deps::third_party(root)
        .join("ffmpeg")
        .join("bin")
        .join("ffmpeg.exe");
    let output = Command::new(&exe)
        .args(["-hide_banner", "-buildconf"])
        .output()
        .map_err(|e| format!("could not run {}: {e}", exe.display()))?;
    let conf = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    if conf.contains("--enable-gpl") || conf.contains("--enable-nonfree") {
        Err("this FFmpeg build includes GPL or non-free parts; use the LGPL build".into())
    } else {
        Ok("LGPL (no --enable-gpl or --enable-nonfree)".into())
    }
}

fn staged_dlls(root: &Path) -> Result<String, String> {
    let dlls = deps::dlls(root).map_err(|e| e.to_string())?;
    let debug = deps::target_dir(root).join("debug");
    let missing: Vec<_> = dlls
        .iter()
        .filter_map(|d| d.file_name())
        .filter(|name| !debug.join(name).exists())
        .collect();
    if missing.is_empty() {
        Ok(format!("{} DLL(s) in {}", dlls.len(), debug.display()))
    } else {
        Err(format!(
            "{} DLL(s) missing from target/debug; run `cargo xtask stage-dlls`",
            missing.len()
        ))
    }
}

fn npm_version() -> Result<String, String> {
    // npm is a .cmd script on Windows, which Command can't start directly.
    if cfg!(windows) {
        output_of("cmd", &["/C", "npm", "--version"])
    } else {
        output_of("npm", &["--version"])
    }
}

fn front_end_packages(root: &Path) -> Result<String, String> {
    let modules = root.join("apps").join("desktop").join("node_modules");
    if modules.exists() {
        Ok("apps/desktop/node_modules present".into())
    } else {
        Err("run `npm install` in apps/desktop".into())
    }
}

#[cfg(windows)]
fn webview2() -> Result<String, String> {
    const KEY: &str =
        r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
    for hive in ["HKLM", "HKCU"] {
        if let Ok(out) = output_of("reg", &["query", &format!(r"{hive}\{KEY}"), "/v", "pv"])
            && let Some(version) = out.split_whitespace().last()
        {
            return Ok(format!("version {version}"));
        }
    }
    Err("not installed; Windows 11 ships it, otherwise install the Evergreen runtime".into())
}

#[cfg(windows)]
fn vigem_service() -> Result<String, String> {
    match output_of("sc", &["query", "ViGEmBus"]) {
        Ok(out) if out.contains("RUNNING") => Ok("ViGEmBus service running".into()),
        _ => Err(
            "not installed; needed from M2 (winget install ViGEm.ViGEmBus --version 1.22.0)".into(),
        ),
    }
}

/// The trimmed stdout of a command that must succeed.
fn output_of(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {} failed with {}",
            args.join(" "),
            output.status
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
