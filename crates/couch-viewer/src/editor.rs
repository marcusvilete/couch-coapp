//! The stream window's drawing editor (M27a, ADR 0013): tools, the
//! selection, undo and redo, copy and paste, and the operations they make,
//! batched for the host. Pure: positions come in the picture's own
//! coordinates, and the board (`crate::board`) is what it reads and changes.
//!
//! Every operation it makes is applied to the window's own board as it is
//! made, then sent: what this window shows of its own is what it sent, and
//! the host's resync after a refusal sets it right.
//!
//! **Only the author's own:** it selects, erases, moves and undoes only
//! elements of this member; the host refuses anything else anyway.

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use couch_input::shortcuts::{DrawAction, DrawTool};
use couch_proto::{
    board::{
        BoardOp, DEFAULT_STAY, Element, ElementKind, FADE_STAYS, MAX_LASER_POINTS,
        MAX_MEMBER_ELEMENTS, MAX_STROKE_POINTS, PALETTE, Point, WIDTHS, batches, line_width,
    },
    ids::MemberId,
};

use crate::board::BoardView;

/// Batches go out at most this often: the host takes 40 a second.
pub const BATCH_EVERY: Duration = Duration::from_millis(30);
/// Laser points at most this often (the host takes 60 marks a second).
const LASER_EVERY: Duration = Duration::from_millis(20);
/// Pings at most this often.
const PING_EVERY: Duration = Duration::from_millis(500);
/// A stroke's points at least this far apart, in picture heights.
const MIN_SPACING: f32 = 0.0015;
/// How near the pointer must be to touch a line, in picture heights,
/// beyond half its width.
const TOUCH: f32 = 0.012;
/// A nudge, in picture coordinates; Shift: ten.
const NUDGE: i32 = 131;
/// Where a paste lands from what was copied, in picture coordinates.
const PASTE_STEP: i32 = 1_300;

/// How the toolbar's one fade button steps (the author's choice, M27b):
/// through "stays", then each fade time offered, shortest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeStep {
    /// A click: on round, from the longest back to "stays".
    Next,
    /// The wheel up: longer, stopping at the longest.
    Longer,
    /// The wheel down: shorter, stopping at "stays".
    Shorter,
}

/// What a key did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyed {
    Done,
    /// Esc with nothing left to leave: drawing mode ends.
    LeaveDrawing,
}

/// One change to one of our elements: what it was and what it became.
#[derive(Debug, Clone, PartialEq)]
struct Change {
    id: u32,
    before: Option<Element>,
    after: Option<Element>,
}

#[derive(Debug, Clone)]
enum Gesture {
    /// A freehand line, and the elements it made (a long one goes on in a
    /// new element past the limit).
    Stroke {
        id: u32,
        last: Point,
        made: Vec<u32>,
    },
    Shape {
        id: u32,
        kind: ElementKind,
        start: Point,
    },
    Laser {
        last_at: Option<Instant>,
        fresh: bool,
    },
    Erase {
        removed: Vec<Element>,
    },
    Move {
        from: Point,
        originals: Vec<Element>,
    },
    Marquee {
        from: Point,
        to: Point,
        kept: BTreeSet<u32>,
    },
}

#[derive(Debug)]
pub struct Editor {
    me: MemberId,
    pub tool: DrawTool,
    /// The tool before Esc went to Select: drawing mode starts with it.
    last_tool: DrawTool,
    pub colour: u8,
    pub width: u8,
    pub fades: bool,
    /// How long a fading element stays before it fades, in seconds: one of
    /// `FADE_STAYS` (wire revision 16).
    pub stay: u8,
    /// The picture's width over its height: distances are measured in
    /// picture heights.
    aspect: f32,
    next_id: u32,
    gesture: Option<Gesture>,
    selection: BTreeSet<u32>,
    undo: Vec<Vec<Change>>,
    redo: Vec<Vec<Change>>,
    clipboard: Vec<Element>,
    pastes: i32,
    outbox: Vec<BoardOp>,
    /// The board's generation as last heard, and the one the queued
    /// operations were made in: sent with it, so ops made before a clear
    /// are refused by the host (and our own set right), never taken after.
    generation: u32,
    outbox_generation: u32,
    sent_at: Option<Instant>,
    pinged_at: Option<Instant>,
    note: Option<&'static str>,
}

impl Editor {
    pub fn new(me: MemberId) -> Self {
        Self {
            me,
            tool: DrawTool::Pen,
            last_tool: DrawTool::Pen,
            colour: 0,
            width: 1,
            fades: false,
            stay: DEFAULT_STAY,
            aspect: 16.0 / 9.0,
            next_id: 1,
            gesture: None,
            selection: BTreeSet::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            clipboard: Vec::new(),
            pastes: 0,
            outbox: Vec::new(),
            generation: 0,
            outbox_generation: 0,
            sent_at: None,
            pinged_at: None,
            note: None,
        }
    }

    pub fn me(&self) -> MemberId {
        self.me
    }

    /// The picture's shape, as shown.
    pub fn set_aspect(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.aspect = width as f32 / height as f32;
        }
    }

    pub fn selection(&self) -> &BTreeSet<u32> {
        &self.selection
    }

    /// The marquee being dragged, corner to corner.
    pub fn marquee(&self) -> Option<(Point, Point)> {
        match &self.gesture {
            Some(Gesture::Marquee { from, to, .. }) => Some((*from, *to)),
            _ => None,
        }
    }

    /// Something to tell the user, once.
    pub fn take_note(&mut self) -> Option<&'static str> {
        self.note.take()
    }

    /// Drawing mode begins: with the tool it last drew with.
    pub fn begin(&mut self) {
        if self.tool == DrawTool::Select && self.selection.is_empty() {
            self.tool = self.last_tool;
        }
    }

    /// Drawing mode ends: whatever was half done is finished or dropped.
    pub fn end(&mut self, board: &mut BoardView, now: Instant) {
        self.release(None, false, board, now);
        self.selection.clear();
    }

    pub fn set_tool(&mut self, tool: DrawTool, board: &mut BoardView, now: Instant) {
        self.release(None, false, board, now);
        if tool != DrawTool::Select {
            self.selection.clear();
            self.last_tool = tool;
        }
        self.tool = tool;
    }

    /// A colour, width or fading picked: what is selected takes it too.
    pub fn set_style(
        &mut self,
        colour: Option<u8>,
        width: Option<u8>,
        fades: Option<bool>,
        board: &mut BoardView,
        now: Instant,
    ) {
        if let Some(c) = colour.filter(|c| usize::from(*c) < PALETTE.len()) {
            self.colour = c;
        }
        if let Some(w) = width.filter(|w| usize::from(*w) < WIDTHS) {
            self.width = w;
        }
        if let Some(f) = fades {
            self.fades = f;
        }
        let changed: Vec<Element> = self
            .selected(board)
            .into_iter()
            .map(|mut e| {
                if let Some(c) = colour {
                    e.colour = c;
                }
                if let Some(w) = width {
                    e.width = w;
                }
                if let Some(f) = fades {
                    e.fades = f;
                }
                e
            })
            .collect();
        self.replace_all(changed, board, now);
    }

    /// The fade button stepped: "stays", or one of `FADE_STAYS`, as
    /// [`Self::set_style`] and [`Self::set_stay`] would make it (what is
    /// selected takes it too).
    pub fn step_fade(&mut self, step: FadeStep, board: &mut BoardView, now: Instant) {
        // 0 stays; 1.. the stays offered, in order.
        let at = if self.fades {
            FADE_STAYS
                .iter()
                .position(|s| *s == self.stay)
                .map_or(0, |i| i + 1)
        } else {
            0
        };
        let last = FADE_STAYS.len();
        let to = match step {
            FadeStep::Next if at == last => 0,
            FadeStep::Next => at + 1,
            FadeStep::Longer => (at + 1).min(last),
            FadeStep::Shorter => at.saturating_sub(1),
        };
        if to == at {
            return;
        }
        match to {
            0 => self.set_style(None, None, Some(false), board, now),
            n => self.set_stay(FADE_STAYS[n - 1], board, now),
        }
    }

    /// How long fading elements stay before they fade (one of
    /// `FADE_STAYS`): fading turns on, and what is selected takes both.
    pub fn set_stay(&mut self, stay: u8, board: &mut BoardView, now: Instant) {
        if !FADE_STAYS.contains(&stay) {
            return;
        }
        self.stay = stay;
        self.fades = true;
        let changed: Vec<Element> = self
            .selected(board)
            .into_iter()
            .map(|e| Element {
                fades: true,
                stay,
                ..e
            })
            .collect();
        self.replace_all(changed, board, now);
    }

    // ---- The pointer ----

    /// A press of the main button at `at` (Shift held or not).
    pub fn press(&mut self, at: Point, shift: bool, board: &mut BoardView, now: Instant) {
        self.release(None, false, board, now);
        self.gesture = match self.tool {
            DrawTool::Pen | DrawTool::Highlighter => {
                let kind = if self.tool == DrawTool::Pen {
                    ElementKind::Pen
                } else {
                    ElementKind::Highlighter
                };
                let Some(id) = self.new_id(board) else {
                    return;
                };
                let element = self.element(id, kind, vec![at], false);
                self.emit(BoardOp::Put(element), board, now);
                Some(Gesture::Stroke {
                    id,
                    last: at,
                    made: vec![id],
                })
            }
            DrawTool::Arrow | DrawTool::Line | DrawTool::Rectangle | DrawTool::Ellipse => {
                let kind = match self.tool {
                    DrawTool::Arrow => ElementKind::Arrow,
                    DrawTool::Line => ElementKind::Line,
                    DrawTool::Rectangle => ElementKind::Rectangle,
                    _ => ElementKind::Ellipse,
                };
                let Some(id) = self.new_id(board) else {
                    return;
                };
                let element = self.element(id, kind, vec![at, at], true);
                self.emit(BoardOp::Put(element), board, now);
                Some(Gesture::Shape {
                    id,
                    kind,
                    start: at,
                })
            }
            DrawTool::Laser => {
                let mut g = Gesture::Laser {
                    last_at: None,
                    fresh: true,
                };
                self.laser(&mut g, at, board, now);
                Some(g)
            }
            DrawTool::Eraser => {
                let mut removed = Vec::new();
                self.erase_at(at, &mut removed, board, now);
                Some(Gesture::Erase { removed })
            }
            DrawTool::Select => match self.hit(at, board) {
                Some(id) => {
                    if shift {
                        if !self.selection.remove(&id) {
                            self.selection.insert(id);
                        }
                    } else if !self.selection.contains(&id) {
                        self.selection = BTreeSet::from([id]);
                    }
                    Some(Gesture::Move {
                        from: at,
                        originals: self.selected(board),
                    })
                }
                None => {
                    let kept = if shift {
                        self.selection.clone()
                    } else {
                        BTreeSet::new()
                    };
                    self.selection = kept.clone();
                    Some(Gesture::Marquee {
                        from: at,
                        to: at,
                        kept,
                    })
                }
            },
        };
    }

    /// The pointer moved to `at` with the main button down.
    pub fn drag(&mut self, at: Point, shift: bool, board: &mut BoardView, now: Instant) {
        let Some(mut gesture) = self.gesture.take() else {
            return;
        };
        match &mut gesture {
            Gesture::Stroke { id, last, made } => {
                if self.distance(*last, at) >= MIN_SPACING {
                    let full = board
                        .get(self.me, *id)
                        .is_some_and(|s| s.element.points.len() >= MAX_STROKE_POINTS - 1);
                    if full {
                        // Finished here, and on in a new element.
                        self.emit(
                            BoardOp::Append {
                                id: *id,
                                points: vec![at],
                                done: true,
                            },
                            board,
                            now,
                        );
                        let kind = board
                            .get(self.me, *id)
                            .map_or(ElementKind::Pen, |s| s.element.kind);
                        if let Some(next) = self.new_id(board) {
                            let element = self.element(next, kind, vec![at], false);
                            self.emit(BoardOp::Put(element), board, now);
                            *id = next;
                            made.push(next);
                        } else {
                            self.gesture = None;
                            self.finish(made.clone(), board);
                            return;
                        }
                    } else {
                        self.emit(
                            BoardOp::Append {
                                id: *id,
                                points: vec![at],
                                done: false,
                            },
                            board,
                            now,
                        );
                    }
                    *last = at;
                }
            }
            Gesture::Shape { id, kind, start } => {
                let end = self.constrained(*kind, *start, at, shift);
                let element = self.element(*id, *kind, vec![*start, end], true);
                self.emit(BoardOp::Put(element), board, now);
            }
            Gesture::Laser { .. } => self.laser(&mut gesture, at, board, now),
            Gesture::Erase { removed } => {
                let mut r = std::mem::take(removed);
                self.erase_at(at, &mut r, board, now);
                *removed = r;
            }
            Gesture::Move { from, originals } => {
                // Shown moving here only; sent once let go.
                let moved = shifted(originals, self.offset(originals, *from, at));
                board.apply(
                    self.me,
                    &moved.into_iter().map(BoardOp::Put).collect::<Vec<_>>(),
                    now,
                );
            }
            Gesture::Marquee { from, to, kept } => {
                *to = at;
                let (a, b) = (*from, *to);
                let inside: BTreeSet<u32> = board
                    .of(self.me)
                    .filter(|s| within(bounds(&s.element), a, b))
                    .map(|s| s.element.id)
                    .collect();
                self.selection = kept.union(&inside).copied().collect();
            }
        }
        self.gesture = Some(gesture);
    }

    /// The main button let go (at `at`, if known), or the gesture ended
    /// another way (`None`: drawing mode ending, a tool picked).
    pub fn release(&mut self, at: Option<Point>, shift: bool, board: &mut BoardView, now: Instant) {
        let Some(gesture) = self.gesture.take() else {
            return;
        };
        match gesture {
            Gesture::Stroke { id, made, .. } => {
                self.emit(
                    BoardOp::Append {
                        id,
                        points: Vec::new(),
                        done: true,
                    },
                    board,
                    now,
                );
                self.finish(made, board);
            }
            Gesture::Shape { id, kind, start } => {
                if let Some(at) = at {
                    let end = self.constrained(kind, start, at, shift);
                    let element = self.element(id, kind, vec![start, end], true);
                    self.emit(BoardOp::Put(element), board, now);
                }
                self.finish(vec![id], board);
            }
            Gesture::Laser { .. } | Gesture::Marquee { .. } => {}
            Gesture::Erase { removed } => {
                if !removed.is_empty() {
                    self.record(
                        removed
                            .into_iter()
                            .map(|e| Change {
                                id: e.id,
                                before: Some(e),
                                after: None,
                            })
                            .collect(),
                    );
                }
            }
            Gesture::Move { from, originals } => {
                let offset = match at {
                    Some(at) => self.offset(&originals, from, at),
                    None => (0, 0),
                };
                if offset == (0, 0) {
                    // Back where they were, as shown.
                    let back: Vec<BoardOp> = originals.into_iter().map(BoardOp::Put).collect();
                    board.apply(self.me, &back, now);
                } else {
                    let moved = shifted(&originals, offset);
                    self.replace_from(originals, moved, board, now);
                }
            }
        }
    }

    /// A middle click: a ping where it was.
    pub fn ping(&mut self, at: Point, board: &mut BoardView, now: Instant) {
        if self
            .pinged_at
            .is_some_and(|t| now.saturating_duration_since(t) < PING_EVERY)
        {
            return;
        }
        self.pinged_at = Some(now);
        self.emit(BoardOp::Ping(at), board, now);
    }

    // ---- Keys ----

    pub fn key(
        &mut self,
        action: DrawAction,
        shift: bool,
        board: &mut BoardView,
        now: Instant,
    ) -> Keyed {
        match action {
            DrawAction::Undo => self.undo(board, now),
            DrawAction::Redo => self.redo(board, now),
            DrawAction::Copy => self.clipboard = self.selected(board),
            DrawAction::Paste => {
                let copied = self.clipboard.clone();
                self.pastes += 1;
                self.paste(copied, self.pastes, board, now);
            }
            DrawAction::Duplicate => {
                let selected = self.selected(board);
                self.paste(selected, 1, board, now);
            }
            DrawAction::SelectAll => {
                self.release(None, false, board, now);
                self.tool = DrawTool::Select;
                self.selection = board.of(self.me).map(|s| s.element.id).collect();
            }
            DrawAction::Delete => {
                let selected = self.selected(board);
                let changes: Vec<Change> = selected
                    .into_iter()
                    .map(|e| Change {
                        id: e.id,
                        before: Some(e),
                        after: None,
                    })
                    .collect();
                self.selection.clear();
                self.perform(changes, board, now);
            }
            DrawAction::Nudge(dx, dy) => {
                let step = if shift { NUDGE * 10 } else { NUDGE };
                let selected = self.selected(board);
                let want = (i32::from(dx) * step, i32::from(dy) * step);
                let offset = clamp_offset(&selected, want);
                if offset != (0, 0) {
                    let moved = shifted(&selected, offset);
                    self.replace_from(selected, moved, board, now);
                }
            }
            DrawAction::Tool(tool) => self.set_tool(tool, board, now),
            DrawAction::Leave => {
                if self.gesture.is_some() {
                    self.release(None, false, board, now);
                } else if !self.selection.is_empty() {
                    self.selection.clear();
                } else if self.tool != DrawTool::Select {
                    self.tool = DrawTool::Select;
                } else {
                    return Keyed::LeaveDrawing;
                }
            }
        }
        Keyed::Done
    }

    // ---- What the host said ----

    /// The host cleared our elements, or everyone's: what it cleared is
    /// gone from the history, and anything half drawn stops.
    pub fn cleared(&mut self, board: &BoardView) {
        self.undo.clear();
        self.redo.clear();
        // Made before the clear: the host drops it, and the clear took it
        // from our board too.
        self.outbox.clear();
        self.sync(board);
    }

    /// Finishes our strokes left open that we aren't drawing: a window
    /// started again after a crash mid-stroke gets that stroke back open,
    /// and nobody else would ever finish it (a fading one would never fade).
    /// After a clear of ours (a snapshot among them).
    pub fn finish_strays(&mut self, board: &mut BoardView, now: Instant) {
        let drawing = match &self.gesture {
            Some(Gesture::Stroke { id, .. }) => Some(*id),
            _ => None,
        };
        let strays: Vec<u32> = board
            .of(self.me)
            .filter(|s| !s.element.done && Some(s.element.id) != drawing)
            .map(|s| s.element.id)
            .collect();
        for id in strays {
            self.emit(
                BoardOp::Append {
                    id,
                    points: Vec::new(),
                    done: true,
                },
                board,
                now,
            );
        }
    }

    /// After any news: our numbers stay new, and the selection and a
    /// gesture forget what is gone.
    pub fn sync(&mut self, board: &BoardView) {
        self.generation = board.generation();
        let most = board.of(self.me).map(|s| s.element.id).max().unwrap_or(0);
        self.next_id = self.next_id.max(most.saturating_add(1));
        self.selection
            .retain(|id| board.get(self.me, *id).is_some());
        let gone = match &self.gesture {
            Some(Gesture::Stroke { id, .. } | Gesture::Shape { id, .. }) => {
                board.get(self.me, *id).is_none()
            }
            _ => false,
        };
        if gone {
            self.gesture = None;
        }
    }

    // ---- Sending ----

    /// The next batch to send, if one is due (at most every
    /// [`BATCH_EVERY`]), with the generation it was made in.
    pub fn take_batch(&mut self, now: Instant) -> Option<(u32, Vec<BoardOp>)> {
        let early = self
            .sent_at
            .is_some_and(|t| now.saturating_duration_since(t) < BATCH_EVERY);
        if self.outbox.is_empty() || early {
            return None;
        }
        self.sent_at = Some(now);
        let mut parts = batches(std::mem::take(&mut self.outbox));
        let first = parts.remove(0);
        self.outbox = parts.concat();
        Some((self.outbox_generation, first))
    }

    /// Whether operations wait to go out.
    #[cfg(test)]
    pub fn pending(&self) -> bool {
        !self.outbox.is_empty()
    }

    // ---- Within ----

    fn element(&self, id: u32, kind: ElementKind, points: Vec<Point>, done: bool) -> Element {
        Element {
            id,
            kind,
            colour: self.colour,
            width: self.width,
            fades: self.fades,
            stay: self.stay,
            done,
            points,
        }
    }

    /// A number for a new element, unless we have all the elements we may.
    fn new_id(&mut self, board: &BoardView) -> Option<u32> {
        if board.of(self.me).count() >= MAX_MEMBER_ELEMENTS {
            self.note = Some("You've drawn all you may: erase some first");
            return None;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        Some(id)
    }

    /// Applies `op` to our board and queues it, merged with the one before
    /// when that is the same element's.
    fn emit(&mut self, op: BoardOp, board: &mut BoardView, now: Instant) {
        board.apply(self.me, std::slice::from_ref(&op), now);
        if self.outbox.is_empty() {
            self.outbox_generation = self.generation;
        }
        if let Some(last) = self.outbox.last_mut()
            && merge(last, &op)
        {
            return;
        }
        self.outbox.push(op);
    }

    fn laser(&mut self, gesture: &mut Gesture, at: Point, board: &mut BoardView, now: Instant) {
        let Gesture::Laser { last_at, fresh } = gesture else {
            return;
        };
        if last_at.is_some_and(|t| now.saturating_duration_since(t) < LASER_EVERY) {
            return;
        }
        *last_at = Some(now);
        let start = std::mem::replace(fresh, false);
        self.emit(
            BoardOp::Laser {
                points: vec![at],
                start,
                colour: self.colour,
            },
            board,
            now,
        );
    }

    fn erase_at(
        &mut self,
        at: Point,
        removed: &mut Vec<Element>,
        board: &mut BoardView,
        now: Instant,
    ) {
        let hit: Vec<Element> = board
            .of(self.me)
            .filter(|s| self.touches(&s.element, at, false))
            .map(|s| s.element.clone())
            .collect();
        if hit.is_empty() {
            return;
        }
        let ids: Vec<u32> = hit.iter().map(|e| e.id).collect();
        self.selection.retain(|id| !ids.contains(id));
        self.emit(BoardOp::Remove(ids), board, now);
        removed.extend(hit);
    }

    /// The topmost of ours under `at`.
    fn hit(&self, at: Point, board: &BoardView) -> Option<u32> {
        board
            .of(self.me)
            .filter(|s| self.touches(&s.element, at, true))
            .last()
            .map(|s| s.element.id)
    }

    /// Our selected elements, in drawing order.
    fn selected(&self, board: &BoardView) -> Vec<Element> {
        board
            .of(self.me)
            .filter(|s| self.selection.contains(&s.element.id))
            .map(|s| s.element.clone())
            .collect()
    }

    /// A finished gesture's new elements, as one step.
    fn finish(&mut self, made: Vec<u32>, board: &BoardView) {
        let changes: Vec<Change> = made
            .into_iter()
            .filter_map(|id| board.get(self.me, id))
            .map(|s| Change {
                id: s.element.id,
                before: None,
                after: Some(s.element.clone()),
            })
            .collect();
        if !changes.is_empty() {
            self.record(changes);
        }
    }

    fn record(&mut self, step: Vec<Change>) {
        self.undo.push(step);
        self.redo.clear();
        // A long session's history, within reason.
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
    }

    /// Makes `changes` and records them as one step.
    fn perform(&mut self, changes: Vec<Change>, board: &mut BoardView, now: Instant) {
        if changes.is_empty() {
            return;
        }
        for c in &changes {
            self.change_to(c.id, c.after.clone(), board, now);
        }
        self.record(changes);
    }

    /// Our element `id` becomes `to` (or goes).
    fn change_to(&mut self, id: u32, to: Option<Element>, board: &mut BoardView, now: Instant) {
        match to {
            Some(e) => self.emit(BoardOp::Put(e), board, now),
            None => self.emit(BoardOp::Remove(vec![id]), board, now),
        }
    }

    fn replace_all(&mut self, after: Vec<Element>, board: &mut BoardView, now: Instant) {
        let before = self.selected(board);
        self.replace_from(before, after, board, now);
    }

    fn replace_from(
        &mut self,
        before: Vec<Element>,
        after: Vec<Element>,
        board: &mut BoardView,
        now: Instant,
    ) {
        let changes: Vec<Change> = before
            .into_iter()
            .zip(after)
            .filter(|(b, a)| b != a)
            .map(|(b, a)| Change {
                id: a.id,
                before: Some(b),
                after: Some(a),
            })
            .collect();
        self.perform(changes, board, now);
    }

    fn undo(&mut self, board: &mut BoardView, now: Instant) {
        self.release(None, false, board, now);
        let Some(step) = self.undo.pop() else {
            return;
        };
        for c in step.iter().rev() {
            self.change_to(c.id, c.before.clone(), board, now);
        }
        self.selection.clear();
        self.redo.push(step);
    }

    fn redo(&mut self, board: &mut BoardView, now: Instant) {
        self.release(None, false, board, now);
        let Some(step) = self.redo.pop() else {
            return;
        };
        if step.iter().any(|c| c.before.is_none())
            && board.of(self.me).count() >= MAX_MEMBER_ELEMENTS
        {
            self.note = Some("You've drawn all you may: erase some first");
            self.redo.push(step);
            return;
        }
        for c in &step {
            self.change_to(c.id, c.after.clone(), board, now);
        }
        self.selection.clear();
        self.undo.push(step);
    }

    /// `elements` again as new ones of ours, `times` paste steps away, and
    /// selected.
    fn paste(&mut self, elements: Vec<Element>, times: i32, board: &mut BoardView, now: Instant) {
        if elements.is_empty() {
            return;
        }
        let want = (PASTE_STEP * times, PASTE_STEP * times);
        let offset = clamp_offset(&elements, want);
        let mut changes = Vec::new();
        for e in shifted(&elements, offset) {
            let Some(id) = self.new_id(board) else {
                break;
            };
            let e = Element {
                id,
                done: true,
                ..e
            };
            changes.push(Change {
                id,
                before: None,
                after: Some(e),
            });
        }
        self.selection = changes.iter().map(|c| c.id).collect();
        self.tool = DrawTool::Select;
        self.perform(changes, board, now);
    }

    /// Where a move by the pointer from `from` to `at` takes `elements`,
    /// kept inside the picture.
    fn offset(&self, elements: &[Element], from: Point, at: Point) -> (i32, i32) {
        let want = (
            i32::from(at.x) - i32::from(from.x),
            i32::from(at.y) - i32::from(from.y),
        );
        clamp_offset(elements, want)
    }

    /// A shape's end with Shift: lines at 15° steps, squares and circles.
    fn constrained(&self, kind: ElementKind, start: Point, at: Point, shift: bool) -> Point {
        if !shift {
            return at;
        }
        let (sx, sy) = self.uniform(start);
        let (ax, ay) = self.uniform(at);
        let (dx, dy) = (ax - sx, ay - sy);
        let (ex, ey) = match kind {
            ElementKind::Line | ElementKind::Arrow => {
                let len = (dx * dx + dy * dy).sqrt();
                let step = std::f32::consts::PI / 12.0;
                let angle = (dy.atan2(dx) / step).round() * step;
                (sx + len * angle.cos(), sy + len * angle.sin())
            }
            _ => {
                let side = dx.abs().max(dy.abs());
                (sx + side * dx.signum(), sy + side * dy.signum())
            }
        };
        self.picture(ex, ey)
    }

    /// A point in picture heights: x across `aspect`, y across 1.
    fn uniform(&self, p: Point) -> (f32, f32) {
        (
            f32::from(p.x) / 65_535.0 * self.aspect,
            f32::from(p.y) / 65_535.0,
        )
    }

    fn picture(&self, x: f32, y: f32) -> Point {
        let x = (x / self.aspect * 65_535.0).round().clamp(0.0, 65_535.0) as u16;
        let y = (y * 65_535.0).round().clamp(0.0, 65_535.0) as u16;
        Point::new(x, y)
    }

    fn distance(&self, a: Point, b: Point) -> f32 {
        let (ax, ay) = self.uniform(a);
        let (bx, by) = self.uniform(b);
        ((ax - bx).powi(2) + (ay - by).powi(2)).sqrt()
    }

    /// Whether `at` touches `e`: near its line, or, picking, inside a box or
    /// an ellipse.
    pub fn touches(&self, e: &Element, at: Point, picking: bool) -> bool {
        let reach = TOUCH + line_width(e.kind, e.width) / 2.0;
        let p = self.uniform(at);
        let pts: Vec<(f32, f32)> = e.points.iter().map(|q| self.uniform(*q)).collect();
        match e.kind {
            ElementKind::Pen
            | ElementKind::Highlighter
            | ElementKind::Line
            | ElementKind::Arrow => {
                if pts.len() == 1 {
                    return dist(p, pts[0]) <= reach;
                }
                pts.windows(2)
                    .any(|w| segment_distance(p, w[0], w[1]) <= reach)
            }
            ElementKind::Rectangle => {
                let (a, b) = (pts[0], pts[1]);
                let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
                let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
                let inside = p.0 >= x0 && p.0 <= x1 && p.1 >= y0 && p.1 <= y1;
                let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)];
                let edge = corners
                    .windows(2)
                    .any(|w| segment_distance(p, w[0], w[1]) <= reach);
                edge || (picking && inside)
            }
            ElementKind::Ellipse => {
                let (a, b) = (pts[0], pts[1]);
                let c = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
                let r = (((a.0 - b.0) / 2.0).abs(), ((a.1 - b.1) / 2.0).abs());
                if r.0 < f32::EPSILON || r.1 < f32::EPSILON {
                    return segment_distance(p, a, b) <= reach;
                }
                let n = (((p.0 - c.0) / r.0).powi(2) + ((p.1 - c.1) / r.1).powi(2)).sqrt();
                let near = (n - 1.0).abs() * r.0.min(r.1) <= reach;
                near || (picking && n <= 1.0)
            }
        }
    }
}

/// Folds `op` into `last`, the queued operation before it, when both are
/// about the same element (or trail): a stroke's points, a shape dragged
/// out, a laser's trail. `false` if it must go on its own.
fn merge(last: &mut BoardOp, op: &BoardOp) -> bool {
    match (last, op) {
        // Points for a stroke still queued whole, or already growing.
        (
            BoardOp::Put(e),
            BoardOp::Append {
                id, points, done, ..
            },
        ) if e.id == *id && !e.done && e.points.len() + points.len() <= MAX_STROKE_POINTS => {
            e.points.extend_from_slice(points);
            e.done = *done;
            true
        }
        (
            BoardOp::Append {
                id: a,
                points,
                done: d,
            },
            BoardOp::Append {
                id: b,
                points: more,
                done,
            },
        ) if a == b && !*d && points.len() + more.len() <= MAX_STROKE_POINTS => {
            points.extend_from_slice(more);
            *d = *done;
            true
        }
        (BoardOp::Put(e), BoardOp::Put(new)) if e.id == new.id => {
            *e = new.clone();
            true
        }
        (
            BoardOp::Laser { points, colour, .. },
            BoardOp::Laser {
                points: more,
                start: false,
                colour: same,
            },
        ) if colour == same && points.len() + more.len() <= MAX_LASER_POINTS => {
            points.extend_from_slice(more);
            true
        }
        _ => false,
    }
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    if len2 < f32::EPSILON {
        return dist(p, a);
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0);
    dist(p, (a.0 + t * dx, a.1 + t * dy))
}

/// An element's box, corner to corner, in picture coordinates.
pub fn bounds(e: &Element) -> (Point, Point) {
    let xs = e.points.iter().map(|p| p.x);
    let ys = e.points.iter().map(|p| p.y);
    (
        Point::new(xs.clone().min().unwrap_or(0), ys.clone().min().unwrap_or(0)),
        Point::new(xs.max().unwrap_or(0), ys.max().unwrap_or(0)),
    )
}

/// Whether the box `(lo, hi)` lies within the one `a` to `b` spans.
fn within((lo, hi): (Point, Point), a: Point, b: Point) -> bool {
    lo.x >= a.x.min(b.x) && hi.x <= a.x.max(b.x) && lo.y >= a.y.min(b.y) && hi.y <= a.y.max(b.y)
}

/// `want`, cut short so every one of `elements` stays inside the picture.
fn clamp_offset(elements: &[Element], want: (i32, i32)) -> (i32, i32) {
    let mut lo = (i32::MAX, i32::MAX);
    let mut hi = (i32::MIN, i32::MIN);
    for e in elements {
        let (a, b) = bounds(e);
        lo = (lo.0.min(i32::from(a.x)), lo.1.min(i32::from(a.y)));
        hi = (hi.0.max(i32::from(b.x)), hi.1.max(i32::from(b.y)));
    }
    if elements.is_empty() {
        return (0, 0);
    }
    (
        want.0.clamp(-lo.0, 65_535 - hi.0),
        want.1.clamp(-lo.1, 65_535 - hi.1),
    )
}

fn shifted(elements: &[Element], (dx, dy): (i32, i32)) -> Vec<Element> {
    elements
        .iter()
        .map(|e| Element {
            points: e
                .points
                .iter()
                .map(|p| {
                    Point::new(
                        (i32::from(p.x) + dx).clamp(0, 65_535) as u16,
                        (i32::from(p.y) + dy).clamp(0, 65_535) as u16,
                    )
                })
                .collect(),
            ..e.clone()
        })
        .collect()
}

#[cfg(test)]
mod tests;
