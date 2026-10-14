//! Pacing captured frames to the stream's frame rate (M9d).
//!
//! Windows only composes on the display's refresh, so a capture limited to
//! one frame per interval gets the first refresh *after* the interval: 48 fps
//! of a 60 fps stream on a 144 Hz screen, 80 of 100 on a 240 Hz one. Instead
//! the capture runs faster (up to every refresh), and the [`Pacer`] picks
//! which frames go out:
//! - due times advance by exactly one interval, so the average rate is the
//!   frame rate;
//! - a frame goes at once if it is at or past its due time, or early by less
//!   than half the shortest recent gap between frames (at most a quarter of
//!   an interval), since the next one would land further from the due time
//!   than this one;
//! - an earlier frame is held, and dropped if a newer one comes within an
//!   interval of its due time: the newer one is fresher, and goes at once,
//!   being late. So a game a little faster than the stream (61 fps into
//!   60) loses a frame now and then, and none waits. A shorter wait sent the
//!   held frame first, which pushed every later frame to be held too: up to
//!   an interval of latency each, for seconds (M9d's review).
//! - a held frame that nothing replaces goes after that wait: the last
//!   change before a still picture.
//!
//! The schedule runs on the frames' capture times, which come from the
//! compositor; the held frame's wait is measured on the clock frames arrive
//! by (`offer`'s `arrived`, [`Pacer::due`], [`Pacer::take_due`]), so a
//! difference between the two clocks can't hold a frame longer.

use crate::Micros;

/// Recent gaps between frames kept for estimating the next one.
const GAPS: usize = 8;
/// Recent delays from capture to arrival kept for converting between them.
const DELAYS: usize = 32;

pub struct Pacer<T> {
    interval: u64,
    /// When the next frame is due, in capture time; `None` until the first.
    due: Option<u64>,
    held: Option<T>,
    last_capture: Option<u64>,
    gaps: [u64; GAPS],
    next_gap: usize,
    /// Arrival minus capture time of recent frames: the smallest is the
    /// offset from capture time to arrival time, delivery aside.
    delays: [i64; DELAYS],
    delays_seen: usize,
    /// Held frames sent after their wait, and held frames a newer one
    /// replaced.
    sent_held: u64,
    replaced: u64,
}

impl<T> Pacer<T> {
    pub fn new(fps: u32) -> Self {
        Self {
            interval: 1_000_000 / u64::from(fps.max(1)),
            due: None,
            held: None,
            last_capture: None,
            gaps: [0; GAPS],
            next_gap: 0,
            delays: [0; DELAYS],
            delays_seen: 0,
            sent_held: 0,
            replaced: 0,
        }
    }

    /// A frame captured at `captured` arrived at `arrived` (each on its own
    /// clock): returns it if it goes now. Otherwise it is held until
    /// [`Pacer::due`], and dropped if a newer one comes first.
    pub fn offer(&mut self, frame: T, captured: Micros, arrived: Micros) -> Option<T> {
        let at = captured.0;
        if let Some(last) = self.last_capture
            && at > last
        {
            self.gaps[self.next_gap] = at - last;
            self.next_gap = (self.next_gap + 1) % GAPS;
        }
        self.last_capture = Some(at);
        self.delays[self.delays_seen % DELAYS] = arrived.0 as i64 - at as i64;
        self.delays_seen += 1;
        if self.held.take().is_some() {
            self.replaced += 1;
        }
        match self.due {
            Some(due) if at + self.slack() < due => {
                self.held = Some(frame);
                None
            }
            _ => {
                self.sent(at);
                Some(frame)
            }
        }
    }

    /// When, on the arrival clock, the held frame goes if nothing newer
    /// comes; `None` if none is held.
    pub fn due(&self) -> Option<Micros> {
        let due = self.held.as_ref().and(self.due)?;
        Some(Micros(
            ((due + self.interval) as i64 + self.offset()).max(0) as u64,
        ))
    }

    /// The held frame, if its time ([`Pacer::due`]) has come by `now`, on
    /// the arrival clock.
    pub fn take_due(&mut self, now: Micros) -> Option<T> {
        if now < self.due()? {
            return None;
        }
        let frame = self.held.take()?;
        self.sent_held += 1;
        // Sent now: after a whole interval's wait, the schedule starts
        // again from here, so a frame arriving next can't follow at once.
        self.sent((now.0 as i64 - self.offset()).max(0) as u64);
        Some(frame)
    }

    /// Held frames sent after their wait, and held frames replaced by a
    /// newer one, since the pacer started.
    pub fn counts(&self) -> (u64, u64) {
        (self.sent_held, self.replaced)
    }

    /// Arrival time minus capture time, delivery aside.
    fn offset(&self) -> i64 {
        self.delays[..self.delays_seen.min(DELAYS)]
            .iter()
            .copied()
            .min()
            .unwrap_or(0)
    }

    /// A frame went at `at`: the next is due one interval after the last
    /// due time. After a pause (more than an interval late), the schedule
    /// starts again from this frame instead of catching up; and no frame is
    /// due sooner than half an interval after this one.
    fn sent(&mut self, at: u64) {
        self.due = Some(match self.due {
            Some(due) if at < due + self.interval => {
                (due + self.interval).max(at + self.interval / 2)
            }
            _ => at + self.interval,
        });
    }

    /// How early a frame may go: half the shortest recent gap between
    /// frames, at most a quarter of an interval.
    fn slack(&self) -> u64 {
        let gap = self.gaps.iter().copied().filter(|&g| g > 0).min();
        gap.map_or(0, |g| (g / 2).min(self.interval / 4))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows' minimum update interval with frames on the GPU.
    const CAPTURE_FLOOR_US: f64 = 4_000.0;

    /// How long a captured frame takes to reach the capture thread.
    const DELIVERY_US: u64 = 1_000;

    /// A screen at `hz` showing a game that renders `game_fps` frames a
    /// second (each shown from the next refresh), captured with the 4 ms
    /// floor, each frame arriving [`DELIVERY_US`] after its capture, and
    /// paced to `fps` for `secs`. Held frames go at their time if `timer` (a
    /// precise wait), else only when the next frame arrives. Returns every
    /// frame sent: when it went, and how long it waited after arriving.
    fn simulate(hz: f64, game_fps: f64, fps: u32, secs: f64, timer: bool) -> Vec<(u64, u64)> {
        let period = 1e6 / hz;
        let mut pacer = Pacer::new(fps);
        let mut sent = Vec::new();
        let mut last_capture = f64::NEG_INFINITY;
        let mut shown = 0u64;
        let mut k = 0u64;
        loop {
            let t = k as f64 * period;
            k += 1;
            if t > secs * 1e6 {
                break;
            }
            // Windows delivers a frame only when the picture changed.
            let rendered = (t * game_fps / 1e6).floor() as u64 + 1;
            if rendered == shown || t - last_capture < CAPTURE_FLOOR_US {
                continue;
            }
            (shown, last_capture) = (rendered, t);
            let captured = t.round() as u64;
            let arrived = captured + DELIVERY_US;
            if timer
                && let Some(due) = pacer.due()
                && due.0 <= arrived
                && let Some(held_arrived) = pacer.take_due(due)
            {
                sent.push((due.0, due.0 - held_arrived));
            }
            if pacer
                .offer(arrived, Micros(captured), Micros(arrived))
                .is_some()
            {
                sent.push((arrived, 0));
            }
        }
        sent
    }

    /// A screen whose picture changes every refresh: a game faster than it.
    fn busy(hz: f64) -> f64 {
        hz * 4.0
    }

    fn rate(sent: &[(u64, u64)]) -> f64 {
        (sent.len() - 1) as f64 * 1e6 / (sent[sent.len() - 1].0 - sent[0].0) as f64
    }

    fn min_gap(sent: &[(u64, u64)]) -> u64 {
        sent.windows(2).map(|w| w[1].0 - w[0].0).min().unwrap()
    }

    #[test]
    fn the_average_rate_is_the_frame_rate_or_the_displays() {
        for hz in [60.0, 75.0, 144.0, 165.0, 239.76, 240.0] {
            for fps in [30, 60, 100, 120] {
                for timer in [true, false] {
                    let sent = simulate(hz, busy(hz), fps, 10.0, timer);
                    let expected = f64::from(fps).min(hz);
                    let got = rate(&sent);
                    assert!(
                        (got - expected).abs() / expected < 0.005,
                        "{hz} Hz at {fps} fps (timer {timer}): {got:.2} fps"
                    );
                    let interval = 1_000_000 / u64::from(fps);
                    assert!(
                        min_gap(&sent) >= interval / 4,
                        "{hz} Hz at {fps} fps (timer {timer}): frames {} us apart",
                        min_gap(&sent)
                    );
                }
            }
        }
    }

    #[test]
    fn a_game_near_the_streams_rate_has_no_frame_held() {
        // A game a little faster than the 60 fps stream (the review's case),
        // or slower: every frame goes as it arrives, and the stream's rate
        // is the lower of the two.
        for (hz, game) in [
            (240.0, 61.0),
            (240.0, 66.0),
            (240.0, 70.0),
            (239.76, 61.0),
            (144.0, 70.0),
            (144.0, 100.0),
            (165.0, 90.0),
            (240.0, 50.0),
        ] {
            let sent = simulate(hz, game, 60, 20.0, true);
            let expected = game.min(60.0);
            let got = rate(&sent);
            assert!(
                (got - expected).abs() / expected < 0.01,
                "a {game} fps game on {hz} Hz: {got:.2} fps"
            );
            let waited: Vec<_> = sent.iter().filter(|s| s.1 > 0).collect();
            assert!(
                waited.is_empty(),
                "a {game} fps game on {hz} Hz: {} frames waited, the longest {} us",
                waited.len(),
                waited.iter().map(|s| s.1).max().unwrap_or(0)
            );
        }
    }

    #[test]
    fn frames_already_at_the_rate_go_straight_through() {
        // The test pattern paces itself: every frame goes, none is held.
        let mut pacer = Pacer::new(60);
        for i in 0..600u64 {
            // A little late, as a sleeping source is.
            let at = Micros(i * 16_667 + i % 3 * 300);
            assert!(pacer.offer(i, at, at).is_some(), "frame {i} held");
        }
        assert_eq!(pacer.counts(), (0, 0));
    }

    #[test]
    fn a_frame_slightly_early_goes_at_once() {
        // 120 fps on a 144 Hz screen: frames 6.9 ms apart, due every 8.3 ms.
        let mut pacer = Pacer::new(120);
        let offer = |pacer: &mut Pacer<()>, at: u64| pacer.offer((), Micros(at), Micros(at));
        for i in 0..10u64 {
            offer(&mut pacer, i * 6_944);
        }
        // The last frame went at 62.5 ms, due again at 66.7: 69.4 is late
        // and goes. The next is due at 75.0: 74.4 is early by 0.6 ms, under
        // the slack (a quarter interval here), and goes too.
        assert!(offer(&mut pacer, 69_444).is_some());
        assert!(offer(&mut pacer, 74_400).is_some());
        // 76.0 is early by 7.3 ms: held.
        assert!(offer(&mut pacer, 76_000).is_none());
    }

    #[test]
    fn an_early_frame_before_a_still_picture_goes_after_an_interval() {
        let mut pacer = Pacer::new(60);
        assert!(pacer.offer(1, Micros(0), Micros(0)).is_some());
        // A change 5 ms later, then nothing more. Due at 16.7 ms, it waits
        // an interval more for a newer frame.
        assert!(pacer.offer(2, Micros(5_000), Micros(5_000)).is_none());
        assert_eq!(pacer.due(), Some(Micros(33_332)));
        assert_eq!(pacer.take_due(Micros(30_000)), None);
        assert_eq!(pacer.take_due(Micros(33_400)), Some(2));
        assert_eq!(pacer.due(), None);
        assert_eq!(pacer.counts(), (1, 0));
        // The schedule starts again from then: a change right after waits.
        assert!(pacer.offer(3, Micros(34_000), Micros(34_000)).is_none());
    }

    #[test]
    fn the_wait_runs_on_the_arrival_clock() {
        // Capture times 50 ms ahead of the clock frames arrive by: the held
        // frame goes an interval past its due time on the arrival clock.
        let mut pacer = Pacer::new(60);
        assert!(pacer.offer(1, Micros(50_000), Micros(0)).is_some());
        assert!(pacer.offer(2, Micros(55_000), Micros(5_000)).is_none());
        assert_eq!(pacer.due(), Some(Micros(33_332)));
        assert_eq!(pacer.take_due(Micros(33_400)), Some(2));
    }

    #[test]
    fn the_frame_captured_at_the_due_time_beats_the_held_one() {
        // 120 fps on a 240 Hz screen: frames every 4.2 ms, due every 8.3,
        // each arriving 1 ms after its capture.
        let mut pacer = Pacer::new(120);
        assert!(pacer.offer(0, Micros(0), Micros(1_000)).is_some());
        assert!(
            pacer.offer(1, Micros(4_167), Micros(5_167)).is_none(),
            "early: held"
        );
        // At 9.0 ms the frame captured at 8.3 hasn't reached us yet: the
        // held one waits for it.
        assert_eq!(pacer.take_due(Micros(9_000)), None);
        assert_eq!(pacer.offer(2, Micros(8_334), Micros(9_334)), Some(2));
        assert_eq!(pacer.counts(), (0, 1));
    }

    #[test]
    fn a_newer_frame_replaces_the_held_one() {
        let mut pacer = Pacer::new(60);
        pacer.offer(1, Micros(0), Micros(0));
        assert!(pacer.offer(2, Micros(4_000), Micros(4_000)).is_none());
        assert!(pacer.offer(3, Micros(8_000), Micros(8_000)).is_none());
        assert_eq!(pacer.take_due(Micros(34_000)), Some(3));
        assert_eq!(pacer.counts(), (1, 1));
    }

    #[test]
    fn after_a_pause_frames_dont_catch_up() {
        let mut pacer = Pacer::new(60);
        pacer.offer((), Micros(0), Micros(0));
        // A still picture for a second, then a change every 4 ms.
        let mut sent = Vec::new();
        for i in 0..50u64 {
            let at = 1_000_000 + i * 4_000;
            if pacer.offer((), Micros(at), Micros(at)).is_some() {
                sent.push((at, 0));
            }
        }
        // 200 ms at 60 fps: about 12 frames, never two within an interval.
        assert!((12..=13).contains(&sent.len()), "{sent:?}");
        assert!(min_gap(&sent) >= 12_000, "{sent:?}");
    }
}
