//! What an AppImage carries besides our own (M16b): the AppImage's runtime
//! and start-up scripts, and the system libraries Tauri's bundler
//! (linuxdeploy and its GTK plugin) copies from the build machine:
//! WebKitGTK, GTK, GLib and what they need. Each library came from one of
//! Ubuntu's packages, whose copyright file the notices carry and whose
//! source package they name: the LGPL's offer of source, as FFmpeg's
//! archives are.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};

use super::{Group, push_group, read, section, wrap};
use crate::deps;

/// What Tauri's bundler made: the AppImage's folder, and the AppImage
/// packed from it (whose runtime the notices name).
pub struct Bundle<'a> {
    pub appdir: &'a Path,
    pub appimage: &'a Path,
}

/// Files in the AppImage that are ours, or its own parts' (named in the
/// notices' sections before the system libraries), by their path there.
/// Every other file must come from one of Ubuntu's packages.
const KNOWN: &[&str] = &[
    "AppRun",
    "AppRun.wrapped",
    "apprun-hooks/linuxdeploy-plugin-gtk.sh",
    ".DirIcon",
    "Couch Co-App.desktop",
    "Couch Co-App.png",
    "couch-desktop.png",
    "usr/bin/couch-desktop",
    "usr/bin/couch-viewer",
    "usr/bin/lucide.ttf",
    "usr/bin/LICENSE",
    "usr/bin/THIRD-PARTY-NOTICES.txt",
];

/// Our own files under `usr/share`, by name before the first dot: the
/// desktop entry and the icons Tauri writes.
const OURS: &[&str] = &["Couch Co-App", "couch-desktop"];

/// What the runtime says is built into it (its "License:" lines), each
/// with its licence text pinned in `xtask/licences/appimage/`.
const RUNTIME_PARTS: &[(&str, &str)] = &[
    ("runtime", "type2-runtime.LICENSE"),
    ("musl libc", "musl.COPYRIGHT"),
    ("libfuse", "libfuse.LGPL2.txt"),
    ("squashfuse", "squashfuse.LICENSE"),
    ("libzstd", "zstd.LICENSE"),
    ("zlib", "zlib.LICENSE"),
];

pub fn sections(root: &Path, bundle: Option<&Bundle>) -> anyhow::Result<String> {
    let mut out = String::new();
    let Some(bundle) = bundle else {
        section(&mut out, "System libraries");
        out.push_str(
            "\nNone: this build is for this PC, and uses the system's own libraries.\n\
             An AppImage's notices list the ones it carries.\n",
        );
        return Ok(out);
    };
    out.push_str(&tooling(root, bundle.appimage)?);
    out.push_str(&libraries(root, bundle.appdir)?);
    Ok(out)
}

/// The runtime at the AppImage's start, which mounts it, and the scripts
/// that start the app inside it.
fn tooling(root: &Path, appimage: &Path) -> anyhow::Result<String> {
    let pinned = root.join("xtask/licences/appimage");
    let version = runtime_version(appimage)?;
    let commit = version
        .rsplit('/')
        .next()
        .filter(|c| !c.is_empty())
        .context("the runtime's version names no commit")?;
    let parts = runtime_parts(appimage)?;
    let expected: BTreeSet<String> = RUNTIME_PARTS.iter().map(|(n, _)| n.to_string()).collect();
    if parts != expected {
        bail!(
            "the AppImage's runtime names {parts:?} as built into it, not {expected:?}: pin their licences in {}",
            pinned.display()
        );
    }

    let mut out = String::new();
    section(&mut out, "The AppImage's own parts");
    let runtime_source = format!("https://github.com/AppImage/type2-runtime/tree/{commit}");
    for (name, file) in RUNTIME_PARTS {
        let title = match *name {
            "runtime" => format!(
                "The AppImage's runtime, the program at its start that mounts it:\n\
                 AppImage's type2-runtime ({version})\n\
                 Its source, and the build of what is built into it: {runtime_source}"
            ),
            "libfuse" => format!(
                "libfuse, built into the runtime (LGPL-2.1; the runtime's source\n\
                 and build scripts: {runtime_source})"
            ),
            other => format!("{other}, built into the runtime"),
        };
        push_group(
            &mut out,
            &Group {
                title,
                text: read(&pinned.join(file))?,
                users: BTreeSet::new(),
            },
        );
    }
    for (title, file) in [
        (
            "AppRun.wrapped, which starts the app inside the AppImage (AppImageKit's\n\
             AppRun, as Tauri's bundler ships it)",
            "AppImageKit.LICENSE",
        ),
        (
            "AppRun, the script linuxdeploy writes to run the hooks before it",
            "linuxdeploy.LICENSE",
        ),
        (
            "apprun-hooks/linuxdeploy-plugin-gtk.sh, which sets up GTK inside the AppImage\n\
             (linuxdeploy-plugin-gtk)",
            "linuxdeploy-plugin-gtk.LICENSE",
        ),
    ] {
        push_group(
            &mut out,
            &Group {
                title: title.to_string(),
                text: read(&pinned.join(file))?,
                users: BTreeSet::new(),
            },
        );
    }
    Ok(out)
}

/// The runtime's commit, as it prints it.
pub fn runtime_version(appimage: &Path) -> anyhow::Result<String> {
    let output = Command::new(appimage)
        .arg("--appimage-version")
        .output()
        .with_context(|| format!("could not run {}", appimage.display()))?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    text.lines()
        .find_map(|l| l.strip_prefix("AppImage runtime version:"))
        .map(|v| v.trim().to_string())
        .context("the AppImage printed no runtime version")
}

/// The components the runtime's own licence lines name ("  * libfuse,
/// licensed under the terms of"), read from the runtime's bytes.
fn runtime_parts(appimage: &Path) -> anyhow::Result<BTreeSet<String>> {
    let output = Command::new(appimage)
        .arg("--appimage-offset")
        .output()
        .with_context(|| format!("could not run {}", appimage.display()))?;
    let offset: usize = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .context("the AppImage printed no offset")?;
    let bytes = fs::read(appimage)?;
    let runtime = String::from_utf8_lossy(&bytes[..offset.min(bytes.len())]).into_owned();
    // Its own strings, each line apart, among the rest of its bytes.
    let parts: BTreeSet<String> = runtime
        .split(['\n', '\0'])
        .filter_map(|l| {
            l.trim()
                .strip_prefix("* ")?
                .strip_suffix(", licensed under the terms of")
        })
        .map(str::to_string)
        .collect();
    if parts.is_empty() {
        bail!("the AppImage's runtime names no licences");
    }
    Ok(parts)
}

/// One of Ubuntu's source packages, and what of it the AppImage carries.
#[derive(Default)]
struct SourcePackage {
    version: String,
    binaries: BTreeMap<String, String>,
    files: BTreeSet<String>,
    texts: BTreeSet<String>,
}

fn libraries(root: &Path, appdir: &Path) -> anyhow::Result<String> {
    // The notices, SOURCES.txt and the docs say Ubuntu 24.04, and promise
    // the glibc it has.
    let system = os_release()?;
    if system.get("ID").map(String::as_str) != Some("ubuntu")
        || system.get("VERSION_ID").map(String::as_str) != Some("24.04")
    {
        bail!(
            "build the AppImage on Ubuntu 24.04: its notices name that system's source packages, and it needs that system's glibc to run"
        );
    }
    let ffmpeg: BTreeSet<String> = deps::dlls(root)?
        .iter()
        .filter_map(|p| Some(p.file_name()?.to_str()?.to_string()))
        .collect();

    let mut files = Vec::new();
    walk(appdir, appdir, &mut files)?;
    files.retain(|rel| {
        let name = file_name(rel);
        let stem = name.split('.').next().unwrap_or(name);
        !KNOWN.contains(&rel.as_str())
            && !(rel.starts_with("usr/share/") && OURS.contains(&stem))
            && !(rel.starts_with("usr/lib/") && ffmpeg.contains(name))
            // The copyright files linuxdeploy copies, from the packages below.
            && !rel.starts_with("usr/share/doc/")
            // Made by the bundler from the files it copied.
            && !name.ends_with(".cache")
            && name != "gschemas.compiled"
    });

    let owners = owners(&files)?;
    let mut missing = Vec::new();
    let mut by_package: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for rel in &files {
        match owners.get(rel) {
            Some(packages) => {
                for package in packages {
                    by_package
                        .entry(package.clone())
                        .or_default()
                        .insert(rel.clone());
                }
            }
            None => missing.push(rel.as_str()),
        }
    }
    if !missing.is_empty() {
        bail!("files in the AppImage that no package installed: {missing:?}");
    }

    let mut sources: BTreeMap<String, SourcePackage> = BTreeMap::new();
    for (binary, (version, source, source_version)) in package_versions(by_package.keys())? {
        let name = binary.split(':').next().unwrap_or(&binary);
        let copyright = PathBuf::from("/usr/share/doc").join(name).join("copyright");
        let text = read(&copyright).with_context(|| format!("{binary} has no copyright file"))?;
        let files = by_package
            .get(&binary)
            .with_context(|| format!("dpkg-query named {binary}, which dpkg -S didn't"))?;
        let entry = sources.entry(source).or_default();
        entry.version = source_version;
        entry.files.extend(files.iter().cloned());
        entry.binaries.insert(name.to_string(), version);
        entry.texts.insert(text);
    }

    let mut out = String::new();
    section(
        &mut out,
        &format!(
            "System libraries in the AppImage, from {}",
            system
                .get("PRETTY_NAME")
                .map(String::as_str)
                .unwrap_or("Ubuntu")
        ),
    );
    out.push_str(
        "\nEach comes from one of Ubuntu's packages: its section names the source\n\
         package and where Ubuntu publishes that exact version's source, and gives\n\
         the package's copyright file, which holds its licences.\n",
    );
    for (source, package) in &sources {
        let binaries: Vec<String> = package
            .binaries
            .iter()
            .map(|(name, version)| format!("{name} {version}"))
            .collect();
        let files: Vec<&str> = package.files.iter().map(String::as_str).collect();
        push_group(
            &mut out,
            &Group {
                title: format!(
                    "{source} {version}\n\
                     Source: https://launchpad.net/ubuntu/+source/{source}/{version}\n\
                     From: {}\n\
                     In the AppImage: {}",
                    wrap(&binaries.join(", "), 76),
                    wrap(&files.join(", "), 76),
                    version = package.version,
                ),
                text: package
                    .texts
                    .iter()
                    .map(|t| t.trim_end())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                users: BTreeSet::new(),
            },
        );
    }
    println!(
        "The AppImage carries {} system file(s) from {} Ubuntu source package(s)",
        files.len(),
        sources.len()
    );
    Ok(out)
}

/// Every file under `dir`, as a path relative to `base` with `/`.
fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            walk(base, &path, out)?;
        } else {
            let rel = path.strip_prefix(base).expect("walked from the base");
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

fn file_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// Which installed package each file came from. The bundler moves
/// libraries (into `usr/lib`), so a file is looked up by its name, and of
/// the paths with that name the ones sharing the most of its path win
/// (`usr/share/icons/Adwaita/index.theme` is Adwaita's, not another
/// theme's). A library moved into `usr/lib` must also be the one the
/// system's loader finds by that name (`ldconfig -p`): one from elsewhere
/// (`/usr/local/lib`) with a packaged one's name would otherwise be taken
/// for the packaged one.
fn owners(files: &[String]) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
    let patterns: BTreeSet<String> = files
        .iter()
        .map(|f| format!("*/{}", file_name(f)))
        .collect();
    // It fails when a pattern matches nothing; the files left without an
    // owner are reported below instead.
    let output = Command::new("dpkg")
        .arg("-S")
        .args(&patterns)
        .output()
        .context("could not run dpkg")?;
    let mut by_name: BTreeMap<String, Vec<(Vec<String>, String)>> = BTreeMap::new();
    for (packages, path) in String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(dpkg_line)
    {
        by_name
            .entry(file_name(&path).to_string())
            .or_default()
            .push((packages, path));
    }
    let loader = ldconfig()?;

    let mut owners = BTreeMap::new();
    let mut elsewhere = Vec::new();
    for rel in files {
        let Some(candidates) = by_name.get(file_name(rel)) else {
            continue;
        };
        let best = candidates
            .iter()
            .map(|(_, path)| shared_tail(rel, path))
            .max()
            .unwrap_or(0);
        let best: Vec<&(Vec<String>, String)> = candidates
            .iter()
            .filter(|(_, path)| shared_tail(rel, path) == best)
            .collect();
        let moved_library = rel
            .strip_prefix("usr/lib/")
            .is_some_and(|name| !name.contains('/'));
        if moved_library && let Some(found) = loader.get(file_name(rel)) {
            let packaged: BTreeSet<PathBuf> = best
                .iter()
                .filter_map(|(_, path)| fs::canonicalize(path).ok())
                .collect();
            if found.is_disjoint(&packaged) {
                elsewhere.push(format!("{rel} (the loader finds {found:?})"));
                continue;
            }
        }
        let packages: BTreeSet<String> = best
            .iter()
            .flat_map(|(packages, _)| packages.iter().cloned())
            .collect();
        owners.insert(rel.clone(), packages);
    }
    if !elsewhere.is_empty() {
        bail!(
            "libraries in the AppImage that aren't the packaged ones of their name: {elsewhere:?}"
        );
    }
    Ok(owners)
}

/// The libraries the system's loader finds by name (`ldconfig -p`), each
/// as the files it resolves to.
fn ldconfig() -> anyhow::Result<BTreeMap<String, BTreeSet<PathBuf>>> {
    let output = Command::new("ldconfig")
        .arg("-p")
        .output()
        .or_else(|_| Command::new("/sbin/ldconfig").arg("-p").output())
        .context("could not run ldconfig")?;
    let mut found: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        // "	libgtk-3.so.0 (libc6,x86-64) => /lib/x86_64-linux-gnu/libgtk-3.so.0"
        let Some((left, path)) = line.split_once(" => ") else {
            continue;
        };
        let Some(name) = left.split_whitespace().next() else {
            continue;
        };
        if let Ok(path) = fs::canonicalize(path.trim()) {
            found.entry(name.to_string()).or_default().insert(path);
        }
    }
    Ok(found)
}

/// `libgtk-3-0t64:amd64, other:amd64: /usr/lib/...` as its packages and
/// path. Anything else dpkg prints (diversions, "local diversion from: …")
/// is none.
fn dpkg_line(line: &str) -> Option<(Vec<String>, String)> {
    let (packages, path) = line.split_once(": /")?;
    let packages: Vec<String> = packages.split(", ").map(str::to_string).collect();
    if packages.iter().any(|p| p.is_empty() || p.contains(' ')) {
        return None;
    }
    Some((packages, format!("/{path}")))
}

/// How many of their last path components two paths share.
fn shared_tail(a: &str, b: &str) -> usize {
    a.rsplit('/')
        .zip(b.rsplit('/'))
        .take_while(|(x, y)| x == y)
        .count()
}

/// Each package's version, and its source package's name and version.
fn package_versions<'a>(
    packages: impl Iterator<Item = &'a String>,
) -> anyhow::Result<BTreeMap<String, (String, String, String)>> {
    let output = Command::new("dpkg-query")
        .args([
            "-W",
            "-f",
            "${binary:Package}\t${Version}\t${source:Package}\t${source:Version}\n",
        ])
        .args(packages)
        .output()
        .context("could not run dpkg-query")?;
    if !output.status.success() {
        bail!(
            "dpkg-query failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut versions = BTreeMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let [binary, version, source, source_version] = fields[..] else {
            bail!("dpkg-query printed {line:?}");
        };
        versions.insert(
            binary.to_string(),
            (
                version.to_string(),
                source.to_string(),
                source_version.to_string(),
            ),
        );
    }
    Ok(versions)
}

/// `/etc/os-release`'s fields.
fn os_release() -> anyhow::Result<BTreeMap<String, String>> {
    let text = read(Path::new("/etc/os-release"))?;
    Ok(text
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.trim_matches('"').to_string()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_belongs_to_the_paths_that_share_most_of_it() {
        assert_eq!(
            shared_tail(
                "usr/share/icons/Adwaita/index.theme",
                "/usr/share/icons/Adwaita/index.theme"
            ),
            5
        );
        assert_eq!(
            shared_tail(
                "usr/share/icons/Adwaita/index.theme",
                "/usr/share/icons/hicolor/index.theme"
            ),
            1
        );
        // Libraries move to usr/lib: only the name is shared.
        assert_eq!(
            shared_tail(
                "usr/lib/libgtk-3.so.0",
                "/usr/lib/x86_64-linux-gnu/libgtk-3.so.0"
            ),
            1
        );
    }

    #[test]
    fn dpkg_lines_name_every_package_sharing_a_path() {
        assert_eq!(
            dpkg_line("libglib2.0-0t64:amd64, libglib2.0-bin: /usr/lib/x86_64-linux-gnu/a.so"),
            Some((
                vec![
                    "libglib2.0-0t64:amd64".to_string(),
                    "libglib2.0-bin".to_string()
                ],
                "/usr/lib/x86_64-linux-gnu/a.so".to_string()
            ))
        );
        assert_eq!(dpkg_line("dpkg-query: no path found"), None);
        assert_eq!(dpkg_line("local diversion from: /usr/bin/a"), None);
        assert_eq!(dpkg_line("diversion by b from: /usr/bin/a"), None);
    }
}
