//! Building the iroh endpoint every install runs.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use iroh::{
    Endpoint, EndpointId, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey,
    endpoint::{
        AfterHandshakeOutcome, Connection, EndpointHooks, QuicTransportConfig, Side, VarInt,
        presets,
    },
};
use noq_proto::congestion;

use crate::NetError;

/// Which relays the endpoint registers with.
#[derive(Debug, Clone)]
pub enum RelayChoice {
    /// iroh's public relays and address lookup. Development only: their terms
    /// cover hobby use, with unpublished rate limits.
    N0Public,
    /// Only these relays and no third-party address lookup. Production uses
    /// our own `iroh-relay` (docs/architecture.md, M7), with its token.
    Custom {
        urls: Vec<RelayUrl>,
        /// Sent to these relays only: iroh never sends it to a relay outside
        /// its own list, such as another member's.
        token: Option<RelayToken>,
    },
    /// No relays: direct and LAN paths only. For tests.
    Disabled,
}

impl RelayChoice {
    /// These relays, without a token.
    pub fn custom(urls: Vec<RelayUrl>) -> Self {
        Self::Custom { urls, token: None }
    }

    fn mode(&self) -> RelayMode {
        match self {
            Self::N0Public => RelayMode::Default,
            Self::Custom { urls, token } => RelayMode::Custom(
                urls.iter()
                    .map(|url| {
                        // Plain http is a relay on this PC or the LAN (`--dev`),
                        // which has no QUIC address discovery.
                        let config = if url.scheme() == "https" {
                            RelayConfig::from(url.clone())
                        } else {
                            RelayConfig::new(url.clone(), None)
                        };
                        match token {
                            Some(token) => config.with_auth_token(token.0.clone()),
                            None => config,
                        }
                    })
                    .collect::<RelayMap>(),
            ),
            Self::Disabled => RelayMode::Disabled,
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
    let mut builder = match &options.relays {
        RelayChoice::N0Public => Endpoint::builder(presets::N0),
        other => Endpoint::builder(presets::Minimal).relay_mode(other.mode()),
    };
    builder = builder
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
        let choice = RelayChoice::Custom {
            urls: vec!["https://relay.example.com".parse().unwrap()],
            token: Some(token.clone()),
        };
        assert!(!format!("{choice:?}").contains("s3cret"));
        assert_eq!(token.0, "s3cret");
        assert!(RelayToken::new("  ").is_none());
    }

    #[test]
    fn the_token_goes_to_our_relays_only_and_plain_http_skips_quic() {
        let https: RelayUrl = "https://relay.example.com".parse().unwrap();
        let http: RelayUrl = "http://192.168.1.10:3340".parse().unwrap();
        let choice = RelayChoice::Custom {
            urls: vec![https.clone(), http.clone()],
            token: RelayToken::new("t"),
        };
        let RelayMode::Custom(map) = choice.mode() else {
            panic!("a custom map");
        };
        let ours = map.get(&https).unwrap();
        assert_eq!(ours.auth_token.as_deref(), Some("t"));
        assert!(ours.quic.is_some());
        assert!(map.get(&http).unwrap().quic.is_none());
        let other: RelayUrl = "https://someone.else".parse().unwrap();
        assert!(map.get(&other).is_none());
    }
}
