//! The source of the system libraries an AppImage carries (M26c): each of
//! Ubuntu's source packages its notices name, at that exact version, from
//! Launchpad (which keeps every version Ubuntu published, where the
//! archive drops superseded ones), packed into one tar beside the
//! AppImage. The LGPL asks for the source from the same place as the
//! binary; a release page attaching both is that place.
//!
//! Each package is its `.dsc` and the files it lists (the upstream tarball,
//! Ubuntu's changes), every file checked against the `.dsc`'s SHA-256,
//! which comes from Launchpad over https. `dpkg-source -x <name>.dsc`
//! unpacks one. Downloads are kept in `third_party/_downloads` for the
//! next build.

use std::{collections::BTreeMap, fs, path::Path, process::Command};

use anyhow::{Context, bail};

use crate::deps;

/// Where Launchpad serves a source package's files, by name and version.
const LAUNCHPAD: &str = "https://launchpad.net/ubuntu/+archive/primary/+sourcefiles";

/// Fetches every package (source name to version) and packs them into
/// `tar`, with a README. Returns how many files it holds.
pub(crate) fn fetch(
    root: &Path,
    packages: &BTreeMap<String, String>,
    tar: &Path,
) -> anyhow::Result<usize> {
    let cache = deps::third_party(root).join("_downloads/system-sources");
    fs::create_dir_all(&cache)?;
    let staging = deps::target_dir(root).join("system-sources");
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;

    let mut count = 0;
    for (source, version) in packages {
        check_name(source, version)?;
        let base = format!("{LAUNCHPAD}/{source}/{version}");
        let dsc_name = format!("{source}_{}.dsc", without_epoch(version));
        let dsc_path = cache.join(&dsc_name);
        if !dsc_path.exists() {
            download(&format!("{base}/{dsc_name}"), &dsc_path)?;
        }
        let dsc = fs::read_to_string(&dsc_path)?;
        let files = match parse_dsc(&dsc, source, version) {
            Ok(files) => files,
            Err(e) => {
                // A broken download mustn't stay cached.
                fs::remove_file(&dsc_path)?;
                return Err(e.context(format!("reading {dsc_name}")));
            }
        };
        for (file, sha256) in &files {
            let cached = cache.join(file);
            if !(cached.exists() && deps::sha256_file(&cached)? == *sha256) {
                download(&format!("{base}/{file}"), &cached)?;
                let actual = deps::sha256_file(&cached)?;
                if actual != *sha256 {
                    fs::remove_file(&cached)?;
                    bail!("{file}: SHA-256 {actual}, but {dsc_name} says {sha256}");
                }
            }
            fs::copy(&cached, staging.join(file))?;
            count += 1;
        }
        fs::copy(&dsc_path, staging.join(&dsc_name))?;
        count += 1;
    }
    fs::write(staging.join("README.txt"), readme(packages))?;

    let status = Command::new("tar")
        .args([
            "--create",
            "--sort=name",
            "--owner=0",
            "--group=0",
            "--file",
        ])
        .arg(tar)
        .arg("--directory")
        .arg(&staging)
        .arg(".")
        .status()
        .context("could not run tar")?;
    if !status.success() {
        bail!("tar failed with {status}");
    }
    fs::remove_dir_all(&staging)?;
    Ok(count)
}

fn readme(packages: &BTreeMap<String, String>) -> String {
    let list: Vec<String> = packages
        .iter()
        .map(|(source, version)| format!("  {source} {version}"))
        .collect();
    format!(
        "The source of the system libraries in Couch Co-App's AppImage

The AppImage carries libraries from Ubuntu 24.04's packages (WebKitGTK,
GTK, GLib and what they need), under the LGPL and other licences: its
THIRD-PARTY-NOTICES name each one with its copyright file. These are
those packages' sources, at exactly the versions it carries, as Ubuntu
published them (https://launchpad.net/ubuntu/+source/<package>/<version>).

Each package is its .dsc file and the files the .dsc lists. To unpack one,
on Debian or Ubuntu: dpkg-source -x <package>_<version>.dsc

{}
",
        list.join("\n")
    )
}

/// The files a `.dsc` lists, with their SHA-256, after checking it is the
/// package and version asked for.
fn parse_dsc(text: &str, source: &str, version: &str) -> anyhow::Result<Vec<(String, String)>> {
    // A signed .dsc wraps the fields in the signature's armour.
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{name}: ")))
            .map(str::trim)
    };
    if field("Source") != Some(source) || field("Version") != Some(version) {
        bail!(
            "it names {:?} {:?}, not {source} {version}",
            field("Source"),
            field("Version")
        );
    }
    let mut files = Vec::new();
    let mut lines = text
        .lines()
        .skip_while(|l| l.trim_end() != "Checksums-Sha256:");
    lines.next();
    for line in lines.take_while(|l| l.starts_with(' ')) {
        let mut parts = line.split_whitespace();
        let (Some(sha256), Some(_size), Some(file), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            bail!("a Checksums-Sha256 line isn't `sha256 size file`");
        };
        let name_ok = !file.starts_with('.')
            && file
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '_' | '~'));
        let hash_ok = sha256.len() == 64 && sha256.chars().all(|c| c.is_ascii_hexdigit());
        if !name_ok || !hash_ok {
            bail!("a Checksums-Sha256 line names {file:?} with {sha256:?}");
        }
        files.push((file.to_string(), sha256.to_ascii_lowercase()));
    }
    if files.is_empty() {
        bail!("it lists no files under Checksums-Sha256");
    }
    Ok(files)
}

/// Debian's names and versions, as dpkg gave them: nothing that could
/// leave the cache folder or change the URL.
fn check_name(source: &str, version: &str) -> anyhow::Result<()> {
    let source_ok = !source.is_empty()
        && source
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '+' | '-'));
    let version_ok = !version.is_empty()
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '~' | ':'));
    if !source_ok || !version_ok || source.starts_with('.') {
        bail!("not a Debian source package and version: {source:?} {version:?}");
    }
    Ok(())
}

/// A version as file names carry it: without its epoch (`1:`).
fn without_epoch(version: &str) -> &str {
    version.split_once(':').map_or(version, |(_, rest)| rest)
}

fn download(url: &str, path: &Path) -> anyhow::Result<()> {
    println!("Fetching {url}");
    let status = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--retry",
            "3",
            "--user-agent",
            "couch-coapp-build",
            "--output",
        ])
        .arg(path)
        .arg(url)
        .status()
        .context("could not run curl")?;
    if !status.success() {
        let _ = fs::remove_file(path);
        bail!("could not download {url} ({status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DSC: &str = "-----BEGIN PGP SIGNED MESSAGE-----
Hash: SHA512

Format: 3.0 (quilt)
Source: zlib
Binary: zlib1g, zlib1g-dev
Version: 1:1.3.dfsg-3.1ubuntu2.1
Checksums-Sha1:
 aaaa 1128572 zlib_1.3.dfsg.orig.tar.xz
Checksums-Sha256:
 5eea0322c1c21c75cad3b607ac1c43ff5c71e014b8ac4a34300b5e2b80d02e70 1128572 zlib_1.3.dfsg.orig.tar.xz
 958c7031c02f894516492954153c8d760d94e20a4039e48ca7231880b913ae26 61028 zlib_1.3.dfsg-3.1ubuntu2.1.debian.tar.xz
Files:
 bbbb 1128572 zlib_1.3.dfsg.orig.tar.xz
";

    #[test]
    fn a_dsc_lists_its_files_by_sha256() {
        let files = parse_dsc(DSC, "zlib", "1:1.3.dfsg-3.1ubuntu2.1").unwrap();
        assert_eq!(
            files,
            vec![
                (
                    "zlib_1.3.dfsg.orig.tar.xz".to_string(),
                    "5eea0322c1c21c75cad3b607ac1c43ff5c71e014b8ac4a34300b5e2b80d02e70".to_string()
                ),
                (
                    "zlib_1.3.dfsg-3.1ubuntu2.1.debian.tar.xz".to_string(),
                    "958c7031c02f894516492954153c8d760d94e20a4039e48ca7231880b913ae26".to_string()
                ),
            ]
        );
        // Another package or version than asked for.
        assert!(parse_dsc(DSC, "zlib", "1:1.3.dfsg-3.1ubuntu2").is_err());
        assert!(parse_dsc(DSC, "glib2.0", "1:1.3.dfsg-3.1ubuntu2.1").is_err());
        // A file name that could leave the folder.
        let evil = DSC.replace(
            "61028 zlib_1.3.dfsg-3.1ubuntu2.1.debian.tar.xz",
            "61028 ../evil",
        );
        assert!(parse_dsc(&evil, "zlib", "1:1.3.dfsg-3.1ubuntu2.1").is_err());
    }

    #[test]
    fn names_and_versions_stay_debians() {
        assert!(check_name("glib2.0", "2.80.0-6ubuntu3.4").is_ok());
        assert!(check_name("zlib", "1:1.3.dfsg-3.1ubuntu2.1").is_ok());
        assert!(check_name("libx11", "2:1.8.7-1build1").is_ok());
        assert!(check_name("../x", "1").is_err());
        assert!(check_name("zlib", "1/2").is_err());
        assert!(check_name("Zlib", "1").is_err());
        assert_eq!(without_epoch("1:1.3.dfsg-3"), "1.3.dfsg-3");
        assert_eq!(without_epoch("2.80.0-6"), "2.80.0-6");
    }
}
