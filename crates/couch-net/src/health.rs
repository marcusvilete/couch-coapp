//! What iroh can tell about our own connection (M26a, ADR 0014), as plain
//! values: the home relay's state, and the latest network report (does UDP
//! work, is a sign-in page in the way, which relays answered). The engine
//! turns them into what the user reads.

use iroh::{
    Endpoint, RelayUrl, Watcher,
    endpoint::{Connection, RelayStatus},
    unstable_net_report::NetReport,
};
use tokio::sync::watch;

/// The relay others reach us through, and its connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeRelay {
    pub url: RelayUrl,
    pub connected: bool,
    /// It refused our token, in its own words: it won't take it again.
    pub refused: Option<String>,
    /// The last error connecting to it, while not connected.
    pub error: Option<String>,
}

/// What the latest network report found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetReportSummary {
    /// A UDP round trip to a relay worked (IPv4 or IPv6): direct
    /// connections are possible.
    pub udp: bool,
    /// Something intercepts web requests: a network that wants a sign-in
    /// first. `None` when it wasn't checked.
    pub captive_portal: Option<bool>,
    /// The relays that answered the report's probes.
    pub reached: Vec<RelayUrl>,
}

/// Our connection as iroh sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointHealth {
    pub home: Option<HomeRelay>,
    /// `None` until the first report is in.
    pub report: Option<NetReportSummary>,
}

/// Follows `endpoint`'s home relay and network reports until it closes.
pub fn watch_health(endpoint: &Endpoint) -> watch::Receiver<EndpointHealth> {
    let mut status = endpoint.home_relay_status();
    let mut report = endpoint.net_report();
    let (tx, rx) = watch::channel(read(&status.get(), report.get().as_ref()));
    let endpoint = endpoint.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = endpoint.closed() => return,
                changed = status.updated() => if changed.is_err() { return },
                changed = report.updated() => if changed.is_err() { return },
            }
            let now = read(&status.get(), report.get().as_ref());
            tx.send_if_modified(|was| {
                let changed = *was != now;
                *was = now;
                changed
            });
            if tx.is_closed() {
                return;
            }
        }
    });
    rx
}

fn read(status: &[RelayStatus], report: Option<&NetReport>) -> EndpointHealth {
    EndpointHealth {
        home: status.first().map(|s| HomeRelay {
            url: s.url().clone(),
            connected: s.is_connected(),
            refused: s.auth_denied_reason().map(str::to_string),
            error: (!s.is_connected())
                .then(|| s.last_error().map(|e| format!("{e:#}")))
                .flatten(),
        }),
        report: report.map(|r| {
            let mut reached: Vec<RelayUrl> = r
                .relay_latency
                .iter()
                .map(|(_, url, _)| url.clone())
                .collect();
            reached.sort();
            reached.dedup();
            NetReportSummary {
                udp: r.has_udp(),
                captive_portal: r.captive_portal,
                reached,
            }
        }),
    }
}

/// The relay a connection's selected path goes through, if it is relayed.
pub fn selected_relay(conn: &Connection) -> Option<RelayUrl> {
    let paths = conn.paths();
    paths
        .iter()
        .find(|p| p.is_selected())
        .and_then(|p| match p.remote_addr() {
            iroh::TransportAddr::Relay(url) => Some(url.clone()),
            _ => None,
        })
}
