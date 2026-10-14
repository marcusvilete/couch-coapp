//! Our relay (M7): the private release configuration, and the relay server
//! itself for running on this PC.
//!
//! - `release-config` creates `release.toml`, outside the repository, with
//!   a fresh access token. `scripts/build-installer.ps1` reads it.
//! - `relay-tool` builds the official `iroh-relay` binary, the version our
//!   iroh pin names, into `target/tools`. `scripts/relay.ps1` runs it.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};

use crate::deps;

/// Where the release configuration lives: `COUCH_RELEASE_CONFIG`, or
/// `%USERPROFILE%\.couch-coapp\release.toml`. Never inside the repository.
pub fn release_config_path() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("COUCH_RELEASE_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .context("no USERPROFILE to put release.toml in")?;
    Ok(PathBuf::from(home)
        .join(".couch-coapp")
        .join("release.toml"))
}

pub fn release_config(root: &Path) -> anyhow::Result<()> {
    let path = release_config_path()?;
    if path.exists() {
        bail!(
            "{} already exists; edit it, or move it away to start over (its token is in use wherever you deployed it)",
            path.display()
        );
    }
    // Windows paths ignore case, so compare them that way (as the build
    // script does).
    let lower = |p: &Path| -> anyhow::Result<PathBuf> {
        Ok(PathBuf::from(
            std::path::absolute(p)?.to_string_lossy().to_lowercase(),
        ))
    };
    let absolute = std::path::absolute(&path)?;
    if lower(&path)?.starts_with(lower(root)?) {
        bail!(
            "{} is inside the repository; keep release.toml outside it",
            absolute.display()
        );
    }
    let token = new_token()?;
    let text = format!(
        r#"# Couch Co-App's release configuration. Read by scripts/build-installer.ps1.
# Private: it lives outside the repository on purpose. Back it up in a
# password manager. The layout is explained in deploy/release.example.toml.

# Our relay's address, once deployed (deploy/relay/README.md). Empty: the
# installer can't be built without -TestRelay.
relay_url = ""

# The token our relay asks for. The same value goes in the server's
# /etc/iroh-relay/token.env as IROH_RELAY_ACCESS_TOKEN.
relay_token = "{token}"

# Where invites link to (deploy/invite-page), ending in "/j/": published on
# GitHub Pages by the repository's workflow, https://<user>.github.io/<repository>/j/.
# Empty for couchco:// links. build-installer.ps1 -InvitePage <address> or
# -NoInvitePage overrides it for one build.
invite_page = ""

# true only in a copy for testing on your LAN: allows an http:// relay_url
# and invite_page, and names the installer "-test".
test = false
"#
    );
    std::fs::create_dir_all(path.parent().expect("a file path has a parent"))?;
    std::fs::write(&path, text)?;
    println!("Created {}", path.display());
    println!();
    println!("The relay server's environment file (/etc/iroh-relay/token.env) takes:");
    println!("IROH_RELAY_ACCESS_TOKEN={token}");
    Ok(())
}

/// 32 random bytes as hex: no character the relay's config or a shell
/// would read specially.
fn new_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    let mut hex = String::with_capacity(64);
    for byte in bytes {
        write!(hex, "{byte:02x}").expect("writing to a String");
    }
    Ok(hex)
}

/// The iroh version the workspace pins (`iroh = "=X.Y.Z"`): the relay
/// server must be the same release.
pub fn iroh_pin(root: &Path) -> anyhow::Result<String> {
    let manifest = std::fs::read_to_string(root.join("Cargo.toml"))?;
    manifest
        .lines()
        .find_map(|l| l.trim().strip_prefix("iroh = \"="))
        .and_then(|rest| rest.strip_suffix('"'))
        .map(str::to_string)
        .context("no `iroh = \"=X.Y.Z\"` pin in Cargo.toml")
}

pub fn relay_exe(root: &Path) -> PathBuf {
    deps::target_dir(root)
        .join("tools")
        .join("bin")
        .join(if cfg!(windows) {
            "iroh-relay.exe"
        } else {
            "iroh-relay"
        })
}

pub fn relay_tool(root: &Path) -> anyhow::Result<()> {
    let version = iroh_pin(root)?;
    let tools = deps::target_dir(root).join("tools");
    println!(
        "Building iroh-relay {version} (the official server) into {}",
        tools.display()
    );
    let status = Command::new("cargo")
        .args(["install", "--locked", "iroh-relay", "--version"])
        .arg(&version)
        .args(["--features", "server", "--root"])
        .arg(&tools)
        .status()
        .context("could not run cargo")?;
    if !status.success() {
        bail!("cargo install iroh-relay failed with {status}");
    }
    println!("Built {}", relay_exe(root).display());
    Ok(())
}

/// For `doctor`: the relay tool matches the iroh pin.
pub fn relay_tool_version(root: &Path) -> Result<String, String> {
    let exe = relay_exe(root);
    if !exe.exists() {
        return Err("not built; run `cargo xtask relay-tool` (for testing our relay here)".into());
    }
    let pin = iroh_pin(root).map_err(|e| e.to_string())?;
    let output = Command::new(&exe)
        .arg("--version")
        .output()
        .map_err(|e| format!("could not run {}: {e}", exe.display()))?;
    let shown = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if shown.contains(&pin) {
        Ok(shown)
    } else {
        Err(format!(
            "{shown}, but iroh is pinned at {pin}; run `cargo xtask relay-tool`"
        ))
    }
}
