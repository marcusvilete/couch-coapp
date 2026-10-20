//! `cargo xtask bump patch|minor`: the app's version, the same in every file
//! that names it, in one step.
//!
//! The rules (CLAUDE.md, "Versions"): every PR that changes the app bumps
//! the version once; a patch for fixes and small features, a minor for a
//! milestone or any change to `couch_proto::PROTOCOL_MINOR` (older builds
//! then refuse the new one, and `couch_proto::REVISIONS` must say which
//! version brought it). Docs, skills, tests and tooling alone don't bump.
//!
//! It sets the version in `Cargo.toml` (`[workspace.package]`),
//! `apps/desktop/src-tauri/tauri.conf.json` and `apps/desktop/package.json`,
//! and our own crates' entries in `Cargo.lock`, refusing if those three
//! don't agree to begin with. `docs/architecture.md`'s version line, which
//! says what each version carries, is left to the person bumping.

use std::{fs, path::Path};

use anyhow::{Context, Result, anyhow, bail};

/// Which part of the version goes up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Patch,
    Minor,
}

pub fn run(root: &Path, part: Option<&str>) -> Result<()> {
    let part = match part {
        Some("patch") => Part::Patch,
        Some("minor") => Part::Minor,
        _ => bail!("usage: cargo xtask bump <patch | minor>"),
    };
    let cargo_path = root.join("Cargo.toml");
    let tauri_path = root.join("apps/desktop/src-tauri/tauri.conf.json");
    let npm_path = root.join("apps/desktop/package.json");
    let lock_path = root.join("Cargo.lock");
    let read = |p: &Path| fs::read_to_string(p).with_context(|| format!("reading {}", p.display()));
    let (cargo, tauri, npm, lock) = (
        read(&cargo_path)?,
        read(&tauri_path)?,
        read(&npm_path)?,
        read(&lock_path)?,
    );

    let old = cargo_version(&cargo)?;
    let (tauri_old, npm_old) = (json_version(&tauri)?, json_version(&npm)?);
    if tauri_old != old || npm_old != old {
        bail!(
            "the versions differ: Cargo.toml {old}, tauri.conf.json {tauri_old}, package.json {npm_old}; make them the same first"
        );
    }
    let new = bumped(&old, part)?;

    let writes = [
        (&cargo_path, set_cargo_version(&cargo, &old, &new)?),
        (&tauri_path, set_json_version(&tauri, &old, &new)?),
        (&npm_path, set_json_version(&npm, &old, &new)?),
        (&lock_path, set_lock_versions(&lock, &old, &new)),
    ];
    for (path, text) in writes {
        fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    }
    println!("{old} -> {new}");
    println!(
        "Now say what {new} carries in docs/architecture.md's version line (\"Installing\"), and commit it with the change."
    );
    Ok(())
}

/// `major.minor.patch`, numbers only.
fn parse(version: &str) -> Result<[u32; 3]> {
    let parts: Vec<u32> = version
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| anyhow!("not a version: {version:?}"))?;
    parts
        .try_into()
        .map_err(|_| anyhow!("not major.minor.patch: {version:?}"))
}

fn bumped(version: &str, part: Part) -> Result<String> {
    let [major, minor, patch] = parse(version)?;
    Ok(match part {
        Part::Patch => format!("{major}.{minor}.{}", patch + 1),
        Part::Minor => format!("{major}.{}.0", minor + 1),
    })
}

/// The `version = "…"` line of `[workspace.package]`.
fn cargo_version(cargo: &str) -> Result<String> {
    let mut in_section = false;
    for line in cargo.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == "[workspace.package]";
        } else if in_section && let Some(value) = line.strip_prefix("version") {
            let value = value.trim_start().strip_prefix('=').map(str::trim);
            if let Some(v) = value.and_then(|v| v.strip_prefix('"')?.strip_suffix('"')) {
                return Ok(v.to_string());
            }
        }
    }
    bail!("no version in Cargo.toml's [workspace.package]")
}

fn set_cargo_version(cargo: &str, old: &str, new: &str) -> Result<String> {
    let mut in_section = false;
    let mut done = false;
    let out: Vec<String> = cargo
        .split_inclusive('\n')
        .map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_section = trimmed == "[workspace.package]";
            } else if in_section && !done && trimmed == format!("version = \"{old}\"") {
                done = true;
                return line.replacen(old, new, 1);
            }
            line.to_string()
        })
        .collect();
    if !done {
        bail!("Cargo.toml's [workspace.package] version isn't {old:?}");
    }
    Ok(out.concat())
}

/// The top-level `"version"` of a JSON file.
fn json_version(json: &str) -> Result<String> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    value["version"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("no top-level \"version\""))
}

/// Replaces the first `"version": "<old>"`, the top-level one in these
/// files (they list it before anything nested), keeping the formatting.
fn set_json_version(json: &str, old: &str, new: &str) -> Result<String> {
    let from = format!("\"version\": \"{old}\"");
    if !json.contains(&from) {
        bail!("no {from}");
    }
    let out = json.replacen(&from, &format!("\"version\": \"{new}\""), 1);
    // Still the same file, but for the version.
    if json_version(&out)? != new {
        bail!("the first \"version\" wasn't the top-level one");
    }
    Ok(out)
}

/// Our own crates' entries in `Cargo.lock`: those without a `source` line
/// (only path crates have none), at `old`, become `new`. Crates from
/// crates.io that happen to share the version keep theirs.
fn set_lock_versions(lock: &str, old: &str, new: &str) -> String {
    let mut out = String::with_capacity(lock.len());
    for block in lock.split_inclusive("[[package]]") {
        let ours = !block.contains("\nsource = ");
        if ours {
            out.push_str(&block.replacen(
                &format!("\nversion = \"{old}\""),
                &format!("\nversion = \"{new}\""),
                1,
            ));
        } else {
            out.push_str(block);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_go_up_by_their_part() {
        assert_eq!(bumped("0.11.1", Part::Patch).unwrap(), "0.11.2");
        assert_eq!(bumped("0.11.1", Part::Minor).unwrap(), "0.12.0");
        assert!(bumped("0.11", Part::Patch).is_err());
        assert!(bumped("0.11.x", Part::Patch).is_err());
    }

    #[test]
    fn only_the_workspace_version_changes_in_cargo_toml() {
        let cargo = "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.11.1\"\nedition = \"2024\"\n\n[workspace.dependencies]\nfoo = { version = \"0.11.1\" }\n";
        assert_eq!(cargo_version(cargo).unwrap(), "0.11.1");
        let out = set_cargo_version(cargo, "0.11.1", "0.11.2").unwrap();
        assert!(out.contains("[workspace.package]\nversion = \"0.11.2\""));
        assert!(
            out.contains("foo = { version = \"0.11.1\" }"),
            "a dependency keeps its own"
        );
        assert!(set_cargo_version(cargo, "0.9.0", "0.9.1").is_err());
    }

    #[test]
    fn json_keeps_its_formatting() {
        let json = "{\r\n  \"name\": \"x\",\r\n  \"version\": \"0.11.1\",\r\n  \"deps\": { \"a\": { \"version\": \"0.11.1\" } }\r\n}\r\n";
        let out = set_json_version(json, "0.11.1", "0.11.2").unwrap();
        assert_eq!(json_version(&out).unwrap(), "0.11.2");
        assert!(out.contains("\r\n  \"version\": \"0.11.2\",\r\n"));
        assert!(
            out.contains("{ \"version\": \"0.11.1\" }"),
            "nested ones untouched"
        );
    }

    #[test]
    fn the_lock_file_changes_only_our_crates() {
        let lock = "version = 4\n\n[[package]]\nname = \"chacha20\"\nversion = \"0.11.1\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"couch-proto\"\nversion = \"0.11.1\"\ndependencies = [\n \"serde\",\n]\n";
        let out = set_lock_versions(lock, "0.11.1", "0.11.2");
        assert!(out.contains("name = \"couch-proto\"\nversion = \"0.11.2\""));
        assert!(out.contains("name = \"chacha20\"\nversion = \"0.11.1\""));
        assert!(
            out.starts_with("version = 4\n"),
            "the lock format's own version stays"
        );
    }
}
