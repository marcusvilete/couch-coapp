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
//! take. A pointer the game has locked doesn't follow our moves: once moves
//! went unanswered for [`LOCKED_AFTER`], they pass whole, until the
//! position changes again.
//!
//! Pure: the caller gives the reports and the time.

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
}

/// Cuts a guest's relative moves to the game window's area.
#[derive(Debug, Default)]
pub struct Confiner {
    /// The changes count of the position the motion below is counted from.
    from: Option<u64>,
    /// Motion sent since that position, each way: right, left, down, up.
    sent: [i64; 4],
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
    /// (the desktop says nothing of the pointer), the move passes whole.
    pub fn cut(
        &mut self,
        report: Option<PointerReport>,
        dx: i32,
        dy: i32,
        now: Instant,
    ) -> (i32, i32) {
        let Some(report) = report else {
            *self = Self::default();
            return (dx, dy);
        };
        if self.from != Some(report.changes) {
            // A new position: counted afresh, and followed again.
            *self = Self {
                from: Some(report.changes),
                ..Self::default()
            };
        }
        if !self.locked
            && self
                .since
                .is_some_and(|since| now.saturating_duration_since(since) >= LOCKED_AFTER)
            && self.sent.iter().sum::<i64>() >= LOCKED_MOTION
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
        let [r, l, d, u] = &mut self.sent;
        let cx = along(dx, x, left, right, r, l);
        let cy = along(dy, y, top, bottom, d, u);
        if (cx, cy) != (0, 0) && self.since.is_none() {
            self.since = Some(now);
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
        })
    }

    fn ms(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn without_a_report_moves_pass_whole() {
        let mut c = Confiner::new();
        assert_eq!(c.cut(None, 5000, -3000, Instant::now()), (5000, -3000));
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
        // Unanswered for long enough: locked, and moves pass whole.
        assert_eq!(c.cut(at(500, 350, 1), 300, 0, ms(t, 200)), (300, 0));
        assert!(c.locked());
        assert_eq!(c.cut(at(500, 350, 1), -5000, 0, ms(t, 900)), (-5000, 0));
        // The heartbeat's report of the same position changes nothing; a
        // new position (the game let the pointer go) is followed again.
        assert_eq!(c.cut(at(500, 350, 2), 1000, 0, ms(t, 1000)), (99, 0));
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
    fn a_report_gone_starts_over() {
        let mut c = Confiner::new();
        let t = Instant::now();
        assert_eq!(c.cut(at(800, 300, 1), 1000, 0, t), (24, 0));
        assert_eq!(c.cut(None, 1000, 0, t), (1000, 0));
        assert_eq!(c.cut(at(800, 300, 1), 1000, 0, t), (24, 0));
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
        });
        assert_eq!(c.cut(report, 50, -50, Instant::now()), (0, 0));
    }
}
