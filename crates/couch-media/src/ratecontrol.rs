//! Per-viewer bitrate control (docs/protocol.md, "Bitrate control").
//!
//! [`FeedbackTracker`] turns receiver reports and path statistics into a
//! [`FeedbackSnapshot`]; [`RateController`] turns snapshots into bitrate
//! changes and catch-up decisions; [`UploadShare`] splits the host's upload
//! cap between its viewers. All are pure: time is an argument.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::FeedbackSnapshot;

/// Path statistics as the transport reports them.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PathSample {
    pub rtt: Duration,
    pub sent_packets: u64,
    pub lost_packets: u64,
    pub congestion_events: u64,
    pub cwnd: u64,
    pub relayed: bool,
}

/// How long minimums (delay, RTT) are remembered.
const MIN_WINDOW: Duration = Duration::from_secs(10);
/// The current one-way delay is the minimum over this window. A queue delays
/// every frame; a retransmission only delays the frames stuck behind it on the
/// epoch stream, about one round trip's worth, so they don't count. At 1%
/// loss and 20 Mbit/s such stalls come every 50 ms, so the window is long;
/// round-trip inflation, which retransmissions don't affect, sees a queue
/// sooner.
const DELAY_WINDOW: Duration = Duration::from_millis(400);
/// Window for the delivery-rate estimate: short enough to see a path that
/// just got slower, long enough to average over a few frames.
const RATE_WINDOW: Duration = Duration::from_millis(300);
/// No reports yet is normal for this long after the stream starts.
const STARTUP_GRACE: Duration = Duration::from_secs(2);

pub struct FeedbackTracker {
    started: Instant,
    epoch: u32,
    sent_in_epoch: u64,
    received_in_epoch: u64,
    sent_total: u64,
    /// (send time, cumulative bytes sent on the connection)
    sent: VecDeque<(Instant, u64)>,
    /// (report time, cumulative bytes received on the connection)
    received: VecDeque<(Instant, u64)>,
    last_report: Option<Instant>,
    /// (time, smallest one-way delay in a report)
    delays: VecDeque<(Instant, i64)>,
    rtts: VecDeque<(Instant, u32)>,
    /// Whether the path the minimums belong to is relayed.
    relayed: Option<bool>,
}

impl FeedbackTracker {
    pub fn new(now: Instant) -> Self {
        Self {
            started: now,
            epoch: 0,
            sent_in_epoch: 0,
            received_in_epoch: 0,
            sent_total: 0,
            sent: VecDeque::new(),
            received: VecDeque::new(),
            last_report: None,
            delays: VecDeque::new(),
            rtts: VecDeque::new(),
            relayed: None,
        }
    }

    /// A new epoch started; whatever was queued for the old one is gone.
    pub fn on_epoch(&mut self, epoch: u32) {
        self.epoch = epoch;
        self.sent_in_epoch = 0;
        self.received_in_epoch = 0;
    }

    pub fn on_sent(&mut self, now: Instant, bytes: u64) {
        self.sent_in_epoch += bytes;
        self.sent_total += bytes;
        self.sent.push_back((now, self.sent_total));
        while self.sent.len() > 2 && now.duration_since(self.sent[1].0) >= RATE_WINDOW {
            self.sent.pop_front();
        }
    }

    /// Records a receiver report: bytes received in `epoch` and on the whole
    /// connection, and the one-way delays (arrival minus capture) of recent frames.
    pub fn on_report(
        &mut self,
        now: Instant,
        epoch: u32,
        epoch_bytes: u64,
        total_bytes: u64,
        delays_us: &[i64],
    ) {
        self.last_report = Some(now);
        if epoch == self.epoch {
            self.received_in_epoch = epoch_bytes;
        }
        self.received.push_back((now, total_bytes));
        while self.received.len() > 2 && now.duration_since(self.received[1].0) >= RATE_WINDOW {
            self.received.pop_front();
        }
        if let Some(&min) = delays_us.iter().min() {
            self.delays.push_back((now, min));
            prune(&mut self.delays, now);
        }
    }

    pub fn snapshot(&mut self, now: Instant, path: PathSample) -> FeedbackSnapshot {
        // A switch between relay and direct is a different path: its round
        // trip and delay are measured from scratch, or the new path's longer
        // ones would read as queueing for the next 10 s.
        if self.relayed.is_some_and(|r| r != path.relayed) {
            self.rtts.clear();
            self.delays.clear();
        }
        self.relayed = Some(path.relayed);
        let rtt_us = path.rtt.as_micros() as u32;
        if rtt_us > 0 {
            self.rtts.push_back((now, rtt_us));
            prune(&mut self.rtts, now);
        }
        let delivery_rate_bps = match (self.received.front(), self.received.back()) {
            (Some(&(t0, b0)), Some(&(t1, b1)))
                if t1.duration_since(t0) >= Duration::from_millis(100) =>
            {
                ((b1.saturating_sub(b0)) as f64 * 8.0 / t1.duration_since(t0).as_secs_f64()) as u64
            }
            _ => 0,
        };
        let send_rate_bps = match self.sent.front() {
            Some(&(t0, b0)) if now.duration_since(t0) >= Duration::from_millis(100) => {
                ((self.sent_total - b0) as f64 * 8.0 / now.duration_since(t0).as_secs_f64()) as u64
            }
            _ => 0,
        };
        // No frames lately (an outage, or a send queue the transport isn't
        // draining) leaves the newest sample as the best guess.
        let current = self
            .delays
            .iter()
            .filter(|&&(t, _)| now.duration_since(t) <= DELAY_WINDOW)
            .map(|&(_, d)| d)
            .min()
            .or_else(|| self.delays.back().map(|&(_, d)| d));
        let delay_trend_us = match (current, self.delays.iter().map(|&(_, d)| d).min()) {
            // The delays come from the peer: saturate rather than overflow.
            (Some(current), Some(base)) => {
                current.saturating_sub(base).clamp(0, i64::from(i32::MAX)) as i32
            }
            _ => 0,
        };
        let report_age = match self.last_report {
            Some(t) => now.duration_since(t),
            None if now.duration_since(self.started) < STARTUP_GRACE => Duration::ZERO,
            None => now.duration_since(self.started),
        };
        FeedbackSnapshot {
            delivery_rate_bps,
            send_rate_bps,
            outstanding_bytes: self.sent_in_epoch.saturating_sub(self.received_in_epoch),
            rtt_us,
            min_rtt_us: self.rtts.iter().map(|&(_, r)| r).min().unwrap_or(rtt_us),
            delay_trend_us,
            sent_packets: path.sent_packets,
            lost_packets: path.lost_packets,
            congestion_events: path.congestion_events,
            cwnd_bytes: path.cwnd,
            relayed: path.relayed,
            report_age_ms: report_age.as_millis().min(u128::from(u32::MAX)) as u32,
        }
    }
}

fn prune<T>(q: &mut VecDeque<(Instant, T)>, now: Instant) {
    while q
        .front()
        .is_some_and(|(t, _)| now.duration_since(*t) > MIN_WINDOW)
    {
        q.pop_front();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateConfig {
    pub min_bps: u32,
    pub max_bps: u32,
    /// Ceiling while the path goes through a relay server.
    pub relay_max_bps: u32,
    pub start_bps: u32,
    /// The encoder pays a keyframe per bitrate change (FFmpeg's NVENC), so
    /// increases are batched.
    pub keyframe_costly: bool,
}

impl RateConfig {
    /// The bitrate to open the encoder at: the start rate within [min, max].
    /// A max below min counts as min.
    pub fn clamped_start(&self) -> u32 {
        self.start_bps
            .clamp(self.min_bps, self.max_bps.max(self.min_bps))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateAction {
    Keep,
    SetBitrate(u32),
    /// Abandon the current epoch, restart with a keyframe at this bitrate.
    CatchUp {
        bitrate: u32,
    },
    /// The viewer went silent with video outstanding: abandon the epoch and
    /// stop sending. Frames sent into an outage only become stale backlog
    /// when it ends.
    Pause,
    /// Reports are back after a pause: resume with one keyframe at this bitrate.
    Resume {
        bitrate: u32,
    },
}

const INCREASE_PER_SEC: f64 = 0.08;
const DECREASE_FACTOR: f64 = 0.85;
const HOLD_AFTER_DECREASE: Duration = Duration::from_millis(500);
const DELAY_OVERUSE_US: i32 = 10_000;
const RTT_INFLATION_MIN_US: f64 = 15_000.0;
const CATCH_UP_MIN: Duration = Duration::from_millis(150);
const CATCH_UP_COOLDOWN: Duration = Duration::from_secs(1);
const REPORT_TIMEOUT_MS: u32 = 500;
/// A report this fresh after a pause means the path is back.
const RESUME_REPORT_AGE_MS: u32 = 100;
const LOSS_WINDOW: Duration = Duration::from_secs(1);
const LOSS_HOLD: f64 = 0.02;
const LOSS_DECREASE: f64 = 0.10;
const COSTLY_MIN_STEP: f64 = 1.15;
const COSTLY_MIN_INTERVAL: Duration = Duration::from_secs(1);

pub struct RateController {
    cfg: RateConfig,
    /// This viewer's share of the host's upload cap, if there is a cap.
    share: Option<u32>,
    target: f64,
    applied: u32,
    last_tick: Option<Instant>,
    hold_until: Option<Instant>,
    last_catch_up: Option<Instant>,
    last_change: Option<Instant>,
    backlog_strikes: u8,
    paused: bool,
    /// (time, packets sent, packets lost) over the last second.
    loss_window: VecDeque<(Instant, u64, u64)>,
}

impl RateController {
    pub fn new(cfg: RateConfig) -> Self {
        let start = cfg.clamped_start();
        Self {
            cfg,
            share: None,
            target: f64::from(start),
            applied: start,
            last_tick: None,
            hold_until: None,
            last_catch_up: None,
            last_change: None,
            backlog_strikes: 0,
            paused: false,
            loss_window: VecDeque::new(),
        }
    }

    /// Sending is suspended until the viewer's reports return.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// The bitrate the encoder currently runs at.
    pub fn applied_bps(&self) -> u32 {
        self.applied
    }

    /// Where the controller wants to go (may run ahead of `applied_bps`).
    pub fn target_bps(&self) -> u32 {
        self.target as u32
    }

    /// This viewer's share of the host's upload cap ([`UploadShare`]), or
    /// `None` without a cap. It lowers the ceiling from the next tick on; a
    /// bitrate above it comes down at once.
    pub fn set_share(&mut self, bps: Option<u32>) {
        self.share = bps;
    }

    pub fn tick(&mut self, now: Instant, fb: &FeedbackSnapshot) -> RateAction {
        let dt = self.last_tick.map_or(Duration::ZERO, |t| {
            now.duration_since(t).min(Duration::from_millis(250))
        });
        self.last_tick = Some(now);

        // 0. Outages: stop sending into the void, resume with one keyframe.
        if self.paused {
            if fb.report_age_ms > RESUME_REPORT_AGE_MS {
                return RateAction::Keep;
            }
            self.paused = false;
            self.last_change = Some(now);
            self.hold_until = Some(now + HOLD_AFTER_DECREASE);
            // The resume keyframe may wait while the transport finds the
            // path again; queueing behind it is no reason for another one.
            self.last_catch_up = Some(now);
            self.backlog_strikes = 0;
            self.loss_window.clear();
            return RateAction::Resume {
                bitrate: self.applied,
            };
        }
        if fb.report_age_ms >= REPORT_TIMEOUT_MS && fb.outstanding_bytes > 0 {
            self.paused = true;
            self.target = (f64::from(self.applied) / 2.0).max(f64::from(self.cfg.min_bps));
            self.applied = self.target as u32;
            return RateAction::Pause;
        }

        let loss_rate = self.loss_rate(now, fb);
        let cap = if fb.relayed {
            self.cfg.max_bps.min(self.cfg.relay_max_bps)
        } else {
            self.cfg.max_bps
        }
        .min(self.share.unwrap_or(u32::MAX))
        .max(self.cfg.min_bps);

        // 1. Catch up when video queues up beyond what the path can drain soon.
        //    Outstanding bytes include what is on the wire and what arrived
        //    after the viewer's last report: about a round trip plus the
        //    report's age. Only the rest is queued. One retransmission holds
        //    the stream up for about a round trip, so that alone isn't enough.
        //    The bytes went out at the recent send rate, which right after a
        //    cut is still above the new bitrate.
        let pipeline = f64::from(fb.rtt_us) / 1e6 + f64::from(fb.report_age_ms) / 1e3;
        let sent_at = f64::from(self.applied)
            .max(fb.send_rate_bps as f64)
            .max(1.0);
        let queued = fb.outstanding_bytes as f64 * 8.0 / sent_at - pipeline;
        let backlog_limit = (Duration::from_micros(u64::from(fb.min_rtt_us)) * 2).max(CATCH_UP_MIN);
        self.backlog_strikes = if queued > backlog_limit.as_secs_f64() {
            self.backlog_strikes.saturating_add(1)
        } else {
            0
        };
        let cooled = self
            .last_catch_up
            .is_none_or(|t| now.duration_since(t) >= CATCH_UP_COOLDOWN);
        let rtt_inflation = f64::from(fb.rtt_us.saturating_sub(fb.min_rtt_us))
            > RTT_INFLATION_MIN_US.max(0.5 * f64::from(fb.min_rtt_us));
        if self.backlog_strikes >= 2 && cooled {
            // Drop the stale video either way. Cut the bitrate only if the path
            // shows congestion; otherwise the queue is ours (the transport's
            // window still opening at the start of a stream, say).
            let congested =
                rtt_inflation || loss_rate > LOSS_HOLD || fb.delay_trend_us > DELAY_OVERUSE_US;
            // A congested path that queues this much is delivering all it can:
            // aim just below that, but move between a quarter and half of the
            // old rate.
            let applied = f64::from(self.applied);
            let target = if !congested {
                applied
            } else if fb.delivery_rate_bps > 0 {
                (DECREASE_FACTOR * fb.delivery_rate_bps as f64).clamp(applied / 4.0, applied / 2.0)
            } else {
                applied / 2.0
            };
            self.target = target.max(f64::from(self.cfg.min_bps));
            self.applied = self.target as u32;
            self.last_catch_up = Some(now);
            self.last_change = Some(now);
            self.hold_until = Some(now + CATCH_UP_COOLDOWN);
            self.backlog_strikes = 0;
            return RateAction::CatchUp {
                bitrate: self.applied,
            };
        }

        // 2. Move the target: down on overuse, slowly up otherwise.
        let in_hold = self.hold_until.is_some_and(|t| now < t);
        let overuse =
            fb.delay_trend_us > DELAY_OVERUSE_US || rtt_inflation || loss_rate > LOSS_DECREASE;
        if overuse {
            if !in_hold {
                // A path that delivers less than we send shows its capacity:
                // cut below that. One that keeps up says nothing about it;
                // the sender may just have little to send (a static scene),
                // so cut from the target instead of from that trickle.
                let falling_behind = fb.delivery_rate_bps > 0
                    && (fb.delivery_rate_bps as f64) < 0.9 * fb.send_rate_bps as f64;
                let basis = if falling_behind {
                    (fb.delivery_rate_bps as f64).min(self.target)
                } else {
                    self.target
                };
                self.target = DECREASE_FACTOR * basis;
                self.hold_until = Some(now + HOLD_AFTER_DECREASE);
            }
        } else if !in_hold && loss_rate <= LOSS_HOLD {
            self.target *= 1.0 + INCREASE_PER_SEC * dt.as_secs_f64();
        }
        self.target = self
            .target
            .clamp(f64::from(self.cfg.min_bps), f64::from(cap));

        // 3. Apply: decreases at once, increases batched when they cost a keyframe.
        let target = self.target as u32;
        let applied = f64::from(self.applied);
        if f64::from(target) < applied * 0.97 || (target == cap && target < self.applied) {
            self.applied = target;
            self.last_change = Some(now);
            return RateAction::SetBitrate(target);
        }
        if target > self.applied && (f64::from(target) > applied * 1.03 || target == cap) {
            let ready = !self.cfg.keyframe_costly
                || ((f64::from(target) >= applied * COSTLY_MIN_STEP || target == cap)
                    && self
                        .last_change
                        .is_none_or(|t| now.duration_since(t) >= COSTLY_MIN_INTERVAL));
            if ready {
                self.applied = target;
                self.last_change = Some(now);
                return RateAction::SetBitrate(target);
            }
        }
        RateAction::Keep
    }

    fn loss_rate(&mut self, now: Instant, fb: &FeedbackSnapshot) -> f64 {
        self.loss_window
            .push_back((now, fb.sent_packets, fb.lost_packets));
        while self
            .loss_window
            .front()
            .is_some_and(|(t, _, _)| now.duration_since(*t) > LOSS_WINDOW)
        {
            self.loss_window.pop_front();
        }
        let (&(_, sent0, lost0), &(_, sent1, lost1)) = (
            self.loss_window.front().unwrap(),
            self.loss_window.back().unwrap(),
        );
        let sent = sent1.saturating_sub(sent0);
        if sent < 50 {
            return 0.0;
        }
        lost1.saturating_sub(lost0) as f64 / sent as f64
    }
}

/// Frames whose host delay is remembered: a few seconds' worth, far more
/// than a report can be behind.
const SEND_DELAYS_KEPT: usize = 512;

/// How long each recent frame spent on the host before it was handed to the
/// network: waiting for the encoder and inside it.
///
/// Viewers report each frame's arrival minus its capture time. Taking off
/// the host's part leaves the time on the network, which is what congestion
/// control must react to. Without it, an encoder slowed by load (several
/// viewers, a busy GPU) reads as a filling queue, and cutting the bitrate,
/// which can't speed the encoder up, collapses every viewer to the floor.
#[derive(Debug, Default)]
pub struct SendDelays {
    /// (epoch, seq, microseconds from capture to send), oldest first.
    frames: VecDeque<(u32, u32, i64)>,
}

impl SendDelays {
    pub fn new() -> Self {
        Self::default()
    }

    /// Frame `seq` of `epoch` went out `host_us` after it was captured.
    pub fn sent(&mut self, epoch: u32, seq: u32, host_us: i64) {
        if self.frames.len() == SEND_DELAYS_KEPT {
            self.frames.pop_front();
        }
        self.frames.push_back((epoch, seq, host_us.max(0)));
    }

    /// The network's part of a reported delay. A report's samples all belong
    /// to its epoch (the viewer drops an abandoned epoch's). An unknown
    /// frame's delay is left as it is.
    pub fn network_part(&self, epoch: u32, seq: u32, one_way_us: i64) -> i64 {
        let host = self
            .frames
            .iter()
            .rev()
            .find(|&&(e, s, _)| e == epoch && s == seq)
            .map_or(0, |&(_, _, host)| host);
        one_way_us.saturating_sub(host)
    }
}

/// A viewer held back by its own network keeps this much room above its
/// target, so it can still grow into it.
const SHARE_MARGIN: f64 = 1.25;
/// Shares move in steps of this many bit/s.
const SHARE_STEP: u32 = 500_000;
/// A share comes down only when it falls by more than this fraction: every
/// bitrate change costs an NVENC keyframe, so small swings change nothing...
const SHARE_HYSTERESIS: f64 = 0.10;
/// ...unless keeping the old shares would overrun the cap by more than this.
const SHARE_OVERRUN: f64 = 0.10;

/// Splits the host's upload cap, `total_bps`, between viewers whose
/// controllers aim at `targets` ([`RateController::target_bps`]). A viewer
/// held back by its own network (aiming below an equal share) gets its
/// target plus a margin to grow into; the others split what is left
/// equally. Shares are rounded down to whole steps, so they never add up to
/// more than the cap. [`UploadShare`] adds hysteresis.
pub fn share_upload(total_bps: u32, targets: &[u32]) -> Vec<u32> {
    let mut order: Vec<usize> = (0..targets.len()).collect();
    order.sort_by_key(|&i| targets[i]);
    let mut shares = vec![0; targets.len()];
    let mut left = f64::from(total_bps);
    let mut rest = targets.len();
    for (k, &i) in order.iter().enumerate() {
        let fair = left / rest as f64;
        // At least one step above the target, or rounding down could hand a
        // viewer at the minimum a share equal to it, and it could never grow.
        let target = f64::from(targets[i]);
        let wants = (target * SHARE_MARGIN).max(target + f64::from(SHARE_STEP));
        if wants >= fair {
            // This one and every larger one can use an equal share.
            for &j in &order[k..] {
                shares[j] = step_down(fair);
            }
            break;
        }
        shares[i] = step_down(wants);
        left -= f64::from(shares[i]);
        rest -= 1;
    }
    shares
}

fn step_down(bps: f64) -> u32 {
    let bps = bps.clamp(0.0, f64::from(u32::MAX)) as u32;
    bps / SHARE_STEP * SHARE_STEP
}

/// [`share_upload`] with memory, for the host to call about once a second.
/// A share rises at once (that costs nothing until the controller grows
/// into it) but comes down only when the cut is large, or when keeping the
/// old shares would overrun the cap. Viewers are told apart by `K`, so one
/// leaving and another joining start afresh.
#[derive(Debug, Clone)]
pub struct UploadShare<K> {
    total_bps: u32,
    shares: std::collections::BTreeMap<K, u32>,
}

impl<K: Ord + Copy> UploadShare<K> {
    pub fn new(total_bps: u32) -> Self {
        Self {
            total_bps,
            shares: std::collections::BTreeMap::new(),
        }
    }

    pub fn total_bps(&self) -> u32 {
        self.total_bps
    }

    /// The share of each viewer in `targets` (viewer, its controller's
    /// target), in the same order.
    pub fn update(&mut self, targets: &[(K, u32)]) -> Vec<(K, u32)> {
        let wanted: Vec<u32> = targets.iter().map(|&(_, t)| t).collect();
        let fresh = share_upload(self.total_bps, &wanted);
        let kept: Vec<u32> = targets
            .iter()
            .zip(&fresh)
            .map(|(&(viewer, _), &new)| match self.shares.get(&viewer) {
                Some(&old)
                    if new < old && f64::from(new) >= f64::from(old) * (1.0 - SHARE_HYSTERESIS) =>
                {
                    old
                }
                _ => new,
            })
            .collect();
        let sum: u64 = kept.iter().map(|&s| u64::from(s)).sum();
        let chosen = if sum as f64 > f64::from(self.total_bps) * (1.0 + SHARE_OVERRUN) {
            fresh
        } else {
            kept
        };
        self.shares = targets
            .iter()
            .map(|&(viewer, _)| viewer)
            .zip(chosen.iter().copied())
            .collect();
        targets
            .iter()
            .map(|&(viewer, _)| viewer)
            .zip(chosen)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MBPS: u32 = 1_000_000;

    fn config(keyframe_costly: bool) -> RateConfig {
        RateConfig {
            min_bps: 2 * MBPS,
            max_bps: 20 * MBPS,
            relay_max_bps: 3 * MBPS,
            start_bps: 10 * MBPS,
            keyframe_costly,
        }
    }

    fn clean() -> FeedbackSnapshot {
        FeedbackSnapshot {
            delivery_rate_bps: u64::from(10 * MBPS),
            send_rate_bps: u64::from(10 * MBPS),
            rtt_us: 20_000,
            min_rtt_us: 20_000,
            ..Default::default()
        }
    }

    /// Runs the controller at 20 Hz for `secs`, feeding `fb` (packets count up
    /// so loss rates can be computed). Returns the actions taken.
    fn run(
        c: &mut RateController,
        start: Instant,
        secs: f64,
        mut fb: impl FnMut(f64) -> FeedbackSnapshot,
    ) -> Vec<RateAction> {
        let ticks = (secs * 20.0) as u32;
        (0..ticks)
            .map(|i| {
                let t = f64::from(i) / 20.0;
                c.tick(start + Duration::from_secs_f64(t), &fb(t))
            })
            .filter(|a| *a != RateAction::Keep)
            .collect()
    }

    #[test]
    fn grows_to_the_ceiling_on_a_clean_path() {
        let start = Instant::now();
        let mut c = RateController::new(config(false));
        run(&mut c, start, 12.0, |_| clean());
        assert_eq!(c.applied_bps(), 20 * MBPS);
    }

    #[test]
    fn costly_increases_are_batched() {
        let start = Instant::now();
        let mut c = RateController::new(config(true));
        let actions = run(&mut c, start, 12.0, |_| clean());
        // About 8.7 s from 10 to 20 Mbit/s at 8%/s; batched into >=15% steps
        // at most once a second, that is a handful of changes, not 200.
        assert!(actions.len() <= 6, "{actions:?}");
        assert_eq!(c.applied_bps(), 20 * MBPS);
    }

    #[test]
    fn delay_growth_cuts_below_the_delivery_rate() {
        let start = Instant::now();
        let mut c = RateController::new(config(false));
        let actions = run(&mut c, start, 0.2, |_| FeedbackSnapshot {
            delay_trend_us: 30_000,
            delivery_rate_bps: u64::from(8 * MBPS),
            ..clean()
        });
        assert_eq!(actions.first(), Some(&RateAction::SetBitrate(6_800_000)));
    }

    #[test]
    fn an_app_limited_sender_is_cut_from_its_target() {
        let start = Instant::now();
        let mut c = RateController::new(config(false));
        // A static scene: the encoder sends 1 Mbit/s and all of it arrives.
        // A delay spike says nothing about what the path can carry.
        let actions = run(&mut c, start, 0.2, |_| FeedbackSnapshot {
            delay_trend_us: 30_000,
            delivery_rate_bps: u64::from(MBPS),
            send_rate_bps: u64::from(MBPS),
            ..clean()
        });
        assert_eq!(actions.first(), Some(&RateAction::SetBitrate(8_500_000)));
    }

    #[test]
    fn bytes_sent_before_a_cut_are_not_backlog() {
        let start = Instant::now();
        let mut c = RateController::new(config(false));
        // 300 KB outstanding went out at 20 Mbit/s: 120 ms of it, mostly the
        // round trip and the report's age. At the new 10 Mbit/s it would
        // look like 240 ms.
        let actions = run(&mut c, start, 1.0, |_| FeedbackSnapshot {
            outstanding_bytes: 300_000,
            send_rate_bps: u64::from(20 * MBPS),
            rtt_us: 40_000,
            min_rtt_us: 40_000,
            report_age_ms: 25,
            ..clean()
        });
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, RateAction::CatchUp { .. })),
            "{actions:?}"
        );
    }

    #[test]
    fn a_max_below_min_does_not_panic() {
        let c = RateController::new(RateConfig {
            min_bps: 5 * MBPS,
            max_bps: 2 * MBPS,
            ..config(false)
        });
        assert_eq!(c.applied_bps(), 5 * MBPS);
    }

    #[test]
    fn random_one_percent_loss_does_not_cut() {
        let start = Instant::now();
        let mut c = RateController::new(config(false));
        run(&mut c, start, 5.0, |t| FeedbackSnapshot {
            sent_packets: (t * 2000.0) as u64,
            lost_packets: (t * 20.0) as u64,
            ..clean()
        });
        assert!(c.applied_bps() > 10 * MBPS);
    }

    #[test]
    fn heavy_loss_cuts() {
        let start = Instant::now();
        let mut c = RateController::new(config(false));
        run(&mut c, start, 3.0, |t| FeedbackSnapshot {
            sent_packets: (t * 2000.0) as u64,
            lost_packets: (t * 300.0) as u64,
            ..clean()
        });
        assert!(c.applied_bps() < 10 * MBPS);
    }

    /// A congested path: 5% loss, enough to stop growth but not to cut.
    fn lossy(t: f64) -> FeedbackSnapshot {
        FeedbackSnapshot {
            sent_packets: (t * 2000.0) as u64,
            lost_packets: (t * 100.0) as u64,
            ..clean()
        }
    }

    #[test]
    fn a_backlog_triggers_one_catch_up_per_cooldown() {
        let start = Instant::now();
        let mut c = RateController::new(config(false));
        // 10 Mbit/s of outstanding video is a full second of backlog.
        let actions = run(&mut c, start, 0.9, |t| FeedbackSnapshot {
            outstanding_bytes: 1_250_000,
            ..lossy(t)
        });
        let catch_ups: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, RateAction::CatchUp { .. }))
            .collect();
        assert_eq!(catch_ups, [&RateAction::CatchUp { bitrate: 5 * MBPS }]);
    }

    #[test]
    fn a_catch_up_aims_below_the_delivery_rate() {
        let start = Instant::now();
        let mut c = RateController::new(config(false));
        // The path dropped to 4 Mbit/s and a second of video queued up.
        let actions = run(&mut c, start, 0.2, |t| FeedbackSnapshot {
            outstanding_bytes: 1_250_000,
            delivery_rate_bps: u64::from(4 * MBPS),
            ..lossy(t)
        });
        assert!(
            actions.contains(&RateAction::CatchUp { bitrate: 3_400_000 }),
            "{actions:?}"
        );
    }

    #[test]
    fn a_backlog_without_congestion_keeps_the_bitrate() {
        let start = Instant::now();
        let mut c = RateController::new(config(false));
        // Video queued while the transport's window was still opening: no
        // delay growth, no loss. Drop the stale frames but not the bitrate.
        let actions = run(&mut c, start, 0.2, |_| FeedbackSnapshot {
            outstanding_bytes: 1_250_000,
            delivery_rate_bps: u64::from(2 * MBPS),
            ..clean()
        });
        assert!(
            actions.contains(&RateAction::CatchUp { bitrate: 10 * MBPS }),
            "{actions:?}"
        );
    }

    #[test]
    fn a_retransmission_stall_does_not_catch_up() {
        let start = Instant::now();
        let mut c = RateController::new(config(false));
        // 130 ms of video outstanding: 85 ms is the round trip and the
        // report's age, the rest a stream held up by one retransmission.
        let actions = run(&mut c, start, 2.0, |_| FeedbackSnapshot {
            outstanding_bytes: 162_500,
            rtt_us: 40_000,
            min_rtt_us: 40_000,
            report_age_ms: 45,
            ..clean()
        });
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, RateAction::CatchUp { .. })),
            "{actions:?}"
        );
    }

    #[test]
    fn an_outage_pauses_once_and_resumes_with_one_keyframe() {
        let start = Instant::now();
        let mut c = RateController::new(config(false));
        // Two seconds of silence with video outstanding, then reports return.
        let actions = run(&mut c, start, 3.0, |t| {
            if (0.2..2.2).contains(&t) {
                FeedbackSnapshot {
                    report_age_ms: ((t - 0.2) * 1000.0) as u32 + 50,
                    outstanding_bytes: 10_000,
                    ..clean()
                }
            } else {
                clean()
            }
        });
        let pauses = actions.iter().filter(|a| **a == RateAction::Pause).count();
        let resumes: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, RateAction::Resume { .. }))
            .collect();
        assert_eq!(pauses, 1, "{actions:?}");
        // Resumes once, at about half the rate it had before the outage.
        let [RateAction::Resume { bitrate }] = resumes[..] else {
            panic!("expected exactly one resume: {actions:?}");
        };
        assert!((5 * MBPS..6 * MBPS).contains(bitrate), "{bitrate}");
        assert!(!c.is_paused());
    }

    #[test]
    fn relayed_paths_are_clamped() {
        let start = Instant::now();
        let mut c = RateController::new(config(false));
        let actions = run(&mut c, start, 0.2, |_| FeedbackSnapshot {
            relayed: true,
            ..clean()
        });
        assert_eq!(actions.first(), Some(&RateAction::SetBitrate(3 * MBPS)));
    }

    #[test]
    fn tracker_measures_delivery_and_backlog() {
        let start = Instant::now();
        let mut t = FeedbackTracker::new(start);
        t.on_epoch(1);
        t.on_sent(start, 2_000_000);
        t.on_report(start, 1, 0, 0, &[5_000]);
        t.on_report(
            start + Duration::from_millis(500),
            1,
            625_000,
            625_000,
            &[25_000, 30_000, 25_000],
        );
        let fb = t.snapshot(
            start + Duration::from_millis(500),
            PathSample {
                rtt: Duration::from_millis(20),
                ..Default::default()
            },
        );
        assert_eq!(fb.delivery_rate_bps, 10_000_000);
        assert_eq!(fb.outstanding_bytes, 1_375_000);
        assert_eq!(fb.delay_trend_us, 20_000);
        assert_eq!(fb.report_age_ms, 0);

        // A new epoch forgets the old backlog; stale reports don't count against it.
        t.on_epoch(2);
        t.on_sent(start + Duration::from_millis(550), 1_000);
        t.on_report(start + Duration::from_millis(550), 1, 700_000, 700_000, &[]);
        let fb = t.snapshot(start + Duration::from_millis(550), PathSample::default());
        assert_eq!(fb.outstanding_bytes, 1_000);
    }

    #[test]
    fn tracker_measures_the_send_rate() {
        let start = Instant::now();
        let mut t = FeedbackTracker::new(start);
        t.on_epoch(1);
        // 60 frames a second of 20 KB: 9.6 Mbit/s.
        for i in 0..60 {
            t.on_sent(start + Duration::from_micros(16_667 * i), 20_000);
        }
        let fb = t.snapshot(start + Duration::from_secs(1), PathSample::default());
        let rate = fb.send_rate_bps as f64 / 1e6;
        assert!((9.0..10.2).contains(&rate), "{rate}");
        // Nothing sent for a second: the rate falls with it.
        let fb = t.snapshot(start + Duration::from_secs(2), PathSample::default());
        assert!(fb.send_rate_bps < u64::from(4 * MBPS), "{fb:?}");
    }

    #[test]
    fn a_new_path_starts_its_minimums_over() {
        let start = Instant::now();
        let mut t = FeedbackTracker::new(start);
        let direct = PathSample {
            rtt: Duration::from_millis(20),
            ..Default::default()
        };
        let relayed = PathSample {
            rtt: Duration::from_millis(80),
            relayed: true,
            ..Default::default()
        };
        t.on_report(start, 0, 0, 0, &[10_000]);
        t.snapshot(start, direct);
        // The relay's longer round trip and delay are its baseline, not a queue.
        let later = start + Duration::from_millis(100);
        t.on_report(later, 0, 0, 0, &[40_000]);
        let fb = t.snapshot(later, relayed);
        assert_eq!(fb.min_rtt_us, 80_000);
        assert_eq!(fb.delay_trend_us, 0);
    }

    #[test]
    fn hostile_delays_do_not_overflow() {
        let start = Instant::now();
        let mut t = FeedbackTracker::new(start);
        let later = start + Duration::from_secs(1);
        t.on_report(start, 0, 0, 0, &[i64::MIN]);
        t.on_report(later, 0, 0, 0, &[i64::MAX]);
        let fb = t.snapshot(later, PathSample::default());
        assert_eq!(fb.delay_trend_us, i32::MAX);
    }

    /// Reports every 50 ms for `n` reports; `delays(i)` gives report i's frames.
    fn delay_trend_after(n: u64, delays: impl Fn(u64) -> Vec<i64>) -> i32 {
        let start = Instant::now();
        let mut t = FeedbackTracker::new(start);
        t.on_epoch(1);
        for i in 0..n {
            t.on_report(start + Duration::from_millis(50 * i), 1, 0, 0, &delays(i));
        }
        t.snapshot(
            start + Duration::from_millis(50 * (n - 1)),
            PathSample::default(),
        )
        .delay_trend_us
    }

    #[test]
    fn retransmission_stalls_are_not_queueing() {
        // Every other report only has frames released at once after a lost
        // packet was retransmitted: late, the oldest latest.
        let trend = delay_trend_after(20, |i| {
            if i % 2 == 0 {
                vec![20_000, 21_000, 20_000]
            } else {
                vec![65_000, 48_000, 31_000]
            }
        });
        assert_eq!(trend, 0);
    }

    #[test]
    fn a_growing_queue_shows_as_delay() {
        // Every frame waits 5 ms longer than the ones in the report before.
        let trend = delay_trend_after(14, |i| {
            let queue = 5_000 * i as i64;
            vec![20_000 + queue, 22_000 + queue]
        });
        // The 400 ms window still holds the report from 8 reports back.
        assert_eq!(trend, 25_000);
    }

    #[test]
    fn a_lower_share_brings_the_bitrate_down_at_once() {
        let start = Instant::now();
        let mut c = RateController::new(config(true));
        run(&mut c, start, 12.0, |_| clean());
        assert_eq!(c.applied_bps(), 20 * MBPS);
        c.set_share(Some(12 * MBPS));
        let later = start + Duration::from_secs(12);
        assert_eq!(c.tick(later, &clean()), RateAction::SetBitrate(12 * MBPS));
        // It stays there on a clean path, and grows again once lifted.
        run(&mut c, later, 3.0, |_| clean());
        assert_eq!(c.applied_bps(), 12 * MBPS);
        c.set_share(None);
        run(&mut c, later + Duration::from_secs(3), 10.0, |_| clean());
        assert_eq!(c.applied_bps(), 20 * MBPS);
    }

    #[test]
    fn the_hosts_own_delay_is_taken_off() {
        let mut sent = SendDelays::new();
        sent.sent(3, 0, 4_000);
        sent.sent(3, 1, 25_000); // a busy encoder
        assert_eq!(sent.network_part(3, 0, 24_000), 20_000);
        assert_eq!(sent.network_part(3, 1, 45_000), 20_000);
        // A new epoch starts over at seq 0; frames are told apart by epoch.
        sent.sent(4, 0, 6_000);
        assert_eq!(sent.network_part(4, 0, 26_000), 20_000);
        assert_eq!(sent.network_part(3, 0, 24_000), 20_000);
        assert_eq!(sent.network_part(4, 1, 45_000), 45_000, "not sent yet");
        // Unknown: left as it is. Hostile values don't overflow.
        assert_eq!(sent.network_part(9, 77, 30_000), 30_000);
        assert_eq!(sent.network_part(4, 0, i64::MIN), i64::MIN);
    }

    #[test]
    fn a_busy_encoder_is_not_congestion() {
        // Every other frame waits 30 ms more for the encoder; the network
        // takes 20 ms throughout.
        let start = Instant::now();
        let mut tracker = FeedbackTracker::new(start);
        let mut sent = SendDelays::new();
        tracker.on_epoch(1);
        let mut trend = 0;
        for report in 0..40u32 {
            let now = start + Duration::from_millis(50 * u64::from(report));
            let mut delays = Vec::new();
            for i in 0..3 {
                let seq = report * 3 + i;
                let host = if seq % 2 == 0 { 2_000 } else { 32_000 } + i64::from(report) * 300;
                sent.sent(1, seq, host);
                delays.push(sent.network_part(1, seq, host + 20_000));
            }
            tracker.on_report(now, 1, 0, 0, &delays);
            trend = tracker.snapshot(now, PathSample::default()).delay_trend_us;
        }
        // Encoding got slower all along (300 µs per report); none of it shows.
        assert_eq!(trend, 0);
    }

    #[test]
    fn viewers_split_the_upload_cap() {
        // Everyone wants more than an equal share: equal shares.
        assert_eq!(
            share_upload(30 * MBPS, &[20 * MBPS, 20 * MBPS, 20 * MBPS]),
            vec![10 * MBPS; 3]
        );
        // One held back at 4 Mbit/s by its network keeps room to grow; the
        // rest is split between the others.
        assert_eq!(
            share_upload(30 * MBPS, &[20 * MBPS, 4 * MBPS, 20 * MBPS]),
            vec![12_500_000, 5 * MBPS, 12_500_000]
        );
        // Nobody needs much: margins only, and what is left goes unused.
        assert_eq!(
            share_upload(100 * MBPS, &[2 * MBPS, 3 * MBPS]),
            vec![2_500_000, 3_500_000]
        );
        // A viewer down at the minimum still gets room to grow.
        assert_eq!(
            share_upload(30 * MBPS, &[1_500_000, 20 * MBPS]),
            vec![2 * MBPS, 25 * MBPS]
        );
        assert!(share_upload(30 * MBPS, &[]).is_empty());
    }

    #[test]
    fn shares_never_add_up_to_more_than_the_cap() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..2000 {
            let total = (next() % 100 + 1) as u32 * MBPS;
            let n = (next() % 7 + 1) as usize;
            let targets: Vec<u32> = (0..n).map(|_| (next() % 40_000_000) as u32).collect();
            let shares = share_upload(total, &targets);
            let sum: u64 = shares.iter().map(|&s| u64::from(s)).sum();
            assert!(sum <= u64::from(total), "{targets:?} -> {shares:?}");
        }
    }

    /// A viewer pushed down to the minimum by a bad patch climbs back once
    /// its network recovers, with the cap's share recomputed every second.
    #[test]
    fn a_viewer_at_the_minimum_climbs_back_under_a_cap() {
        let start = Instant::now();
        let cfg = RateConfig {
            min_bps: 1_500_000,
            start_bps: 1_500_000,
            ..config(false)
        };
        let mut c = RateController::new(cfg);
        let mut share = UploadShare::new(30 * MBPS);
        for second in 0..20u64 {
            let shares = share.update(&[('b', c.target_bps()), ('c', 20 * MBPS)]);
            c.set_share(Some(shares[0].1));
            run(&mut c, start + Duration::from_secs(second), 1.0, |_| {
                clean()
            });
        }
        assert!(c.applied_bps() >= 5 * MBPS, "{}", c.applied_bps());
    }

    #[test]
    fn small_swings_leave_the_shares_alone() {
        let mut share = UploadShare::new(30 * MBPS);
        let (bob, carol) = ('b', 'c');
        let update = |share: &mut UploadShare<char>, bob_target: u32| {
            share.update(&[(bob, bob_target), (carol, 20 * MBPS)])
        };
        assert_eq!(
            update(&mut share, 4 * MBPS),
            vec![(bob, 5 * MBPS), (carol, 25 * MBPS)]
        );
        // Bob grows a little: his share rises; Carol's cut is small, so she
        // keeps hers (the cap is overrun by under 10%).
        assert_eq!(
            update(&mut share, 4_800_000),
            vec![(bob, 6 * MBPS), (carol, 25 * MBPS)]
        );
        // Bob grows more: now Carol's cut is large enough to take.
        assert_eq!(
            update(&mut share, 6_500_000),
            vec![(bob, 8 * MBPS), (carol, 22 * MBPS)]
        );
        // A newcomer starts afresh rather than inheriting Bob's share.
        let dan = 'd';
        let shares = share.update(&[(dan, 20 * MBPS), (carol, 20 * MBPS)]);
        assert_eq!(shares, vec![(dan, 15 * MBPS), (carol, 15 * MBPS)]);
    }
}
