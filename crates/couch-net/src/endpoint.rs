//! Building the iroh endpoint every install runs.

use std::time::{Duration, Instant};

use iroh::{Endpoint, RelayMode, RelayUrl, SecretKey, endpoint::presets};

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

#[derive(Debug, Clone)]
pub struct EndpointOptions {
    pub secret_key: SecretKey,
    pub relays: RelayChoice,
}

/// Binds an endpoint that speaks the Couch Co-App ALPN.
pub async fn bind(options: EndpointOptions) -> Result<Endpoint, NetError> {
    let builder = match options.relays {
        RelayChoice::N0Public => Endpoint::builder(presets::N0),
        RelayChoice::Custom(urls) => {
            Endpoint::builder(presets::Minimal).relay_mode(RelayMode::custom(urls))
        }
        RelayChoice::Disabled => {
            Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
        }
    };
    builder
        .secret_key(options.secret_key)
        .alpns(vec![couch_proto::ALPN.to_vec()])
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
    let endpoint = bind(EndpointOptions {
        secret_key: SecretKey::generate(),
        relays,
    })
    .await?;
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
        let endpoint = bind(EndpointOptions {
            secret_key: SecretKey::generate(),
            relays: RelayChoice::Disabled,
        })
        .await
        .unwrap();
        assert!(endpoint.addr().ip_addrs().count() > 0);
        endpoint.close().await;
    }
}
