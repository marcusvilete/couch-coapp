//! Values a release build is made with, from the private `release.toml`
//! that `scripts/build-installer.ps1` reads (outside the repo; see
//! `deploy/release.example.toml`). Development builds get none of them and
//! use iroh's public relays. Cargo rebuilds when one changes, since rustc
//! records the variables `option    option_env!("COUCH_BUILD_RELAY_URL").filter(|_| !public_build() && relay().is_none())env!` reads.

use couch_net::{RelayUrl, endpoint::RelayToken};

/// Our relay, with the token it asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltInRelay {
    pub url: RelayUrl,
    pub token: Option<RelayToken>,
}

/// The relay this build was made with, if any. A value that doesn't parse
/// counts as none: the build script checks it first, and
/// [`unreadable_relay`] lets the app say so. A public build has none,
/// whatever its environment held: it carries no secret.
pub fn relay() -> Option<BuiltInRelay> {
    if public_build() {
        return None;
    }
    let url = option_env!("COUCH_BUILD_RELAY_URL")?.trim();
    let url = url.parse().ok()?;
    Some(BuiltInRelay {
        url,
        token: option_env!("COUCH_BUILD_RELAY_TOKEN").and_then(RelayToken::new),
    })
}

/// The build was given a relay URL it can't read: it runs as a build
/// without one, which the app logs as an error at start.
pub fn unreadable_relay() -> Option<&'static str> {
    option_env!("COUCH_BUILD_RELAY_URL").filter(|_| !public_build() && relay().is_none())
}

/// Where invites point when shared as `https` links (the static page in
/// `deploy/invite-page`), or `None` for `couchco://` links.
pub fn invite_page() -> Option<&'static str> {
    option_env!("COUCH_BUILD_INVITE_PAGE")
        .map(str::trim)
        .filter(|page| {
            page.starts_with("https://") || (test_build() && page.starts_with("http://"))
        })
}

/// The commit an installer was built from (`build-installer.ps1`, ending in
/// `-dirty` for a test build with uncommitted changes); `None` in
/// development builds.
pub fn commit() -> Option<&'static str> {
    option_env!("COUCH_BUILD_COMMIT")
        .map(str::trim)
        .filter(|c| !c.is_empty())
}

/// A development build, or an installer built for testing
/// (`build-installer.ps1 -TestRelay`, or a `release.toml` with `test`):
/// plain http relays on the LAN are allowed.
pub fn test_build() -> bool {
    cfg!(debug_assertions) || option_env!("COUCH_BUILD_TEST").is_some_and(|v| v == "1")
}

/// A public build (M26c, ADR 0014: `build-installer.ps1 -Public`,
/// `build-appimage.sh --public`, made by the release workflow on a tag):
/// no relay and no token, on iroh's public relays until the user registers
/// one; never a test build.
pub fn public_build() -> bool {
    option_env!("COUCH_BUILD_PUBLIC").is_some_and(|v| v == "1") && !test_build()
}

/// The GitHub repository whose releases the update notice checks, as
/// `owner/name`: a public build's, or a friends' build's with `releases` in
/// `release.toml`. Never in a development or test build.
pub fn releases() -> Option<&'static str> {
    option_env!("COUCH_BUILD_RELEASES")
        .map(str::trim)
        .filter(|repo| !test_build() && is_repository(repo))
}

/// What the build kind is called in Copy diagnostics.
pub fn kind() -> &'static str {
    if cfg!(debug_assertions) {
        "development build"
    } else if test_build() {
        "test build"
    } else if public_build() {
        "public build"
    } else {
        "release build"
    }
}

/// `owner/name` as GitHub allows them: an owner of letters, digits and
/// inner hyphens, a name of letters, digits, `.`, `-` and `_` (not `.` or
/// `..`). The build scripts check the same.
fn is_repository(text: &str) -> bool {
    let Some((owner, name)) = text.split_once('/') else {
        return false;
    };
    let owner_ok = !owner.is_empty()
        && owner.len() <= 39
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let name_ok = !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    owner_ok && name_ok
}

#[cfg(test)]
mod tests {
    use super::is_repository;

    #[test]
    fn a_releases_repository_is_owner_and_name() {
        for good in ["marcusvilete/couch-coapp", "a/b", "my-org/app.v2_x"] {
            assert!(is_repository(good), "{good}");
        }
        for bad in [
            "",
            "couch-coapp",
            "a/b/c",
            "/b",
            "a/",
            "-a/b",
            "a-/b",
            "a/..",
            "a/.",
            "a b/c",
            "https://github.com/a/b",
            "a/b?x",
        ] {
            assert!(!is_repository(bad), "{bad}");
        }
    }
}
