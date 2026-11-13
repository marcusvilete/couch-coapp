//! Why a member can't be reached, as far as we can tell (M26a-2, [ADR
//! 0014] "Rooms: why a member can't be reached"): a hint, never a verdict,
//! since a direct path may still connect, and nothing is refused for it.
//!
//! Worked out from what we know without asking anyone: the relay the
//! member's signed record names, our own relays' states and the network's
//! (`network`), and, for a relay we don't hold, probing it without a code
//! (`network::Probe`). Shown only once a dial to them has failed, so a
//! member we simply haven't tried yet gets none.
//!
//! [ADR 0014]: ../../../docs/adr/0014-public-builds-and-registered-relays.md

use std::time::Duration;

use couch_net::{
    RelayUrl,
    addr::DialRules,
    endpoint::{Relay, RelayCheck, check_relay, is_public_relay},
};
use serde::Serialize;
use ts_rs::TS;

use crate::network::{self, Network, NetworkTrouble, Probe, RelayStateView};

/// Why a member can't be reached, the one thing we can tell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum MemberReason {
    /// Our own connection: no internet, a sign-in page, or none of our
    /// relays answering. The shell's strip says it; the room says it once.
    OurConnection,
    /// They are on a relay that isn't answering: one of ours (its state in
    /// Settings → Network), or one of theirs we probed.
    RelayUnreachable {
        url: String,
        host: String,
        /// One we hold.
        ours: bool,
    },
    /// They are on a relay of ours that no longer accepts our code.
    RelayRefused { url: String, host: String },
    /// They are on a relay that needs a code we don't have: ask them (or
    /// anyone on it) for it, or give them ours.
    NeedsCode { url: String, host: String },
    /// Their relay is one this build won't dial (only https to a named
    /// server): they need another relay, or a direct path.
    RelayNotAllowed { host: String },
}

/// A member's reason, with iroh's own words for Details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UnreachableView {
    pub reason: MemberReason,
    /// The last dial's error, or the relay's; never a token.
    pub details: Option<String>,
}

/// A reason in a few words, after "Reconnecting to Ana: " in the stream
/// window.
pub(crate) fn short(reason: &MemberReason) -> String {
    match reason {
        MemberReason::OurConnection => "your connection isn't working".into(),
        MemberReason::RelayUnreachable { host, .. } => format!("{host} stopped answering"),
        MemberReason::RelayRefused { host, .. } => format!("{host} no longer accepts your code"),
        MemberReason::NeedsCode { host, .. } => format!("{host} needs a code you don't have"),
        MemberReason::RelayNotAllowed { host } => format!("{host} isn't a relay this app uses"),
    }
}

/// The relay, in iroh's own spelling (`https://relay.example.com/`), as our
/// list and the network's reports write it; `None` for one that doesn't
/// parse.
pub(crate) fn normalized(url: &str) -> Option<RelayUrl> {
    url.trim().parse().ok()
}

fn host(url: &RelayUrl) -> String {
    url.host_str().unwrap_or_default().to_string()
}

/// The relay an invite names (M26a-2): the owner's home relay as the link
/// was made, shown before the password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InviteRelayView {
    pub url: String,
    pub host: String,
    pub relay: InviteRelay,
}

/// What we know of an invite's relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum InviteRelay {
    /// One we hold: our code takes us there.
    Ours,
    /// One of iroh's public relays: open to all.
    Public,
    /// One this build won't dial (only https to a named server).
    NotAllowed,
    /// Not checked yet: [`crate::Command::CheckInviteRelay`].
    Unchecked,
    /// It lets anyone in.
    Open,
    /// It wants a code we don't have: ask the owner for it.
    NeedsCode,
    /// It didn't answer; iroh's error, for Details.
    Unreachable { details: Option<String> },
}

/// Which step of a join failed, when the owner wasn't reached (M26a-2):
/// told apart by connecting to the invite's relay afterwards. A refusal by
/// the owner (the password, the link's expiry, a ban) is said as before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum JoinProblem {
    /// This PC's own connection: no internet, a sign-in page, or none of
    /// our relays answering.
    OurConnection,
    /// The owner's relay didn't answer.
    RelayUnreachable {
        url: String,
        host: String,
        details: Option<String>,
    },
    /// The owner's relay is one of ours, and it no longer accepts our code.
    RelayRefused {
        url: String,
        host: String,
        reason: String,
    },
    /// The owner's relay wants a code we don't have.
    NeedsCode { url: String, host: String },
    /// The relay answered, but the owner isn't on it: offline, or the link
    /// is old (their app moved to another relay since).
    OwnerNotOnRelay { host: String },
}

impl std::fmt::Display for JoinProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OurConnection => f.write_str(
                "couldn't reach the room's owner: this PC's connection isn't working (Settings → Network says why)",
            ),
            Self::RelayUnreachable { host, .. } => write!(
                f,
                "couldn't reach {host}, the relay the room's owner uses: try again later"
            ),
            Self::RelayRefused { host, .. } => write!(
                f,
                "{host}, the relay the room's owner uses, no longer accepts your code: ask its owner for a new one"
            ),
            Self::NeedsCode { host, .. } => write!(
                f,
                "the room's owner uses {host}, which needs a code: ask them for it"
            ),
            Self::OwnerNotOnRelay { host } => write!(
                f,
                "{host} answered, but the room's owner isn't on it: they may be offline, or the link may be old (ask for a new one)"
            ),
        }
    }
}

/// What we know of relay `url` without connecting: held, public, not
/// dialed, or what probing it found lately.
fn classify(url: &RelayUrl, network: &Network, rules: &DialRules) -> InviteRelay {
    let key = url.to_string();
    if !rules.allows(url) {
        return InviteRelay::NotAllowed;
    }
    if network.held(&key).is_some() {
        return InviteRelay::Ours;
    }
    if is_public_relay(url) {
        return InviteRelay::Public;
    }
    match network.probe(&key) {
        Some(Probe::Open) => InviteRelay::Open,
        Some(Probe::NeedsCode) => InviteRelay::NeedsCode,
        Some(Probe::Unreachable(details)) => InviteRelay::Unreachable {
            details: details.clone(),
        },
        None => InviteRelay::Unchecked,
    }
}

/// An invite's relay, as the preview shows it at once.
pub(crate) fn invite_relay(
    relay: Option<&str>,
    network: &Network,
    rules: &DialRules,
) -> Option<InviteRelayView> {
    let url = normalized(relay?)?;
    Some(InviteRelayView {
        host: host(&url),
        relay: classify(&url, network, rules),
        url: url.to_string(),
    })
}

/// An invite's relay, connecting to it without a code when nothing is
/// known of it: whether it wants one.
pub(crate) async fn check_invite_relay(
    relay: Option<&str>,
    network: &network::Shared,
    rules: &DialRules,
    timeout: Duration,
) -> Option<InviteRelayView> {
    let mut view = invite_relay(relay, &network::lock(network), rules)?;
    if view.relay == InviteRelay::Unchecked {
        let url: RelayUrl = view.url.parse().ok()?;
        view.relay = match check_relay(&Relay::new(url, None), timeout).await {
            Ok(RelayCheck::Accepted) => InviteRelay::Open,
            Ok(RelayCheck::Refused(_)) => InviteRelay::NeedsCode,
            Ok(RelayCheck::Unreachable(details)) => InviteRelay::Unreachable { details },
            Err(e) => InviteRelay::Unreachable {
                details: Some(e.to_string()),
            },
        };
    }
    Some(view)
}

/// Which step failed, for a join whose dial to the owner at the invite's
/// `relay` never reached them (`error`): our connection, the relay (with
/// our code, if we hold it), or the owner not being on it. `None` without
/// a relay to look at, or for one the dial rules don't allow.
pub(crate) async fn join_problem(
    relay: Option<&str>,
    error: &str,
    network: &network::Shared,
    rules: &DialRules,
    timeout: Duration,
) -> Option<JoinProblem> {
    let url = normalized(relay?)?;
    // Never a relay we wouldn't dial: a link naming one beside an address
    // must not make us connect there (the dial dropped it).
    if !rules.allows(&url) {
        return None;
    }
    let key = url.to_string();
    let (trouble, held, token) = {
        let network = network::lock(network);
        (
            network.trouble().cloned(),
            network.held(&key).is_some(),
            network.token(&key),
        )
    };
    if matches!(
        trouble,
        Some(NetworkTrouble::NoInternet | NetworkTrouble::SignIn | NetworkTrouble::NoRelay)
    ) {
        return Some(JoinProblem::OurConnection);
    }
    let host = host(&url);
    Some(match check_relay(&Relay::new(url, token), timeout).await {
        Ok(RelayCheck::Accepted) => JoinProblem::OwnerNotOnRelay { host },
        Ok(RelayCheck::Refused(reason)) if held => JoinProblem::RelayRefused {
            url: key,
            host,
            reason,
        },
        Ok(RelayCheck::Refused(_)) => JoinProblem::NeedsCode { url: key, host },
        Ok(RelayCheck::Unreachable(details)) => JoinProblem::RelayUnreachable {
            url: key,
            host,
            details: details.or_else(|| Some(error.to_string())),
        },
        Err(e) => JoinProblem::RelayUnreachable {
            url: key,
            host,
            details: Some(e.to_string()),
        },
    })
}

/// What a member whose dial failed (`dial_error`) is told by: `relay` from
/// their record. `None`: nothing we can tell (they may be away).
pub(crate) fn member_reason(
    relay: Option<&str>,
    dial_error: Option<&str>,
    network: &Network,
    rules: &DialRules,
) -> Option<UnreachableView> {
    let with = |reason, details: Option<String>| {
        Some(UnreachableView {
            reason,
            details: details.or_else(|| dial_error.map(str::to_string)),
        })
    };
    if matches!(
        network.trouble(),
        Some(NetworkTrouble::NoInternet | NetworkTrouble::SignIn | NetworkTrouble::NoRelay)
    ) {
        return with(MemberReason::OurConnection, None);
    }
    let url = normalized(relay?)?;
    let host = host(&url);
    if !rules.allows(&url) {
        return with(MemberReason::RelayNotAllowed { host }, None);
    }
    let key = url.to_string();
    if let Some(state) = network.held(&key) {
        return match state {
            RelayStateView::Refused { reason } => {
                with(MemberReason::RelayRefused { url: key, host }, Some(reason))
            }
            RelayStateView::Unreachable { details } => with(
                MemberReason::RelayUnreachable {
                    url: key,
                    host,
                    ours: true,
                },
                details,
            ),
            // It works: they may simply be away.
            _ => None,
        };
    }
    if is_public_relay(&url) {
        // Open to all; only worth saying while ours to use aren't answering.
        return match network.public() {
            Some(RelayStateView::Unreachable { details }) => with(
                MemberReason::RelayUnreachable {
                    url: key,
                    host,
                    ours: true,
                },
                details,
            ),
            _ => None,
        };
    }
    match network.probe(&key)? {
        Probe::NeedsCode => with(MemberReason::NeedsCode { url: key, host }, None),
        Probe::Unreachable(details) => with(
            MemberReason::RelayUnreachable {
                url: key,
                host,
                ours: false,
            },
            details.clone(),
        ),
        Probe::Open => None,
    }
}

/// The relay to probe for a member whose dial failed: theirs, if we don't
/// hold it, it isn't iroh's public one and we would dial it at all.
pub(crate) fn wants_probe(relay: Option<&str>, rules: &DialRules) -> Option<String> {
    let url = normalized(relay?)?;
    (rules.allows(&url) && !is_public_relay(&url)).then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use couch_net::endpoint::RelayCheck;

    use super::*;

    const OURS: &str = "https://ours.example/";
    const THEIRS: &str = "https://theirs.example/";

    fn rules() -> DialRules {
        DialRules {
            lan: false,
            ours: Default::default(),
        }
    }

    #[test]
    fn nothing_is_said_without_a_relay_or_while_theirs_works() {
        let network = Network::new(false, true);
        assert_eq!(
            member_reason(None, Some("timed out"), &network, &rules()),
            None
        );
        // Not probed yet: nothing known.
        assert_eq!(
            member_reason(Some(THEIRS), Some("timed out"), &network, &rules()),
            None
        );
    }

    #[test]
    fn a_relay_we_wont_dial_is_named() {
        let network = Network::new(false, true);
        let reason = member_reason(Some("http://192.168.1.5:3340/"), None, &network, &rules());
        assert_eq!(
            reason.map(|r| r.reason),
            Some(MemberReason::RelayNotAllowed {
                host: "192.168.1.5".into()
            })
        );
        assert_eq!(
            wants_probe(Some("http://192.168.1.5:3340/"), &rules()),
            None
        );
    }

    #[test]
    fn a_probe_tells_a_relay_wanting_a_code_from_one_not_answering() {
        let mut network = Network::new(false, true);
        let want = wants_probe(Some(THEIRS), &rules()).unwrap();
        assert!(network.want_probes([want.clone()].into()));
        assert!(network.probed(
            &want,
            Probe::from(RelayCheck::Refused("no".into())),
            Instant::now()
        ));
        let reason = member_reason(Some(THEIRS), Some("timed out"), &network, &rules()).unwrap();
        assert_eq!(
            reason,
            UnreachableView {
                reason: MemberReason::NeedsCode {
                    url: THEIRS.into(),
                    host: "theirs.example".into()
                },
                details: Some("timed out".into()),
            }
        );
        network.probed(
            &want,
            Probe::from(RelayCheck::Unreachable(Some("no such name".into()))),
            Instant::now(),
        );
        let reason = member_reason(Some(THEIRS), Some("timed out"), &network, &rules()).unwrap();
        assert_eq!(
            reason.reason,
            MemberReason::RelayUnreachable {
                url: THEIRS.into(),
                host: "theirs.example".into(),
                ours: false
            }
        );
        assert_eq!(reason.details.as_deref(), Some("no such name"));
        // Open to all: they may just be away.
        network.probed(&want, Probe::Open, Instant::now());
        assert_eq!(member_reason(Some(THEIRS), None, &network, &rules()), None);
        // No longer wanted: what was found stays a while, but a late answer
        // isn't taken.
        assert!(!network.want_probes(Default::default()));
        assert!(!network.probed(&want, Probe::NeedsCode, Instant::now()));
        assert_eq!(network.probe(&want), Some(&Probe::Open));
    }

    /// The review's finding: a link naming a relay we won't dial beside an
    /// address must never make a failed join connect there.
    #[tokio::test]
    async fn a_join_never_looks_at_a_relay_the_rules_forbid() {
        let network = std::sync::Arc::new(std::sync::Mutex::new(Network::new(false, true)));
        for relay in [
            "http://192.168.1.5:3340/",
            "http://localhost:3340/",
            "https://relay:443/",
        ] {
            let problem = join_problem(
                Some(relay),
                "no answer",
                &network,
                &rules(),
                std::time::Duration::from_millis(1),
            )
            .await;
            assert_eq!(problem, None, "{relay}");
        }
    }

    /// The review's finding: what a probe found outlives the member's need
    /// for a while, so one coming and going isn't probed each time.
    #[test]
    fn a_probe_s_answer_is_kept_while_a_member_comes_and_goes() {
        let mut network = Network::new(false, true);
        assert!(network.want_probes([THEIRS.to_string()].into()));
        assert_eq!(
            crate::network::tests::due_probes_now(&mut network),
            [THEIRS]
        );
        network.probed(THEIRS, Probe::NeedsCode, Instant::now());
        // Connected: no longer wanted, still known.
        network.want_probes(Default::default());
        assert_eq!(network.probe(THEIRS), Some(&Probe::NeedsCode));
        // Away again: wanted, and not due a probe yet.
        network.want_probes([THEIRS.to_string()].into());
        let due = crate::network::tests::due_probes_now(&mut network);
        assert!(due.is_empty(), "{due:?}");
    }

    #[test]
    fn our_own_relays_and_iroh_s_are_never_probed() {
        let mut network = Network::new(false, true);
        network.listed = vec![crate::network::tests::listed(OURS, Some("t"))];
        let public = couch_net::endpoint::public_relay_urls()
            .into_iter()
            .next()
            .expect("iroh has public relays");
        assert!(!network.want_probes([OURS.to_string(), public.to_string()].into()));
        assert_eq!(network.probe(OURS), None);
    }
}
