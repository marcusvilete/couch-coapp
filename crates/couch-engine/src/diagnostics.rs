//! What the user copies when asking someone for help with the connection
//! (M26a, Settings → Network → Copy diagnostics; [ADR 0014]). Anonymous by
//! the author's choice: versions, the build, each relay by its server's name
//! with its state and error, the network, and each room's members' links,
//! numbered. Never a name, an IP address, a token, a password or an invite:
//! it may be posted publicly (M26c's issues).
//!
//! [ADR 0014]: ../../../docs/adr/0014-public-builds-and-registered-relays.md

use std::fmt::Write;

use crate::{InternetView, MemberLink, NetworkTrouble, NetworkView, RelayStateView, RoomView};

/// What the text is made from.
pub(crate) struct Facts<'a> {
    pub app_version: &'a str,
    pub commit: Option<&'a str>,
    pub build: &'a str,
    pub system: String,
    pub wire: u16,
    pub network: &'a NetworkView,
    pub rooms: &'a [RoomView],
}

/// The diagnostics, as plain text.
pub(crate) fn text(facts: &Facts) -> String {
    let mut out = String::new();
    let n = facts.network;
    let _ = writeln!(
        out,
        "Couch Co-App {} ({}{}), {}, wire revision {}",
        facts.app_version,
        facts.build,
        facts
            .commit
            .map(|c| format!(", commit {}", &c[..c.len().min(12)]))
            .unwrap_or_default(),
        facts.system,
        facts.wire,
    );
    let _ = writeln!(
        out,
        "Network: {}; direct connections: {}",
        match n.internet {
            InternetView::Unknown => "not known yet",
            InternetView::Online => "online",
            InternetView::Offline => "offline",
            InternetView::SignIn => "behind a sign-in page",
        },
        match n.udp {
            None => "not known yet",
            Some(true) => "possible",
            Some(false) => "blocked (UDP)",
        },
    );
    let _ = writeln!(
        out,
        "Trouble: {}",
        match &n.trouble {
            None => "none".to_string(),
            Some(NetworkTrouble::NoInternet) => "no internet".to_string(),
            Some(NetworkTrouble::SignIn) => "sign-in page".to_string(),
            Some(NetworkTrouble::RelayRefused { host, .. }) => {
                format!("{} refuses our code", scrub(host))
            }
            Some(NetworkTrouble::NoRelay) => "no relay answers".to_string(),
        }
    );
    let _ = writeln!(out, "Relays:");
    if !n.changeable {
        let _ = writeln!(out, "  fixed at start");
    }
    for relay in &n.relays {
        let mut marks = Vec::new();
        if relay.built_in {
            marks.push("built in");
        }
        marks.push(if relay.has_code {
            "with a code"
        } else {
            "open"
        });
        if relay.personal {
            marks.push("personal");
        }
        let _ = writeln!(
            out,
            "  {} ({}): {}",
            scrub(&relay.host),
            marks.join(", "),
            state(&relay.state),
        );
    }
    let _ = writeln!(
        out,
        "  public relays: {}{}",
        match &n.public {
            Some(s) => format!("in use, {}", state(s)),
            None => "not in use".to_string(),
        },
        match (n.keep_public, n.public_fallback, n.falling_back) {
            (true, _, _) => "; kept on",
            (false, true, true) => "; standing in now",
            (false, true, false) => "; may stand in",
            (false, false, _) => "",
        },
    );
    let _ = writeln!(
        out,
        "  reachable through: {}",
        n.home_relay
            .as_deref()
            .map(|url| scrub(&host_of(url)))
            .unwrap_or_else(|| "nothing yet".to_string()),
    );
    let _ = writeln!(out, "Rooms: {}", facts.rooms.len());
    for (i, room) in facts.rooms.iter().enumerate() {
        let others: Vec<&str> = room
            .members
            .iter()
            .filter(|m| m.link != MemberLink::Me)
            .map(|m| match (m.link, &m.other_version) {
                (_, Some(v)) if v.newer => "on a newer version",
                (_, Some(_)) => "on an older version",
                (MemberLink::Direct, None) => "direct",
                (MemberLink::Relay, None) => "through a relay",
                (MemberLink::Online, None) => "online, not connected to us",
                (MemberLink::Offline | MemberLink::Me, None) => "offline",
            })
            .collect();
        let mut marks = Vec::new();
        if room.is_owner {
            marks.push("ours");
        }
        if room.catching_up {
            marks.push("catching up");
        }
        if !room.sessions.is_empty() {
            marks.push("in a session");
        }
        let _ = write!(out, "  room {}", i + 1);
        if !marks.is_empty() {
            let _ = write!(out, " ({})", marks.join(", "));
        }
        if others.is_empty() {
            let _ = writeln!(out, ": nobody else");
        } else {
            let _ = writeln!(out, ":");
            for (j, link) in others.iter().enumerate() {
                let _ = writeln!(out, "    member {}: {link}", j + 1);
            }
        }
    }
    out
}

fn state(state: &RelayStateView) -> String {
    match state {
        RelayStateView::Checking => "checking".into(),
        RelayStateView::Home => "connected, others reach us through it".into(),
        RelayStateView::Connected => "connected".into(),
        RelayStateView::Idle => "answers, not needed now".into(),
        RelayStateView::Unreachable { details } => match details {
            Some(details) => format!("unreachable: {}", scrub(details)),
            None => "unreachable".into(),
        },
        RelayStateView::Refused { reason } => format!("refuses our code: {}", scrub(reason)),
    }
}

fn host_of(url: &str) -> String {
    url.parse::<couch_net::RelayUrl>()
        .ok()
        .and_then(|u| u.host_str().map(|h| h.trim_end_matches('.').to_string()))
        .unwrap_or_else(|| url.to_string())
}

/// Takes IP addresses out of text from iroh or a relay (an error may name
/// the address it tried): each word that reads as one, with or without a
/// port or brackets, becomes `<address>`.
pub(crate) fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        // The word as it is (`::1`), then without the colons before it
        // (`host:10.0.0.5`, whose `host` ended the word before).
        let colons = word.len() - word.trim_start_matches(':').len();
        for start in [0, colons] {
            let from = &word[start..];
            // A sentence's last dot or colon isn't part of it.
            let core = from.trim_end_matches(['.', ':']);
            let hit = if is_address(core) {
                Some(core.len())
            } else if is_address(from) {
                // One ending in `::`.
                Some(from.len())
            } else {
                None
            };
            if let Some(len) = hit {
                out.push_str(&word[..start]);
                out.push_str("<address>");
                out.push_str(&from[len..]);
                word.clear();
                return;
            }
        }
        out.push_str(word);
        word.clear();
    };
    for c in text.chars() {
        // What an address is made of, with a port and IPv6's brackets and
        // zone; anything else ends a word.
        if c.is_ascii_hexdigit() || matches!(c, '.' | ':' | '[' | ']' | '%') {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

fn is_address(word: &str) -> bool {
    // `::` alone (a path's separator) reads as IPv6's unspecified address.
    if !word.chars().any(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    if word.parse::<std::net::IpAddr>().is_ok() || word.parse::<std::net::SocketAddr>().is_ok() {
        return true;
    }
    let bare = word.trim_start_matches('[');
    let bare = bare.split(']').next().unwrap_or(bare);
    let bare = bare.split('%').next().unwrap_or(bare);
    // `[192.168.1.20]:3340` too.
    bare.parse::<std::net::IpAddr>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemberView, RelayView};

    #[test]
    fn addresses_are_taken_out() {
        assert_eq!(
            scrub("failed to connect to 192.168.1.20:3340: refused"),
            "failed to connect to <address>: refused"
        );
        assert_eq!(
            scrub("no route to [2001:db8::1]:443."),
            "no route to <address>."
        );
        assert_eq!(scrub("fe80::1 timed out"), "<address> timed out");
        // After a word's colon, and in brackets (the review's cases).
        assert_eq!(scrub("host:192.168.1.2:443 down"), "host:<address> down");
        assert_eq!(scrub("error:10.0.0.5"), "error:<address>");
        assert_eq!(scrub("at [192.168.1.20]:3340."), "at <address>.");
        assert_eq!(scrub("::1 refused"), "<address> refused");
        // Paths in error texts aren't addresses.
        assert_eq!(scrub("iroh::magicsock: gone"), "iroh::magicsock: gone");
        assert_eq!(scrub("dns error at 10.0.0.1."), "dns error at <address>.");
        // Words that only look like parts of one stay.
        assert_eq!(
            scrub("TLS 1.3 handshake: deadbeef"),
            "TLS 1.3 handshake: deadbeef"
        );
        assert_eq!(scrub("relay.example.com"), "relay.example.com");
    }

    fn member(name: &str, link: MemberLink) -> MemberView {
        MemberView {
            id: format!("{name}-id"),
            name: name.into(),
            is_owner: false,
            link,
            other_version: None,
            remote: None,
        }
    }

    #[test]
    fn nothing_names_anyone() {
        let network = NetworkView {
            relays: vec![RelayView {
                url: "https://relay.example.com/".into(),
                host: "relay.example.com".into(),
                built_in: true,
                has_code: true,
                personal: false,
                state: RelayStateView::Unreachable {
                    details: Some("timed out reaching 203.0.113.7:443".into()),
                },
            }],
            public_in_use: false,
            keep_public: false,
            public_fallback: true,
            falling_back: false,
            public: None,
            home_relay: Some("https://relay.example.com/".into()),
            internet: InternetView::Online,
            udp: Some(false),
            trouble: Some(NetworkTrouble::NoRelay),
            test_build: false,
            changeable: true,
            ask_for_relay: false,
        };
        let rooms = vec![RoomView {
            id: "secret-room-id".into(),
            revision: 1,
            name: "Friday games".into(),
            is_owner: true,
            members: vec![
                member("Me Myself", MemberLink::Me),
                member("Bia", MemberLink::Relay),
                member("Caio", MemberLink::Offline),
            ],
            joins_paused_for_secs: None,
            sessions: vec![],
            catching_up: false,
            waiting_for: vec![],
            needs_password: false,
            offering_to: None,
            offered: None,
            can_offer: true,
        }];
        let text = text(&Facts {
            app_version: "0.35.0",
            commit: Some("0123456789abcdef"),
            build: "release build",
            system: "windows x86_64".into(),
            wire: 18,
            network: &network,
            rooms: &rooms,
        });
        for secret in [
            "Friday",
            "Bia",
            "Caio",
            "Myself",
            "secret-room-id",
            "-id",
            "203.0.113",
        ] {
            assert!(!text.contains(secret), "{secret:?} in {text}");
        }
        for said in [
            "0.35.0 (release build, commit 0123456789ab)",
            "relay.example.com (built in, with a code): unreachable: timed out reaching <address>",
            "direct connections: blocked (UDP)",
            "Trouble: no relay answers",
            "public relays: not in use; may stand in",
            "room 1 (ours):",
            "member 1: through a relay",
            "member 2: offline",
        ] {
            assert!(text.contains(said), "{said:?} not in {text}");
        }
    }
}
