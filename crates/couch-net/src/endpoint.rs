//! Building the iroh endpoint every install runs.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use iroh::{
    Endpoint, EndpointId, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey, Watcher,
    endpoint::{
        AfterHandshakeOutcome, Connection, EndpointHooks, QuicTransportConfig, Side, VarInt,
        presets,
    },
};
use noq_proto::congestion;

use crate::NetError;

/// One relay the endpoint holds, with the token it asks for, if any (M26a,
/// ADR 0014: a token per relay, sent to that relay alone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relay {
    pub url: RelayUrl,
    pub token: Option<RelayToken>,
}

impl Relay {
    pub fn new(url: RelayUrl, token: Option<RelayToken>) -> Self {
        Self { url, token }
    }

    /// Its entry in iroh's relay map. Plain http is a relay on this PC or
    /// the LAN (`--dev`), which has no QUIC address discovery.
    fn config(&self) -> RelayConfig {
        let config = if self.url.scheme() == "https" {
            RelayConfig::from(self.url.clone())
        } else {
            RelayConfig::new(self.url.clone(), None)
        };
        match &self.token {
            Some(token) => config.with_auth_token(token.0.clone()),
            None => config,
        }
    }
}

/// Which relays the endpoint holds. Never any third-party address lookup
/// (M26a): members' records and invites carry where each one is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayChoice {
    /// Our relays, each with its own token, and iroh's public relays beside
    /// them when `public` (their terms cover hobby use, with unpublished
    /// rate limits). A member at any other relay is still reached there: the
    /// list decides only the tokens sent and where our home relay can be.
    Relays { ours: Vec<Relay>, public: bool },
    /// No relays: direct and LAN paths only. For tests.
    Disabled,
}

impl RelayChoice {
    /// iroh's public relays alone.
    pub fn public() -> Self {
        Self::Relays {
            ours: Vec::new(),
            public: true,
        }
    }

    /// These relays, each with its token.
    pub fn ours(ours: Vec<Relay>) -> Self {
        Self::Relays {
            ours,
            public: false,
        }
    }

    /// These relays, without a token.
    pub fn custom(urls: Vec<RelayUrl>) -> Self {
        Self::ours(urls.into_iter().map(|url| Relay::new(url, None)).collect())
    }

    /// Every entry of the relay map this choice binds with: ours over a
    /// public one at the same address.
    fn configs(&self) -> Vec<RelayConfig> {
        let Self::Relays { ours, public } = self else {
            return Vec::new();
        };
        let mut configs: Vec<RelayConfig> = ours.iter().map(Relay::config).collect();
        if *public {
            for config in public_relays() {
                if !configs.iter().any(|c| c.url == config.url) {
                    configs.push(config);
                }
            }
        }
        configs
    }

    fn mode(&self) -> RelayMode {
        match self {
            Self::Relays { .. } => {
                RelayMode::Custom(self.configs().into_iter().collect::<RelayMap>())
            }
            Self::Disabled => RelayMode::Disabled,
        }
    }
}

/// iroh's public relays, as plain entries of our relay map.
fn public_relays() -> Vec<RelayConfig> {
    iroh::defaults::prod::default_relay_map()
        .relays::<Vec<_>>()
        .into_iter()
        .map(|config| (*config).clone())
        .collect()
}

/// Whether `url` is one of iroh's public relays.
pub fn is_public_relay(url: &RelayUrl) -> bool {
    public_relays().iter().any(|config| config.url == *url)
}

/// Moves a running endpoint from the relays of `from` to those of `to`
/// (M26a): each relay added, given another token, or gone. Our vendored
/// iroh starts a changed relay's connection again, so a new token applies at
/// once (third_party/iroh/PATCHED.md). From or to `Disabled` changes
/// nothing: an endpoint bound without relays has no relay transport.
pub async fn change_relays(endpoint: &Endpoint, from: &RelayChoice, to: &RelayChoice) {
    if matches!(from, RelayChoice::Disabled) || matches!(to, RelayChoice::Disabled) {
        return;
    }
    let (before, after) = (from.configs(), to.configs());
    for config in &before {
        if !after.iter().any(|c| c.url == config.url) {
            endpoint.remove_relay(&config.url).await;
        }
    }
    for config in after {
        if !before.contains(&config) {
            endpoint
                .insert_relay(config.url.clone(), Arc::new(config))
                .await;
        }
    }
}

/// The token our relay asks of clients (`access.shared_token` in its
/// config). It ships inside every build, so it keeps other apps off the
/// relay rather than proving anything; still, it never reaches a log.
#[derive(Clone, PartialEq, Eq)]
pub struct RelayToken(String);

impl RelayToken {
    /// `None` for an empty token, which the relay refuses to start with.
    pub fn new(token: impl Into<String>) -> Option<Self> {
        let token = token.into();
        (!token.trim().is_empty()).then(|| Self(token.trim().to_string()))
    }

    /// The token itself, for handing it on (a child process's arguments);
    /// never for logs.
    pub fn reveal(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RelayToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("..")
    }
}

/// QUIC congestion controller. `Media` is ours (ADR 0005): it leaves
/// congestion control to the per-viewer rate controller. The others are
/// noq's, kept for comparison; Cubic is noq's default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Congestion {
    #[default]
    Media,
    Bbr3,
    Cubic,
    NewReno,
}

/// Decides, as soon as the TLS handshake names an incoming peer, whether its
/// connection may go on: `Some(close code)` refuses it before any of our
/// protocol runs. Used for admission backoff (`couch_room::limits`).
#[derive(Clone)]
pub struct Gate(pub Arc<dyn Fn(EndpointId) -> Option<u32> + Send + Sync>);

impl std::fmt::Debug for Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gate")
    }
}

impl EndpointHooks for Gate {
    async fn after_handshake(&self, conn: &Connection) -> AfterHandshakeOutcome {
        // Our own dials are not ours to refuse.
        if conn.side() == Side::Client {
            return AfterHandshakeOutcome::Accept;
        }
        match (self.0)(conn.remote_id()) {
            Some(code) => AfterHandshakeOutcome::Reject {
                error_code: VarInt::from_u32(code),
                reason: crate::admission::close_name(code).as_bytes().to_vec(),
            },
            None => AfterHandshakeOutcome::Accept,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EndpointOptions {
    pub secret_key: SecretKey,
    pub relays: RelayChoice,
    /// Refuses incoming connections early; `None` takes everyone to the
    /// next step.
    pub gate: Option<Gate>,
    /// Bind IP sockets to this port (IPv4 and IPv6) instead of a random one.
    pub port: Option<u16>,
    /// Use only the relay: no direct IP paths at all (the G6 relay test).
    pub relay_only: bool,
    pub congestion: Congestion,
    /// Route everything through the impairing test transport instead of IP
    /// and relays.
    #[cfg(feature = "netem")]
    pub netem: Option<Arc<crate::netem::NetemTransport>>,
}

impl EndpointOptions {
    pub fn new(secret_key: SecretKey, relays: RelayChoice) -> Self {
        Self {
            secret_key,
            relays,
            gate: None,
            port: None,
            relay_only: false,
            congestion: Congestion::Media,
            #[cfg(feature = "netem")]
            netem: None,
        }
    }
}

/// Starting QUIC settings from docs/protocol.md, tuned in M1.
pub fn transport_config(congestion: Congestion) -> QuicTransportConfig {
    let mut builder = QuicTransportConfig::builder()
        .max_idle_timeout(Some(
            Duration::from_secs(15)
                .try_into()
                .expect("15 s is a valid idle timeout"),
        ))
        .keep_alive_interval(Duration::from_secs(2))
        .max_concurrent_uni_streams(VarInt::from_u32(64))
        .stream_receive_window(VarInt::from_u32(8 << 20))
        .receive_window(VarInt::from_u32(16 << 20))
        .send_window(16 << 20)
        // Stale audio and input should drop, not queue.
        .datagram_send_buffer_size(64 * 1024);
    builder = match congestion {
        Congestion::Media => builder
            .congestion_controller_factory(Arc::new(crate::congestion::MediaCongestionConfig)),
        Congestion::Bbr3 => {
            builder.congestion_controller_factory(Arc::new(congestion::Bbr3Config::default()))
        }
        Congestion::Cubic => {
            builder.congestion_controller_factory(Arc::new(congestion::CubicConfig::default()))
        }
        Congestion::NewReno => {
            builder.congestion_controller_factory(Arc::new(congestion::NewRenoConfig::default()))
        }
    };
    builder.build()
}

/// Binds an endpoint that speaks the Couch Co-App ALPN.
pub async fn bind(options: EndpointOptions) -> Result<Endpoint, NetError> {
    // Never n0's preset: it publishes us to n0's DNS and looks peers up
    // there (M26a), which records and invites make unnecessary.
    let mut builder = Endpoint::builder(presets::Minimal)
        .relay_mode(options.relays.mode())
        .secret_key(options.secret_key)
        .alpns(vec![couch_proto::ALPN.to_vec()])
        .transport_config(transport_config(options.congestion));
    if let Some(gate) = options.gate {
        builder = builder.hooks(gate);
    }

    // The impaired link must be the only path, so nothing below (a fixed
    // port) may add IP transports back.
    #[cfg(feature = "netem")]
    if let Some(netem) = options.netem {
        return builder
            .clear_ip_transports()
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .add_custom_transport(netem)
            .bind()
            .await
            .map_err(|e| NetError::Bind(e.to_string()));
    }

    if options.relay_only {
        builder = builder.clear_ip_transports();
    } else if let Some(port) = options.port {
        builder = builder
            .clear_ip_transports()
            .bind_addr(format!("0.0.0.0:{port}"))
            .map_err(|e| NetError::Bind(e.to_string()))?
            .bind_addr(format!("[::]:{port}"))
            .map_err(|e| NetError::Bind(e.to_string()))?;
    }

    builder
        .bind()
        .await
        .map_err(|e| NetError::Bind(e.to_string()))
}

/// What [`probe`] found.
#[derive(Debug, Clone)]
pub struct NetProbe {
    pub endpoint_id: String,
    /// Time from bind until a relay accepted us, or `None` if that didn't
    /// happen within the timeout.
    pub online_after: Option<Duration>,
    pub relay_urls: Vec<String>,
    pub direct_addrs: usize,
}

/// What [`check_relay`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayCheck {
    /// It let us in: with its token, or open to everyone.
    Accepted,
    /// It refused the token, for this reason (the relay's own words).
    Refused(String),
    /// Nothing let us in within the time: no relay there, or it didn't
    /// answer. iroh's last error, if it had one.
    Unreachable(Option<String>),
}

/// Connects to `relay` alone, with its token, and says whether it lets us
/// in (M26a: a relay code is checked before it is saved). From a throwaway
/// endpoint with an id of its own: with ours, the relay would replace our
/// real connection with this one. Takes at most `timeout`.
pub async fn check_relay(relay: &Relay, timeout: Duration) -> Result<RelayCheck, NetError> {
    let mut options = EndpointOptions::new(
        SecretKey::generate(),
        RelayChoice::ours(vec![relay.clone()]),
    );
    options.relay_only = true;
    let endpoint = bind(options).await?;
    let mut status = endpoint.home_relay_status();
    let seen = tokio::time::timeout(timeout, async {
        loop {
            if let Some(ours) = status.get().iter().find(|s| *s.url() == relay.url) {
                if ours.is_connected() {
                    return RelayCheck::Accepted;
                }
                if let Some(reason) = ours.auth_denied_reason() {
                    return RelayCheck::Refused(reason.to_string());
                }
            }
            if status.updated().await.is_err() {
                return RelayCheck::Unreachable(None);
            }
        }
    })
    .await;
    let check = seen.unwrap_or_else(|_| {
        RelayCheck::Unreachable(
            status
                .get()
                .iter()
                .find_map(|s| s.last_error().map(|e| format!("{e:#}"))),
        )
    });
    endpoint.close().await;
    Ok(check)
}

/// Binds a throwaway endpoint and waits for it to come online.
pub async fn probe(relays: RelayChoice, timeout: Duration) -> Result<NetProbe, NetError> {
    let endpoint = bind(EndpointOptions::new(SecretKey::generate(), relays)).await?;
    let started = Instant::now();
    let online_after = tokio::time::timeout(timeout, endpoint.online())
        .await
        .ok()
        .map(|()| started.elapsed());
    let addr = endpoint.addr();
    let report = NetProbe {
        endpoint_id: endpoint.id().to_string(),
        online_after,
        relay_urls: addr.relay_urls().map(ToString::to_string).collect(),
        direct_addrs: addr.ip_addrs().count(),
    };
    endpoint.close().await;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn binds_without_relays() {
        let endpoint = bind(EndpointOptions::new(
            SecretKey::generate(),
            RelayChoice::Disabled,
        ))
        .await
        .unwrap();
        assert!(endpoint.addr().ip_addrs().count() > 0);
        endpoint.close().await;
    }

    #[test]
    fn a_relay_token_never_prints() {
        let token = RelayToken::new(" s3cret ").unwrap();
        let choice = RelayChoice::ours(vec![Relay::new(
            "https://relay.example.com".parse().unwrap(),
            Some(token.clone()),
        )]);
        assert!(!format!("{choice:?}").contains("s3cret"));
        assert_eq!(token.0, "s3cret");
        assert!(RelayToken::new("  ").is_none());
    }

    #[test]
    fn each_relay_gets_its_own_token_and_plain_http_skips_quic() {
        let https: RelayUrl = "https://relay.example.com".parse().unwrap();
        let other: RelayUrl = "https://friends.example.org".parse().unwrap();
        let http: RelayUrl = "http://192.168.1.10:3340".parse().unwrap();
        let choice = RelayChoice::ours(vec![
            Relay::new(https.clone(), RelayToken::new("t")),
            Relay::new(other.clone(), RelayToken::new("u")),
            Relay::new(http.clone(), None),
        ]);
        let RelayMode::Custom(map) = choice.mode() else {
            panic!("a custom map");
        };
        let ours = map.get(&https).unwrap();
        assert_eq!(ours.auth_token.as_deref(), Some("t"));
        assert!(ours.quic.is_some());
        assert_eq!(map.get(&other).unwrap().auth_token.as_deref(), Some("u"));
        let lan = map.get(&http).unwrap();
        assert!(lan.quic.is_none() && lan.auth_token.is_none());
        let stranger: RelayUrl = "https://someone.else".parse().unwrap();
        assert!(map.get(&stranger).is_none());
        assert!(!map.urls::<Vec<_>>().iter().any(is_public_relay));
    }

    #[test]
    fn the_public_relays_sit_beside_ours_without_a_token() {
        let ours: RelayUrl = "https://relay.example.com".parse().unwrap();
        let choice = RelayChoice::Relays {
            ours: vec![Relay::new(ours.clone(), RelayToken::new("t"))],
            public: true,
        };
        let RelayMode::Custom(map) = choice.mode() else {
            panic!("a custom map");
        };
        let public: Vec<RelayUrl> = map
            .urls::<Vec<_>>()
            .into_iter()
            .filter(is_public_relay)
            .collect();
        assert!(!public.is_empty());
        assert!(
            public
                .iter()
                .all(|url| map.get(url).unwrap().auth_token.is_none())
        );
        assert_eq!(map.len(), public.len() + 1);
        let RelayMode::Custom(alone) = RelayChoice::public().mode() else {
            panic!("a custom map");
        };
        assert_eq!(alone.len(), public.len());
    }

    #[tokio::test]
    async fn a_changed_relay_list_reaches_the_running_endpoint() {
        let a: RelayUrl = "http://127.0.0.1:1/".parse().unwrap();
        let b: RelayUrl = "http://127.0.0.1:2/".parse().unwrap();
        let from = RelayChoice::ours(vec![
            Relay::new(a.clone(), RelayToken::new("old")),
            Relay::new(b.clone(), None),
        ]);
        let endpoint = bind(EndpointOptions::new(SecretKey::generate(), from.clone()))
            .await
            .unwrap();
        let c: RelayUrl = "http://127.0.0.1:3/".parse().unwrap();
        let to = RelayChoice::ours(vec![
            Relay::new(a.clone(), RelayToken::new("new")),
            Relay::new(c.clone(), None),
        ]);
        change_relays(&endpoint, &from, &to).await;
        // What the endpoint holds now: inserting each again returns it.
        let held = |url: &RelayUrl| {
            let endpoint = endpoint.clone();
            let url = url.clone();
            async move {
                let config = endpoint.remove_relay(&url).await;
                if let Some(config) = &config {
                    endpoint.insert_relay(url, config.clone()).await;
                }
                config
            }
        };
        assert_eq!(held(&a).await.unwrap().auth_token.as_deref(), Some("new"));
        assert!(held(&b).await.is_none());
        assert!(held(&c).await.is_some());
        endpoint.close().await;
    }
}
