//! Converting between iroh's endpoint addresses and the `PeerAddr` members
//! sign into their room records and invites.

use std::str::FromStr;

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

/// Where to dial `member`. Unparsable parts are skipped; `None` if the id
/// isn't a valid key.
pub fn endpoint_addr(member: MemberId, addr: &PeerAddr) -> Option<EndpointAddr> {
    let id = EndpointId::from_bytes(&member.0).ok()?;
    let mut out = EndpointAddr::new(id);
    if let Some(url) = addr
        .relay
        .as_deref()
        .and_then(|u| RelayUrl::from_str(u).ok())
    {
        out = out.with_relay_url(url);
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
        let back = endpoint_addr(member_id(id), &peer).unwrap();
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
        let addr = endpoint_addr(member_id(id), &peer).unwrap();
        assert_eq!(addr.relay_urls().count(), 0);
    }
}
