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
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The user's switch "Pass this relay on to my rooms" is off (M26b).
    #[serde(default)]
    pub kept_to_me: bool,
    /// Passed on to us by members of our rooms (M26b), rather than
    /// registered by the user: for those rooms alone.
    #[serde(default)]
    pub received: Option<Received>,
    /// Our code for it was refused, and a member of a room gave us the one
    /// it holds now (M26b): who, by name, to show.
    #[serde(default)]
    pub renewed_by: Option<String>,
    /// Kept without a check letting us in (the relay didn't answer): tried
    /// again later.
    #[serde(default)]
    pub unchecked: bool,
}

/// Where a received relay came from (M26b).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Received {
    pub rooms: Vec<ReceivedFor>,
}

/// One room a relay was received for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReceivedFor {
    /// The room's id, as its file is named.
    pub room: String,
    /// Who gave it: their member id and their name then.
    pub from: String,
    pub from_name: String,
    /// When a member of that room last needed it (its record named it), in
    /// seconds since 1970 (UTC): unneeded for [`FORGET_RECEIVED_SECS`], it
    /// goes.
    pub needed: u64,
}

/// A relay received for rooms that none of them has needed for this long is
/// forgotten.
pub const FORGET_RECEIVED_SECS: u64 = 30 * 24 * 60 * 60;

/// Relays received from rooms, at most: a member running several relays
/// can't fill the 16 and keep the user's own out.
pub const MAX_RECEIVED: usize = 8;

/// Relays the user removed that rooms passed on, or would: never asked for
/// again until added by hand. At most this many remembered.
pub const MAX_DONT_ASK: usize = 64;

impl SavedRelay {
    /// To which rooms its code may go (M26b): a relay with a token, not from
    /// a personal code, not kept to the user; registered by the user (the
    /// build's own included) to every room, received only to the rooms it
    /// came for.
    pub fn passable(&self) -> crate::network::Passable {
        use crate::network::Passable;
        if self.token.is_none() || self.personal || self.kept_to_me {
            return Passable::Never;
        }
        match &self.received {
            None => Passable::Everywhere,
            Some(r) => Passable::Rooms(r.rooms.iter().map(|f| f.room.clone()).collect()),
        }
    }
}

impl std::fmt::Debug for SavedRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SavedRelay")
            .field("url", &self.url)
            .field("token", &self.token.as_ref().map(|_| ".."))
            .field("built_in", &self.built_in)
            .field("token_from_build", &self.token_from_build)
            .field("personal", &self.personal)
            .field("kept_to_me", &self.kept_to_me)
            .field("received", &self.received)
            .field("unchecked", &self.unchecked)
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
    /// "Use relays my rooms pass on" is off (M26b): nothing is asked for
    /// or kept.
    #[serde(default)]
    pub refuse_passed_on: bool,
    /// Relays the user removed: never asked for again until added by hand.
    #[serde(default)]
    pub dont_ask: Vec<String>,
}

/// What [`SavedRelays::receive`] did with a code a room passed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Receipt {
    /// A relay new to us, for that room.
    New,
    /// A relay received before, for that room too now, or with the code
    /// it holds now.
    Joined,
    /// One of the user's own, refusing the code we held: it has the new
    /// one, and stays theirs.
    Renewed,
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
                        ..Default::default()
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
                    relay.received = None;
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
                        ..Default::default()
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
        // Relays rooms passed on don't count: they're for those rooms alone,
        // so a user who holds nothing of their own stays on the public ones.
        let own = self.relays.iter().any(|r| r.received.is_none());
        let public = rules.public_relays && (self.keep_public || !own);
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
        // Added by hand: asked for again if it goes.
        self.dont_ask.retain(|u| u != url.as_str());
        if let Some(relay) = self.relays.iter_mut().find(|r| r.url == url.as_str()) {
            relay.token = token;
            relay.token_from_build = false;
            relay.personal = personal;
            // Given by the user: theirs now, for every room.
            relay.received = None;
            relay.renewed_by = None;
            relay.unchecked = false;
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
            ..Default::default()
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
        // Rooms would pass it on again: never asked for until added by
        // hand.
        self.dont_ask.retain(|u| u != url.as_str());
        self.dont_ask.push(url.to_string());
        if self.dont_ask.len() > MAX_DONT_ASK {
            self.dont_ask.remove(0);
        }
        Ok(())
    }

    /// Whether a code for `url` may be asked for and kept (M26b).
    pub fn may_ask(&self, url: &str) -> bool {
        !self.refuse_passed_on && !self.dont_ask.iter().any(|u| u == url)
    }

    /// Keeps a code a member of `room` passed on (M26b), checked or not
    /// (`checked`: it let us in; else the relay didn't answer). A relay of
    /// the user's own takes it only when checked and not from a personal
    /// code (a personal code taken back was taken back on purpose), and
    /// stays theirs.
    #[allow(clippy::too_many_arguments)]
    pub fn receive(
        &mut self,
        url: &RelayUrl,
        token: &RelayToken,
        room: &str,
        from: &str,
        from_name: &str,
        checked: bool,
        now_unix: u64,
    ) -> Result<Receipt, EngineError> {
        if !self.may_ask(url.as_str()) {
            return Err(EngineError::Refused(
                "relays from rooms aren't taken here, or not this one",
            ));
        }
        let token = token.reveal().to_string();
        let source = || ReceivedFor {
            room: room.to_string(),
            from: from.to_string(),
            from_name: from_name.to_string(),
            needed: now_unix,
        };
        if let Some(relay) = self.relays.iter_mut().find(|r| r.url == url.as_str()) {
            return match &mut relay.received {
                None if relay.personal => Err(EngineError::Refused(
                    "a relay from a personal code takes no code from rooms",
                )),
                None if !checked => Err(EngineError::Refused(
                    "a relay of your own takes a code only once it lets us in",
                )),
                None => {
                    relay.token = Some(token);
                    relay.token_from_build = false;
                    relay.renewed_by = Some(from_name.to_string());
                    relay.unchecked = false;
                    Ok(Receipt::Renewed)
                }
                Some(received) => {
                    let new_code = relay.token.as_deref() != Some(token.as_str());
                    // A code that didn't let us in never replaces one held:
                    // a member could plant one that can never be checked.
                    if new_code && !checked {
                        return Err(EngineError::Refused(
                            "a code that didn't let us in never replaces one held",
                        ));
                    }
                    match received.rooms.iter_mut().find(|f| f.room == room) {
                        Some(known) => {
                            known.needed = now_unix;
                            // Whoever gave the code held now, to show.
                            if new_code {
                                known.from = from.to_string();
                                known.from_name = from_name.to_string();
                            }
                        }
                        None => received.rooms.push(source()),
                    }
                    if checked {
                        relay.unchecked = false;
                    }
                    relay.token = Some(token);
                    Ok(Receipt::Joined)
                }
            };
        }
        if self.relays.len() >= MAX_RELAYS {
            return Err(EngineError::Refused(
                "the app holds 16 relays, its most: remove one to take this one",
            ));
        }
        // Rooms can't fill the list: the user's own always fit.
        if self.relays.iter().filter(|r| r.received.is_some()).count() >= MAX_RECEIVED {
            return Err(EngineError::Refused(
                "your rooms passed on 8 relays already, the most kept: remove one to take this one",
            ));
        }
        self.relays.push(SavedRelay {
            url: url.to_string(),
            token: Some(token),
            added: now_unix,
            received: Some(Received {
                rooms: vec![source()],
            }),
            unchecked: !checked,
            ..Default::default()
        });
        Ok(Receipt::New)
    }

    /// We left `room`, or forgot it: the relays received for it alone go.
    /// Whether anything changed.
    pub fn drop_room(&mut self, room: &str) -> bool {
        let before = self.relays.len();
        let mut changed = false;
        for relay in &mut self.relays {
            if let Some(received) = &mut relay.received {
                let had = received.rooms.len();
                received.rooms.retain(|f| f.room != room);
                changed |= received.rooms.len() != had;
            }
        }
        self.relays
            .retain(|r| r.received.as_ref().is_none_or(|x| !x.rooms.is_empty()));
        changed || self.relays.len() != before
    }

    /// Members of these rooms need these relays now (their records name
    /// them): received ones are kept another 30 days for them; then
    /// whatever no room has needed for 30 days goes, and whatever was
    /// received for a room no longer `live` (a code checked after we left,
    /// say). Whether anything changed worth saving (a day's difference or
    /// more, or a relay gone).
    pub fn needed(&mut self, needed: &[(String, String)], live: &[String], now_unix: u64) -> bool {
        const DAY: u64 = 24 * 60 * 60;
        let mut changed = false;
        for relay in &mut self.relays {
            let Some(received) = &mut relay.received else {
                continue;
            };
            for f in &mut received.rooms {
                if needed.iter().any(|(r, u)| *r == f.room && *u == relay.url)
                    && now_unix.saturating_sub(f.needed) >= DAY
                {
                    f.needed = now_unix;
                    changed = true;
                }
            }
            let had = received.rooms.len();
            received.rooms.retain(|f| {
                now_unix.saturating_sub(f.needed) < FORGET_RECEIVED_SECS
                    // Kept since `live` was taken (a join just admitted)
                    // isn't the old snapshot's to drop.
                    && (live.contains(&f.room) || f.needed >= now_unix)
            });
            changed |= received.rooms.len() != had;
        }
        self.relays
            .retain(|r| r.received.as_ref().is_none_or(|x| !x.rooms.is_empty()));
        changed
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

    fn token(text: &str) -> RelayToken {
        RelayToken::new(text).unwrap()
    }

    /// M26b: a code a room passes on is kept for that room alone, joins
    /// others as they pass it too, and goes with the last of them.
    #[test]
    fn a_received_relay_is_for_its_rooms_alone() {
        use crate::network::Passable;
        let mut saved = SavedRelays::default();
        let r = url("https://r.example");
        assert_eq!(
            saved
                .receive(&r, &token("x"), "room1", "bia-id", "Bia", true, NOW)
                .unwrap(),
            Receipt::New
        );
        let relay = &saved.relays[0];
        assert_eq!(relay.passable(), Passable::Rooms(vec!["room1".into()]));
        assert!(relay.passable().allows("room1") && !relay.passable().allows("room2"));
        assert!(!relay.unchecked);

        // A code that didn't let us in never replaces one held (the
        // review's finding: one that can never be checked was planted).
        assert!(
            saved
                .receive(&r, &token("planted"), "room1", "eve-id", "Eve", false, NOW)
                .is_err()
        );
        assert_eq!(saved.relays[0].token.as_deref(), Some("x"));
        // Another room passes it too, with a newer code that let us in.
        assert_eq!(
            saved
                .receive(&r, &token("y"), "room2", "cai-id", "Cai", true, NOW)
                .unwrap(),
            Receipt::Joined
        );
        assert_eq!(saved.relays[0].token.as_deref(), Some("y"));
        assert_eq!(saved.relays[0].received.as_ref().unwrap().rooms.len(), 2);
        // A new code for room1 credits its giver there.
        saved
            .receive(&r, &token("z"), "room1", "dee-id", "Dee", true, NOW)
            .unwrap();
        assert_eq!(
            saved.relays[0].received.as_ref().unwrap().rooms[0].from_name,
            "Dee"
        );

        // Relays rooms passed on aren't the user's own: the public relays
        // stay on beside them.
        let rules = rules(None, true);
        assert!(held(&saved.choice(&rules)).1);

        // Kept to the user, it goes nowhere.
        saved.relays[0].kept_to_me = true;
        assert_eq!(saved.relays[0].passable(), Passable::Never);
        saved.relays[0].kept_to_me = false;

        assert!(saved.drop_room("room1"));
        assert_eq!(saved.relays.len(), 1);
        assert!(saved.drop_room("room2"));
        assert!(saved.relays.is_empty(), "gone with its last room");
        assert!(!saved.drop_room("room2"));
    }

    /// M26b: the user's own relay takes a room's code only once it lets us
    /// in, and stays theirs; one from a personal code never does.
    #[test]
    fn the_user_s_own_relay_is_renewed_only_when_checked_and_never_personal() {
        use crate::network::Passable;
        let mut saved = SavedRelays::default();
        let r = url("https://r.example");
        saved
            .add(r.clone(), Some(token("old")), false, NOW)
            .unwrap();
        assert!(
            saved
                .receive(&r, &token("new"), "room1", "dee-id", "Dee", false, NOW)
                .is_err(),
            "unchecked"
        );
        assert_eq!(
            saved
                .receive(&r, &token("new"), "room1", "dee-id", "Dee", true, NOW)
                .unwrap(),
            Receipt::Renewed
        );
        let relay = &saved.relays[0];
        assert_eq!(relay.token.as_deref(), Some("new"));
        assert_eq!(relay.renewed_by.as_deref(), Some("Dee"));
        assert_eq!(
            relay.passable(),
            Passable::Everywhere,
            "still the user's own"
        );

        let p = url("https://p.example");
        saved
            .add(p.clone(), Some(token("mine")), true, NOW)
            .unwrap();
        assert_eq!(saved.relays[1].passable(), Passable::Never);
        assert!(
            saved
                .receive(&p, &token("theirs"), "room1", "dee-id", "Dee", true, NOW)
                .is_err()
        );
        assert_eq!(saved.relays[1].token.as_deref(), Some("mine"));
        // Added by hand, a received relay becomes the user's own.
        let q = url("https://q.example");
        saved
            .receive(&q, &token("q"), "room1", "dee-id", "Dee", true, NOW)
            .unwrap();
        saved.add(q.clone(), Some(token("q")), false, NOW).unwrap();
        assert!(saved.relays[2].received.is_none());
        assert!(!saved.drop_room("room1"));
    }

    /// M26b: a relay the user removed, or every relay with "Use relays my
    /// rooms pass on" off, is never asked for or kept; adding it by hand
    /// lifts that.
    #[test]
    fn removed_relays_and_the_switch_stop_asking() {
        let mut saved = SavedRelays::default();
        let r = url("https://r.example");
        saved
            .receive(&r, &token("x"), "room1", "bia-id", "Bia", true, NOW)
            .unwrap();
        saved.remove(&r).unwrap();
        assert!(!saved.may_ask(r.as_str()));
        assert!(
            saved
                .receive(&r, &token("x"), "room1", "bia-id", "Bia", true, NOW)
                .is_err()
        );
        saved.add(r.clone(), Some(token("x")), false, NOW).unwrap();
        assert!(saved.may_ask(r.as_str()));

        saved.refuse_passed_on = true;
        let s = url("https://s.example");
        assert!(!saved.may_ask(s.as_str()));
        assert!(
            saved
                .receive(&s, &token("y"), "room1", "bia-id", "Bia", true, NOW)
                .is_err()
        );
        // The list stays bounded.
        saved.refuse_passed_on = false;
        for i in 0..MAX_DONT_ASK + 5 {
            let u = url(&format!("https://gone{i}.example"));
            saved.add(u.clone(), None, false, NOW).unwrap();
            saved.remove(&u).unwrap();
        }
        assert_eq!(saved.dont_ask.len(), MAX_DONT_ASK);
    }

    /// M26b: a received relay no room has needed for 30 days goes; one
    /// still needed stays.
    #[test]
    fn received_relays_unneeded_for_30_days_go() {
        let mut saved = SavedRelays::default();
        let (r, s) = (url("https://r.example"), url("https://s.example"));
        saved
            .receive(&r, &token("x"), "room1", "bia-id", "Bia", true, NOW)
            .unwrap();
        saved
            .receive(&s, &token("y"), "room1", "bia-id", "Bia", true, NOW)
            .unwrap();
        let later = NOW + FORGET_RECEIVED_SECS - 10;
        let live = ["room1".to_string()];
        assert!(saved.needed(&[("room1".into(), r.to_string())], &live, later));
        let much_later = later + 20;
        assert!(saved.needed(&[], &live, much_later));
        assert_eq!(saved.relays.len(), 1);
        assert_eq!(saved.relays[0].url, r.to_string());
        assert!(!saved.needed(&[], &live, much_later), "nothing new");
        // Kept after the snapshot of live rooms was taken (a join admitted
        // meanwhile): not that snapshot's to drop.
        let fresh = url("https://fresh.example");
        saved
            .receive(
                &fresh,
                &token("f"),
                "room9",
                "ana-id",
                "Ana",
                true,
                much_later,
            )
            .unwrap();
        saved.needed(&[], &live, much_later);
        assert!(saved.relays.iter().any(|r| r.url == fresh.to_string()));
        // A room we're no longer in takes its relays with it.
        assert!(saved.needed(&[], &[], much_later + 1));
        assert!(saved.relays.is_empty());
    }

    /// The second review's finding: rooms can't fill the list, so the
    /// user's own always fit beside what they passed on.
    #[test]
    fn rooms_pass_on_at_most_eight_relays() {
        let mut saved = SavedRelays::default();
        for i in 0..MAX_RECEIVED {
            saved
                .receive(
                    &url(&format!("https://r{i}.example")),
                    &token("x"),
                    "room1",
                    "m",
                    "M",
                    true,
                    NOW,
                )
                .unwrap();
        }
        assert!(
            saved
                .receive(
                    &url("https://one-more.example"),
                    &token("x"),
                    "room1",
                    "m",
                    "M",
                    true,
                    NOW
                )
                .is_err()
        );
        saved
            .add(url("https://mine.example"), Some(token("y")), false, NOW)
            .unwrap();
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
