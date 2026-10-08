//! Values a release build is made with, from the private `release.toml`
//! that `scripts/build-installer.ps1` reads (outside the repo; see
//! `deploy/release.example.toml`). Development builds get none of them and
//! use iroh's public relays. Cargo rebuilds when one changes, since rustc
//! records the variables `option_env!` reads.

use couch_net::{RelayUrl, endpoint::RelayToken};

/// Our relay, with the token it asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltInRelay {
    pub url: RelayUrl,
    pub token: Option<RelayToken>,
}

/// The relay this build was made with, if any. A value that doesn't parse
/// counts as none: the build script checks it first, and
/// [`unreadable_relay`] lets the app say so.
pub fn relay() -> Option<BuiltInRelay> {
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
    option_env!("COUCH_BUILD_RELAY_URL").filter(|_| relay().is_none())
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
