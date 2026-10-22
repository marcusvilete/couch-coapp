//! `cargo xtask notices`: THIRD-PARTY-NOTICES.txt, the licences of
//! everything bundled with the app (M10b). Every installer carries it, even
//! one for friends: handing out an installer is distributing all of it.
//!
//! - The Rust crates linked into the two binaries, from cargo-about
//!   (`about.toml`), each licence text once with the crates under it.
//! - The front end's runtime npm packages, from `npm ls`.
//! - What isn't a crate or a package: FFmpeg, SDL3, the drivers.
//!
//! It fails rather than leave anything out: a crate cargo-about lists
//! without a licence text, or a package without a licence file.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use serde::Deserialize;

use crate::deps;

pub const FILE_NAME: &str = "THIRD-PARTY-NOTICES.txt";

/// Where `notices` writes; `stage-dlls` copies it next to the built app.
pub fn output(root: &Path) -> PathBuf {
    deps::target_dir(root).join(FILE_NAME)
}

/// The binaries that ship, whose dependencies the notices cover.
const SHIPPED: &[&str] = &[
    "apps/desktop/src-tauri/Cargo.toml",
    "crates/couch-viewer/Cargo.toml",
];

#[derive(Deserialize)]
struct About {
    licenses: Vec<AboutLicense>,
    crates: Vec<AboutCrate>,
}

#[derive(Deserialize)]
struct AboutLicense {
    id: String,
    name: String,
    text: String,
    used_by: Vec<UsedBy>,
}

#[derive(Deserialize)]
struct UsedBy {
    #[serde(rename = "crate")]
    krate: Package,
}

#[derive(Deserialize)]
struct AboutCrate {
    package: Package,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
}

impl Package {
    fn label(&self) -> String {
        format!("{} {}", self.name, self.version)
    }

    /// Ours: GPL-3.0-only, covered by LICENSE.
    fn is_ours(&self) -> bool {
        self.name.starts_with("couch-") || self.name == "xtask"
    }
}

/// One licence text and who uses it.
struct Group {
    title: String,
    text: String,
    users: BTreeSet<String>,
}

pub fn generate(root: &Path) -> anyhow::Result<()> {
    let crates = crate_licences(root)?;
    let packages = npm_licences(root)?;
    let mut out = header(root)?;
    out.push_str(&components(root)?);
    section(&mut out, "The front end's packages (npm)");
    for group in &packages {
        push_group(&mut out, group);
    }
    section(&mut out, "Rust crates");
    for group in &crates {
        push_group(&mut out, group);
    }

    let path = output(root);
    fs::create_dir_all(path.parent().expect("a file has a folder"))?;
    fs::write(&path, out)?;
    let crate_count = crates
        .iter()
        .flat_map(|g| &g.users)
        .collect::<BTreeSet<_>>()
        .len();
    println!(
        "Wrote {} ({crate_count} crates under {} licence texts, {} npm packages)",
        path.display(),
        crates.len(),
        packages.len()
    );
    Ok(())
}

fn header(root: &Path) -> anyhow::Result<String> {
    let version = workspace_field(root, "version")?.context("no workspace version")?;
    let commit = git(root, &["rev-parse", "HEAD"])?;
    let dirty = !git(root, &["status", "--porcelain"])?.is_empty();
    let source = match workspace_field(root, "repository")? {
        Some(repo) if !dirty => format!("{repo}/tree/{commit}"),
        Some(repo) => format!(
            "{repo} (this build had uncommitted changes: ask whoever gave you\n  this installer for its exact source)"
        ),
        None => "ask whoever gave you this installer: they must provide it".to_string(),
    };
    let sources = deps::SOURCES
        .iter()
        .map(|s| format!("  - {}: {}\n    ({})", s.file, s.name, s.url))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "Couch Co-App {version}, built from commit {short}{dirty}
Copyright (C) 2026 the Couch Co-App contributors

This program is free software: you can redistribute it and/or modify it
under the terms of the GNU General Public License, version 3 only, as
published by the Free Software Foundation: see the LICENSE file beside it.
It comes with ABSOLUTELY NO WARRANTY.

Its source code:
  {source}

FFmpeg's source code and the scripts that built the copy included here
come with the installer, as files beside it (see SOURCES.txt there):
{sources}

The rest of this file lists the licences of everything bundled with it:
FFmpeg, SDL3 and the drivers first, then the front end's packages, then the
Rust crates.
",
        short = &commit[..10.min(commit.len())],
        dirty = if dirty {
            " (with uncommitted changes)"
        } else {
            ""
        },
    ))
}

fn components(root: &Path) -> anyhow::Result<String> {
    let third = deps::third_party(root);
    let mut out = String::new();
    section(&mut out, "Bundled components");

    let ffmpeg = third.join("ffmpeg");
    let configuration = ffmpeg_configuration(&ffmpeg)?;
    let ffmpeg_dep = deps::DEPS
        .iter()
        .find(|d| d.dir == "ffmpeg")
        .expect("FFmpeg is pinned");
    push_group(
        &mut out,
        &Group {
            title: format!(
                "FFmpeg: the libav* DLLs beside the app ({})\n\
                 Built by BtbN/FFmpeg-Builds: {}\n\
                 Its configuration, which names the libraries built into it:\n{}\n\
                 You may replace these DLLs with your own build of the same FFmpeg\n\
                 version: the app loads them dynamically.",
                ffmpeg_dep.name,
                ffmpeg_dep.url,
                wrap(&configuration, 76)
            ),
            text: read(&ffmpeg.join("LICENSE.txt"))?,
            users: BTreeSet::new(),
        },
    );

    let sdl = deps::DEPS
        .iter()
        .find(|d| d.dir == "sdl3")
        .expect("SDL3 is pinned");
    push_group(
        &mut out,
        &Group {
            title: format!("SDL3: SDL3.dll beside the app ({})", sdl.name),
            text: read(&third.join("sdl3").join("LICENSE.txt"))?,
            users: BTreeSet::new(),
        },
    );

    for driver in deps::DRIVERS {
        push_group(
            &mut out,
            &Group {
                title: format!(
                    "{}: its official installer, in drivers\\ beside the app",
                    driver.name
                ),
                text: read(
                    &root
                        .join("apps/desktop/src-tauri/drivers")
                        .join(driver.license),
                )?,
                users: BTreeSet::new(),
            },
        );
    }
    Ok(out)
}

/// The crates linked into the shipped binaries, grouped by licence text.
fn crate_licences(root: &Path) -> anyhow::Result<Vec<Group>> {
    let work = deps::target_dir(root).join("notices");
    fs::create_dir_all(&work)?;
    let mut groups: BTreeMap<(String, String), Group> = BTreeMap::new();
    let mut listed = BTreeSet::new();
    let mut covered = BTreeSet::new();
    for (i, manifest) in SHIPPED.iter().enumerate() {
        let json = work.join(format!("about-{i}.json"));
        let status = Command::new("cargo")
            .current_dir(root)
            .args([
                "about", "generate", "--format", "json", "--fail", "--locked",
            ])
            .args(["-c", "about.toml", "-m", manifest, "-o"])
            .arg(&json)
            .status()
            .context(
                "could not run cargo about (cargo install --locked cargo-about --features cli)",
            )?;
        if !status.success() {
            bail!("cargo about failed for {manifest} with {status}");
        }
        let about: About = serde_json::from_str(&read(&json)?)
            .with_context(|| format!("reading {}", json.display()))?;
        for krate in about.crates.iter().map(|c| &c.package) {
            if !krate.is_ours() {
                listed.insert(krate.label());
            }
        }
        for licence in about.licenses {
            let group = groups
                .entry((licence.id.clone(), licence.text.clone()))
                .or_insert_with(|| Group {
                    title: format!("{} ({})", licence.name, licence.id),
                    text: licence.text,
                    users: BTreeSet::new(),
                });
            for user in licence.used_by {
                covered.insert(user.krate.label());
                group.users.insert(user.krate.label());
            }
        }
    }
    let missing: Vec<_> = listed.difference(&covered).collect();
    if !missing.is_empty() {
        bail!("crates without a licence text: {missing:?}");
    }
    let mut groups: Vec<Group> = groups.into_values().collect();
    // The most used first, so the long tail of one-crate texts comes last.
    groups.sort_by(|a, b| {
        b.users
            .len()
            .cmp(&a.users.len())
            .then(a.title.cmp(&b.title))
    });
    Ok(groups)
}

/// The front end's runtime packages (what Vite bundles from), each with
/// its own licence file.
fn npm_licences(root: &Path) -> anyhow::Result<Vec<Group>> {
    let app = root.join("apps/desktop");
    // npm is a .cmd script on Windows, which Command can't start directly.
    let output = if cfg!(windows) {
        Command::new("cmd")
            .current_dir(&app)
            .args(["/C", "npm", "ls", "--omit=dev", "--all", "--json"])
            .output()
    } else {
        Command::new("npm")
            .current_dir(&app)
            .args(["ls", "--omit=dev", "--all", "--json"])
            .output()
    }
    .context("could not run npm")?;
    if !output.status.success() {
        bail!(
            "npm ls failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let tree: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let mut found = BTreeMap::new();
    collect_npm(&tree, &mut found);

    let mut groups = Vec::new();
    for (name, version) in found {
        let dir = app.join("node_modules").join(&name);
        let manifest: serde_json::Value = serde_json::from_str(&read(&dir.join("package.json"))?)?;
        let licence = manifest["license"]
            .as_str()
            .unwrap_or("see below")
            .to_string();
        let shipped = fs::read_dir(&dir)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| {
                p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    let n = n.to_ascii_uppercase();
                    n.starts_with("LICENSE") || n.starts_with("LICENCE") || n.starts_with("COPYING")
                })
            });
        // A package published without its licence file: the text from its
        // repository, pinned in `xtask/licences/npm/` (M21), and said so.
        let (file, title) = match shipped {
            Some(file) => (file, format!("{name} {version} ({licence})")),
            None => {
                let pinned = root
                    .join("xtask/licences/npm")
                    .join(format!("{}.LICENSE", name.replace('/', "__")));
                if !pinned.is_file() {
                    bail!(
                        "npm package {name} has no licence file: pin its repository's in {}",
                        pinned.display()
                    );
                }
                let title = format!(
                    "{name} {version} ({licence}; the package ships no licence file, this is its repository's)"
                );
                (pinned, title)
            }
        };
        groups.push(Group {
            title,
            text: read(&file)?,
            users: BTreeSet::new(),
        });
    }
    Ok(groups)
}

fn collect_npm(node: &serde_json::Value, found: &mut BTreeMap<String, String>) {
    let Some(deps) = node["dependencies"].as_object() else {
        return;
    };
    for (name, dep) in deps {
        if let Some(version) = dep["version"].as_str() {
            found.insert(name.clone(), version.to_string());
        }
        collect_npm(dep, found);
    }
}

fn ffmpeg_configuration(ffmpeg: &Path) -> anyhow::Result<String> {
    let exe = ffmpeg.join("bin").join("ffmpeg.exe");
    let output = Command::new(&exe)
        .args(["-hide_banner", "-version"])
        .output()
        .with_context(|| format!("could not run {}", exe.display()))?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    text.lines()
        .find_map(|l| l.strip_prefix("configuration:"))
        .map(|c| c.trim().to_string())
        .context("ffmpeg -version printed no configuration")
}

fn section(out: &mut String, title: &str) {
    out.push_str(&format!(
        "\n\n{rule}\n{title}\n{rule}\n",
        rule = "=".repeat(78)
    ));
}

fn push_group(out: &mut String, group: &Group) {
    out.push_str(&format!("\n{}\n{}\n", "-".repeat(78), group.title));
    if !group.users.is_empty() {
        let users: Vec<&str> = group.users.iter().map(String::as_str).collect();
        out.push_str(&format!("Used by: {}\n", wrap(&users.join(", "), 76)));
    }
    out.push('\n');
    out.push_str(group.text.trim_end());
    out.push('\n');
}

/// Wraps a long line at spaces, for reading in a narrow window.
fn wrap(text: &str, width: usize) -> String {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines.join("\n  ")
}

/// A `key = "value"` from the root Cargo.toml's `[workspace.package]`.
fn workspace_field(root: &Path, key: &str) -> anyhow::Result<Option<String>> {
    let manifest = read(&root.join("Cargo.toml"))?;
    let in_section = manifest
        .split("[workspace.package]")
        .nth(1)
        .and_then(|rest| rest.split("\n[").next())
        .unwrap_or_default();
    Ok(in_section.lines().find_map(|l| {
        l.trim()
            .strip_prefix(key)
            .map(str::trim_start)
            .and_then(|r| r.strip_prefix('='))
            .map(|v| v.trim().trim_matches('"').to_string())
    }))
}

fn git(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .context("could not run git")?;
    if !output.status.success() {
        bail!("git {} failed", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn read(path: &Path) -> anyhow::Result<String> {
    // Some licence files aren't valid UTF-8 (Latin-1 names); keep them readable.
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).replace("\r\n", "\n"))
}
