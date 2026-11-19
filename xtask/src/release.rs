//! `cargo xtask release-notes [--tag v<version>] --out <file>`: the release
//! workflow's first step (M26c, ADR 0014, `.github/workflows/release.yml`).
//! It refuses a tag that doesn't name the version every file carries, then
//! writes the draft release's notes: the wire revision (which the update
//! notice reads), the downloads, how to check them, and where the source
//! and the licences are. Without `--tag` (a run by hand, which makes no
//! release) it writes the notes for the version and commit checked out.

use std::{fs, path::Path};

use anyhow::{Context, bail};

use crate::appimage::{git, one_version, workspace_repository};

/// The first line of a release's notes, which the update notice reads
/// (`couch_proto::RELEASE_WIRE_MARKER`).
pub(crate) use couch_proto::RELEASE_WIRE_MARKER as WIRE_MARKER;

pub(crate) fn run(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let mut tag = None;
    let mut out = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--tag" => tag = Some(args.next().context("--tag takes v<version>")?.clone()),
            "--out" => out = Some(args.next().context("--out takes a file")?.clone()),
            other => bail!("unknown option {other}; the options: --tag v<version>, --out <file>"),
        }
    }
    let out = out.context("--out <file> is needed")?;
    let version = one_version(root)?;
    if let Some(tag) = &tag {
        check_tag(tag, &version)?;
    }
    let repository = workspace_repository(root)?
        .context("Cargo.toml's workspace has no repository: the notes link the source there")?;
    let commit = git(root, &["rev-parse", "HEAD"])?;
    let text = notes(&Facts {
        version: &version,
        tag: tag.as_deref(),
        commit: &commit,
        repository: &repository,
        wire: couch_proto::PROTOCOL_MINOR,
    });
    fs::write(&out, text).with_context(|| format!("writing {out}"))?;
    println!(
        "Notes for {version} (wire revision {}) in {out}",
        couch_proto::PROTOCOL_MINOR
    );
    Ok(())
}

/// A release's tag is `v` and the version, exactly: `v0.39.0`, never
/// `0.39.0` nor `v0.39.0-rc1` (`cargo xtask bump` makes no such version).
fn check_tag(tag: &str, version: &str) -> anyhow::Result<()> {
    if tag.strip_prefix('v') != Some(version) {
        bail!(
            "the tag {tag} doesn't name this version ({version}, in Cargo.toml, tauri.conf.json and package.json): tag v{version}, or bump first"
        );
    }
    Ok(())
}

struct Facts<'a> {
    version: &'a str,
    tag: Option<&'a str>,
    commit: &'a str,
    repository: &'a str,
    wire: u16,
}

/// What 1.0's notes say of profiles from earlier versions: the tidy pass
/// (0.42.1, `docs/tidy-results.md`) removed the code that read what they
/// wrote. From 1.0 on a profile survives an upgrade (the room-security-checks
/// skill), so no later release needs this.
const UPGRADING_TO_1_0: &str = "
### Coming from an earlier version

Your identity and settings carry over, but not everything versions before 1.0 saved:

- a room last saved by a version before 0.16 is set aside (as `*.room.old` in the profile's `rooms` folder): join it again with a new invite;
- a backup made by a version before 0.35 can't be restored: make a new one in Settings → Backup;
- a profile last opened by a version before 0.35 loses its relay choice (a relay's address, or iroh's public relays): set it again in Settings → Network.
";

fn notes(facts: &Facts<'_>) -> String {
    let Facts {
        version,
        tag,
        commit,
        repository,
        wire,
    } = facts;
    let upgrading = if *version == "1.0.0" {
        UPGRADING_TO_1_0
    } else {
        ""
    };
    let source = match tag {
        Some(tag) => format!("[{tag}]({repository}/tree/{tag}) (commit `{commit}`)"),
        None => format!("commit [`{commit}`]({repository}/tree/{commit})"),
    };
    format!(
        "<!-- {WIRE_MARKER}: {wire} -->
Couch Co-App {version}, built from {source} by the release workflow.

**Wire revision {wire}.** Members of a room play together only on builds of the same wire revision: the app says who needs to update.
{upgrading}
### Downloads

- **Windows** 10 and 11 (x64): `Couch-Co-App_{version}_x64-setup.exe`. It asks whether to install for you or for everyone, and offers the two controller drivers it uses after installing.
- **Linux** (x86-64, with glibc 2.39 or newer: Ubuntu 24.04, say): `Couch-Co-App_{version}_amd64.AppImage`. Make it executable (Properties, or `chmod +x`), then open it.

Neither is signed. To check a download against `SHA256SUMS`: on Windows, `Get-FileHash .\\Couch-Co-App_{version}_x64-setup.exe` in PowerShell; on Linux, `sha256sum --check --ignore-missing SHA256SUMS` in the download's folder.

### Source and licences

Couch Co-App is free software under the GNU GPL, version 3. Its source for exactly these packages: {source}.

The licences of everything each package carries are in `THIRD-PARTY-NOTICES-windows.txt` and `THIRD-PARTY-NOTICES-linux.txt` (also inside each package: Settings → About). FFmpeg, which both include, is under the LGPL: its source and the scripts that built it are the two `.tar.gz` files here, explained in `SOURCES-windows.txt` and `SOURCES-linux.txt`. The AppImage also carries system libraries from Ubuntu 24.04, whose source packages its notices name: their sources, at exactly those versions, are `Couch-Co-App_{version}_amd64-system-sources.tar`.
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_names_the_version_exactly() {
        assert!(check_tag("v0.39.0", "0.39.0").is_ok());
        assert!(check_tag("0.39.0", "0.39.0").is_err());
        assert!(check_tag("v0.39.1", "0.39.0").is_err());
        assert!(check_tag("v0.39.0-rc1", "0.39.0").is_err());
        assert!(check_tag("V0.39.0", "0.39.0").is_err());
    }

    #[test]
    fn the_notes_carry_the_wire_revision_and_the_source() {
        let text = notes(&Facts {
            version: "0.39.0",
            tag: Some("v0.39.0"),
            commit: "0123456789abcdef",
            repository: "https://github.com/a/b",
            wire: 21,
        });
        assert!(text.starts_with("<!-- couch-wire-revision: 21 -->\n"));
        assert!(text.contains("https://github.com/a/b/tree/v0.39.0"));
        assert!(text.contains("Couch-Co-App_0.39.0_x64-setup.exe"));
        assert!(text.contains("Couch-Co-App_0.39.0_amd64.AppImage"));
        // A run by hand has no tag: the commit stands in.
        let untagged = notes(&Facts {
            tag: None,
            version: "0.39.0",
            commit: "0123456789abcdef",
            repository: "https://github.com/a/b",
            wire: 21,
        });
        assert!(untagged.contains("https://github.com/a/b/tree/0123456789abcdef"));
        assert!(!text.contains("Coming from an earlier version"));
    }

    #[test]
    fn only_1_0_says_what_earlier_profiles_lose() {
        let text = notes(&Facts {
            version: "1.0.0",
            tag: Some("v1.0.0"),
            commit: "0123456789abcdef",
            repository: "https://github.com/a/b",
            wire: 22,
        });
        assert!(text.contains("### Coming from an earlier version"));
        assert!(text.contains("\n\n### Downloads"));
        let later = notes(&Facts {
            version: "1.0.1",
            tag: Some("v1.0.1"),
            commit: "0123456789abcdef",
            repository: "https://github.com/a/b",
            wire: 22,
        });
        assert!(!later.contains("Coming from an earlier version"));
    }
}
