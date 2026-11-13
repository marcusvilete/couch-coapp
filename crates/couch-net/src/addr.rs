//! Converting between iroh's endpoint addresses and the `PeerAddr` members
//! sign into their room records and invites, and which relay addresses from
//! others we dial (M26a).

use std::{collections::BTreeSet, net::IpAddr, str::FromStr};

use couch_proto::{ids::MemberId, room::PeerAddr};
use iroh::{EndpointAddr, EndpointId, RelayUrl};

/// Our addresses as a record carries them: the first relay, and direct
/// addresses IPv4 first in a stable order, so a reordering alone isn't a
/// change worth announcing.
pub fn peer_addr(addr: &EndpointAddr) -> PeerAddr {
    let mut ips: Vec<_> = addr.ip_addrs().copied().collect();
    ips.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    ips.dedup();
    PeerAddr {
        relay: addr.relay_urls().next().map(ToString::to_string),
        ips,
    }
}

/// Which relay addresses that came from others (a member's record, an
/// invite, a received code) may be dialed (M26a, ADR 0014). In a release
/// build only https to a name: never `localhost`, an IP address or a name
/// without a dot (a LAN's), so a member can't make our app send requests
/// into its own network. Our own relays are always fine, and development
/// and test builds keep LAN relays.
#[derive(Debug, Clone, Default)]
pub struct DialRules {
    /// LAN relays allowed: a development or test build.
    pub lan: bool,
    /// The relays we registered ourselves.
    pub ours: BTreeSet<RelayUrl>,
}

impl DialRules {
    pub fn allows(&self, url: &RelayUrl) -> bool {
        if self.lan || self.ours.contains(url) {
            return true;
        }
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
            return false;
        }
        let Some(host) = url.host_str() else {
            return false;
        };
        let name = host.trim_end_matches('.').to_ascii_lowercase();
        let bare = name.trim_start_matches('[').trim_end_matches(']');
        !name.is_empty()
            && bare.parse::<IpAddr>().is_err()
            && name != "localhost"
            && !name.ends_with(".localhost")
            && name.contains('.')
    }
}

/// Where to dial `member`. Unparsable parts are skipped, and so is a relay
/// `rules` don't allow (its direct addresses are still dialed); `None` if
/// the id isn't a valid key.
pub fn endpoint_addr(member: MemberId, addr: &PeerAddr, rules: &DialRules) -> Option<EndpointAddr> {
    let id = EndpointId::from_bytes(&member.0).ok()?;
    let mut out = EndpointAddr::new(id);
    if let Some(url) = addr
        .relay
        .as_deref()
        .and_then(|u| RelayUrl::from_str(u).ok())
    {
        if rules.allows(&url) {
            out = out.with_relay_url(url);
        } else {
            tracing::debug!(%url, "not dialing a relay address this build doesn't take from others");
        }
    }
    for ip in &addr.ips {
        out = out.with_ip_addr(*ip);
    }
    Some(out)
}

pub fn member_id(id: EndpointId) -> MemberId {
    MemberId(*id.as_bytes())
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    #[test]
    fn roundtrip_and_stable_order() {
        let id = SecretKey::generate().public();
        let relay: RelayUrl = "https://relay.example.com./".parse().unwrap();
        let addr = EndpointAddr::new(id)
            .with_relay_url(relay.clone())
            .with_ip_addr("[fe80::1]:5000".parse().unwrap())
            .with_ip_addr("192.168.0.9:5000".parse().unwrap());
        let peer = peer_addr(&addr);
        assert_eq!(peer.relay.as_deref(), Some(relay.to_string().as_str()));
        assert!(peer.ips[0].is_ipv4(), "IPv4 first");
        let back = endpoint_addr(member_id(id), &peer, &DialRules::default()).unwrap();
        assert_eq!(back.id, id);
        assert_eq!(back.relay_urls().next(), Some(&relay));
        assert_eq!(back.ip_addrs().count(), 2);
    }

    #[test]
    fn junk_is_skipped_not_fatal() {
        let id = SecretKey::generate().public();
        let peer = PeerAddr {
            relay: Some("not a url".into()),
            ips: vec![],
        };
        let addr = endpoint_addr(member_id(id), &peer, &DialRules::default()).unwrap();
        assert_eq!(addr.relay_urls().count(), 0);
    }

    #[test]
    fn a_release_build_dials_others_relays_only_as_https_to_a_name() {
        let release = DialRules::default();
        let url = |u: &str| u.parse::<RelayUrl>().unwrap();
        for fine in [
            "https://relay.example.com",
            "https://relay.example.com./",
            "https://use1-1.relay.n0.iroh.link./",
        ] {
            assert!(release.allows(&url(fine)), "{fine}");
        }
        for never in [
            "http://relay.example.com",
            "https://localhost",
            "https://relay.localhost",
            "https://192.168.1.1",
            "https://10.0.0.5:3340",
            "https://[::1]",
            "https://[fe80::1]",
            "https://router",
            "https://8.8.8.8",
            "http://127.0.0.1:3340",
            "https://user:pass@relay.example.com",
        ] {
            assert!(!release.allows(&url(never)), "{never}");
        }
        // Our own LAN relay stays ours.
        let ours = DialRules {
            lan: false,
            ours: [url("http://192.168.1.10:3340")].into(),
        };
        assert!(ours.allows(&url("http://192.168.1.10:3340")));
        assert!(!ours.allows(&url("http://192.168.1.11:3340")));
        // Development and test builds keep LAN relays.
        let dev = DialRules {
            lan: true,
            ours: BTreeSet::new(),
        };
        assert!(dev.allows(&url("http://192.168.1.11:3340")));

        // A record naming a refused relay is still dialed at its IPs.
        let id = SecretKey::generate().public();
        let peer = PeerAddr {
            relay: Some("http://192.168.1.1:3340".into()),
            ips: vec!["192.168.1.20:5000".parse().unwrap()],
        };
        let addr = endpoint_addr(member_id(id), &peer, &release).unwrap();
        assert_eq!(addr.relay_urls().count(), 0);
        assert_eq!(addr.ip_addrs().count(), 1);
    }
}
