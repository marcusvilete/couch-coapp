//! The board as this stream window shows it (M27a, ADR 0013): every
//! element in drawing order, when each finished arriving here (fading is
//! timed from that: no clocks to agree on), and the passing marks, laser
//! trails and pings. Pure: no drawing, no SDL; `draw_ui` paints it.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use couch_proto::{
    board::{BoardOp, Element, FADE_OUT_MS, LASER_MS, PING_MS, Point, Refusal},
    ids::MemberId,
    ipc::BoardNews,
};

pub const FADE_OUT: Duration = Duration::from_millis(FADE_OUT_MS);
pub const LASER: Duration = Duration::from_millis(LASER_MS);
pub const PING: Duration = Duration::from_millis(PING_MS);

/// One element as shown.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub author: MemberId,
    pub element: Element,
    /// When it finished arriving here, for fading.
    pub done_at: Option<Instant>,
}

impl Shown {
    /// How long it stays once finished, before fading: its author's pick.
    pub fn stay(&self) -> Duration {
        Duration::from_millis(self.element.stay_ms())
    }

    /// How opaque it is now: 1 until a fading one has stayed its while,
    /// then down to 0 over the fade.
    pub fn alpha(&self, now: Instant) -> f32 {
        let Some(done) = self.done_at.filter(|_| self.element.fades) else {
            return 1.0;
        };
        let age = now.saturating_duration_since(done);
        let stay = self.stay();
        if age <= stay {
            1.0
        } else {
            let out = (age - stay).as_secs_f32() / FADE_OUT.as_secs_f32();
            (1.0 - out).max(0.0)
        }
    }

    fn faded(&self, now: Instant) -> bool {
        self.alpha(now) <= 0.0
    }
}

/// A laser trail's point, and when it arrived.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaserPoint {
    pub at: Point,
    pub since: Instant,
    /// The first of a new trail: no line to it from the one before.
    pub start: bool,
    /// Its colour, an index into the palette.
    pub colour: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ping {
    pub author: MemberId,
    pub at: Point,
    pub since: Instant,
}

#[derive(Debug, Default)]
pub struct BoardView {
    generation: u32,
    elements: Vec<Shown>,
    lasers: BTreeMap<MemberId, Vec<LaserPoint>>,
    pings: Vec<Ping>,
}

impl BoardView {
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// In drawing order.
    pub fn elements(&self) -> &[Shown] {
        &self.elements
    }

    pub fn lasers(&self) -> &BTreeMap<MemberId, Vec<LaserPoint>> {
        &self.lasers
    }

    pub fn pings(&self) -> &[Ping] {
        &self.pings
    }

    pub fn get(&self, author: MemberId, id: u32) -> Option<&Shown> {
        self.elements
            .iter()
            .find(|s| s.author == author && s.element.id == id)
    }

    /// `author`'s elements, in drawing order.
    pub fn of(&self, author: MemberId) -> impl Iterator<Item = &Shown> {
        self.elements.iter().filter(move |s| s.author == author)
    }

    /// What the host said, to `me`. Returns its refusal, if that is what it
    /// was. Only a clear of our own elements or everyone's starts a new
    /// generation for us; another member's has its own.
    pub fn news(&mut self, news: BoardNews, me: Option<MemberId>, now: Instant) -> Option<Refusal> {
        match news {
            BoardNews::Snapshot(s) => {
                if s.first {
                    self.elements.clear();
                }
                self.generation = s.generation;
                for a in s.elements {
                    self.apply(a.author, &[BoardOp::Put(a.element)], now);
                }
            }
            BoardNews::Update(u) => self.apply(u.author, &u.ops, now),
            BoardNews::Cleared(c) => {
                if c.author.is_none() || c.author == me {
                    self.generation = c.generation;
                }
                match c.author {
                    Some(author) => {
                        self.elements.retain(|s| s.author != author);
                        self.lasers.remove(&author);
                        self.pings.retain(|p| p.author != author);
                    }
                    None => {
                        self.elements.clear();
                        self.lasers.clear();
                        self.pings.clear();
                    }
                }
            }
            BoardNews::Refused(why) => return Some(why),
        }
        None
    }

    /// `author`'s operations: the host's update, or our own as we send them.
    pub fn apply(&mut self, author: MemberId, ops: &[BoardOp], now: Instant) {
        for op in ops {
            match op {
                BoardOp::Put(e) => {
                    let done_at = e.done.then_some(now);
                    match self
                        .elements
                        .iter_mut()
                        .find(|s| s.author == author && s.element.id == e.id)
                    {
                        // Replaced, it keeps its place; finished before, it
                        // keeps its time.
                        Some(s) => {
                            s.done_at = if e.done { s.done_at.or(done_at) } else { None };
                            s.element = e.clone();
                        }
                        None => self.elements.push(Shown {
                            author,
                            element: e.clone(),
                            done_at,
                        }),
                    }
                }
                BoardOp::Append { id, points, done } => {
                    if let Some(s) = self
                        .elements
                        .iter_mut()
                        .find(|s| s.author == author && s.element.id == *id)
                        .filter(|s| !s.element.done)
                    {
                        s.element.points.extend_from_slice(points);
                        s.element.done = *done;
                        if *done {
                            s.done_at = Some(now);
                        }
                    }
                }
                BoardOp::Remove(ids) => self
                    .elements
                    .retain(|s| s.author != author || !ids.contains(&s.element.id)),
                BoardOp::Laser {
                    points,
                    start,
                    colour,
                } => {
                    let trail = self.lasers.entry(author).or_default();
                    for (i, at) in points.iter().enumerate() {
                        trail.push(LaserPoint {
                            at: *at,
                            since: now,
                            start: *start && i == 0,
                            colour: *colour,
                        });
                    }
                }
                BoardOp::Ping(at) => {
                    // One ring per member at a time.
                    self.pings.retain(|p| p.author != author);
                    self.pings.push(Ping {
                        author,
                        at: *at,
                        since: now,
                    });
                }
            }
        }
    }

    /// Lets go of what has faded, the laser points past their time, and the
    /// pings that are over.
    pub fn prune(&mut self, now: Instant) {
        self.elements.retain(|s| !s.faded(now));
        for trail in self.lasers.values_mut() {
            trail.retain(|p| now.saturating_duration_since(p.since) < LASER);
        }
        self.lasers.retain(|_, t| !t.is_empty());
        self.pings
            .retain(|p| now.saturating_duration_since(p.since) < PING);
    }

    /// Whether something changes by itself (fading, a laser, a ping): the
    /// window then paints again soon, though no video comes.
    pub fn moving(&self, now: Instant) -> bool {
        !self.lasers.is_empty()
            || !self.pings.is_empty()
            || self.elements.iter().any(|s| {
                s.element.fades
                    && s.done_at
                        .is_some_and(|d| now.saturating_duration_since(d) > s.stay())
            })
    }

    /// When the next fading element starts to fade, if one is still staying:
    /// nothing moves until then, and a still picture brings no frames, so
    /// the window must wake itself then (the author's run of M27b).
    pub fn next_fade(&self, now: Instant) -> Option<Instant> {
        self.elements
            .iter()
            .filter(|s| s.element.fades)
            .filter_map(|s| s.done_at.map(|d| d + s.stay()))
            .filter(|at| *at > now)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use couch_proto::board::{BoardCleared, BoardSnapshot, BoardUpdate, ElementKind};
    use couch_proto::ids::SessionId;

    use super::*;

    const ANA: MemberId = MemberId([1; 32]);
    const BIA: MemberId = MemberId([2; 32]);

    fn stroke(id: u32, done: bool, fades: bool) -> Element {
        Element {
            id,
            kind: ElementKind::Pen,
            colour: 0,
            width: 0,
            fades,
            stay: 4,
            done,
            points: vec![Point::new(1, 1)],
        }
    }

    #[test]
    fn a_fading_element_stays_then_fades_from_when_it_finished_here() {
        let t0 = Instant::now();
        let mut board = BoardView::default();
        board.apply(ANA, &[BoardOp::Put(stroke(1, false, true))], t0);
        assert!(
            !board.moving(t0 + Duration::from_secs(60)),
            "not while drawn"
        );
        let done = t0 + Duration::from_secs(2);
        board.apply(
            ANA,
            &[BoardOp::Append {
                id: 1,
                points: vec![Point::new(2, 2)],
                done: true,
            }],
            done,
        );
        let s = &board.elements()[0];
        const FADE_STAY: Duration = Duration::from_secs(4);
        assert_eq!(s.stay(), FADE_STAY, "the default");
        // Nothing moves while it stays: the window wakes when it fades.
        assert_eq!(
            board.next_fade(done + Duration::from_secs(1)),
            Some(done + FADE_STAY)
        );
        assert_eq!(board.next_fade(done + FADE_STAY), None);
        assert_eq!(s.alpha(done + FADE_STAY), 1.0);
        let half = s.alpha(done + FADE_STAY + FADE_OUT / 2);
        assert!((half - 0.5).abs() < 0.01, "{half}");
        assert!(!board.moving(done + FADE_STAY), "staying: nothing to paint");
        assert!(board.moving(done + FADE_STAY + FADE_OUT / 2), "fading");
        board.prune(done + FADE_STAY + FADE_OUT);
        assert!(board.elements().is_empty());
    }

    #[test]
    fn each_element_fades_after_its_own_stay() {
        let now = Instant::now();
        let mut board = BoardView::default();
        let long = Element {
            stay: 15,
            ..stroke(1, true, true)
        };
        board.apply(
            ANA,
            &[BoardOp::Put(stroke(2, true, true)), BoardOp::Put(long)],
            now,
        );
        assert_eq!(board.next_fade(now), Some(now + Duration::from_secs(4)));
        board.prune(now + Duration::from_secs(5));
        assert_eq!(board.elements().len(), 1, "the 4 s one gone");
        assert_eq!(board.elements()[0].element.id, 1);
        assert_eq!(
            board.next_fade(now + Duration::from_secs(5)),
            Some(now + Duration::from_secs(15))
        );
        board.prune(now + Duration::from_secs(16));
        assert!(board.elements().is_empty());
    }

    #[test]
    fn the_host_replaces_clears_and_refuses() {
        let now = Instant::now();
        let mut board = BoardView::default();
        board.apply(ANA, &[BoardOp::Put(stroke(1, true, false))], now);
        board.apply(BIA, &[BoardOp::Put(stroke(1, true, false))], now);
        assert_eq!(board.elements().len(), 2, "each member's own numbers");
        let cleared = |author| {
            BoardNews::Cleared(BoardCleared {
                session: SessionId(1),
                generation: 4,
                author,
            })
        };
        // Bia's clear isn't ours: our generation stays.
        board.news(cleared(Some(BIA)), Some(ANA), now);
        assert_eq!(board.generation(), 0);
        board.apply(BIA, &[BoardOp::Put(stroke(1, true, false))], now);
        board.news(cleared(Some(ANA)), Some(ANA), now);
        assert_eq!(board.elements().len(), 1);
        assert_eq!(board.generation(), 4);
        let snapshot = BoardNews::Snapshot(BoardSnapshot {
            session: SessionId(1),
            generation: 5,
            first: true,
            elements: vec![couch_proto::board::Authored {
                author: ANA,
                element: stroke(7, true, false),
            }],
        });
        board.news(snapshot, Some(ANA), now);
        assert_eq!(board.elements().len(), 1, "the first part replaces");
        assert!(board.get(ANA, 7).is_some());
        let update = BoardNews::Update(BoardUpdate {
            session: SessionId(1),
            author: BIA,
            ops: vec![BoardOp::Ping(Point::new(3, 3))],
        });
        board.news(update, Some(ANA), now);
        assert_eq!(board.pings().len(), 1);
        assert!(board.moving(now));
        board.prune(now + PING);
        assert!(!board.moving(now + PING));
        assert_eq!(
            board.news(BoardNews::Refused(Refusal::TooFast), Some(ANA), now),
            Some(Refusal::TooFast)
        );
        board.news(cleared(None), Some(ANA), now);
        assert!(board.elements().is_empty());
    }

    #[test]
    fn a_laser_trail_fades_within_its_time() {
        let now = Instant::now();
        let mut board = BoardView::default();
        board.apply(
            ANA,
            &[BoardOp::Laser {
                points: vec![Point::new(1, 1), Point::new(2, 2)],
                start: true,
                colour: 5,
            }],
            now,
        );
        let trail = &board.lasers()[&ANA];
        assert!(
            trail.iter().all(|p| p.colour == 5),
            "in its author's colour"
        );
        assert!(trail[0].start && !trail[1].start);
        board.prune(now + LASER);
        assert!(board.lasers().is_empty());
    }
}
