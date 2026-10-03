//! Building the iroh endpoint every install runs.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use iroh::{
    Endpoint, RelayMode, RelayUrl, SecretKey,
    endpoint::{QuicTransportConfig, VarInt, presets},
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
    /// our own `iroh-relay` (docs/architecture.md, M7).
    Custom(Vec<RelayUrl>),
    /// No relays: direct and LAN paths only. For tests.
    Disabled,
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

#[derive(Debug, Clone)]
pub struct EndpointOptions {
    pub secret_key: SecretKey,
    pub relays: RelayChoice,
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
        RelayChoice::Custom(urls) => {
            Endpoint::builder(presets::Minimal).relay_mode(RelayMode::custom(urls.clone()))
        }
        RelayChoice::Disabled => {
            Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
        }
    };
    builder = builder
        .secret_key(options.secret_key)
        .alpns(vec![couch_proto::ALPN.to_vec()])
        .transport_config(transport_config(options.congestion));

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
}
