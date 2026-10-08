//! Native dependencies: where they come from and how they get into place.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};

pub struct Dep {
    pub name: &'static str,
    /// Folder under third_party/.
    pub dir: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    /// Files that must exist after extraction, relative to `dir`.
    pub expect: &'static [&'static str],
    /// Folder holding the runtime DLLs, relative to `dir`.
    pub dll_dir: &'static str,
}

/// Bump a pin by changing the URL and hash together; `fetch-deps` replaces the
/// old copy. FFmpeg comes from BtbN's month-end builds, which are kept for two
/// years (daily builds expire after two weeks).
pub const DEPS: &[Dep] = &[
    Dep {
        name: "FFmpeg 8.1.3 (LGPL, shared)",
        dir: "ffmpeg",
        url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/ffmpeg-n8.1.3-9-g29e619e767-win64-lgpl-shared-8.1.zip",
        sha256: "3e47bda1607740550141e37c0e49d1e5182b34699f15adfd137ee266d346811a",
        expect: &[
            "include/libavcodec/avcodec.h",
            "lib/avcodec.lib",
            "bin/ffmpeg.exe",
        ],
        dll_dir: "bin",
    },
    Dep {
        name: "SDL3 3.4.16 (Visual C++ development package)",
        dir: "sdl3",
        url: "https://github.com/libsdl-org/SDL/releases/download/release-3.4.16/SDL3-devel-3.4.16-VC.zip",
        sha256: "1a784cb2a5c64d56fe7a62090fe9d242d9865f235e4ea9678f1a6ba4e693e7de",
        expect: &["include/SDL3/SDL.h", "lib/x64/SDL3.lib", "lib/x64/SDL3.dll"],
        dll_dir: "lib/x64",
    },
];

/// A driver the installer offers (M10a): its official installer, pinned like
/// the archives above but used as it is, not extracted.
pub struct Driver {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    /// Its name in `third_party/drivers/` and next to the app (`drivers/`),
    /// where the engine and the installer's hooks look for it.
    pub staged_as: &'static str,
    /// Its licence text, kept in the repository (`apps/desktop/src-tauri/
    /// drivers/`), shipped beside it.
    pub license: &'static str,
}

pub const DRIVERS: &[Driver] = &[
    Driver {
        name: "ViGEmBus 1.22.0 (BSD-3-Clause)",
        url: "https://github.com/nefarius/ViGEmBus/releases/download/v1.22.0/ViGEmBus_1.22.0_x64_x86_arm64.exe",
        sha256: "89220a7865076b342892f98865f3499fb7c4cfd673159e89d352c360fd014c6a",
        staged_as: "vigembus-setup.exe",
        license: "ViGEmBus-LICENSE.txt",
    },
    Driver {
        name: "HidHide 1.5.230 (MIT)",
        url: "https://github.com/nefarius/HidHide/releases/download/v1.5.230.0/HidHide_1.5.230_x64.exe",
        sha256: "f4bbbcb82e6258641b887c74bc81c4c5f66e4aa811808dfc304347687b7605f6",
        staged_as: "hidhide-setup.exe",
        license: "HidHide-LICENSE.txt",
    },
];

/// Where the drivers' licence texts live in the repository.
fn driver_licenses(root: &Path) -> PathBuf {
    root.join("apps/desktop/src-tauri/drivers")
}

/// True when the driver's installer is downloaded at the pinned version.
pub fn driver_fetched(root: &Path, driver: &Driver) -> bool {
    let path = third_party(root).join("drivers").join(driver.staged_as);
    path.exists() && sha256_file(&path).is_ok_and(|h| h == driver.sha256)
}

fn fetch_drivers(root: &Path) -> anyhow::Result<()> {
    let dir = third_party(root).join("drivers");
    fs::create_dir_all(&dir)?;
    for driver in DRIVERS {
        if driver_fetched(root, driver) {
            println!("{}: up to date", driver.name);
            continue;
        }
        let path = dir.join(driver.staged_as);
        println!("{}: downloading {}", driver.name, driver.url);
        run(Command::new("curl")
            .args([
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--output",
            ])
            .arg(&path)
            .arg(driver.url))?;
        let actual = sha256_file(&path)?;
        if actual != driver.sha256 {
            fs::remove_file(&path)?;
            bail!(
                "{}: SHA-256 mismatch (expected {}, got {actual})",
                driver.name,
                driver.sha256
            );
        }
        println!("{}: saved to {}", driver.name, path.display());
    }
    Ok(())
}

/// Copies the drivers' installers and licences next to the built app
/// (`target/<profile>/drivers/`), where Settings → Drivers and the installer
/// find them.
fn stage_drivers(root: &Path, target: &Path) -> anyhow::Result<()> {
    if !DRIVERS.iter().all(|d| driver_fetched(root, d)) {
        bail!("driver installers are missing; run `cargo xtask fetch-deps` first");
    }
    for profile in ["debug", "release"] {
        let dir = target.join(profile).join("drivers");
        fs::create_dir_all(&dir)?;
        for driver in DRIVERS {
            for (from, name) in [
                (
                    third_party(root).join("drivers").join(driver.staged_as),
                    driver.staged_as,
                ),
                (driver_licenses(root).join(driver.license), driver.license),
            ] {
                let dest = dir.join(name);
                if same_file_contents_likely(&from, &dest) {
                    continue;
                }
                fs::copy(&from, &dest)
                    .with_context(|| format!("copying {} to {}", from.display(), dest.display()))?;
            }
        }
    }
    println!("{} driver installer(s) staged", DRIVERS.len());
    Ok(())
}

const PIN_FILE: &str = ".couch-pin";

pub fn third_party(root: &Path) -> PathBuf {
    root.join("third_party")
}

/// True when the dependency is extracted at the pinned version.
pub fn is_installed(root: &Path, dep: &Dep) -> bool {
    let dir = third_party(root).join(dep.dir);
    let pinned = fs::read_to_string(dir.join(PIN_FILE)).unwrap_or_default();
    pinned.trim() == dep.sha256 && dep.expect.iter().all(|f| dir.join(f).exists())
}

pub fn fetch(root: &Path) -> anyhow::Result<()> {
    let downloads = third_party(root).join("_downloads");
    fs::create_dir_all(&downloads)?;

    for dep in DEPS {
        if is_installed(root, dep) {
            println!("{}: up to date", dep.name);
            continue;
        }

        let file_name = dep.url.rsplit('/').next().expect("URL has a file name");
        let archive = downloads.join(file_name);
        if !(archive.exists() && sha256_file(&archive)? == dep.sha256) {
            println!("{}: downloading {}", dep.name, dep.url);
            run(Command::new("curl")
                .args([
                    "--fail",
                    "--location",
                    "--silent",
                    "--show-error",
                    "--output",
                ])
                .arg(&archive)
                .arg(dep.url))?;
            let actual = sha256_file(&archive)?;
            if actual != dep.sha256 {
                fs::remove_file(&archive)?;
                bail!(
                    "{}: SHA-256 mismatch (expected {}, got {actual})",
                    dep.name,
                    dep.sha256
                );
            }
        }

        // Each archive holds one top-level folder; its contents become third_party/<dir>.
        let staging = third_party(root).join(format!("_extract_{}", dep.dir));
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir_all(&staging)?;
        run(Command::new("tar")
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(&staging))?;
        let inner = single_subdirectory(&staging)?;

        let dest = third_party(root).join(dep.dir);
        if dest.exists() {
            fs::remove_dir_all(&dest)
                .with_context(|| format!("removing old {}", dest.display()))?;
        }
        fs::rename(&inner, &dest)?;
        fs::remove_dir_all(&staging)?;
        fs::write(dest.join(PIN_FILE), dep.sha256)?;

        if !is_installed(root, dep) {
            bail!("{}: extracted, but expected files are missing", dep.name);
        }
        println!("{}: installed to {}", dep.name, dest.display());
    }
    fetch_drivers(root)
}

/// The runtime DLLs of every dependency.
pub fn dlls(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut dlls = Vec::new();
    for dep in DEPS {
        let dir = third_party(root).join(dep.dir).join(dep.dll_dir);
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("dll"))
            {
                dlls.push(path);
            }
        }
    }
    Ok(dlls)
}

pub fn target_dir(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"))
}

pub fn stage_dlls(root: &Path) -> anyhow::Result<()> {
    if !DEPS.iter().all(|dep| is_installed(root, dep)) {
        bail!("native dependencies are missing; run `cargo xtask fetch-deps` first");
    }
    let dlls = dlls(root)?;
    let target = target_dir(root);
    let mut placed = 0;
    for profile in ["debug", "release"] {
        // Test binaries run from deps/, everything else from the profile folder.
        for dir in [target.join(profile), target.join(profile).join("deps")] {
            fs::create_dir_all(&dir)?;
            for dll in &dlls {
                let dest = dir.join(dll.file_name().expect("DLL has a file name"));
                if same_file_contents_likely(dll, &dest) {
                    continue;
                }
                let _ = fs::remove_file(&dest);
                // A hard link costs nothing on the same volume; fall back to a copy.
                if fs::hard_link(dll, &dest).is_err() {
                    fs::copy(dll, &dest).with_context(|| {
                        format!("copying {} to {}", dll.display(), dest.display())
                    })?;
                }
                placed += 1;
            }
        }
    }
    println!(
        "{} DLL(s) staged in {} ({} updated)",
        dlls.len(),
        target.display(),
        placed
    );
    stage_drivers(root, &target)
}

fn same_file_contents_likely(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(a), Ok(b)) => {
            a.len() == b.len()
                && a.modified().ok().is_some()
                && a.modified().ok() == b.modified().ok()
        }
        _ => false,
    }
}

pub fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn single_subdirectory(dir: &Path) -> anyhow::Result<PathBuf> {
    let mut subdirs = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.path());
    match (subdirs.next(), subdirs.next()) {
        (Some(only), None) => Ok(only),
        _ => bail!("expected exactly one top-level folder in the archive"),
    }
}

fn run(command: &mut Command) -> anyhow::Result<()> {
    let status = command
        .status()
        .with_context(|| format!("could not run {:?}", command.get_program()))?;
    if !status.success() {
        bail!("{:?} failed with {status}", command.get_program());
    }
    Ok(())
}
