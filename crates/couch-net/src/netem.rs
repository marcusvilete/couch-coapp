//! A UDP transport that impairs what it sends: added delay, random loss, a
//! bandwidth cap with a bottleneck queue, and timed blackouts or rate changes.
//! It lets rate control be tested on one machine without admin rights.
//!
//! An endpoint whose only transport is this one (IP transports cleared,
//! relays off) has exactly one possible path, so every packet is impaired.
//! Each side impairs its own sending direction.

use std::{
    collections::VecDeque,
    fmt, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    str::FromStr,
    sync::{Arc, Mutex, OnceLock},
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use iroh::endpoint::transports::{
    CustomEndpoint, CustomSender, CustomTransport, RecvInfo, Transmit,
};
use iroh_base::{CustomAddr, EndpointAddr, EndpointId, TransportAddr};
use tokio::{sync::mpsc, time::Instant};

/// Transport id carried in [`CustomAddr`]s of this transport.
pub const NETEM_TRANSPORT_ID: u64 = 0xC0C0_0001;

/// A timed change to the link.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// Drop everything for this long.
    Blackout(Duration),
    /// Change the bandwidth cap (`None` removes it).
    Rate(Option<u64>),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetemConfig {
    pub delay: Duration,
    /// Probability of dropping a packet, 0.0 to 1.0.
    pub loss: f64,
    /// Bandwidth cap in bits per second.
    pub rate_bps: Option<u64>,
    /// Bottleneck queue length, as time at the capped rate; packets beyond it drop.
    pub queue: Duration,
    /// Changes at times after the transport starts.
    pub schedule: Vec<(Duration, Change)>,
}

impl FromStr for NetemConfig {
    type Err = String;

    /// `delay=20ms,loss=1%,rate=20mbit,queue=100ms,at10s=blackout:2s,at10s=rate:5mbit`
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut config = NetemConfig {
            queue: Duration::from_millis(100),
            ..Default::default()
        };
        for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (key, value) = part
                .split_once('=')
                .ok_or(format!("expected key=value: {part}"))?;
            match key {
                "delay" => config.delay = duration(value)?,
                "loss" => config.loss = percent(value)?,
                "rate" => config.rate_bps = Some(bitrate(value)?),
                "queue" => config.queue = duration(value)?,
                at if at.starts_with("at") => {
                    let when = duration(&at[2..])?;
                    let (kind, arg) = value
                        .split_once(':')
                        .ok_or(format!("expected change:value in {part}"))?;
                    let change = match kind {
                        "blackout" => Change::Blackout(duration(arg)?),
                        "rate" if arg == "none" => Change::Rate(None),
                        "rate" => Change::Rate(Some(bitrate(arg)?)),
                        other => return Err(format!("unknown change {other}")),
                    };
                    config.schedule.push((when, change));
                }
                other => return Err(format!("unknown netem key {other}")),
            }
        }
        Ok(config)
    }
}

fn duration(s: &str) -> Result<Duration, String> {
    let (num, scale) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1e-3)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1.0)
    } else {
        return Err(format!("duration needs ms or s: {s}"));
    };
    let v: f64 = num.parse().map_err(|_| format!("bad duration {s}"))?;
    Ok(Duration::from_secs_f64(v * scale))
}

fn percent(s: &str) -> Result<f64, String> {
    let v: f64 = s
        .trim_end_matches('%')
        .parse()
        .map_err(|_| format!("bad percentage {s}"))?;
    Ok(v / 100.0)
}

fn bitrate(s: &str) -> Result<u64, String> {
    let lower = s.to_ascii_lowercase();
    let (num, scale) = if let Some(n) = lower.strip_suffix("mbit") {
        (n.to_string(), 1e6)
    } else if let Some(n) = lower.strip_suffix("kbit") {
        (n.to_string(), 1e3)
    } else {
        return Err(format!("bitrate needs mbit or kbit: {s}"));
    };
    let v: f64 = num.parse().map_err(|_| format!("bad bitrate {s}"))?;
    Ok((v * scale) as u64)
}

/// Encodes a socket address as a [`CustomAddr`] of this transport.
pub fn custom_addr(addr: SocketAddr) -> CustomAddr {
    let mut data = Vec::with_capacity(18);
    match addr.ip() {
        IpAddr::V4(ip) => data.extend_from_slice(&ip.octets()),
        IpAddr::V6(ip) => data.extend_from_slice(&ip.octets()),
    }
    data.extend_from_slice(&addr.port().to_be_bytes());
    CustomAddr::from_parts(NETEM_TRANSPORT_ID, &data)
}

fn socket_addr(addr: &CustomAddr) -> io::Result<SocketAddr> {
    if addr.id() != NETEM_TRANSPORT_ID {
        return Err(io::Error::other("not a netem address"));
    }
    let data = addr.data();
    let (ip, port): (IpAddr, &[u8]) = match data.len() {
        6 => (
            Ipv4Addr::from(<[u8; 4]>::try_from(&data[..4]).unwrap()).into(),
            &data[4..],
        ),
        18 => (
            Ipv6Addr::from(<[u8; 16]>::try_from(&data[..16]).unwrap()).into(),
            &data[16..],
        ),
        _ => return Err(io::Error::other("bad netem address length")),
    };
    Ok(SocketAddr::new(ip, u16::from_be_bytes([port[0], port[1]])))
}

/// The address another endpoint dials to reach `id` through this transport.
pub fn endpoint_addr(id: EndpointId, socket: SocketAddr) -> EndpointAddr {
    EndpointAddr::from_parts(id, [TransportAddr::Custom(custom_addr(socket))])
}

/// The transport factory handed to the endpoint builder.
pub struct NetemTransport {
    bind: SocketAddr,
    config: NetemConfig,
    bound: Arc<OnceLock<SocketAddr>>,
    /// When the link started; scheduled changes count from here.
    started: OnceLock<Instant>,
}

impl fmt::Debug for NetemTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetemTransport")
            .field("bind", &self.bind)
            .field("config", &self.config)
            .finish()
    }
}

impl NetemTransport {
    pub fn new(bind: SocketAddr, config: NetemConfig) -> Arc<Self> {
        Arc::new(Self {
            bind,
            config,
            bound: Arc::new(OnceLock::new()),
            started: OnceLock::new(),
        })
    }

    /// The local socket address, once the endpoint has bound.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.bound.get().copied()
    }

    /// Time since the link started, which the schedule counts from. Lets a
    /// trace place `at10s=...` changes exactly.
    pub fn elapsed(&self) -> Option<Duration> {
        self.started.get().map(Instant::elapsed)
    }
}

impl CustomTransport for NetemTransport {
    fn bind(&self) -> io::Result<Box<dyn CustomEndpoint>> {
        let std_socket = std::net::UdpSocket::bind(self.bind)?;
        std_socket.set_nonblocking(true)?;
        let socket = Arc::new(tokio::net::UdpSocket::from_std(std_socket)?);
        let local = socket.local_addr()?;
        let _ = self.bound.set(local);
        let started = *self.started.get_or_init(Instant::now);
        let schedule = Arc::new(Mutex::new(Schedule::new(&self.config, started)));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run_link(
            rx,
            socket.clone(),
            self.config.clone(),
            schedule.clone(),
        ));
        Ok(Box::new(NetemEndpoint {
            socket,
            local: custom_addr(local),
            addrs: n0_watcher::Watchable::new(vec![custom_addr(local)]),
            tx,
            schedule,
        }))
    }
}

struct NetemEndpoint {
    socket: Arc<tokio::net::UdpSocket>,
    local: CustomAddr,
    addrs: n0_watcher::Watchable<Vec<CustomAddr>>,
    tx: mpsc::UnboundedSender<(SocketAddr, Bytes)>,
    /// Shared with the link task: a blackout cuts receiving as well, so one
    /// side's schedule can black out the whole path.
    schedule: Arc<Mutex<Schedule>>,
}

impl fmt::Debug for NetemEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetemEndpoint")
            .field("local", &self.local)
            .finish()
    }
}

impl CustomEndpoint for NetemEndpoint {
    fn watch_local_addrs(&self) -> n0_watcher::Direct<Vec<CustomAddr>> {
        self.addrs.watch()
    }

    fn create_sender(&self) -> Arc<dyn CustomSender> {
        Arc::new(NetemSender {
            tx: self.tx.clone(),
        })
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context,
        bufs: &mut [io::IoSliceMut<'_>],
        metas: &mut [noq_udp::RecvMeta],
        recv_infos: &mut [RecvInfo],
    ) -> Poll<io::Result<usize>> {
        if bufs.is_empty() {
            return Poll::Ready(Ok(0));
        }
        loop {
            let mut buf = tokio::io::ReadBuf::new(&mut bufs[0]);
            match self.socket.poll_recv_from(cx, &mut buf) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(from)) => {
                    if self.schedule.lock().unwrap().at(Instant::now()).0 {
                        continue; // dropped: the link is blacked out
                    }
                    let len = buf.filled().len();
                    metas[0].len = len;
                    metas[0].stride = len;
                    recv_infos[0] = RecvInfo::new(custom_addr(from), Some(self.local.clone()));
                    return Poll::Ready(Ok(1));
                }
            }
        }
    }
}

struct NetemSender {
    tx: mpsc::UnboundedSender<(SocketAddr, Bytes)>,
}

impl fmt::Debug for NetemSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NetemSender")
    }
}

impl CustomSender for NetemSender {
    fn is_valid_send_addr(&self, addr: &CustomAddr) -> bool {
        addr.id() == NETEM_TRANSPORT_ID
    }

    fn poll_send(
        &self,
        _cx: &mut Context,
        dst: &CustomAddr,
        _src: Option<&CustomAddr>,
        transmit: &Transmit<'_>,
    ) -> Poll<io::Result<()>> {
        let dst = socket_addr(dst)?;
        let segment = transmit
            .segment_size
            .unwrap_or(transmit.contents.len())
            .max(1);
        for chunk in transmit.contents.chunks(segment) {
            // The link task owns all impairment; a closed link means shutdown.
            let _ = self.tx.send((dst, Bytes::copy_from_slice(chunk)));
        }
        Poll::Ready(Ok(()))
    }
}

/// The timed changes and the link state they produce.
struct Schedule {
    pending: VecDeque<(Instant, Change)>,
    blackout_until: Option<Instant>,
    rate_bps: Option<u64>,
}

impl Schedule {
    fn new(config: &NetemConfig, started: Instant) -> Self {
        let mut changes = config.schedule.clone();
        changes.sort_by_key(|(at, _)| *at);
        Self {
            pending: changes
                .into_iter()
                .map(|(at, c)| (started + at, c))
                .collect(),
            blackout_until: None,
            rate_bps: config.rate_bps,
        }
    }

    /// Applies changes that are due and returns (blacked out, rate cap).
    fn at(&mut self, now: Instant) -> (bool, Option<u64>) {
        while self.pending.front().is_some_and(|(at, _)| *at <= now) {
            let (at, change) = self.pending.pop_front().unwrap();
            match change {
                Change::Blackout(d) => self.blackout_until = Some(at + d),
                Change::Rate(r) => self.rate_bps = r,
            }
        }
        (
            self.blackout_until.is_some_and(|until| now < until),
            self.rate_bps,
        )
    }
}

async fn run_link(
    mut rx: mpsc::UnboundedReceiver<(SocketAddr, Bytes)>,
    socket: Arc<tokio::net::UdpSocket>,
    config: NetemConfig,
    schedule: Arc<Mutex<Schedule>>,
) {
    let mut rng = XorShift::seeded();
    // When the bottleneck finishes serializing what it already holds.
    let mut busy_until = Instant::now();
    let mut in_flight: VecDeque<(Instant, SocketAddr, Bytes)> = VecDeque::new();

    loop {
        let next_delivery = in_flight.front().map(|(at, _, _)| *at);
        tokio::select! {
            packet = rx.recv() => {
                let Some((dst, data)) = packet else { return };
                let now = Instant::now();
                let (blacked_out, rate_bps) = schedule.lock().unwrap().at(now);
                if blacked_out || rng.next_f64() < config.loss {
                    continue;
                }
                let depart = match rate_bps {
                    Some(rate) => {
                        let start = busy_until.max(now);
                        if start.duration_since(now) > config.queue {
                            continue; // the bottleneck queue is full
                        }
                        busy_until = start + Duration::from_secs_f64(data.len() as f64 * 8.0 / rate as f64);
                        busy_until
                    }
                    None => now,
                };
                let deliver_at = depart + config.delay;
                if deliver_at <= now && in_flight.is_empty() {
                    // Nothing to emulate for this packet: skip the timer, whose
                    // granularity would otherwise add milliseconds.
                    if let Err(e) = socket.send_to(&data, dst).await {
                        tracing::debug!("netem send failed: {e}");
                    }
                    continue;
                }
                // Keep delivery order by time (rate changes can reorder slightly).
                let pos = in_flight.partition_point(|(at, _, _)| *at <= deliver_at);
                in_flight.insert(pos, (deliver_at, dst, data));
            }
            () = sleep_until(next_delivery) => {
                let now = Instant::now();
                let (blacked_out, _) = schedule.lock().unwrap().at(now);
                while in_flight.front().is_some_and(|(at, _, _)| *at <= now) {
                    let (_, dst, data) = in_flight.pop_front().unwrap();
                    if blacked_out {
                        continue; // packets in flight die in a blackout too
                    }
                    if let Err(e) = socket.send_to(&data, dst).await {
                        tracing::debug!("netem send failed: {e}");
                    }
                }
            }
        }
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// A small PRNG for loss decisions; reproducibility doesn't matter here.
struct XorShift(u64);

impl XorShift {
    fn seeded() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64);
        Self(seed | 1)
    }

    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_profile() {
        let c: NetemConfig = "delay=20ms,loss=1%,rate=20mbit,at10s=blackout:2s,at12s=rate:5mbit"
            .parse()
            .unwrap();
        assert_eq!(c.delay, Duration::from_millis(20));
        assert!((c.loss - 0.01).abs() < 1e-9);
        assert_eq!(c.rate_bps, Some(20_000_000));
        assert_eq!(
            c.schedule,
            vec![
                (
                    Duration::from_secs(10),
                    Change::Blackout(Duration::from_secs(2))
                ),
                (Duration::from_secs(12), Change::Rate(Some(5_000_000))),
            ]
        );
    }

    #[test]
    fn addresses_roundtrip() {
        for addr in ["127.0.0.1:51000", "[::1]:9"] {
            let addr: SocketAddr = addr.parse().unwrap();
            assert_eq!(socket_addr(&custom_addr(addr)).unwrap(), addr);
        }
    }
}
