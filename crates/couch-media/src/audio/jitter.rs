//! The viewer's audio jitter buffer. Sans-IO: the caller passes arrival times
//! in, and the audio device's pull drives [`JitterBuffer::pop`], one 10 ms
//! frame at a time.
//!
//! - **Target delay.** Packets arrive unevenly. The buffer holds enough that a
//!   late packet still makes its turn: the spread of recent transit times
//!   (arrival minus capture, 95th percentile over the last 2 s) plus one
//!   frame, kept between 10 and 40 ms.
//! - **Loss.** A packet missing at its turn while newer ones are already
//!   here is lost (or badly reordered): it is concealed (the decoder
//!   extrapolates) and skipped, and dropped if it turns up later.
//! - **Lateness.** A packet missing with nothing newer here is probably just
//!   late: a frame is concealed but playback waits for it, so it still
//!   plays. The delay that adds is trimmed by the playback ratio.
//! - **Running dry.** After three frames concealed with nothing buffered,
//!   playback stops and outputs silence until the buffer refills to the
//!   target. That covers both network stalls and a host whose source went
//!   quiet. Packets that already missed their turn stay rejected.
//! - **Restart.** A sequence number more than 50 frames behind or ahead of
//!   the newest starts the buffer afresh: the sender restarted, or
//!   everything in between is lost anyway.
//! - **Drift and excess.** The two machines' audio clocks never quite agree,
//!   and a burst after a stall overfills the buffer. [`JitterBuffer::playback_ratio`]
//!   tells the player to play slightly faster or slower (at most 2%) until
//!   the level is back at the target. Far too much is dropped at once.

use std::collections::{BTreeMap, VecDeque};

use super::FRAME_US;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JitterConfig {
    pub frame_us: u64,
    pub min_target_us: u64,
    pub max_target_us: u64,
    /// Buffered audio this far above the target is dropped at once.
    pub overflow_us: u64,
    /// Frames concealed in a row with nothing buffered before playback
    /// stops and waits for the buffer to refill.
    pub max_blind: u32,
}

impl Default for JitterConfig {
    fn default() -> Self {
        Self {
            frame_us: FRAME_US,
            // One frame: a clean path needs no more (M3 measured 10 and 20).
            min_target_us: 10_000,
            max_target_us: 40_000,
            overflow_us: 60_000,
            max_blind: 3,
        }
    }
}

/// What to play for the next frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Pop<T> {
    Packet(T),
    /// The packet is missing: play the decoder's concealment.
    Conceal,
    /// Not playing (empty, or refilling): play silence.
    Silence,
}

/// Counts since the buffer was created, in frames.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JitterStats {
    pub played: u64,
    pub concealed: u64,
    pub late: u64,
    pub dropped: u64,
    pub underruns: u64,
}

/// Arrival history kept for the target.
const ARRIVAL_WINDOW_US: u64 = 2_000_000;
const MAX_ARRIVALS: usize = 400;
/// A packet this many frames behind or ahead of the newest means the sender
/// started over, or everything in between is gone: start afresh.
const RESTART_FRAMES: u64 = 50;
/// Pops averaged for the playback ratio.
const LEVEL_WINDOW: usize = 50;
const MAX_CORRECTION: f64 = 0.02;
/// The ratio aims to remove the level's error over this long.
const CORRECTION_US: f64 = 2_000_000.0;

#[derive(Debug, Clone, Copy)]
struct Arrival {
    at_us: u64,
    /// Arrival time minus capture time. The two clocks belong to different
    /// machines, but only differences matter, so their offset cancels. A
    /// sender whose capture stalls for a while doesn't look like lateness.
    transit_us: i64,
}

#[derive(Debug)]
pub struct JitterBuffer<T> {
    config: JitterConfig,
    /// Keyed by the sequence number extended to 64 bits.
    packets: BTreeMap<u64, T>,
    highest: Option<u64>,
    /// The next frame to play. Once playback has started it stays valid
    /// while refilling too, so packets that already missed their turn stay
    /// rejected; only a restart clears it.
    next: u64,
    next_valid: bool,
    playing: bool,
    blind: u32,
    arrivals: VecDeque<Arrival>,
    target_us: u64,
    levels: VecDeque<u64>,
    level_sum: u64,
    stats: JitterStats,
}

impl<T> JitterBuffer<T> {
    pub fn new(config: JitterConfig) -> Self {
        Self {
            config,
            packets: BTreeMap::new(),
            highest: None,
            next: 0,
            next_valid: false,
            playing: false,
            blind: 0,
            arrivals: VecDeque::new(),
            target_us: config.min_target_us,
            levels: VecDeque::new(),
            level_sum: 0,
            stats: JitterStats::default(),
        }
    }

    /// A packet captured at `captured_us` on the sender's clock arrived at
    /// `now_us` on ours (any two clocks, as long as each stays the same).
    pub fn push(&mut self, seq: u32, item: T, captured_us: u64, now_us: u64) {
        let mut ext = self.extend(seq);
        if self.highest.is_some_and(|h| {
            ext.saturating_add(RESTART_FRAMES) < h || ext > h.saturating_add(RESTART_FRAMES)
        }) {
            self.restart();
            ext = self.extend(seq);
        }
        // Late packets count most: they are what the target must cover.
        self.record_arrival(captured_us, now_us);
        if self.next_valid && ext < self.next {
            self.stats.late += 1;
            return;
        }
        if self.highest.is_none_or(|h| ext > h) {
            self.highest = Some(ext);
        }
        self.packets.insert(ext, item);
    }

    /// The next frame. Call once per 10 ms of playback.
    pub fn pop(&mut self) -> Pop<T> {
        let frame = self.config.frame_us;
        if !self.playing {
            let (Some(&first), Some(highest)) = (self.packets.keys().next(), self.highest) else {
                return Pop::Silence;
            };
            if (highest + 1 - first) * frame < self.target_us {
                return Pop::Silence;
            }
            self.playing = true;
            self.next = first;
            self.next_valid = true;
            self.blind = 0;
        }

        let level = self.level_us();
        if level > self.target_us + self.config.overflow_us {
            // Keep the newest target's worth.
            let keep_from = self.highest.unwrap_or(0) + 1 - self.target_us / frame;
            let newer = self.packets.split_off(&keep_from);
            self.stats.dropped += self.packets.len() as u64;
            self.packets = newer;
            self.next = keep_from;
            self.levels.clear();
            self.level_sum = 0;
        }
        self.record_level(self.level_us());

        if let Some(item) = self.packets.remove(&self.next) {
            self.next += 1;
            self.blind = 0;
            self.stats.played += 1;
            return Pop::Packet(item);
        }
        match self.packets.keys().next().copied() {
            // Nothing newer has arrived either: the packet is more likely
            // late than lost. Conceal without moving on, so it still plays
            // when it comes; the delay this adds is trimmed later.
            None if self.blind < self.config.max_blind => {
                self.blind += 1;
                self.stats.concealed += 1;
                Pop::Conceal
            }
            None => {
                self.playing = false;
                self.stats.underruns += 1;
                self.levels.clear();
                self.level_sum = 0;
                Pop::Silence
            }
            Some(first) => {
                // A long hole with audio after it: conceal one frame, then
                // go straight to the audio instead of concealing the hole.
                if first - self.next > u64::from(self.config.max_blind) {
                    self.next = first;
                } else {
                    self.next += 1;
                }
                self.stats.concealed += 1;
                Pop::Conceal
            }
        }
    }

    /// How fast to play, relative to normal: above 1 drains the buffer,
    /// below 1 fills it. 1 while the level is within a quarter frame of the
    /// target.
    pub fn playback_ratio(&self) -> f64 {
        if !self.playing || self.levels.len() < LEVEL_WINDOW / 2 {
            return 1.0;
        }
        let err = self.buffered_us() as f64 - self.target_us as f64;
        if err.abs() <= self.config.frame_us as f64 / 4.0 {
            return 1.0;
        }
        1.0 + (err / CORRECTION_US).clamp(-MAX_CORRECTION, MAX_CORRECTION)
    }

    pub fn target_us(&self) -> u64 {
        self.target_us
    }

    /// The level averaged over recent pops (0 while not playing).
    pub fn buffered_us(&self) -> u64 {
        if self.levels.is_empty() {
            0
        } else {
            self.level_sum / self.levels.len() as u64
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn stats(&self) -> JitterStats {
        self.stats
    }

    fn extend(&self, seq: u32) -> u64 {
        match self.highest {
            // Start high, so a sequence going a little backwards stays positive.
            None => (1 << 32) + u64::from(seq),
            Some(h) => {
                let delta = seq.wrapping_sub(h as u32) as i32;
                h.saturating_add_signed(i64::from(delta))
            }
        }
    }

    fn restart(&mut self) {
        self.packets.clear();
        self.highest = None;
        self.next_valid = false;
        self.playing = false;
        self.blind = 0;
        self.arrivals.clear();
        self.target_us = self.config.min_target_us;
        self.levels.clear();
        self.level_sum = 0;
    }

    fn level_us(&self) -> u64 {
        self.highest.map_or(0, |h| {
            (h + 1).saturating_sub(self.next) * self.config.frame_us
        })
    }

    fn record_level(&mut self, level: u64) {
        if self.levels.len() == LEVEL_WINDOW {
            self.level_sum -= self.levels.pop_front().unwrap_or(0);
        }
        self.levels.push_back(level);
        self.level_sum += level;
    }

    fn record_arrival(&mut self, captured_us: u64, now_us: u64) {
        self.arrivals.push_back(Arrival {
            at_us: now_us,
            // Saturating: `captured_us` comes from the peer.
            transit_us: (now_us as i64).saturating_sub(captured_us as i64),
        });
        while self.arrivals.len() > MAX_ARRIVALS
            || self
                .arrivals
                .front()
                .is_some_and(|a| now_us.saturating_sub(a.at_us) > ARRIVAL_WINDOW_US)
        {
            self.arrivals.pop_front();
        }

        let mut transits: Vec<i64> = self.arrivals.iter().map(|a| a.transit_us).collect();
        transits.sort_unstable();
        let spread = transits[(transits.len() - 1) * 95 / 100]
            .saturating_sub(transits[0])
            .max(0) as u64;
        self.target_us = spread
            .saturating_add(self.config.frame_us)
            .clamp(self.config.min_target_us, self.config.max_target_us);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: u64 = FRAME_US;

    /// A 20 ms minimum, so filling up takes two frames in these tests.
    fn buffer() -> JitterBuffer<u32> {
        JitterBuffer::new(JitterConfig {
            min_target_us: 20_000,
            ..JitterConfig::default()
        })
    }

    /// Plays `pops` frames while packets arrive one per frame, starting at
    /// `seq`, `delay(i)` microseconds after their slot. Each is captured at
    /// the start of its slot (one clock for both sides, for simplicity).
    /// Returns what played.
    fn run(
        jb: &mut JitterBuffer<u32>,
        seq: u32,
        start_us: u64,
        pops: usize,
        delay: impl Fn(usize) -> u64,
        skip: &[u32],
    ) -> Vec<Pop<u32>> {
        let mut arrivals: Vec<(u64, u32, u64)> = (0..pops)
            .map(|i| {
                let captured = start_us + i as u64 * F;
                (captured + delay(i), seq.wrapping_add(i as u32), captured)
            })
            .filter(|(_, s, _)| !skip.contains(s))
            .collect();
        arrivals.sort();
        let mut out = Vec::new();
        let mut a = 0;
        for i in 0..pops {
            // The device pulls a frame in the middle of each slot.
            let now = start_us + i as u64 * F + F / 2;
            while a < arrivals.len() && arrivals[a].0 <= now {
                let (at, s, captured) = arrivals[a];
                jb.push(s, s, captured, at);
                a += 1;
            }
            out.push(jb.pop());
        }
        out
    }

    fn played(out: &[Pop<u32>]) -> Vec<u32> {
        out.iter()
            .filter_map(|p| match p {
                Pop::Packet(s) => Some(*s),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_steady_stream_plays_in_order_after_filling_to_the_target() {
        let mut jb = buffer();
        let out = run(&mut jb, 100, 0, 200, |_| 0, &[]);
        // Silence until two frames (20 ms) are in, then everything.
        assert_eq!(out[0], Pop::Silence);
        assert_eq!(played(&out), (100..299).collect::<Vec<_>>());
        assert_eq!(jb.stats().concealed, 0);
        assert_eq!(jb.target_us(), 20_000);
        assert_eq!(jb.playback_ratio(), 1.0);
    }

    #[test]
    fn a_lost_packet_is_concealed_and_a_late_one_dropped() {
        let mut jb = buffer();
        let out = run(&mut jb, 0, 0, 50, |_| 0, &[20]);
        assert_eq!(jb.stats().concealed, 1);
        assert!(!played(&out).contains(&20));
        let index = out.iter().position(|p| *p == Pop::Conceal).unwrap();
        assert_eq!(out[index - 1], Pop::Packet(19));
        assert_eq!(out[index + 1], Pop::Packet(21));
        // It turns up after its turn.
        jb.push(20, 20, 200_000, 600_000);
        assert_eq!(jb.stats().late, 1);
    }

    #[test]
    fn a_late_packet_with_nothing_after_it_is_waited_for() {
        let mut jb = buffer();
        run(&mut jb, 0, 0, 20, |_| 0, &[]);
        assert_eq!(jb.pop(), Pop::Packet(19));
        // 20 is late and nothing newer is here: conceal, but keep its place.
        assert_eq!(jb.pop(), Pop::Conceal);
        jb.push(20, 20, 200_000, 225_000);
        assert_eq!(jb.pop(), Pop::Packet(20));
        assert_eq!(jb.stats().late, 0);
    }

    #[test]
    fn reordered_packets_play_in_order() {
        let mut jb = buffer();
        // Every odd packet arrives 8 ms late, after the next even one.
        let out = run(
            &mut jb,
            0,
            0,
            100,
            |i| if i % 2 == 1 { 12_000 } else { 0 },
            &[],
        );
        let p = played(&out);
        assert!(p.windows(2).all(|w| w[1] == w[0] + 1), "{p:?}");
        assert_eq!(jb.stats().concealed, 0);
    }

    #[test]
    fn jitter_raises_the_target_and_keeps_late_packets() {
        let mut jb = buffer();
        // A third of the packets come 25 ms late: a 20 ms buffer would miss them.
        let out = run(
            &mut jb,
            0,
            0,
            300,
            |i| if i % 3 == 0 { 25_000 } else { 0 },
            &[],
        );
        assert_eq!(jb.target_us(), 35_000);
        let late = jb.stats().concealed;
        assert!(late <= 3, "only before the target caught up: {late}");
        assert!(played(&out).len() >= 290);
    }

    #[test]
    fn the_target_is_capped() {
        let mut jb = buffer();
        run(
            &mut jb,
            0,
            0,
            100,
            |i| if i % 2 == 0 { 80_000 } else { 0 },
            &[],
        );
        assert_eq!(jb.target_us(), 40_000);
    }

    #[test]
    fn running_dry_conceals_three_frames_then_waits_to_refill() {
        let mut jb = buffer();
        run(&mut jb, 0, 0, 30, |_| 0, &[]);
        // The sender stops: the buffered packets play out, then three
        // concealed frames, then silence.
        let mut tail = Vec::new();
        for _ in 0..8 {
            tail.push(jb.pop());
        }
        let conceals = tail.iter().filter(|p| **p == Pop::Conceal).count();
        assert_eq!(conceals, 3, "{tail:?}");
        assert_eq!(tail.last(), Some(&Pop::Silence));
        assert_eq!(jb.stats().underruns, 1);
        assert!(!jb.is_playing());

        // A packet that already played turns up again: still too late, and
        // it mustn't restart playback with stale audio.
        jb.push(25, 25, 250_000, 400_000);
        assert_eq!(jb.stats().late, 1);
        assert_eq!(jb.pop(), Pop::Silence);

        // It resumes where it left off (the host's capture stalled).
        let out = run(&mut jb, 30, 5_000_000, 20, |_| 0, &[]);
        assert_eq!(out[0], Pop::Silence);
        assert_eq!(played(&out)[0], 30);
        assert_eq!(jb.stats().late, 1);
    }

    #[test]
    fn short_pauses_in_the_source_are_not_jitter() {
        let mut jb = buffer();
        run(&mut jb, 0, 0, 100, |_| 0, &[]);
        // The host's capture stalls for 200 ms: nothing is sent, and the
        // sequence carries on afterwards. The device keeps pulling meanwhile.
        for _ in 0..20 {
            jb.pop();
        }
        run(&mut jb, 100, 1_200_000, 300, |_| 0, &[]);
        assert_eq!(jb.target_us(), 20_000);
        assert_eq!(jb.playback_ratio(), 1.0);
        assert_eq!(jb.stats().late, 0);
    }

    #[test]
    fn a_hostile_sender_cannot_overflow_or_grow_the_buffer() {
        let mut jb = buffer();
        for i in 0..20_000u32 {
            let seq = i.wrapping_mul(0x7FFF_FFFF);
            let captured = if i % 2 == 0 { u64::MAX } else { 0 };
            jb.push(seq, i, captured, u64::from(i) * F);
            jb.pop();
            jb.playback_ratio();
        }
        // Every jump restarts the buffer, so nothing piles up.
        assert!(jb.packets.len() <= 64, "{}", jb.packets.len());
        assert!((20_000..=40_000).contains(&jb.target_us()));
    }

    #[test]
    fn a_burst_after_a_stall_is_trimmed_to_the_target() {
        let mut jb = buffer();
        run(&mut jb, 0, 0, 20, |_| 0, &[]);
        // 19 is next. 300 ms of packets arrive at once; the spread of
        // arrivals pushes the target to its 40 ms cap.
        for s in 20..50 {
            jb.push(s, s, u64::from(s) * F, 300_000);
        }
        // Everything but the newest 40 ms goes.
        assert_eq!(jb.pop(), Pop::Packet(46));
        assert_eq!(jb.stats().dropped, 27);
    }

    #[test]
    fn a_long_hole_is_skipped_not_concealed() {
        let mut jb = buffer();
        run(&mut jb, 0, 0, 20, |_| 0, &[]);
        // 19 is next. 20 to 23 are lost; 24 and 25 arrive on time.
        jb.push(24, 24, 240_000, 240_000);
        jb.push(25, 25, 250_000, 250_000);
        let out: Vec<_> = (0..4).map(|_| jb.pop()).collect();
        assert_eq!(
            out,
            [
                Pop::Packet(19),
                Pop::Conceal,
                Pop::Packet(24),
                Pop::Packet(25)
            ]
        );
    }

    #[test]
    fn a_fuller_buffer_than_the_target_plays_faster() {
        let mut jb = buffer();
        // The device starts late: five frames are waiting when it does.
        for s in 0..5 {
            jb.push(s, s, u64::from(s) * F, u64::from(s) * F);
        }
        let out = run(&mut jb, 5, 50_000, 60, |_| 0, &[]);
        assert_eq!(played(&out)[0], 0);
        // The level stays 40 ms above the target: play 2% fast (the cap).
        assert!((jb.playback_ratio() - 1.02).abs() < 1e-9);
    }

    #[test]
    fn sequence_numbers_wrap() {
        let mut jb = buffer();
        let out = run(&mut jb, u32::MAX - 20, 0, 60, |_| 0, &[]);
        let p = played(&out);
        assert!(p.contains(&u32::MAX) && p.contains(&0) && p.contains(&10));
        assert_eq!(jb.stats().concealed, 0);
    }

    #[test]
    fn a_sender_that_starts_over_is_followed() {
        let mut jb = buffer();
        run(&mut jb, 5_000, 0, 30, |_| 0, &[]);
        let out = run(&mut jb, 0, 1_000_000, 30, |_| 0, &[]);
        assert!(played(&out).contains(&20));
        assert_eq!(jb.stats().late, 0);
    }
}
