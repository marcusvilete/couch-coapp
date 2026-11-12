//! Keeping a guest's pointer inside the game's window on a host that moves
//! it relatively (a Linux host: its `uinput` mouse), where nothing else
//! stops it reaching the title bar's buttons, other windows or a hot corner.
//!
//! The pointer itself can't be placed: an absolute device would give games
//! that lock the pointer (mouse-look) no motion at all, since the desktops
//! derive no relative motion from one. And only the guest's moves are ours
//! to cut, never the pointer: the host's own mouse drives the same one.
//!
//! So each relative move is cut to what keeps the pointer inside, from the
//! position the desktop last reported, whatever the desktop's acceleration
//! makes of it (assumed at its worst, [`WORST_ACCELERATION`]), counting
//! what was sent since. Every report of a new position starts the count
//! again, so the pointer slows only near an edge, for as long as reports
//! take; but a report was read a moment before it came, so what was sent in
//! the [`IN_FLIGHT`] before it still counts (P18's VM run: forgetting it
//! gave an edge's room twice, and a fast move got out). A pointer the game
//! has locked doesn't follow our moves: once moves went unanswered for
//! [`LOCKED_AFTER`], they pass whole, until the position changes again.
//!
//! Pure: the caller gives the reports and the time.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::Bounds;

/// The most a desktop's pointer acceleration multiplies a move by
/// (libinput's adaptive profile at its highest speed). A move is cut
/// assuming this factor, so the pointer can't overshoot an edge whatever
/// the desktop does with it.
pub const WORST_ACCELERATION: i64 = 4;
/// Moves sent for this long without the reported position changing mean
/// the game has locked the pointer: they pass whole from then on. Well past
/// how long a report takes (a few to 16 ms on KDE and GNOME).
pub const LOCKED_AFTER: Duration = Duration::from_millis(200);
/// …and at least this much motion went unanswered: a few units the desktop
/// slowed below a pixel move nothing.
pub const LOCKED_MOTION: i64 = 16;
/// Moves sent this long before a report came may not be in it: the desktop
/// read the position, then told us (both helpers as the pointer moves:
/// GNOME's extension on the turn of its loop, KWin's script within 8 ms
/// reading it as it sends; then the bus), and its own input may have waited
/// a moment. They still count against the room the report gives. Longer
/// makes the pointer stop and go near an edge (50 ms did, P18's VM run).
pub const IN_FLIGHT: Duration = Duration::from_millis(20);
/// The same position told again (the helpers repeat it every 250 ms) holds
/// the moves sent this long before it was heard: they moved the pointer
/// less than a pixel, and stop counting. A push read before our moves landed
/// and heard late (our process busy) mustn't give the room back, nor prove a
/// lock, so this is well past any lag (P18's second review).
pub const STILL_AFTER: Duration = Duration::from_millis(100);
/// The most moves remembered: past it, the last entry takes the new ones.
const MAX_SENT: usize = 128;

/// Where the desktop last said the pointer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerReport {
    pub x: i32,
    pub y: i32,
    /// The game window's client area, where the pointer is kept.
    pub area: Bounds,
    /// Counts the reported position's changes: a new count is a new
    /// position to start from.
    pub changes: u64,
    /// When this position was first told (`None`: as the move is cut).
    pub came: Option<Instant>,
    /// When the helper last told it, the same or not (its heartbeat at least
    /// every second; `None`: only when it came): what was sent well before
    /// is in it.
    pub told: Option<Instant>,
}

/// Cuts a guest's relative moves to the game window's area.
#[derive(Debug, Default)]
pub struct Confiner {
    /// The changes count of the position the motion below is counted from.
    from: Option<u64>,
    /// When that position came.
    came: Option<Instant>,
    /// Motion that may not be in that position yet, each way (right, left,
    /// down, up), and when it was sent: what was sent before it came only
    /// for [`IN_FLIGHT`] (had it moved the pointer, a newer position would
    /// have come by then; counted for longer, moves that came with the
    /// position stopped the pointer short of the edge for good, P18's VM
    /// run), and nothing sent [`IN_FLIGHT`] before the helper last told it
    /// (the same position told again: those moves are in it, and moved it
    /// less than a pixel; kept, jitter would use the room up both ways and
    /// hold the pointer still for good).
    sent: VecDeque<(Instant, [i64; 4])>,
    /// Motion sent since that position came, net along each axis: whether
    /// moves went unanswered (back and forth adds up to nothing, P18's
    /// review).
    fresh: [i64; 2],
    /// When motion was first sent since that position.
    since: Option<Instant>,
    /// The pointer doesn't follow our moves: they pass whole.
    locked: bool,
}

impl Confiner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether moves pass whole since the pointer stopped following them.
    pub fn locked(&self) -> bool {
        self.locked
    }

    /// What of the move `(dx, dy)` to send, as of `report`. Without one
    /// (the desktop says nothing of the pointer, while the game counts as in
    /// front), nothing goes: never the whole move (P18's review).
    pub fn cut(
        &mut self,
        report: Option<PointerReport>,
        dx: i32,
        dy: i32,
        now: Instant,
    ) -> (i32, i32) {
        let Some(report) = report else {
            // What was sent is kept: it may still be on its way when the
            // next report comes (P18's second review).
            return (0, 0);
        };
        if self.from != Some(report.changes) {
            // A new position: followed again, counted from what may still
            // be on its way.
            *self = Self {
                from: Some(report.changes),
                came: Some(report.came.unwrap_or(now)),
                sent: std::mem::take(&mut self.sent),
                ..Self::default()
            };
        }
        let came = self.came.unwrap_or(now);
        let told = report.told.unwrap_or(came).max(came);
        self.sent.retain(|(at, _)| {
            (*at >= came || now.saturating_duration_since(*at) < IN_FLIGHT)
                && told.saturating_duration_since(*at) < STILL_AFTER
        });
        // Locked: moves went unanswered for a while, net, and the helper
        // said so since, well after them, with nothing newer heard for a
        // moment (a quiet desktop proves nothing, nor does one push that
        // may have been read before the moves landed).
        if !self.locked
            && self.since.is_some_and(|since| {
                now.saturating_duration_since(since) >= LOCKED_AFTER
                    && told.saturating_duration_since(since) >= STILL_AFTER
                    && now.saturating_duration_since(told) >= IN_FLIGHT
            })
            && self.fresh[0].abs() + self.fresh[1].abs() >= LOCKED_MOTION
        {
            self.locked = true;
        }
        if self.locked {
            return (dx, dy);
        }
        let area = report.area;
        let (left, top) = (i64::from(area.x), i64::from(area.y));
        let right = left + i64::from(area.width.max(1)) - 1;
        let bottom = top + i64::from(area.height.max(1)) - 1;
        let (x, y) = (i64::from(report.x), i64::from(report.y));
        let mut sent = [0; 4];
        for (_, moved) in &self.sent {
            for (total, way) in sent.iter_mut().zip(moved) {
                *total += way;
            }
        }
        let before = sent;
        let [r, l, d, u] = &mut sent;
        let cx = along(dx, x, left, right, r, l);
        let cy = along(dy, y, top, bottom, d, u);
        if (cx, cy) != (0, 0) {
            if self.since.is_none() {
                self.since = Some(now);
            }
            let moved: [i64; 4] = std::array::from_fn(|i| sent[i] - before[i]);
            self.fresh[0] += moved[0] - moved[1];
            self.fresh[1] += moved[2] - moved[3];
            let full = self.sent.len() >= MAX_SENT;
            match self.sent.back_mut() {
                // However fast moves come, a bounded history: the last entry
                // takes them, its time now (kept longer, never shorter).
                Some((at, last)) if full => {
                    *at = now;
                    for (total, way) in last.iter_mut().zip(moved) {
                        *total += way;
                    }
                }
                _ => self.sent.push_back((now, moved)),
            }
        }
        (cx, cy)
    }
}

/// One axis: `delta` cut to the room between `at` and the area's edge it
/// goes towards, at the worst acceleration, less what already went that way.
fn along(delta: i32, at: i64, low: i64, high: i64, up: &mut i64, down: &mut i64) -> i32 {
    let delta = i64::from(delta);
    let (room, sent) = if delta > 0 {
        (high - at, &mut *up)
    } else {
        (at - low, &mut *down)
    };
    // Outside the area, only back towards it.
    let budget = (room.max(0) / WORST_ACCELERATION - *sent).max(0);
    let cut = delta.abs().min(budget);
    *sent += cut;
    let cut = if delta > 0 { cut } else { -cut };
    cut as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Bounds = Bounds {
        x: 100,
        y: 50,
        width: 800,
        height: 600,
    };

    fn at(x: i32, y: i32, changes: u64) -> Option<PointerReport> {
        Some(PointerReport {
            x,
            y,
            area: AREA,
            changes,
            came: None,
            told: None,
        })
    }

    /// The same, the helper having last told it at `told` (its heartbeat).
    fn heard(x: i32, y: i32, changes: u64, told: Instant) -> Option<PointerReport> {
        at(x, y, changes).map(|r| PointerReport {
            told: Some(told),
            ..r
        })
    }

    fn ms(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    /// P18's review: with the game in front, nothing said of the pointer
    /// lets nothing through, never the whole move.
    #[test]
    fn without_a_report_nothing_goes() {
        let mut c = Confiner::new();
        assert_eq!(c.cut(None, 5000, -3000, Instant::now()), (0, 0));
    }

    #[test]
    fn a_move_is_cut_to_the_room_at_the_worst_acceleration() {
        let mut c = Confiner::new();
        let t = Instant::now();
        // 899 is the last column: 99 to go, a quarter of it.
        assert_eq!(c.cut(at(800, 300, 1), 1000, 0, t), (24, 0));
        // Nothing more that way until the desktop says where it went.
        assert_eq!(c.cut(at(800, 300, 1), 10, 0, t), (0, 0));
        // The other way has its own room: 700 to the left edge.
        assert_eq!(c.cut(at(800, 300, 1), -1000, 0, t), (-175, 0));
        // Up and down the same: 250 up, 349 down.
        assert_eq!(c.cut(at(800, 300, 1), 0, -1000, t), (0, -62));
        assert_eq!(c.cut(at(800, 300, 1), 0, 1000, t), (0, 87));
    }

    #[test]
    fn small_moves_inside_pass_whole() {
        let mut c = Confiner::new();
        let t = Instant::now();
        assert_eq!(c.cut(at(500, 300, 1), 7, -4, t), (7, -4));
        assert_eq!(c.cut(at(500, 300, 1), 7, -4, t), (7, -4));
    }

    #[test]
    fn each_new_position_gives_room_again_and_never_past_the_edge() {
        let mut c = Confiner::new();
        let t = Instant::now();
        // Moving right as fast as allowed, the desktop applying the worst
        // acceleration each time: the pointer closes in, never past 899.
        let (mut x, mut changes) = (500, 1);
        for i in 0..40 {
            let (dx, _) = c.cut(at(x, 300, changes), 1000, 0, ms(t, i * 8));
            x += dx * WORST_ACCELERATION as i32;
            changes += 1;
            assert!(x <= 899, "{x}");
        }
        assert!(x >= 896, "it gets there: {x}");
    }

    /// The desktop reads the position, then tells us: moves sent meanwhile
    /// aren't in the report. Fast moves at the worst acceleration, each
    /// report 16 ms behind (a GNOME poll and the bus): never past the edge,
    /// and still at it in the end. Forgetting what was on its way at each
    /// report gave the edge's room again, and the pointer got out.
    #[test]
    fn a_report_behind_the_moves_never_lets_the_pointer_out() {
        let mut c = Confiner::new();
        let t = Instant::now();
        // Where the pointer is after each 4 ms step, as the desktop has it.
        let mut path = vec![500i32];
        for step in 0..250u64 {
            let now = ms(t, step * 4);
            // The report: the position 4 steps (16 ms) ago, its count new
            // whenever the position it gives changed (it only grows).
            let seen = path[path.len().saturating_sub(5)];
            let report = at(seen, 300, seen as u64);
            // A steady fast move (a guest flinging the mouse): 30 units
            // every 4 ms, 120 pixels at the worst acceleration.
            let (dx, _) = c.cut(report, 30, 0, now);
            let x = path.last().unwrap() + dx * WORST_ACCELERATION as i32;
            assert!(x <= 899, "out at step {step}: {x}");
            path.push(x);
        }
        assert!(*path.last().unwrap() >= 890, "it gets there: {path:?}");
    }

    #[test]
    fn however_fast_moves_come_the_history_stays_bounded() {
        let mut c = Confiner::new();
        let t = Instant::now();
        // Back and forth, a unit at a time: each move is sent.
        for i in 0..10_000 {
            let dx = if i % 2 == 0 { 1 } else { -1 };
            c.cut(at(500, 300, 1), dx, 0, t + Duration::from_micros(i));
        }
        assert!(c.sent.len() <= MAX_SENT);
        // Nothing of it was forgotten: a quarter of 399 to the right edge,
        // of 400 to the left.
        assert_eq!(c.sent.iter().map(|(_, m)| m[0]).sum::<i64>(), 99);
        assert_eq!(c.sent.iter().map(|(_, m)| m[1]).sum::<i64>(), 100);
    }

    /// P18's VM run: a fast move towards an edge, and the position that
    /// comes leaves less room than what was sent just before it. Nothing
    /// more goes, so the pointer doesn't move and no newer position comes:
    /// the moves sent before it must stop counting once they can't be on
    /// their way any more, or the pointer stays short of the edge for as
    /// long as the guest keeps moving.
    #[test]
    fn a_fast_move_near_an_edge_never_stalls() {
        let mut c = Confiner::new();
        let t = Instant::now();
        // From 500, 30 units every 4 ms for 16 ms: 99 allowed of the 399 to
        // the edge.
        let mut sent = 0;
        for step in 0..5 {
            sent += c.cut(at(500, 300, 1), 30, 0, ms(t, step * 4)).0;
        }
        assert_eq!(sent, 99);
        // The desktop moved it 2.5 times as far: 747, its room 152, a
        // quarter of it 38, less than the 69 sent in the 20 ms before (which
        // may still be on their way).
        assert_eq!(c.cut(at(747, 300, 2), 30, 0, ms(t, 20)), (0, 0));
        // The guest keeps moving, the position stays: within the time a
        // move can be on its way, the room comes back.
        let resumed = (6..20).find(|step| c.cut(at(747, 300, 2), 30, 0, ms(t, step * 4)).0 > 0);
        let step = resumed.expect("the pointer moves again");
        assert!(ms(t, step * 4) <= ms(t, 20) + IN_FLIGHT + Duration::from_millis(4));
    }

    #[test]
    fn a_pointer_outside_only_comes_back() {
        let mut c = Confiner::new();
        let t = Instant::now();
        // Above the area (the title bar): nothing further up, back down at
        // once.
        assert_eq!(c.cut(at(500, 20, 1), 0, -50, t), (0, 0));
        assert_eq!(c.cut(at(500, 20, 1), 0, 50, t), (0, 50));
        // Past the right edge.
        assert_eq!(c.cut(at(950, 300, 2), 30, 0, t), (0, 0));
        assert_eq!(c.cut(at(950, 300, 2), -30, 0, t), (-30, 0));
    }

    #[test]
    fn a_locked_pointer_lets_moves_through_until_it_moves_again() {
        let mut c = Confiner::new();
        let t = Instant::now();
        // A game locked the pointer in the middle: our moves don't move it.
        // At first they are cut as if it would.
        assert_eq!(c.cut(at(500, 350, 1), 300, 0, t), (99, 0));
        assert_eq!(c.cut(at(500, 350, 1), 300, 0, ms(t, 100)), (0, 0));
        assert!(!c.locked());
        // Unanswered for long enough, but the helper hasn't spoken since:
        // no lock.
        assert_eq!(c.cut(at(500, 350, 1), 300, 0, ms(t, 200)), (0, 0));
        assert!(!c.locked());
        // It told the same position, well after the moves, and nothing newer
        // came for a moment: locked, and moves pass whole.
        assert_eq!(
            c.cut(heard(500, 350, 1, ms(t, 150)), 300, 0, ms(t, 210)),
            (300, 0)
        );
        assert!(c.locked());
        assert_eq!(c.cut(at(500, 350, 1), -5000, 0, ms(t, 900)), (-5000, 0));
        // The heartbeat's report of the same position changes nothing; a
        // new position (the game let the pointer go) is followed again.
        assert_eq!(c.cut(at(500, 350, 2), 1000, 0, ms(t, 1000)), (99, 0));
        assert!(!c.locked());
    }

    /// P18's review: a desktop that says nothing for a while (the Shell busy)
    /// proves no lock: its input may have moved the pointer meanwhile.
    #[test]
    fn a_quiet_desktop_is_never_a_lock() {
        let mut c = Confiner::new();
        let t = Instant::now();
        let mut sent = 0;
        for i in 0..250 {
            sent += c.cut(at(500, 350, 1), 30, 0, ms(t, i * 4)).0;
        }
        assert!(!c.locked());
        assert!(sent <= 99, "never past the room: {sent}");
    }

    /// P18's review: back and forth that moves the pointer less than a pixel
    /// (slow aiming) adds up to nothing, so it's no lock; and the heartbeat
    /// telling the same position gives the room back, or the jitter would
    /// use it up both ways and hold the pointer still.
    #[test]
    fn jitter_is_never_a_lock_nor_holds_the_pointer() {
        let mut c = Confiner::new();
        let t = Instant::now();
        let mut last = (0, 0);
        for i in 0..1000u64 {
            let dx = if i % 2 == 0 { 1 } else { -1 };
            // The helpers' beat: the same position told every 250 ms.
            let told = ms(t, i * 4 / 250 * 250);
            last = c.cut(heard(500, 350, 1, told), dx, 0, ms(t, i * 4));
        }
        assert!(!c.locked());
        assert_ne!(last, (0, 0), "still moving after 4 s of jitter");
    }

    /// P18's second review: a push of the same position heard just after
    /// moves (read, maybe, before they landed) holds them still.
    #[test]
    fn the_same_position_told_right_after_moves_keeps_them() {
        let mut c = Confiner::new();
        let t = Instant::now();
        assert_eq!(c.cut(at(800, 300, 1), 1000, 0, t), (24, 0));
        assert_eq!(
            c.cut(heard(800, 300, 1, ms(t, 50)), 1000, 0, ms(t, 50)),
            (0, 0)
        );
        // Well after them, it holds them: they moved the pointer less than a
        // pixel, and the room comes back.
        assert_eq!(
            c.cut(heard(800, 300, 1, ms(t, 150)), 1000, 0, ms(t, 150)),
            (24, 0)
        );
    }

    /// P18's second review: a push read before the moves landed, heard late,
    /// and the new position right behind it: never a lock, so nothing ever
    /// passes whole.
    #[test]
    fn a_late_push_of_the_old_position_is_no_lock() {
        let mut c = Confiner::new();
        let t = Instant::now();
        assert_eq!(c.cut(at(500, 350, 1), 300, 0, t), (99, 0));
        // At 250 ms the old position is heard (read long before); a move cut
        // at once must not pass whole.
        assert_eq!(
            c.cut(heard(500, 350, 1, ms(t, 250)), 300, 0, ms(t, 250)).0,
            99
        );
        assert!(!c.locked());
        // The new position comes right behind it: followed, room from there.
        assert_eq!(c.cut(at(896, 350, 2), 300, 0, ms(t, 255)), (0, 0));
        assert!(!c.locked());
    }

    #[test]
    fn moves_cut_to_nothing_never_count_as_locked() {
        let mut c = Confiner::new();
        let t = Instant::now();
        // Pressed against the edge for seconds: nothing is sent, so the
        // pointer not moving says nothing.
        for i in 0..100 {
            assert_eq!(c.cut(at(899, 300, 1), 20, 0, ms(t, i * 50)), (0, 0));
        }
        assert!(!c.locked());
    }

    #[test]
    fn a_little_motion_left_unanswered_isnt_a_lock() {
        let mut c = Confiner::new();
        let t = Instant::now();
        // Slow moves the desktop turned into less than a pixel.
        assert_eq!(c.cut(at(500, 300, 1), 1, 0, t), (1, 0));
        assert_eq!(c.cut(at(500, 300, 1), 1, 1, ms(t, 500)), (1, 1));
        assert!(!c.locked());
    }

    #[test]
    fn a_report_gone_a_moment_forgets_nothing() {
        let mut c = Confiner::new();
        let t = Instant::now();
        assert_eq!(c.cut(at(800, 300, 1), 1000, 0, t), (24, 0));
        assert_eq!(c.cut(None, 1000, 0, t), (0, 0));
        // What was sent may still be on its way: no room again yet.
        assert_eq!(c.cut(at(800, 300, 1), 1000, 0, t), (0, 0));
    }

    #[test]
    fn an_empty_area_lets_nothing_out() {
        let mut c = Confiner::new();
        let report = Some(PointerReport {
            x: 10,
            y: 10,
            area: Bounds {
                x: 10,
                y: 10,
                width: 0,
                height: 0,
            },
            changes: 1,
            came: None,
            told: None,
        });
        assert_eq!(c.cut(report, 50, -50, Instant::now()), (0, 0));
    }
}
