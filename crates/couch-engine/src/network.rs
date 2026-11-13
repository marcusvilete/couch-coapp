//! What we can tell about our own connection (M26a, [ADR 0014]): each
//! relay's state, the network's, and the trouble that silently breaks every
//! room, for Settings → Network, the shell's strip and the tray.
//!
//! iroh reports only on the home relay (`couch_net::health`), so the other
//! relays we hold are known from the network report (they answered its
//! probes), from the relays our connections go through now, and, for one
//! with a token, from checking its code now and then
//! (`couch_net::endpoint::check_relay`, a throwaway endpoint of its own).
//!
//! [ADR 0014]: ../../../docs/adr/0014-public-builds-and-registered-relays.md

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use couch_net::{
    Endpoint, RelayUrl,
    endpoint::{Relay, RelayCheck, RelayToken, check_relay, is_public_relay},
    health::{EndpointHealth, watch_health},
};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, watch};
use ts_rs::TS;

use crate::{Event, NetworkView, RelayState, RelayView};

/// How the watch is timed. Tests shorten it.
#[derive(Debug, Clone, Copy)]
pub struct NetworkTimings {
    /// A trouble shows only once it has lasted this long (a blip doesn't
    /// alarm), and goes as soon as it clears.
    pub trouble_after: Duration,
    /// The first check of each relay's code, after the start (iroh settles
    /// first).
    pub first_check: Duration,
    /// Checks again a relay that let us in.
    pub check_every: Duration,
    /// Checks again a relay that refused us or didn't answer.
    pub recheck_after: Duration,
    /// How long a relay is given to let us in, checking its code (before
    /// it is added, and now and then).
    pub check_timeout: Duration,
}

impl Default for NetworkTimings {
    fn default() -> Self {
        Self {
            trouble_after: Duration::from_secs(10),
            first_check: Duration::from_secs(15),
            check_every: Duration::from_secs(10 * 60),
            recheck_after: Duration::from_secs(2 * 60),
            check_timeout: Duration::from_secs(10),
        }
    }
}

/// A relay's state, as Settings shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum RelayStateView {
    /// Nothing known yet.
    Checking,
    /// Connected, and the relay others reach us through.
    Home,
    /// Carrying a connection of ours now.
    Connected,
    /// It answers; not needed now.
    Idle,
    /// It didn't answer: no such name, a certificate gone bad, no answer.
    /// iroh's error, for Details.
    Unreachable { details: Option<String> },
    /// It refused our code (changed or taken back): ask its owner for a new
    /// one. The relay's own words.
    Refused { reason: String },
}

/// The network this PC is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum InternetView {
    /// Not known yet.
    Unknown,
    Online,
    /// Nothing outside answers.
    Offline,
    /// Something intercepts web requests: the network wants a sign-in
    /// first (a hotel's or a café's page).
    SignIn,
}

/// What silently breaks every room, shown in the shell's strip and the
/// tray once it has lasted (`NetworkTimings::trouble_after`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum NetworkTrouble {
    NoInternet,
    SignIn,
    /// A relay of ours no longer accepts our code.
    RelayRefused {
        url: String,
        host: String,
    },
    /// None of our relays answers, though the network seems up.
    NoRelay,
}

/// How long a refusal iroh reported before a relay's code changed is
/// ignored at most: its connection starts again within moments.
const FRESH_FOR: Duration = Duration::from_secs(5);

fn is_fresh(fresh: &BTreeMap<String, Instant>, url: &str) -> bool {
    fresh
        .get(url)
        .is_some_and(|since| since.elapsed() < FRESH_FOR)
}

/// A relay's code, as last checked.
#[derive(Debug, Clone)]
struct Checked {
    result: RelayCheck,
    at: Instant,
    /// Due again at once ("Check again").
    stale: bool,
}

/// One relay we hold, as the network view lists it (its state aside).
#[derive(Debug, Clone)]
pub(crate) struct Listed {
    pub view: RelayView,
    /// For checking its code; never shown.
    pub token: Option<RelayToken>,
}

/// Everything the network view is made from.
pub(crate) struct Network {
    pub listed: Vec<Listed>,
    pub public_in_use: bool,
    pub keep_public: bool,
    /// The user lets iroh's public relays stand in while none of ours works.
    pub public_fallback: bool,
    /// This build offers iroh's public relays at all (not in tests).
    pub public_offered: bool,
    /// They stand in now, and since when none of ours has worked.
    falling_back: bool,
    fallback_since: Option<Instant>,
    pub test_build: bool,
    pub changeable: bool,
    endpoint: EndpointHealth,
    carrying: BTreeSet<String>,
    checks: BTreeMap<String, Checked>,
    trouble: Debounce,
    /// Relays set aside for refusing our code: out of the endpoint's list
    /// until a check finds them letting us in again, or their code changes.
    pub benched: BTreeSet<String>,
    /// Relays whose code just changed: a refusal iroh reported before the
    /// change is old news until it reports on them again.
    fresh: BTreeMap<String, Instant>,
}

impl Network {
    pub fn new(test_build: bool, changeable: bool) -> Self {
        Self {
            listed: Vec::new(),
            public_in_use: false,
            keep_public: false,
            public_fallback: false,
            public_offered: false,
            falling_back: false,
            fallback_since: None,
            test_build,
            changeable,
            endpoint: EndpointHealth::default(),
            carrying: BTreeSet::new(),
            checks: BTreeMap::new(),
            trouble: Debounce::default(),
            benched: BTreeSet::new(),
            fresh: BTreeMap::new(),
        }
    }

    /// The relay's code changed or it went: what was learned of it before
    /// no longer holds, and it is tried again.
    pub fn forget_check(&mut self, url: &str) {
        self.checks.remove(url);
        self.fresh.insert(url.to_string(), Instant::now());
        self.benched.remove(url);
    }

    /// A relay was just checked (adding it, or the watch).
    pub fn checked(&mut self, url: &str, result: RelayCheck, now: Instant) {
        self.checks.insert(
            url.to_string(),
            Checked {
                result,
                at: now,
                stale: false,
            },
        );
    }

    /// "Check again": every code is due a check now. One that refused stays
    /// set aside until its check says otherwise, so iroh doesn't take it
    /// back as its home relay meanwhile.
    pub fn forget_checks(&mut self) {
        for check in self.checks.values_mut() {
            check.stale = true;
        }
    }

    /// iroh's word on our connection changed. A relay whose code just
    /// changed stops being fresh once iroh reports it not refusing (its
    /// connection started again), or after `FRESH_FOR` whatever it says.
    fn health_changed(&mut self, health: EndpointHealth) {
        self.endpoint = health;
        let home = self.endpoint.home.as_ref();
        self.fresh.retain(|url, since| {
            since.elapsed() < FRESH_FOR
                && home.is_some_and(|h| h.url.as_str() == url && h.refused.is_some())
        });
    }

    /// Sets aside the relays that refuse our code, and takes back those
    /// that let us in again (or whose code changed). A refusal the home
    /// relay reports is kept as a check, since once set aside iroh no
    /// longer reports on it. Whether the set changed.
    pub fn falling_back(&self) -> bool {
        self.falling_back
    }

    /// The setting went off: no standing in, at once.
    pub fn stop_falling_back(&mut self) {
        self.falling_back = false;
        self.fallback_since = None;
    }

    /// Lets iroh's public relays stand in once none of ours has worked for
    /// `after` (refusing our code, or not answering) while the network
    /// itself is up, if the user allows it; and stops as soon as one of
    /// ours answers again. Whether that changed.
    fn fall_back(&mut self, now: Instant, after: Duration) -> bool {
        let facts = self.facts();
        let ours_work = self.listed.iter().any(|l| {
            matches!(
                relay_state(&l.view.url, &facts),
                RelayStateView::Home | RelayStateView::Connected | RelayStateView::Idle
            )
        });
        let network_up = self.endpoint.report.is_some()
            && !matches!(
                internet(&facts),
                InternetView::Offline | InternetView::SignIn
            );
        let wanted = self.public_fallback
            && self.public_offered
            && !self.keep_public
            && !self.listed.is_empty()
            && !ours_work
            && (self.falling_back || network_up);
        let before = self.falling_back;
        if wanted {
            let since = *self.fallback_since.get_or_insert(now);
            if now.duration_since(since) >= after {
                self.falling_back = true;
            }
        } else {
            self.stop_falling_back();
        }
        if self.falling_back != before {
            tracing::info!(
                falling_back = self.falling_back,
                "iroh's public relays standing in"
            );
        }
        self.falling_back != before
    }

    fn bench(&mut self, now: Instant) -> bool {
        if let Some(home) = &self.endpoint.home
            && let Some(reason) = &home.refused
            && !is_fresh(&self.fresh, home.url.as_str())
            && self.listed.iter().any(|l| l.view.url == home.url.as_str())
            && !matches!(
                self.checks.get(home.url.as_str()).map(|c| &c.result),
                Some(RelayCheck::Refused(_))
            )
        {
            let url = home.url.to_string();
            self.checked(&url, RelayCheck::Refused(reason.clone()), now);
        }
        let benched: BTreeSet<String> = self
            .listed
            .iter()
            .filter(|l| {
                matches!(
                    self.checks.get(&l.view.url).map(|c| &c.result),
                    Some(RelayCheck::Refused(_))
                )
            })
            .map(|l| l.view.url.clone())
            .collect();
        let changed = benched != self.benched;
        if changed {
            tracing::info!(?benched, "relays set aside for refusing our code");
        }
        self.benched = benched;
        changed
    }

    pub fn view(&self) -> NetworkView {
        let facts = self.facts();
        NetworkView {
            relays: self
                .listed
                .iter()
                .map(|l| RelayView {
                    state: relay_state(&l.view.url, &facts),
                    ..l.view.clone()
                })
                .collect(),
            public: self.public_in_use.then(|| public_state(&facts)),
            public_in_use: self.public_in_use,
            keep_public: self.keep_public,
            public_fallback: self.public_fallback,
            falling_back: self.falling_back,
            home_relay: self.endpoint.home.as_ref().map(|h| h.url.to_string()),
            internet: internet(&facts),
            udp: self.endpoint.report.as_ref().map(|r| r.udp),
            trouble: self.trouble.shown.clone(),
            test_build: self.test_build,
            changeable: self.changeable,
        }
    }

    fn facts(&self) -> Facts<'_> {
        Facts {
            endpoint: &self.endpoint,
            carrying: &self.carrying,
            checks: &self.checks,
            fresh: &self.fresh,
        }
    }

    /// The trouble now, before it has lasted.
    fn trouble_now(&self) -> Option<NetworkTrouble> {
        let facts = self.facts();
        match internet(&facts) {
            InternetView::Offline => return Some(NetworkTrouble::NoInternet),
            InternetView::SignIn => return Some(NetworkTrouble::SignIn),
            InternetView::Unknown | InternetView::Online => {}
        }
        let states: Vec<(&RelayView, RelayStateView)> = self
            .listed
            .iter()
            .map(|l| (&l.view, relay_state(&l.view.url, &facts)))
            .collect();
        if let Some((view, _)) = states
            .iter()
            .find(|(_, s)| matches!(s, RelayStateView::Refused { .. }))
        {
            return Some(NetworkTrouble::RelayRefused {
                url: view.url.clone(),
                host: view.host.clone(),
            });
        }
        let answering = |s: &RelayStateView| {
            matches!(
                s,
                RelayStateView::Home | RelayStateView::Connected | RelayStateView::Idle
            )
        };
        let public = self.public_in_use.then(|| public_state(&facts));
        let any_relay = !states.is_empty() || public.is_some();
        let none_answers = states.iter().all(|(_, s)| !answering(s))
            && public.as_ref().is_none_or(|s| !answering(s));
        let known = self.endpoint.report.is_some();
        (any_relay && none_answers && known).then_some(NetworkTrouble::NoRelay)
    }

    /// Takes the trouble now into the debounce; whether what shows changed.
    fn settle(&mut self, now: Instant, after: Duration) -> bool {
        let candidate = self.trouble_now();
        self.trouble.update(candidate, now, after)
    }
}

/// What the states are worked out from.
struct Facts<'a> {
    endpoint: &'a EndpointHealth,
    carrying: &'a BTreeSet<String>,
    checks: &'a BTreeMap<String, Checked>,
    fresh: &'a BTreeMap<String, Instant>,
}

impl Facts<'_> {
    fn reached(&self, url: &str) -> bool {
        self.endpoint
            .report
            .as_ref()
            .is_some_and(|r| r.reached.iter().any(|u| u.as_str() == url))
    }
}

fn relay_state(url: &str, facts: &Facts) -> RelayStateView {
    if let Some(home) = facts
        .endpoint
        .home
        .as_ref()
        .filter(|h| h.url.as_str() == url)
    {
        if let Some(reason) = home
            .refused
            .as_ref()
            .filter(|_| !is_fresh(facts.fresh, url))
        {
            return RelayStateView::Refused {
                reason: reason.clone(),
            };
        }
        if home.connected {
            return RelayStateView::Home;
        }
        if let Some(error) = &home.error {
            return RelayStateView::Unreachable {
                details: Some(error.clone()),
            };
        }
        return RelayStateView::Checking;
    }
    let checked = facts.checks.get(url).map(|c| &c.result);
    if let Some(RelayCheck::Refused(reason)) = checked {
        return RelayStateView::Refused {
            reason: reason.clone(),
        };
    }
    if facts.carrying.contains(url) {
        return RelayStateView::Connected;
    }
    if facts.reached(url) || matches!(checked, Some(RelayCheck::Accepted)) {
        return RelayStateView::Idle;
    }
    if let Some(RelayCheck::Unreachable(details)) = checked {
        return RelayStateView::Unreachable {
            details: details.clone(),
        };
    }
    if facts.endpoint.report.is_some() {
        return RelayStateView::Unreachable { details: None };
    }
    RelayStateView::Checking
}

/// iroh's public relays, as one row: they need no code.
fn public_state(facts: &Facts) -> RelayStateView {
    let is_public = |url: &str| url.parse::<RelayUrl>().is_ok_and(|u| is_public_relay(&u));
    if let Some(home) = facts
        .endpoint
        .home
        .as_ref()
        .filter(|h| is_public_relay(&h.url))
    {
        return relay_state(home.url.as_str(), facts);
    }
    if facts.carrying.iter().any(|u| is_public(u)) {
        return RelayStateView::Connected;
    }
    match &facts.endpoint.report {
        Some(report) if report.reached.iter().any(is_public_relay) => RelayStateView::Idle,
        Some(_) => RelayStateView::Unreachable { details: None },
        None => RelayStateView::Checking,
    }
}

fn internet(facts: &Facts) -> InternetView {
    let home_up = facts.endpoint.home.as_ref().is_some_and(|h| h.connected);
    let Some(report) = &facts.endpoint.report else {
        return if home_up {
            InternetView::Online
        } else {
            InternetView::Unknown
        };
    };
    if report.captive_portal == Some(true) {
        InternetView::SignIn
    } else if home_up || report.udp || !report.reached.is_empty() || !facts.carrying.is_empty() {
        InternetView::Online
    } else {
        InternetView::Offline
    }
}

/// A trouble shows once it has lasted; it goes as soon as it clears.
#[derive(Debug, Default)]
struct Debounce {
    candidate: Option<(NetworkTrouble, Instant)>,
    shown: Option<NetworkTrouble>,
}

impl Debounce {
    /// Whether what shows changed.
    fn update(
        &mut self,
        now_trouble: Option<NetworkTrouble>,
        now: Instant,
        after: Duration,
    ) -> bool {
        let before = self.shown.clone();
        match now_trouble {
            None => {
                self.candidate = None;
                self.shown = None;
            }
            Some(trouble) => {
                let since = match &self.candidate {
                    Some((was, since)) if *was == trouble => *since,
                    _ => now,
                };
                if now.duration_since(since) >= after {
                    self.shown = Some(trouble.clone());
                }
                self.candidate = Some((trouble, since));
            }
        }
        self.shown != before
    }
}

/// The network view's state, shared by the engine and its watch.
pub(crate) type Shared = Arc<Mutex<Network>>;

pub(crate) fn lock(network: &Shared) -> std::sync::MutexGuard<'_, Network> {
    network.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Keeps `network` current until the endpoint closes, telling the UI each
/// time its view changes: iroh's reports, the relays our connections go
/// through, and checks of each relay's code (never the home relay's, whose
/// refusal iroh reports itself). `poke` wakes it after a change of relays.
pub(crate) fn watch(
    network: Shared,
    relays: Arc<tokio::sync::Mutex<RelayState>>,
    endpoint: Endpoint,
    mut carrying: watch::Receiver<BTreeSet<String>>,
    mut poke: mpsc::Receiver<()>,
    events: broadcast::Sender<Event>,
    timings: NetworkTimings,
) {
    let mut health = watch_health(&endpoint);
    tokio::spawn(async move {
        let started = Instant::now();
        let (done_tx, mut done) = mpsc::channel::<(String, RelayCheck)>(16);
        let mut checking: BTreeSet<String> = BTreeSet::new();
        let mut last = None;
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                () = endpoint.closed() => return,
                changed = health.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let now = health.borrow_and_update().clone();
                    lock(&network).health_changed(now);
                }
                changed = carrying.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    lock(&network).carrying = carrying.borrow_and_update().clone();
                }
                Some(()) = poke.recv() => {}
                Some((url, result)) = done.recv() => {
                    checking.remove(&url);
                    if result != RelayCheck::Accepted {
                        tracing::info!(%url, ?result, "a relay's code checked");
                    }
                    let mut network = lock(&network);
                    // Still held with the same code: otherwise the change
                    // already forgot it, and this answer is old.
                    if network.listed.iter().any(|l| l.view.url == url) {
                        network.checked(&url, result, Instant::now());
                    }
                }
                _ = tick.tick() => {}
            }
            let now = Instant::now();
            let (due, rebench) = {
                let mut network = lock(&network);
                let benched = network.bench(now);
                let fallback = network.fall_back(now, timings.trouble_after);
                let rebench =
                    (benched || fallback).then(|| (network.benched.clone(), network.falling_back));
                network.settle(now, timings.trouble_after);
                (
                    due_checks(&network, &checking, started, now, &timings),
                    rebench,
                )
            };
            if let Some((benched, falling_back)) = rebench {
                let mut relays = relays.lock().await;
                relays.apply(&endpoint, &benched, falling_back).await;
                let mut network = lock(&network);
                relays.list_into(&mut network);
            }
            for relay in due {
                let url = relay.url.to_string();
                checking.insert(url.clone());
                let done = done_tx.clone();
                tokio::spawn(async move {
                    let result = check_relay(&relay, timings.check_timeout)
                        .await
                        .unwrap_or_else(|e| RelayCheck::Unreachable(Some(e.to_string())));
                    let _ = done.send((url, result)).await;
                });
            }
            let view = lock(&network).view();
            if last.as_ref() != Some(&view) {
                let _ = events.send(Event::Network(view.clone()));
                last = Some(view);
            }
        }
    });
}

/// The relays whose code is due a check: held with a token, not the home
/// relay (iroh says itself whether that one refuses us), not being checked.
fn due_checks(
    network: &Network,
    checking: &BTreeSet<String>,
    started: Instant,
    now: Instant,
    timings: &NetworkTimings,
) -> Vec<Relay> {
    if now.duration_since(started) < timings.first_check {
        return Vec::new();
    }
    let home = network.endpoint.home.as_ref().map(|h| h.url.as_str());
    network
        .listed
        .iter()
        .filter(|l| l.token.is_some() && Some(l.view.url.as_str()) != home)
        .filter(|l| !checking.contains(&l.view.url))
        .filter(|l| match network.checks.get(&l.view.url) {
            None => true,
            Some(c) => {
                let wait = if c.result == RelayCheck::Accepted {
                    timings.check_every
                } else {
                    timings.recheck_after
                };
                c.stale || now.duration_since(c.at) >= wait
            }
        })
        .filter_map(|l| Some(Relay::new(l.view.url.parse().ok()?, l.token.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use couch_net::health::{HomeRelay, NetReportSummary};

    use super::*;

    const A: &str = "https://a.example/";
    const B: &str = "https://b.example/";

    fn listed(url: &str, token: Option<&str>) -> Listed {
        Listed {
            view: RelayView {
                url: url.into(),
                host: url
                    .trim_start_matches("https://")
                    .trim_end_matches('/')
                    .into(),
                built_in: false,
                has_code: token.is_some(),
                personal: false,
                state: RelayStateView::Checking,
            },
            token: token.and_then(RelayToken::new),
        }
    }

    fn network(relays: &[(&str, Option<&str>)]) -> Network {
        let mut network = Network::new(false, true);
        network.listed = relays.iter().map(|(u, t)| listed(u, *t)).collect();
        network
    }

    fn home(url: &str, connected: bool, refused: Option<&str>) -> Option<HomeRelay> {
        Some(HomeRelay {
            url: url.parse().unwrap(),
            connected,
            refused: refused.map(str::to_string),
            error: (!connected).then(|| "connection refused".to_string()),
        })
    }

    fn report(udp: bool, reached: &[&str]) -> Option<NetReportSummary> {
        Some(NetReportSummary {
            udp,
            captive_portal: Some(false),
            reached: reached.iter().map(|u| u.parse().unwrap()).collect(),
        })
    }

    fn states(network: &Network) -> Vec<RelayStateView> {
        network.view().relays.into_iter().map(|r| r.state).collect()
    }

    #[test]
    fn each_relay_says_what_is_known_of_it() {
        let mut n = network(&[(A, Some("t")), (B, Some("u"))]);
        assert_eq!(
            states(&n),
            [RelayStateView::Checking, RelayStateView::Checking]
        );
        assert_eq!(n.view().internet, InternetView::Unknown);

        n.endpoint.home = home(A, true, None);
        n.endpoint.report = report(true, &[A, B]);
        assert_eq!(states(&n), [RelayStateView::Home, RelayStateView::Idle]);
        assert_eq!(n.view().internet, InternetView::Online);
        assert_eq!(n.view().udp, Some(true));

        n.carrying.insert(B.into());
        assert_eq!(states(&n)[1], RelayStateView::Connected);

        // B refuses our code (checked): that beats carrying, which an open
        // connection keeps for a while.
        n.checked(B, RelayCheck::Refused("no".into()), Instant::now());
        assert_eq!(
            states(&n)[1],
            RelayStateView::Refused {
                reason: "no".into()
            }
        );
        n.forget_check(B);
        n.carrying.clear();
        n.endpoint.report = report(true, &[A]);
        assert_eq!(states(&n)[1], RelayStateView::Unreachable { details: None });
        n.checked(B, RelayCheck::Accepted, Instant::now());
        assert_eq!(states(&n)[1], RelayStateView::Idle, "a check beats a probe");

        // The home relay refusing us, as iroh reports it.
        n.endpoint.home = home(A, false, Some("rotated"));
        assert_eq!(
            states(&n)[0],
            RelayStateView::Refused {
                reason: "rotated".into()
            }
        );
        n.endpoint.home = home(A, false, None);
        assert_eq!(
            states(&n)[0],
            RelayStateView::Unreachable {
                details: Some("connection refused".into())
            }
        );
    }

    #[test]
    fn the_network_itself() {
        let mut n = network(&[(A, None)]);
        n.endpoint.report = report(false, &[]);
        assert_eq!(n.view().internet, InternetView::Offline);
        n.endpoint.report = report(false, &[A]);
        assert_eq!(
            n.view().internet,
            InternetView::Online,
            "relays only, UDP blocked"
        );
        assert_eq!(n.view().udp, Some(false));
        n.endpoint.report.as_mut().unwrap().captive_portal = Some(true);
        assert_eq!(n.view().internet, InternetView::SignIn);
    }

    #[test]
    fn iroh_s_public_relays_are_one_row() {
        let public: RelayUrl = "https://euc1-1.relay.n0.iroh.link./".parse().unwrap();
        assert!(is_public_relay(&public), "iroh's map names it");
        let mut n = network(&[]);
        n.public_in_use = true;
        assert_eq!(n.view().public, Some(RelayStateView::Checking));
        n.endpoint.report = report(true, &[public.as_str()]);
        assert_eq!(n.view().public, Some(RelayStateView::Idle));
        n.endpoint.home = home(public.as_str(), true, None);
        assert_eq!(n.view().public, Some(RelayStateView::Home));
        n.public_in_use = false;
        assert_eq!(n.view().public, None);
    }

    #[test]
    fn trouble_shows_once_it_lasted_and_goes_at_once() {
        let after = Duration::from_secs(10);
        let t0 = Instant::now();
        let mut n = network(&[(A, Some("t"))]);
        n.endpoint.report = report(false, &[]);
        assert!(!n.settle(t0, after), "not yet");
        assert_eq!(n.view().trouble, None);
        assert!(n.settle(t0 + after, after));
        assert_eq!(n.view().trouble, Some(NetworkTrouble::NoInternet));

        // Back online: gone at once.
        n.endpoint.home = home(A, true, None);
        n.endpoint.report = report(true, &[A]);
        assert!(n.settle(t0 + after * 2, after));
        assert_eq!(n.view().trouble, None);

        // Our relay refuses our code.
        n.endpoint.home = home(A, false, Some("rotated"));
        n.settle(t0 + after * 3, after);
        n.settle(t0 + after * 4, after);
        assert_eq!(
            n.view().trouble,
            Some(NetworkTrouble::RelayRefused {
                url: A.into(),
                host: "a.example".into()
            })
        );

        // None answers, though the network does.
        let mut n = network(&[(A, None), (B, None)]);
        n.endpoint.report = report(true, &[]);
        n.carrying.insert("https://elsewhere.example/".into());
        n.settle(t0, after);
        n.settle(t0 + after, after);
        assert_eq!(n.view().trouble, Some(NetworkTrouble::NoRelay));
        // One answering is enough.
        n.endpoint.report = report(true, &[B]);
        n.settle(t0 + after * 2, after);
        assert_eq!(n.view().trouble, None);
    }

    #[test]
    fn a_blip_never_shows() {
        let after = Duration::from_secs(10);
        let t0 = Instant::now();
        let mut n = network(&[(A, None)]);
        for i in 0..5 {
            n.endpoint.report = report(false, &[]);
            n.settle(t0 + Duration::from_secs(i * 6), after);
            n.endpoint.report = report(true, &[A]);
            n.settle(t0 + Duration::from_secs(i * 6 + 3), after);
            assert_eq!(n.view().trouble, None);
        }
    }

    #[test]
    fn a_relay_refusing_our_code_is_set_aside_until_it_lets_us_in() {
        let now = Instant::now();
        let mut n = network(&[(A, Some("t")), (B, Some("u"))]);
        assert!(!n.bench(now));
        // The home relay refuses us: set aside, its refusal kept, since
        // iroh stops reporting on it once it is out of the list.
        n.endpoint.home = home(A, false, Some("rotated"));
        assert!(n.bench(now));
        assert_eq!(n.benched, [A.to_string()].into());
        n.endpoint.home = home(B, true, None);
        assert!(!n.bench(now));
        assert_eq!(
            states(&n)[0],
            RelayStateView::Refused {
                reason: "rotated".into()
            }
        );
        // "Check again" keeps it aside until its check answers.
        n.forget_checks();
        assert!(!n.bench(now));
        assert_eq!(n.benched.len(), 1);
        n.checked(A, RelayCheck::Accepted, now);
        assert!(n.bench(now), "it lets us in again: back");
        assert!(n.benched.is_empty());
        // A new code takes it back at once too.
        n.checked(B, RelayCheck::Refused("no".into()), now);
        assert!(n.bench(now));
        n.forget_check(B);
        assert!(n.benched.is_empty());
        assert!(!n.bench(now));
    }

    /// The room scenario's race: the relay was refusing us while we had no
    /// code for it; its code comes back, and iroh's old refusal must not
    /// set it aside again before its connection starts anew.
    #[test]
    fn a_refusal_from_before_a_new_code_is_old_news() {
        let now = Instant::now();
        let mut n = network(&[(A, Some("t"))]);
        n.health_changed(EndpointHealth {
            home: home(A, false, Some("no token")),
            report: None,
        });
        n.forget_check(A);
        assert!(!n.bench(now), "not set aside on the old refusal");
        assert_eq!(
            states(&n)[0],
            RelayStateView::Unreachable {
                details: Some("connection refused".into())
            }
        );
        // Its connection starts again: the next refusal is news.
        n.health_changed(EndpointHealth {
            home: home(A, false, None),
            report: None,
        });
        n.health_changed(EndpointHealth {
            home: home(A, false, Some("still wrong")),
            report: None,
        });
        assert!(n.bench(now));
        assert_eq!(n.benched, [A.to_string()].into());
    }

    #[test]
    fn the_public_relays_stand_in_only_when_allowed_and_none_of_ours_works() {
        let after = Duration::from_secs(10);
        let t0 = Instant::now();
        let mut n = network(&[(A, Some("t"))]);
        n.endpoint.home = home(A, false, Some("rotated"));
        n.endpoint.report = report(true, &[]);
        // Off by default.
        assert!(!n.fall_back(t0, after));
        assert!(!n.fall_back(t0 + after, after));
        assert!(!n.view().falling_back);

        n.public_fallback = true;
        n.public_offered = true;
        assert!(!n.fall_back(t0 + after, after), "not before it lasted");
        assert!(n.fall_back(t0 + after * 2, after));
        assert!(n.view().falling_back);
        // While standing in, the public ones answer: still on, ours still not.
        n.endpoint.home = home("https://euc1-1.relay.n0.iroh.link./", true, None);
        assert!(!n.fall_back(t0 + after * 3, after));
        assert!(n.falling_back());
        // Ours answers again: back to ours at once.
        n.checked(A, RelayCheck::Accepted, t0);
        assert!(n.fall_back(t0 + after * 3, after));
        assert!(!n.falling_back());

        // Offline: nothing to stand in for.
        let mut n = network(&[(A, None)]);
        n.public_fallback = true;
        n.public_offered = true;
        n.endpoint.report = report(false, &[]);
        n.fall_back(t0, after);
        assert!(!n.fall_back(t0 + after, after));
        // Kept on anyway: nothing to stand in for either.
        let mut n = network(&[(A, None)]);
        n.public_fallback = true;
        n.public_offered = true;
        n.keep_public = true;
        n.endpoint.report = report(true, &[]);
        n.fall_back(t0, after);
        assert!(!n.fall_back(t0 + after, after));
    }

    #[test]
    fn codes_are_checked_off_the_home_relay_now_and_then() {
        let timings = NetworkTimings {
            first_check: Duration::from_secs(15),
            check_every: Duration::from_secs(600),
            recheck_after: Duration::from_secs(120),
            ..NetworkTimings::default()
        };
        let t0 = Instant::now();
        let mut n = network(&[
            (A, Some("t")),
            (B, Some("u")),
            ("https://open.example/", None),
        ]);
        n.endpoint.home = home(A, true, None);
        let none = BTreeSet::new();
        let due = |n: &Network, at: u64| -> Vec<String> {
            due_checks(n, &none, t0, t0 + Duration::from_secs(at), &timings)
                .into_iter()
                .map(|r| r.url.to_string())
                .collect()
        };
        assert!(due(&n, 5).is_empty(), "iroh settles first");
        assert_eq!(due(&n, 15), [B], "not home, not open");
        n.checked(B, RelayCheck::Accepted, t0 + Duration::from_secs(15));
        assert!(due(&n, 300).is_empty());
        assert_eq!(due(&n, 615), [B]);
        n.checked(
            B,
            RelayCheck::Refused("no".into()),
            t0 + Duration::from_secs(615),
        );
        assert!(due(&n, 700).is_empty());
        assert_eq!(due(&n, 735), [B], "a refusal is checked again sooner");
        let busy: BTreeSet<String> = [B.to_string()].into();
        assert!(due_checks(&n, &busy, t0, t0 + Duration::from_secs(735), &timings).is_empty());
    }
}
