//! `cargo xtask appimage`: the Linux AppImage (M16b), what
//! `scripts/build-installer.ps1` is on Windows. `scripts/build-appimage.sh`
//! runs it, on Ubuntu 24.04 (WSL's included), from a clone:
//!
//! - no option: a release AppImage, on our relay, from the private
//!   `release.toml` (`cargo xtask release-config`); it needs a clean tree;
//! - `--config <file>`: another release configuration (`test = true` for
//!   a LAN one);
//! - `--test-relay`: a test AppImage on iroh's public relays, from any tree;
//! - `--invite-page <https://…/j/>` or `--no-invite-page`: where Copy
//!   invite links to, for this build.
//!
//! It bundles twice: the notices list the system libraries the bundler
//! copies, known only once it has, and must be inside the AppImage. Then
//! `target/appimage/` holds the AppImage, the notices, FFmpeg's source
//! archives and `SOURCES.txt`, to hand out together.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};

use crate::{deps, notices, relay};

#[derive(Debug, Default, PartialEq)]
struct Options {
    config: Option<PathBuf>,
    test_relay: bool,
    invite_page: Option<String>,
    no_invite_page: bool,
}

impl Options {
    fn parse(args: &[String]) -> anyhow::Result<Self> {
        let mut options = Options::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--config" => {
                    options.config = Some(args.next().context("--config takes a file")?.into());
                }
                "--test-relay" => options.test_relay = true,
                "--invite-page" => {
                    options.invite_page = Some(
                        args.next()
                            .context("--invite-page takes an address")?
                            .clone(),
                    );
                }
                "--no-invite-page" => options.no_invite_page = true,
                other => bail!(
                    "unknown option {other}; the options: --config <file>, --test-relay, --invite-page <address>, --no-invite-page"
                ),
            }
        }
        if options.config.is_some() && options.test_relay {
            bail!("pass --config or --test-relay, not both");
        }
        if options.invite_page.is_some() && options.no_invite_page {
            bail!("pass --invite-page or --no-invite-page, not both");
        }
        Ok(options)
    }
}

/// `release.toml`'s settings (deploy/release.example.toml).
#[derive(Default, PartialEq)]
struct ReleaseConfig {
    relay_url: String,
    relay_token: String,
    invite_page: String,
    test: bool,
}

/// The token prints as `..`, as `RelayToken` does in the app.
impl std::fmt::Debug for ReleaseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReleaseConfig")
            .field("relay_url", &self.relay_url)
            .field("relay_token", &"..")
            .field("invite_page", &self.invite_page)
            .field("test", &self.test)
            .finish()
    }
}

/// A flat file of `key = "text"` and `key = true|false` lines, read as
/// `read-release-config.ps1` reads it; anything else is refused, a key
/// given twice included. Errors name the line by number, never its text,
/// which could be the token.
fn read_release_config(text: &str) -> anyhow::Result<ReleaseConfig> {
    let mut config = ReleaseConfig::default();
    let mut seen = std::collections::BTreeSet::new();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for (number, line) in text.lines().map(str::trim).enumerate() {
        let number = number + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .map(|(k, v)| (k.trim(), v.trim()))
            .with_context(|| format!("line {number} isn't `key = value`"))?;
        if !seen.insert(key.to_string()) {
            bail!("'{key}' is set twice (line {number})");
        }
        let text = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .filter(|v| !v.contains('"'));
        let flag = match value {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        };
        // `test = "false"` (quoted) must not read as true: each key takes
        // only its own type.
        match (key, text, flag) {
            ("relay_url", Some(v), _) => config.relay_url = v.to_string(),
            ("relay_token", Some(v), _) => config.relay_token = v.to_string(),
            ("invite_page", Some(v), _) => config.invite_page = v.to_string(),
            ("test", _, Some(v)) => config.test = v,
            ("relay_url" | "relay_token" | "invite_page", ..) => {
                bail!("'{key}' takes text in quotes")
            }
            ("test", ..) => bail!("'test' takes true or false, without quotes"),
            _ => bail!("unknown setting on line {number}"),
        }
    }
    Ok(config)
}

/// An address the app can read (`couch_engine::build_config` treats one
/// it can't as none, which would quietly make a build without it):
/// https with a host and no credentials, or http in a test build. Parsed
/// by the parser the app's `RelayUrl` uses; and as written, in lower case,
/// since the app compares the text as it is.
fn check_address(name: &str, value: &str, test: bool) -> anyhow::Result<()> {
    let readable = url::Url::parse(value).is_ok_and(|url| {
        let scheme = url.scheme() == "https" || (test && url.scheme() == "http");
        let written = value.starts_with(&format!("{}://", url.scheme()));
        let host = url.host_str().is_some_and(|h| !h.is_empty());
        let credentials = !url.username().is_empty() || url.password().is_some();
        scheme && written && host && !credentials
    });
    if !readable {
        bail!(
            "{name} must be an https:// address with a host{}, in lower case, not '{value}'",
            if test {
                " (or http:// in a test build)"
            } else {
                ""
            }
        );
    }
    Ok(())
}

pub fn run(root: &Path, args: &[String]) -> anyhow::Result<()> {
    if cfg!(windows) {
        bail!(
            "the AppImage is built on Linux (Ubuntu 24.04, WSL's included): scripts/build-appimage.sh; on Windows, scripts/build-installer.ps1"
        );
    }
    let options = Options::parse(args)?;
    // tauri.appimage.conf.json takes the stream window from target/release.
    if deps::target_dir(root) != root.join("target") {
        bail!(
            "CARGO_TARGET_DIR points elsewhere; unset it: the AppImage's settings take the stream window from target/release"
        );
    }

    // The values baked into the build (couch_engine::build_config).
    let mut build: BTreeMap<&str, Option<String>> = [
        "COUCH_BUILD_RELAY_URL",
        "COUCH_BUILD_RELAY_TOKEN",
        "COUCH_BUILD_INVITE_PAGE",
        "COUCH_BUILD_TEST",
        "COUCH_BUILD_COMMIT",
    ]
    .into_iter()
    .map(|name| (name, None))
    .collect();
    let dirty = !git(root, &["status", "--porcelain"])?.is_empty();
    let suffix = if dirty { "-dirty" } else { "" };
    build.insert(
        "COUCH_BUILD_COMMIT",
        Some(git(root, &["rev-parse", "--short=10", "HEAD"])? + suffix),
    );

    let mut test = options.test_relay;
    let mut release = None;
    if options.test_relay {
        println!(
            "A test AppImage: iroh's public relays, and plain http relays on the LAN allowed."
        );
    } else {
        let path = match &options.config {
            Some(path) => path.clone(),
            None => relay::release_config_path()?,
        };
        // A relative path is the caller's: build-appimage.sh moves to the
        // repository's root before running this.
        let path = match std::env::var_os("COUCH_INVOKED_FROM") {
            Some(from) if path.is_relative() => PathBuf::from(from).join(path),
            _ => path,
        };
        if !path.exists() {
            bail!(
                "no release configuration at {}: run `cargo xtask release-config`, or build a test AppImage with --test-relay",
                path.display()
            );
        }
        // Not in this clone, nor in any other (WSL reaches Windows' own
        // checkout under /mnt/c, whose paths ignore case).
        let canonical = fs::canonicalize(&path)?;
        let folder = canonical.parent().context("a file has a folder")?;
        let in_a_work_tree = Command::new("git")
            .current_dir(folder)
            .args(["rev-parse", "--is-inside-work-tree"])
            .output()
            .is_ok_and(|o| o.status.success());
        if canonical.starts_with(fs::canonicalize(root)?) || in_a_work_tree {
            bail!(
                "{} is inside a repository: keep release.toml outside it (deploy/release.example.toml)",
                path.display()
            );
        }
        let config = read_release_config(&fs::read_to_string(&path)?)
            .with_context(|| format!("reading {}", path.display()))?;
        test = config.test;
        if config.relay_url.is_empty() {
            bail!(
                "{} has no relay_url yet: until our relay is deployed, build test AppImages with --test-relay",
                path.display()
            );
        }
        check_address("relay_url", &config.relay_url, test)?;
        if config.relay_token.is_empty() {
            bail!("{} has no relay_token", path.display());
        }
        println!(
            "Our relay: {}{}",
            config.relay_url,
            if test { " (a test build)" } else { "" }
        );
        build.insert("COUCH_BUILD_RELAY_URL", Some(config.relay_url.clone()));
        build.insert("COUCH_BUILD_RELAY_TOKEN", Some(config.relay_token.clone()));
        release = Some(config);
    }
    if test {
        build.insert("COUCH_BUILD_TEST", Some("1".into()));
    }

    // Where invites link to: this build's choice, else release.toml's.
    let page = if options.no_invite_page {
        String::new()
    } else if let Some(page) = &options.invite_page {
        page.clone()
    } else {
        release.map(|r| r.invite_page).unwrap_or_default()
    };
    if page.is_empty() {
        println!("Invites are couchco:// links (no invite page).");
    } else {
        check_address("The invite page", &page, test)?;
        // The app appends "#<invite>" to it: the page's folder, as in .../j/.
        if !page.ends_with('/') {
            bail!(
                "the invite page's address must end in '/' (the page is at .../j/), not '{page}'"
            );
        }
        println!("Invites link to {page}");
        build.insert("COUCH_BUILD_INVITE_PAGE", Some(page));
    }

    // The notices link the app's source at this commit: it must be that
    // exact commit, and reachable.
    if dirty {
        if !test {
            bail!(
                "uncommitted changes: a release AppImage must match a commit exactly, since its notices link to that commit's source. Commit first, or build a test AppImage (--test-relay)"
            );
        }
        println!(
            "warning: uncommitted changes: this test AppImage's notices ask friends to request its source from you."
        );
    }
    if git(root, &["branch", "-r", "--contains", "HEAD"])?.is_empty() {
        println!(
            "warning: this commit is on no remote branch yet: push it, or friends can't reach the source the notices link to."
        );
    }

    // One version everywhere: other members see Cargo's.
    let version = one_version(root)?;

    // The pinned libraries, the font and FFmpeg's sources; first notices
    // (without the system libraries yet), then all of it next to the
    // release build, where the bundle takes it.
    deps::fetch(root)?;
    notices::generate(root, None)?;
    deps::stage_dlls(root)?;

    let target = deps::target_dir(root);
    let bundles = target.join("release/bundle/appimage");
    let appdir = bundles.join("Couch Co-App.AppDir");
    let built = bundles.join(format!("Couch Co-App_{version}_amd64.AppImage"));

    tauri_build(root, &build)?;
    let runtime = notices::runtime_version(&built)?;
    let first = files_in(&appdir)?;
    notices::generate(
        root,
        Some(&notices::Bundle {
            appdir: &appdir,
            appimage: &built,
        }),
    )?;
    deps::stage_notices(root, &target)?;
    println!("Bundling again, with the notices that list what the first bundle carries");
    tauri_build(root, &build)?;

    // The second bundle must carry those notices, the same files and the
    // same runtime: read from the AppImage itself.
    let notices = fs::read(notices::output(root))?;
    if carried_notices(&built, &target.join("appimage-check"))? != notices {
        bail!("the AppImage doesn't carry the notices just written");
    }
    if files_in(&appdir)? != first {
        bail!(
            "the second bundle carries other files than the first, which the notices list; build again"
        );
    }
    if notices::runtime_version(&built)? != runtime {
        bail!("the AppImage's runtime changed between the two bundles; build again");
    }
    // The build may not change what the notices link to (Cargo.lock, say).
    if !dirty && !git(root, &["status", "--porcelain"])?.is_empty() {
        bail!(
            "the build changed files in the repository (git status): the AppImage no longer matches its commit"
        );
    }

    let commit = git(root, &["rev-parse", "--short", "HEAD"])?;
    let kind = if test { "-test" } else { "" };
    let name = format!("Couch-Co-App_{version}_{commit}{suffix}{kind}.AppImage");
    let out = target.join("appimage");
    if out.exists() {
        fs::remove_dir_all(&out)?;
    }
    fs::create_dir_all(&out)?;
    fs::copy(&built, out.join(&name))?;
    fs::write(out.join(notices::FILE_NAME), &notices)?;
    let mut archives = Vec::new();
    for source in deps::SOURCES {
        let from = deps::third_party(root).join("sources").join(source.file);
        fs::copy(&from, out.join(source.file))?;
        archives.push(format!(
            "{}\n  SHA-256 {}\n",
            source.file,
            deps::sha256_file(&from)?
        ));
    }
    let app_source = match workspace_repository(root)? {
        Some(repo) if !dirty => format!("{repo}/tree/{}", git(root, &["rev-parse", "HEAD"])?),
        _ => "ask whoever gave you this AppImage: they must provide it.".to_string(),
    };
    fs::write(
        out.join("SOURCES.txt"),
        format!(
            "Couch Co-App {version}, the files beside its AppImage

{name}
  The app: make it executable (chmod +x, or its Properties), then open it.
  Couch Co-App is free software under the GNU GPL, version 3 (the LICENSE
  file inside it). Its third-party licences are in
  {notices}, beside it and inside it (Settings -> About).

The app's source code, for exactly this AppImage:
  {app_source}

FFmpeg, which the app includes as libraries, is under the LGPL. Its source,
and the scripts that built the included copy, are these two archives:

{archives}
The AppImage also carries system libraries from Ubuntu 24.04 (WebKitGTK,
GTK, GLib and what they need), under the LGPL and other licences: the
notices name each one's source package and version, and where Ubuntu
publishes it.
",
            notices = notices::FILE_NAME,
            archives = archives.join("\n"),
        ),
    )?;

    let size = fs::metadata(out.join(&name))?.len();
    println!(
        "Built {name} ({:.1} MB) in {}, with the notices, FFmpeg's sources and SOURCES.txt.",
        size as f64 / 1_048_576.0,
        out.display()
    );
    println!(
        "Serve it with: npx --yes http-server {} -p 8000",
        out.display()
    );
    Ok(())
}

/// The workspace's version, which `tauri.conf.json` and `package.json`
/// must share (`cargo xtask bump` keeps them so).
fn one_version(root: &Path) -> anyhow::Result<String> {
    let cargo = fs::read_to_string(root.join("Cargo.toml"))?
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|v| v.strip_suffix('"'))
        .map(str::to_string)
        .context("no version in Cargo.toml")?;
    let json_version = |file: &str| -> anyhow::Result<String> {
        let value: serde_json::Value = serde_json::from_str(&fs::read_to_string(root.join(file))?)?;
        value["version"]
            .as_str()
            .map(str::to_string)
            .with_context(|| format!("no version in {file}"))
    };
    let tauri = json_version("apps/desktop/src-tauri/tauri.conf.json")?;
    let npm = json_version("apps/desktop/package.json")?;
    if cargo != tauri || cargo != npm {
        bail!(
            "versions differ: Cargo.toml {cargo}, tauri.conf.json {tauri}, package.json {npm}; make them the same (cargo xtask bump)"
        );
    }
    Ok(cargo)
}

/// Tauri's release build and AppImage bundle, with this build's values set
/// for it alone (and any a terminal may carry removed).
fn tauri_build(root: &Path, build: &BTreeMap<&str, Option<String>>) -> anyhow::Result<()> {
    let release = deps::target_dir(root).join("release");
    // Nothing of an earlier bundle may be taken for this one's.
    let bundles = release.join("bundle/appimage");
    if bundles.exists() {
        fs::remove_dir_all(&bundles)?;
    }
    // The bundler (linuxdeploy) copies each binary's libraries into the
    // AppImage, but doesn't follow our `$ORIGIN` rpath to FFmpeg's.
    let library_path = match std::env::var_os("LD_LIBRARY_PATH") {
        Some(old) if !old.is_empty() => {
            let mut path = release.clone().into_os_string();
            path.push(":");
            path.push(old);
            path
        }
        _ => release.into_os_string(),
    };
    let mut command = Command::new("npx");
    command
        .current_dir(root.join("apps/desktop"))
        // The installer's settings live apart from tauri.conf.json, as on
        // Windows: its beforeBuildCommand builds couch-viewer (release) and
        // the front end first.
        .args([
            "tauri",
            "build",
            "--config",
            "src-tauri/tauri.appimage.conf.json",
        ])
        .env("LD_LIBRARY_PATH", library_path)
        // The release profile keeps line tables for latency profiles; in
        // the AppImage they would make the two binaries 300 MB.
        .env("CARGO_PROFILE_RELEASE_STRIP", "debuginfo");
    for (name, value) in build {
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
    let status = command.status().context("could not run npx tauri build")?;
    if !status.success() {
        bail!("tauri build failed with {status}");
    }
    Ok(())
}

/// Every file in the AppImage's folder, by its path there.
fn files_in(appdir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                walk(base, &entry.path(), out)?;
            } else {
                out.push(entry.path().strip_prefix(base)?.to_path_buf());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(appdir, appdir, &mut files)?;
    files.sort();
    Ok(files)
}

/// The notices inside the AppImage, unpacked into `scratch`.
fn carried_notices(appimage: &Path, scratch: &Path) -> anyhow::Result<Vec<u8>> {
    if scratch.exists() {
        fs::remove_dir_all(scratch)?;
    }
    fs::create_dir_all(scratch)?;
    let inside = format!("usr/bin/{}", notices::FILE_NAME);
    let status = Command::new(appimage)
        .current_dir(scratch)
        .args(["--appimage-extract", &inside])
        .stdout(std::process::Stdio::null())
        .status()
        .with_context(|| format!("could not run {}", appimage.display()))?;
    if !status.success() {
        bail!("could not unpack {inside} from the AppImage");
    }
    let notices = fs::read(scratch.join("squashfs-root").join(&inside))?;
    fs::remove_dir_all(scratch)?;
    Ok(notices)
}

fn workspace_repository(root: &Path) -> anyhow::Result<Option<String>> {
    let manifest = fs::read_to_string(root.join("Cargo.toml"))?;
    Ok(manifest.lines().find_map(|l| {
        l.strip_prefix("repository = \"")
            .and_then(|v| v.strip_suffix('"'))
            .map(str::to_string)
    }))
}

fn git(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .context("could not run git")?;
    if !output.status.success() {
        bail!(
            "git {} failed (the AppImage is built from a git clone): {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn options_refuse_what_contradicts() {
        assert_eq!(
            Options::parse(&args(&["--test-relay", "--no-invite-page"])).unwrap(),
            Options {
                test_relay: true,
                no_invite_page: true,
                ..Options::default()
            }
        );
        assert!(Options::parse(&args(&["--test-relay", "--config", "a.toml"])).is_err());
        assert!(
            Options::parse(&args(&[
                "--invite-page",
                "https://a/j/",
                "--no-invite-page"
            ]))
            .is_err()
        );
        assert!(Options::parse(&args(&["--config"])).is_err());
        assert!(Options::parse(&args(&["--relay"])).is_err());
    }

    #[test]
    fn the_release_config_reads_as_the_powershell_script_reads_it() {
        let config = read_release_config(
            "# a comment\n\
             relay_url = \"https://relay.example.com\"\n\
             relay_token = \"abc\"\n\
             \n\
             invite_page = \"\"\n\
             test = true\n",
        )
        .unwrap();
        assert_eq!(
            config,
            ReleaseConfig {
                relay_url: "https://relay.example.com".into(),
                relay_token: "abc".into(),
                invite_page: String::new(),
                test: true,
            }
        );
        // Quoted, `false` would read as true by mistake elsewhere.
        assert!(read_release_config("test = \"false\"").is_err());
        assert!(read_release_config("relay_url = https://a").is_err());
        assert!(read_release_config("relay = \"x\"").is_err());
        assert!(read_release_config("[section]").is_err());
        assert!(read_release_config("relay_token = \"a\"b\"").is_err());
        assert!(read_release_config("test = true\ntest = false").is_err());
        // Saved by Notepad, with its mark.
        assert!(read_release_config("\u{feff}test = true").unwrap().test);
    }

    #[test]
    fn the_token_never_prints() {
        let config = read_release_config("relay_token = \"s3cret\"").unwrap();
        assert!(!format!("{config:?}").contains("s3cret"));
        // A line that isn't `key = value` is named by its number alone.
        let error = read_release_config("# a\ns3cret").unwrap_err();
        assert!(!format!("{error:#}").contains("s3cret"));
        assert!(format!("{error:#}").contains("line 2"));
    }

    #[test]
    fn addresses_need_https_and_a_host() {
        assert!(check_address("x", "https://couchrelay.example.org", false).is_ok());
        assert!(check_address("x", "https://a.example:443/j/", false).is_ok());
        assert!(check_address("x", "http://192.168.1.10:3340", true).is_ok());
        assert!(check_address("x", "http://192.168.1.10:3340", false).is_err());
        assert!(check_address("x", "HTTPS://a.example", false).is_err());
        assert!(check_address("x", "https://", false).is_err());
        assert!(check_address("x", "https://:443", false).is_err());
        assert!(check_address("x", "https://user:pw@a.example", false).is_err());
        assert!(check_address("x", "ftp://a.example", true).is_err());
        // What the app's parser refuses, which would make a build without it.
        assert!(check_address("x", "https://relay.example:abc", false).is_err());
        assert!(check_address("x", "https://relay.example:99999", false).is_err());
        assert!(check_address("x", "https://a b.example", false).is_err());
        assert!(check_address("x", "https://[::1", false).is_err());
        assert!(check_address("x", "https://[::1]:3340", false).is_ok());
    }
}
