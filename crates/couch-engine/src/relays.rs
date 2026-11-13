//! The relays a profile holds (M26a, [ADR 0014]): each an address and the
//! token it asks for, all used at once, each token sent to its own relay
//! alone. Kept sealed in the profile (`relays.sealed`, like the room files),
//! never logged; iroh's public relays are used while nothing is registered,
//! or beside our relays when the user keeps them on.
//!
//! [ADR 0014]: ../../../docs/adr/0014-public-builds-and-registered-relays.md

use couch_net::{
    RelayUrl,
    endpoint::{Relay, RelayChoice, RelayToken},
};
use serde::{Deserialize, Serialize};

pub mod code;

use crate::{EngineError, build_config::BuiltInRelay, store::LegacyRelaySetting};

/// At most this many relays held at once: iroh probes each one for its
/// latency.
pub const MAX_RELAYS: usize = 16;

/// What a build allows of relays, and where they come from.
#[derive(Debug, Clone)]
pub struct RelayRules {
    /// The relay the build was made with (`release.toml`): written into the
    /// profile at start and never removed.
    pub built_in: Option<BuiltInRelay>,
    /// Plain http relays at private (LAN) addresses are allowed: a
    /// development or test build.
    pub test_build: bool,
    /// iroh's public relays are offered at all. Off in tests, which must
    /// run without the internet.
    pub public_relays: bool,
}

impl RelayRules {
    /// This build's.
    pub fn of_this_build() -> Self {
        Self {
            built_in: crate::build_config::relay(),
            test_build: crate::build_config::test_build(),
            public_relays: true,
        }
    }
}

/// One relay the profile holds.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedRelay {
    /// As iroh writes it (`https://relay.example.com/`).
    pub url: String,
    pub token: Option<String>,
    /// This build's own relay: it can't be removed.
    #[serde(default)]
    pub built_in: bool,
    /// The token is the build's, so a newer build's replaces it; a code the
    /// user pasted for the same relay stays.
    #[serde(default)]
    pub token_from_build: bool,
    /// From a personal code: never passed on (M26b).
    #[serde(default)]
    pub personal: bool,
    /// When it was added, in seconds since 1970 (UTC).
    #[serde(default)]
    pub added: u64,
}

impl std::fmt::Debug for SavedRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SavedRelay")
            .field("url", &self.url)
            .field("token", &self.token.as_ref().map(|_| ".."))
            .field("built_in", &self.built_in)
            .field("token_from_build", &self.token_from_build)
            .field("personal", &self.personal)
            .finish()
    }
}

/// `relays.sealed`: JSON inside the seal, so later fields come with
/// defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedRelays {
    #[serde(default)]
    pub relays: Vec<SavedRelay>,
    /// iroh's public relays stay on beside ours (Settings → Network).
    #[serde(default)]
    pub keep_public: bool,
    /// iroh's public relays come on by themselves while none of ours works
    /// (refusing our code, or not answering), and go again once one does.
    /// Off by default: our traffic goes through relays we chose.
    #[serde(default)]
    pub public_fallback: bool,
    /// Whether the app asks for a relay of the user's own while only
    /// iroh's public relays are in use (the wizard, on creating a room or
    /// an invite).
    #[serde(default)]
    pub ask: AskForRelay,
}

/// The wizard's answer: ask now, later, or never again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum AskForRelay {
    #[default]
    Now,
    /// "Remind me later": until then (seconds since 1970, UTC), or a
    /// version other than this one, whichever comes first.
    Later { until: u64, version: String },
    /// "Stay on the public relays": Settings still offers adding one.
    Never,
}

/// "Remind me later" puts the question off this long.
pub const ASK_LATER_SECS: u64 = 7 * 24 * 60 * 60;

impl AskForRelay {
    /// Whether to ask now, `version` being this app's.
    pub fn due(&self, now_unix: u64, version: &str) -> bool {
        match self {
            Self::Now => true,
            Self::Later {
                until,
                version: then,
            } => now_unix >= *until || then != version,
            Self::Never => false,
        }
    }
}

/// What [`SavedRelays::add`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Added {
    New,
    /// The relay was there: it has the new token now.
    Replaced,
}

impl SavedRelays {
    /// The relay setting of before M26a (`network.json`), as relays: a
    /// custom address becomes a relay without a token, `public` keeps
    /// iroh's on. The build's own relay is added by [`Self::keep_built_in`].
    pub fn carried_over(old: &LegacyRelaySetting, rules: &RelayRules, now_unix: u64) -> Self {
        let mut saved = Self::default();
        match old {
            LegacyRelaySetting::Builtin => {}
            LegacyRelaySetting::Public => saved.keep_public = true,
            LegacyRelaySetting::Custom(url) => match check_url(url, rules.test_build) {
                Ok(url) => {
                    saved.relays.push(SavedRelay {
                        url: url.to_string(),
                        token: None,
                        built_in: false,
                        token_from_build: false,
                        personal: false,
                        added: now_unix,
                    });
                }
                Err(e) => tracing::warn!("the relay setting saved before ({url:?}): {e}; left out"),
            },
        }
        saved
    }

    /// Writes the build's own relay in (first), or gives it the build's
    /// token: kept by a later build without it, as a relay like any other.
    /// Whether anything changed.
    pub fn keep_built_in(&mut self, built_in: Option<&BuiltInRelay>, now_unix: u64) -> bool {
        let before = self.clone();
        let url = built_in.map(|b| b.url.to_string());
        for relay in &mut self.relays {
            if Some(&relay.url) != url.as_ref() && relay.built_in {
                relay.built_in = false;
                relay.token_from_build = false;
            }
        }
        if let Some(built_in) = built_in {
            let token = built_in.token.as_ref().map(|t| t.reveal().to_string());
            match self
                .relays
                .iter_mut()
                .find(|r| Some(&r.url) == url.as_ref())
            {
                Some(relay) => {
                    relay.built_in = true;
                    if relay.token_from_build || relay.token.is_none() {
                        relay.token_from_build = token.is_some();
                        relay.token = token;
                    }
                }
                None => self.relays.insert(
                    0,
                    SavedRelay {
                        url: built_in.url.to_string(),
                        token_from_build: token.is_some(),
                        token,
                        built_in: true,
                        personal: false,
                        added: now_unix,
                    },
                ),
            }
        }
        *self != before
    }

    /// What the endpoint holds: our relays, and iroh's public ones while
    /// nothing is registered or the user keeps them. A saved address this
    /// build doesn't take (a LAN relay a test build saved) is left out.
    pub fn choice(&self, rules: &RelayRules) -> RelayChoice {
        let ours: Vec<Relay> = self
            .relays
            .iter()
            .filter_map(|saved| {
                let url = if saved.built_in {
                    saved.url.parse().ok()
                } else {
                    check_url(&saved.url, rules.test_build)
                        .inspect_err(|e| {
                            tracing::warn!("relay {}: {e}; left out", shown_host(&saved.url))
                        })
                        .ok()
                }?;
                Some(Relay::new(
                    url,
                    saved.token.clone().and_then(RelayToken::new),
                ))
            })
            .collect();
        let public = rules.public_relays && (self.keep_public || ours.is_empty());
        RelayChoice::Relays { ours, public }
    }

    /// Adds a relay the user gave (typed, or from a code), or gives one
    /// already held a new token.
    pub fn add(
        &mut self,
        url: RelayUrl,
        token: Option<RelayToken>,
        personal: bool,
        now_unix: u64,
    ) -> Result<Added, EngineError> {
        let token = token.map(|t| t.reveal().to_string());
        if let Some(relay) = self.relays.iter_mut().find(|r| r.url == url.as_str()) {
            relay.token = token;
            relay.token_from_build = false;
            relay.personal = personal;
            return Ok(Added::Replaced);
        }
        if self.relays.len() >= MAX_RELAYS {
            return Err(EngineError::Refused(
                "you have 16 relays, the most the app holds: remove one first",
            ));
        }
        self.relays.push(SavedRelay {
            url: url.to_string(),
            token,
            built_in: false,
            token_from_build: false,
            personal,
            added: now_unix,
        });
        Ok(Added::New)
    }

    /// Takes a relay away; the build's own stays.
    pub fn remove(&mut self, url: &RelayUrl) -> Result<(), EngineError> {
        let Some(at) = self.relays.iter().position(|r| r.url == url.as_str()) else {
            return Err(EngineError::Refused("that relay isn't in your list"));
        };
        if self.relays[at].built_in {
            return Err(EngineError::Refused(
                "this app's own relay can't be removed",
            ));
        }
        self.relays.remove(at);
        Ok(())
    }
}

/// A saved relay's server, for the log: never the whole address, which
/// could carry a user name and password if it came from elsewhere.
fn shown_host(url: &str) -> String {
    url.parse::<RelayUrl>()
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "(unreadable address)".into())
}

/// A relay address the user gives: https to a server; plain http only on
/// this PC, or at a private (LAN) address in a test build; never with a user
/// name or password (addresses are shown and logged).
pub fn check_url(url: &str, test_build: bool) -> Result<RelayUrl, EngineError> {
    let invalid = |why: &str| EngineError::InvalidRelay(why.to_string());
    let parsed: RelayUrl = url
        .trim()
        .parse()
        .map_err(|_| invalid("that isn't a web address"))?;
    let host = parsed.host_str().unwrap_or_default();
    let local = matches!(host, "localhost" | "127.0.0.1");
    let lan = host
        .parse::<std::net::Ipv4Addr>()
        .is_ok_and(|ip| ip.is_private());
    match parsed.scheme() {
        "https" => {}
        "http" if local || (lan && test_build) => {}
        _ => return Err(invalid("it must start with https://")),
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(invalid("it names no server"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid("it can't carry a user name or password"));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn rules(built_in: Option<BuiltInRelay>, test_build: bool) -> RelayRules {
        RelayRules {
            built_in,
            test_build,
            public_relays: true,
        }
    }

    fn ours(token: &str) -> BuiltInRelay {
        BuiltInRelay {
            url: "https://relay.couch.example".parse().unwrap(),
            token: RelayToken::new(token),
        }
    }

    fn url(text: &str) -> RelayUrl {
        text.parse().unwrap()
    }

    fn held(choice: &RelayChoice) -> (Vec<(String, Option<String>)>, bool) {
        let RelayChoice::Relays { ours, public } = choice else {
            panic!("relays");
        };
        let ours = ours
            .iter()
            .map(|r| {
                (
                    r.url.to_string(),
                    r.token.as_ref().map(|t| t.reveal().to_string()),
                )
            })
            .collect();
        (ours, *public)
    }

    #[test]
    fn nothing_registered_means_iroh_s_public_relays() {
        let saved = SavedRelays::default();
        assert_eq!(held(&saved.choice(&rules(None, true))), (vec![], true));
        // Tests offer none.
        let tests = RelayRules {
            public_relays: false,
            ..rules(None, true)
        };
        assert_eq!(held(&saved.choice(&tests)), (vec![], false));
    }

    #[test]
    fn the_setting_of_before_carries_over() {
        let release = rules(Some(ours("t")), false);
        let mut built = SavedRelays::carried_over(&LegacyRelaySetting::Builtin, &release, NOW);
        assert!(built.keep_built_in(release.built_in.as_ref(), NOW));
        assert_eq!(
            held(&built.choice(&release)),
            (
                vec![("https://relay.couch.example/".into(), Some("t".into()))],
                false
            )
        );

        let mut public = SavedRelays::carried_over(&LegacyRelaySetting::Public, &release, NOW);
        public.keep_built_in(release.built_in.as_ref(), NOW);
        assert!(held(&public.choice(&release)).1, "the public ones kept");
        assert_eq!(held(&public.choice(&release)).0.len(), 1);

        // A custom address: a relay without a token, beside the build's.
        let custom = LegacyRelaySetting::Custom(" https://mine.example ".into());
        let mut mine = SavedRelays::carried_over(&custom, &release, NOW);
        mine.keep_built_in(release.built_in.as_ref(), NOW);
        let (relays, public) = held(&mine.choice(&release));
        assert!(!public);
        assert_eq!(
            relays,
            vec![
                ("https://relay.couch.example/".into(), Some("t".into())),
                ("https://mine.example/".into(), None),
            ]
        );

        // One this build doesn't take is left out.
        let lan = LegacyRelaySetting::Custom("http://192.168.1.10:3340".into());
        assert!(
            SavedRelays::carried_over(&lan, &release, NOW)
                .relays
                .is_empty()
        );
        let dev = rules(None, true);
        assert_eq!(SavedRelays::carried_over(&lan, &dev, NOW).relays.len(), 1);
    }

    #[test]
    fn the_build_s_own_relay_stays_and_follows_the_build() {
        let mut saved = SavedRelays::default();
        assert!(saved.keep_built_in(Some(&ours("t1")), NOW));
        assert!(!saved.keep_built_in(Some(&ours("t1")), NOW), "nothing new");
        assert!(saved.remove(&url("https://relay.couch.example")).is_err());

        // A newer build with a rotated token.
        assert!(saved.keep_built_in(Some(&ours("t2")), NOW));
        assert_eq!(saved.relays[0].token.as_deref(), Some("t2"));

        // The user pasted a newer code for it: a build's older one doesn't
        // replace it.
        saved
            .add(
                url("https://relay.couch.example"),
                RelayToken::new("t3"),
                false,
                NOW,
            )
            .unwrap();
        saved.keep_built_in(Some(&ours("t2")), NOW);
        assert_eq!(saved.relays[0].token.as_deref(), Some("t3"));
        assert!(saved.relays[0].built_in);

        // A build without it (a public build over a friends' one) keeps
        // it, removable now.
        assert!(saved.keep_built_in(None, NOW));
        assert!(!saved.relays[0].built_in);
        assert_eq!(saved.relays[0].token.as_deref(), Some("t3"));
        saved.remove(&url("https://relay.couch.example")).unwrap();
        assert!(saved.relays.is_empty());
    }

    #[test]
    fn the_build_s_token_fills_its_relay_typed_without_one() {
        let release = rules(Some(ours("t")), false);
        let custom = LegacyRelaySetting::Custom("https://relay.couch.example".into());
        let mut saved = SavedRelays::carried_over(&custom, &release, NOW);
        saved.keep_built_in(release.built_in.as_ref(), NOW);
        assert_eq!(saved.relays.len(), 1);
        assert_eq!(saved.relays[0].token.as_deref(), Some("t"));
        assert!(saved.relays[0].built_in && saved.relays[0].token_from_build);
    }

    #[test]
    fn relays_are_added_replaced_capped_and_removed() {
        let dev = rules(None, true);
        let mut saved = SavedRelays::default();
        let a = url("https://a.example");
        assert_eq!(
            saved
                .add(a.clone(), RelayToken::new("x"), false, NOW)
                .unwrap(),
            Added::New
        );
        assert_eq!(
            saved
                .add(a.clone(), RelayToken::new("y"), true, NOW)
                .unwrap(),
            Added::Replaced
        );
        assert_eq!(saved.relays[0].token.as_deref(), Some("y"));
        assert!(saved.relays[0].personal);
        // Registered: iroh's public relays are off unless kept.
        assert!(!held(&saved.choice(&dev)).1);
        saved.keep_public = true;
        assert!(held(&saved.choice(&dev)).1);

        for i in 1..MAX_RELAYS {
            saved
                .add(url(&format!("https://r{i}.example")), None, false, NOW)
                .unwrap();
        }
        assert!(
            saved
                .add(url("https://one-more.example"), None, false, NOW)
                .is_err()
        );
        // Replacing one still works at the cap.
        assert!(saved.add(a.clone(), None, false, NOW).is_ok());

        saved.remove(&a).unwrap();
        assert!(saved.remove(&a).is_err());
        assert_eq!(saved.relays.len(), MAX_RELAYS - 1);
    }

    #[test]
    fn plain_http_relays_on_the_lan_only_in_test_builds() {
        let at = |url: &str, test_build| check_url(url, test_build).is_ok();
        for lan in [
            "http://192.168.1.10:3340",
            "http://10.0.0.5:3340",
            "http://172.20.1.1:3340",
        ] {
            assert!(at(lan, true), "{lan}");
            assert!(!at(lan, false), "{lan}");
        }
        for anywhere in ["http://localhost:3340", "http://127.0.0.1:3340"] {
            assert!(at(anywhere, false), "{anywhere}");
        }
        for never in [
            "http://8.8.8.8:3340",
            "http://172.32.0.1:3340",
            "http://relay.example.com",
            "relay.example.com",
            "ftp://relay.example.com",
            "https://",
            "https://user:token@relay.example.com",
        ] {
            assert!(!at(never, true), "{never}");
        }
        assert_eq!(
            check_url(" https://relay.example.com ", false)
                .unwrap()
                .to_string(),
            "https://relay.example.com/"
        );
    }

    #[test]
    fn a_token_never_prints() {
        let mut saved = SavedRelays::default();
        saved
            .add(
                url("https://a.example"),
                RelayToken::new("s3cret"),
                false,
                NOW,
            )
            .unwrap();
        assert!(!format!("{saved:?}").contains("s3cret"));
    }
}
