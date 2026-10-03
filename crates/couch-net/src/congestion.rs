//! A QUIC congestion controller for real-time media (ADR 0005).
//!
//! Loss-based controllers (Cubic, NewReno) read random loss as congestion and
//! shrink their window. BBR v3 holds its window to what is in flight while it
//! recovers from a loss, and drains it every few seconds to probe the round
//! trip. Either way, frames wait in the send buffer of a path that has room
//! for them, and the viewer sees it as delay.
//!
//! Here the per-viewer rate controller (`couch_media::ratecontrol`) is the
//! congestion control, as GCC is in WebRTC. It watches one-way delay,
//! round-trip inflation, loss and backlog, and sets the encoder's bitrate.
//! This controller only keeps the sender within a few times what the path has
//! recently delivered, so even a sender that ignores its rate controller can't
//! flood the path:
//!
//! - window = max(initial window, 2 × fastest delivery rate of the last 10 s
//!   × max(smoothed RTT, 50 ms)). A sender held back by the window doubles it
//!   every round trip.
//! - Pacing runs at 4 × window / max(smoothed RTT, 50 ms), about 8 × the
//!   delivery rate. A frame leaves in one burst, but the sender never runs at
//!   line rate.
//! - Isolated losses change nothing. A sample with more than 20% loss forgets
//!   older, faster samples. Persistent congestion starts over from the
//!   initial window.
//! - When nothing has been acknowledged for max(4 × smoothed RTT, 100 ms)
//!   with data outstanding, one more packet may go out every 50 ms. After an
//!   outage, what was sent into it still counts as in flight until a newer
//!   packet is acknowledged, and QUIC's own probe timer has backed off to
//!   seconds by then. These probes find the path again within 50 ms.

use std::{
    any::Any,
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use noq_proto::{
    RttEstimator,
    congestion::{Controller, ControllerFactory, ControllerMetrics},
};

/// 32 packets of 1200 bytes, as Google QUIC started with; also the floor.
/// RFC 9002's 10 packets made a stream that starts at 10 Mbit/s queue its
/// first frames for four round trips while the window opened. Pacing spreads
/// this first flight over about 10 ms.
const INITIAL_WINDOW: u64 = 32 * 1200;
/// How long the fastest delivery rate is remembered. Long enough to keep the
/// window through a static scene, when the encoder sends little.
const RATE_WINDOW: Duration = Duration::from_secs(10);
/// The window covers at least this much time at the delivery rate, so a frame
/// fits in it even when the round trip is short.
const MIN_HORIZON: Duration = Duration::from_millis(50);
/// Shortest span of acknowledgements one rate sample is measured over.
const MIN_SAMPLE: Duration = Duration::from_millis(10);
const WINDOW_GAIN: u64 = 2;
const PACING_GAIN: u64 = 4;
/// A sample losing more than 1 in 5 bytes means the path delivers less now.
const HEAVY_LOSS_DIVISOR: u64 = 5;
/// No acknowledgement for this long, nor for 4 smoothed round trips, while
/// data is outstanding: the path may be gone.
const MIN_SILENCE: Duration = Duration::from_millis(100);
/// One packet beyond the window per this much silence...
const PROBE_INTERVAL: Duration = Duration::from_millis(50);
/// ...for as long as a connection can stay idle (15 s).
const MAX_PROBES: u64 = 300;
/// Bytes each probe may use: a full-size QUIC packet.
const PROBE_BYTES: u64 = 1500;

/// Builds [`MediaCongestion`] controllers; pass to the transport config.
#[derive(Debug, Default)]
pub struct MediaCongestionConfig;

impl ControllerFactory for MediaCongestionConfig {
    fn build(self: Arc<Self>, _now: Instant, _current_mtu: u16) -> Box<dyn Controller> {
        Box::new(MediaCongestion::default())
    }
}

/// The controller noq drives: a thin adapter over [`Window`].
#[derive(Debug, Clone, Default)]
pub struct MediaCongestion {
    window: Window,
}

impl Controller for MediaCongestion {
    /// Only ack-eliciting packets: an ACK-only packet leaves nothing outstanding.
    fn on_packet_sent(&mut self, now: Instant, _bytes: u16, _pn: u64) {
        self.window.on_sent(now);
    }

    fn on_ack(
        &mut self,
        now: Instant,
        _sent: Instant,
        bytes: u64,
        _pn: u64,
        _app_limited: bool,
        rtt: &RttEstimator,
    ) {
        self.window.on_ack(now, bytes, rtt.get());
    }

    fn on_end_acks(
        &mut self,
        now: Instant,
        _in_flight: u64,
        _app_limited: bool,
        _largest: Option<u64>,
    ) {
        self.window.on_end_acks(now);
    }

    fn on_congestion_event(
        &mut self,
        _now: Instant,
        _sent: Instant,
        is_persistent_congestion: bool,
        _is_ecn: bool,
        lost_bytes: u64,
        _largest_lost_pn: u64,
    ) {
        self.window.on_lost(lost_bytes);
        if is_persistent_congestion {
            self.window.restart();
        }
    }

    fn on_mtu_update(&mut self, _new_mtu: u16) {}

    /// noq asks without a time, so silence probes read the clock here. The
    /// one thing that wakes a sender blocked by its window after an outage is
    /// a packet from the peer, and that tells the controller nothing.
    fn window(&self) -> u64 {
        self.window.bytes_at(Instant::now())
    }

    fn metrics(&self) -> ControllerMetrics {
        let mut metrics = ControllerMetrics::default();
        metrics.congestion_window = self.window();
        metrics.pacing_rate = Some(self.window.pacing_rate());
        metrics
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(self.clone())
    }

    fn initial_window(&self) -> u64 {
        INITIAL_WINDOW
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

/// The window arithmetic, free of noq types so tests can drive it.
#[derive(Debug, Clone)]
struct Window {
    srtt: Duration,
    /// Start of the current rate sample; `None` until the first acknowledgement.
    sample_start: Option<Instant>,
    sample_acked: u64,
    sample_lost: u64,
    /// (when, bytes per second) delivery-rate samples.
    rates: VecDeque<(Instant, u64)>,
    bytes: u64,
    last_sent: Option<Instant>,
    last_ack: Option<Instant>,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            srtt: MIN_HORIZON,
            sample_start: None,
            sample_acked: 0,
            sample_lost: 0,
            rates: VecDeque::new(),
            bytes: INITIAL_WINDOW,
            last_sent: None,
            last_ack: None,
        }
    }
}

impl Window {
    fn on_sent(&mut self, now: Instant) {
        self.last_sent = Some(now);
    }

    fn on_ack(&mut self, now: Instant, bytes: u64, srtt: Duration) {
        self.sample_acked += bytes;
        self.srtt = srtt;
        self.last_ack = Some(now);
    }

    fn on_lost(&mut self, bytes: u64) {
        self.sample_lost += bytes;
    }

    /// Closes the rate sample once it spans a round trip.
    fn on_end_acks(&mut self, now: Instant) {
        let Some(start) = self.sample_start else {
            self.start_sample(now);
            return;
        };
        let elapsed = now.saturating_duration_since(start);
        if elapsed < self.srtt.max(MIN_SAMPLE) {
            return;
        }
        let rate =
            (u128::from(self.sample_acked) * 1_000_000_000 / elapsed.as_nanos().max(1)) as u64;
        let total = self.sample_acked + self.sample_lost;
        if self.sample_lost * HEAVY_LOSS_DIVISOR > total {
            self.rates.clear();
        }
        self.rates.push_back((now, rate));
        while self
            .rates
            .front()
            .is_some_and(|&(t, _)| now.saturating_duration_since(t) > RATE_WINDOW)
        {
            self.rates.pop_front();
        }
        self.start_sample(now);
        self.update();
    }

    /// Persistent congestion: the path may be a different one now.
    fn restart(&mut self) {
        self.rates.clear();
        self.sample_start = None;
        self.bytes = INITIAL_WINDOW;
    }

    fn start_sample(&mut self, now: Instant) {
        self.sample_start = Some(now);
        self.sample_acked = 0;
        self.sample_lost = 0;
    }

    fn horizon(&self) -> Duration {
        self.srtt.max(MIN_HORIZON)
    }

    fn update(&mut self) {
        let rate = self.rates.iter().map(|&(_, r)| r).max().unwrap_or(0);
        let target =
            (u128::from(WINDOW_GAIN * rate) * self.horizon().as_nanos() / 1_000_000_000) as u64;
        // Grow at most twofold per sample (a round trip), like slow start;
        // shrink at once.
        self.bytes = target.max(INITIAL_WINDOW).min(self.bytes * 2);
    }

    #[cfg(test)]
    fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The window plus any silence probes due by `now`.
    fn bytes_at(&self, now: Instant) -> u64 {
        self.bytes + PROBE_BYTES * self.probes(now)
    }

    fn probes(&self, now: Instant) -> u64 {
        let (Some(sent), Some(acked)) = (self.last_sent, self.last_ack) else {
            return 0;
        };
        if sent <= acked {
            return 0; // nothing outstanding
        }
        let silence = (self.srtt * 4).max(MIN_SILENCE);
        let quiet = now.saturating_duration_since(acked);
        if quiet < silence {
            return 0;
        }
        let due = (quiet - silence).as_nanos() / PROBE_INTERVAL.as_nanos() + 1;
        (due as u64).min(MAX_PROBES)
    }

    /// Bytes per second.
    fn pacing_rate(&self) -> u64 {
        (u128::from(PACING_GAIN * self.bytes) * 1_000_000_000 / self.horizon().as_nanos()) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RTT: Duration = Duration::from_millis(40);

    /// Delivers `rate` bytes/s for `secs`, acknowledged every 5 ms, losing
    /// `loss_per_mille` of the bytes sent. Returns the time it ended.
    fn deliver(
        w: &mut Window,
        start: Instant,
        secs: f64,
        rate: u64,
        loss_per_mille: u64,
    ) -> Instant {
        let step = Duration::from_millis(5);
        let per_step = rate / 200;
        let steps = (secs * 200.0) as u32;
        let mut now = start;
        for _ in 0..steps {
            now += step;
            w.on_ack(now, per_step, RTT);
            w.on_lost(per_step * loss_per_mille / (1000 - loss_per_mille));
            w.on_end_acks(now);
        }
        now
    }

    /// 10 Mbit/s, in bytes per second.
    const TEN_MBIT: u64 = 1_250_000;

    #[test]
    fn random_loss_keeps_the_window() {
        let mut w = Window::default();
        deliver(&mut w, Instant::now(), 3.0, TEN_MBIT, 10);
        // 2 × 1.25 MB/s × 50 ms.
        assert_eq!(w.bytes(), 125_000);
        // About 8 × the delivery rate.
        assert_eq!(w.pacing_rate(), 10_000_000);
    }

    #[test]
    fn a_window_limited_sender_doubles_each_round_trip() {
        let mut w = Window::default();
        let mut now = Instant::now();
        let mut windows = vec![w.bytes()];
        w.on_end_acks(now);
        for _ in 0..4 {
            // A full window per round trip, acknowledged in 8 batches.
            let per_batch = w.bytes() / 8;
            for _ in 0..8 {
                now += RTT / 8;
                w.on_ack(now, per_batch, RTT);
                w.on_end_acks(now);
            }
            windows.push(w.bytes());
        }
        let iw = INITIAL_WINDOW;
        assert_eq!(windows, [iw, 2 * iw, 4 * iw, 8 * iw, 16 * iw]);
    }

    #[test]
    fn heavy_loss_forgets_faster_samples() {
        let mut w = Window::default();
        let now = deliver(&mut w, Instant::now(), 1.0, 2 * TEN_MBIT, 0);
        assert_eq!(w.bytes(), 250_000);
        // The path now delivers a fifth of that and drops half of what is sent.
        deliver(&mut w, now, 0.5, 2 * TEN_MBIT / 5, 500);
        assert_eq!(w.bytes(), 50_000);
    }

    #[test]
    fn a_quiet_sender_keeps_its_window_for_a_while() {
        let mut w = Window::default();
        let now = deliver(&mut w, Instant::now(), 1.0, TEN_MBIT, 0);
        let now = deliver(&mut w, now, 5.0, TEN_MBIT / 20, 0);
        assert_eq!(
            w.bytes(),
            125_000,
            "a static scene must not shrink the window"
        );
        deliver(&mut w, now, 6.0, TEN_MBIT / 20, 0);
        assert_eq!(w.bytes(), INITIAL_WINDOW);
    }

    #[test]
    fn silence_lets_probes_through() {
        let mut w = Window::default();
        let acked = deliver(&mut w, Instant::now(), 1.0, TEN_MBIT, 0);
        let base = w.bytes();
        w.on_sent(acked + Duration::from_millis(1));
        // Silence starts counting after 4 round trips (160 ms).
        let at = |ms| w.bytes_at(acked + Duration::from_millis(ms));
        assert_eq!(at(150), base);
        assert_eq!(at(160), base + PROBE_BYTES);
        assert_eq!(at(260), base + 3 * PROBE_BYTES);
        assert_eq!(at(60_000), base + MAX_PROBES * PROBE_BYTES);
        // An acknowledgement ends it.
        let back = acked + Duration::from_millis(300);
        w.on_ack(back, 1200, RTT);
        assert_eq!(w.bytes_at(back), base);
    }

    #[test]
    fn no_probes_with_nothing_outstanding() {
        let mut w = Window::default();
        let acked = deliver(&mut w, Instant::now(), 1.0, TEN_MBIT, 0);
        assert_eq!(w.bytes_at(acked + Duration::from_secs(5)), w.bytes());
    }

    #[test]
    fn persistent_congestion_starts_over() {
        let mut w = Window::default();
        deliver(&mut w, Instant::now(), 1.0, TEN_MBIT, 0);
        w.restart();
        assert_eq!(w.bytes(), INITIAL_WINDOW);
    }
}
