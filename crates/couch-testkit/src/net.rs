//! UDP ports for engines in tests.

use std::{
    net::UdpSocket,
    sync::atomic::{AtomicU16, Ordering},
};

/// Below every system's ephemeral range (Linux 32768–60999, Windows
/// 49152–65535), so the OS never hands one of these out by itself.
const FIRST: u16 = 20_000;
const END: u16 = 32_000;

/// A UDP port free on both IPv4 and IPv6, never handed out twice in this
/// process, for an engine that may restart on it.
///
/// Ports come from a counter in a range the OS never picks for `bind(0)`:
/// probing with port 0 instead could be given a port another test's engine
/// just let go of while restarting on it, and that restart would then fail to
/// bind. Each process starts at its own place, so two test processes rarely
/// meet. The IPv4 probe closes before the IPv6 one binds: on Linux an IPv6
/// socket takes IPv4 too, and would collide with it.
pub fn free_udp_port() -> u16 {
    static NEXT: AtomicU16 = AtomicU16::new(0);
    let span = END - FIRST;
    let start = (std::process::id() % u32::from(span)) as u16;
    for _ in 0..span {
        let offset = NEXT.fetch_add(1, Ordering::Relaxed);
        let port = FIRST + (start + offset % span) % span;
        // Its own statement, so the IPv4 socket is closed before the IPv6 bind.
        let v4_free = UdpSocket::bind(("0.0.0.0", port)).is_ok();
        if v4_free && UdpSocket::bind(("::", port)).is_ok() {
            return port;
        }
    }
    panic!("no UDP port free on both IPv4 and IPv6 (is IPv6 off?)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_are_never_handed_out_twice() {
        let ports: Vec<u16> = (0..50).map(|_| free_udp_port()).collect();
        let mut sorted = ports.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ports.len());
        assert!(ports.iter().all(|p| (FIRST..END).contains(p)));
    }
}
