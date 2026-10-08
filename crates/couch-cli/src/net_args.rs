//! Network flags shared by `host`, `view` and `latency-probe`.

use std::{net::SocketAddr, sync::Arc};

use anyhow::{Context, bail};
use clap::{Args, ValueEnum};
use couch_net::{
    EndpointAddr, SecretKey,
    endpoint::{Congestion, EndpointOptions, RelayChoice, RelayToken},
    netem::{NetemConfig, NetemTransport},
};

/// QUIC congestion controller: `media` (ours, the default) or one of noq's.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CongestionArg {
    Media,
    Bbr3,
    Cubic,
    Newreno,
}

#[derive(Debug, Clone, Args)]
pub struct NetArgs {
    /// Relays: `n0` (iroh's public relays, development only), `none`, or a relay URL.
    #[arg(long, default_value = "n0")]
    pub relay: String,
    /// The access token our relay asks for (`release.toml`'s `relay_token`),
    /// sent only to the `--relay` URL. Other users of this PC can see a
    /// command line, and `latency-probe` passes it on to its children.
    #[arg(long, value_parser = parse_token)]
    pub relay_token: Option<RelayToken>,
    /// Use only the relay, no direct paths (the G6 test).
    #[arg(long)]
    pub relay_only: bool,
    /// Bind to this UDP port instead of a random one.
    #[arg(long)]
    pub port: Option<u16>,
    #[arg(long, value_enum, default_value = "media")]
    pub congestion: CongestionArg,
    /// Impair this side's sending direction, e.g. `delay=20ms,loss=1%,rate=20mbit`.
    /// Replaces IP and relays with a loopback-only test transport.
    #[arg(long)]
    pub netem: Option<String>,
}

impl NetArgs {
    /// Options for binding, plus the netem transport if one is in use.
    pub fn options(&self) -> anyhow::Result<(EndpointOptions, Option<Arc<NetemTransport>>)> {
        let relays = match self.relay.as_str() {
            "n0" => RelayChoice::N0Public,
            "none" => RelayChoice::Disabled,
            url => RelayChoice::Custom {
                urls: vec![url.parse().context("relay URL")?],
                token: self.relay_token.clone(),
            },
        };
        if self.relay_token.is_some() && !matches!(relays, RelayChoice::Custom { .. }) {
            bail!("--relay-token goes with a relay URL in --relay");
        }
        let mut options = EndpointOptions::new(SecretKey::generate(), relays);
        options.relay_only = self.relay_only;
        options.port = self.port;
        options.congestion = match self.congestion {
            CongestionArg::Media => Congestion::Media,
            CongestionArg::Bbr3 => Congestion::Bbr3,
            CongestionArg::Cubic => Congestion::Cubic,
            CongestionArg::Newreno => Congestion::NewReno,
        };
        let netem = match &self.netem {
            None => None,
            Some(profile) => {
                if self.relay_only {
                    bail!("--netem and --relay-only can't be combined");
                }
                let config: NetemConfig = profile.parse().map_err(anyhow::Error::msg)?;
                let bind = SocketAddr::from(([127, 0, 0, 1], self.port.unwrap_or(0)));
                let transport = NetemTransport::new(bind, config);
                options.netem = Some(transport.clone());
                Some(transport)
            }
        };
        Ok((options, netem))
    }

    /// The flags to pass on to a child `host` or `view` process.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = vec!["--relay".into(), self.relay.clone()];
        if let Some(token) = &self.relay_token {
            args.push("--relay-token".into());
            args.push(token.reveal().into());
        }
        if self.relay_only {
            args.push("--relay-only".into());
        }
        args.push("--congestion".into());
        let congestion = self
            .congestion
            .to_possible_value()
            .expect("no skipped variants");
        args.push(congestion.get_name().into());
        if let Some(profile) = &self.netem {
            args.push("--netem".into());
            args.push(profile.clone());
        }
        args
    }
}

fn parse_token(text: &str) -> Result<RelayToken, String> {
    RelayToken::new(text).ok_or_else(|| "the relay token is empty".into())
}

/// The address viewers should dial to reach this endpoint.
pub async fn dial_addr(
    endpoint: &couch_net::Endpoint,
    netem: Option<&NetemTransport>,
    uses_relays: bool,
) -> anyhow::Result<EndpointAddr> {
    if let Some(netem) = netem {
        let socket = netem.local_addr().context("netem transport not bound")?;
        return Ok(couch_net::netem::endpoint_addr(endpoint.id(), socket));
    }
    if uses_relays {
        // Wait (briefly) for a relay so the address is reachable beyond the LAN.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), endpoint.online()).await;
    }
    Ok(endpoint.addr())
}

impl NetArgs {
    pub fn uses_relays(&self) -> bool {
        self.relay != "none" && self.netem.is_none()
    }
}
