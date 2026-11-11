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
//!
//! **Text boxes** (M27c): typed into one at a time ([`Typing`]), shown to
//! the others as they grow (unfinished, so not fading), finished as one
//! step of the history. A box's second corner is where this window's font
//! ends it ([`Measure`]); other windows lay its text out with their own.

use std::{
    collections::BTreeSet,
    fmt,
    time::{Duration, Instant},
};

use couch_input::shortcuts::{DrawAction, DrawTool};
use couch_proto::{
    board::{
        BoardOp, DEFAULT_STAY, Element, ElementKind, FADE_STAYS, LINE_HEIGHT, MAX_LASER_POINTS,
        MAX_MEMBER_ELEMENTS, MAX_STROKE_POINTS, MAX_TEXT_CHARS, MAX_TEXT_LINES, PALETTE, PictureId,
        Point, WIDTHS, batches, clean_text, line_width, text_size, valid_text,
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
/// Where pasted text lands without a pointer over the picture.
const TEXT_AT: Point = Point::new(6_554, 6_554);
pub const TOO_MANY: &str = "You've drawn all you may: erase some first";
pub const TOO_LONG: &str = "A text box holds at most 500 characters and 50 lines";
/// A picture's resize handles: how near a corner a press takes it, in
/// picture heights (M27e).
pub const HANDLE: f32 = 0.018;
/// A picture pasted fits this share of the picture's height and width.
const PICTURE_FITS: f32 = 0.4;
/// A picture resized is at least this tall and wide, in picture heights.
const PICTURE_LEAST: f32 = 0.03;

/// How wide a line of text is, in ems of its font: a text box is that times
/// its size wide. The window measures with its own font; until it says, and
/// in the shared cases, [`Measure::estimate`] (half an em a character).
pub struct Measure(Box<dyn Fn(&str) -> f32>);

impl Measure {
    pub fn new(measure: impl Fn(&str) -> f32 + 'static) -> Self {
        Self(Box::new(measure))
    }

    pub fn estimate() -> Self {
        Self::new(|line| line.chars().count() as f32 * 0.5)
    }

    fn of(&self, line: &str) -> f32 {
        (self.0)(line).max(0.0)
    }
}

impl fmt::Debug for Measure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Measure")
    }
}

/// A key while typing in a text box: the window's keys go here, never to
/// the drawing keys, until the box is finished. With Shift, a move extends
/// the selection; with Ctrl (`word`), as text fields do: Left and Right by
/// word, Backspace and Delete a word, Home and End the box's start and end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKey {
    Backspace,
    Delete,
    Left,
    Right,
    /// The line's start and end.
    Home,
    End,
    /// A line up or down, nearest the caret's place across.
    Up,
    Down,
    /// A line break.
    Enter,
    /// Esc or Ctrl+Enter: the box is finished, and selected.
    Finish,
    /// Ctrl+A: all of the box's text.
    SelectAll,
}

/// The text box being typed in.
#[derive(Debug, Clone)]
struct Typing {
    /// Its number, kept while it has no text (and so isn't on the board).
    id: u32,
    /// Its colour, size and fading.
    look: Element,
    /// Its top-left corner.
    anchor: Point,
    text: String,
    /// Characters before the caret.
    caret: usize,
    /// The selection's other end, in characters: the text between it and
    /// the caret is selected. `None`, or the caret's own place: nothing.
    mark: Option<usize>,
    /// The box as it was before this edit; `None` for a new one.
    before: Option<Element>,
    /// On the board: it has text (spaces alone aren't).
    shown: bool,
}

impl Typing {
    /// The selected characters, start and end, if any.
    fn range(&self) -> Option<(usize, usize)> {
        let mark = self.mark.filter(|m| *m != self.caret)?;
        Some((mark.min(self.caret), mark.max(self.caret)))
    }

    /// The caret moved to `to`: with `extend` (Shift, a drag) the selection
    /// grows from where it was; otherwise there is none.
    fn move_to(&mut self, to: usize, extend: bool) {
        if extend {
            self.mark = self.mark.or(Some(self.caret));
        } else {
            self.mark = None;
        }
        self.caret = to;
        if self.mark == Some(self.caret) {
            self.mark = None;
        }
    }
}

/// The text box being typed in, for painting it: its corners, the caret's
/// top and bottom, the selection's boxes, one per line (picture
/// coordinates), and its size (picture heights).
#[derive(Debug, Clone, PartialEq)]
pub struct TextCursor {
    pub corners: (Point, Point),
    pub caret: (Point, Point),
    pub selection: Vec<(Point, Point)>,
    pub size: f32,
    pub colour: u8,
}

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
    /// Tab: the window hides its toolbar, or shows it again.
    ToggleToolbar,
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
    /// Dragging inside the text box being typed in: selecting.
    TextSelect,
    /// Dragging a handle (M27e): a picture's, a box's or an ellipse's corner,
    /// or a line's end; `fixed`, across from it, stays.
    Resize {
        original: Element,
        end: usize,
        fixed: Point,
    },
}

/// What waits to go out, in order: operations, and pictures (M27e), which
/// go on a stream of their own (`take_picture`), never in a batch.
#[derive(Debug, Clone, PartialEq)]
enum Out {
    Op(BoardOp),
    /// A picture put on the board: new, or back (an undo, a redo, a copy).
    Picture(Element),
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
    /// What our last copy put on the system clipboard (the copied boxes'
    /// text): while the clipboard still holds it, a paste is ours.
    copied_text: Option<String>,
    pastes: i32,
    typing: Option<Typing>,
    measure: Measure,
    outbox: Vec<Out>,
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
            copied_text: None,
            pastes: 0,
            typing: None,
            measure: Measure::estimate(),
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

    /// How the window's font measures text, from now on.
    pub fn set_measure(&mut self, measure: Measure) {
        self.measure = measure;
    }

    /// A text box is being typed in: the window's keys and text are its.
    pub fn typing(&self) -> bool {
        self.typing.is_some()
    }

    /// The text box being typed in, for painting its frame and caret.
    pub fn text_cursor(&self) -> Option<TextCursor> {
        let t = self.typing.as_ref()?;
        let size = text_size(t.look.width);
        let (line, col) = line_col(&t.text, t.caret);
        let prefix: String = t
            .text
            .split('\n')
            .nth(line)
            .unwrap_or("")
            .chars()
            .take(col)
            .collect();
        let (ax, ay) = self.uniform(t.anchor);
        let x = ax + self.measure.of(&prefix) * size;
        let y = ay + line as f32 * LINE_HEIGHT * size;
        let corners = if t.shown {
            bounds(&self.text_box(t, false))
        } else {
            (t.anchor, self.picture(ax, ay + LINE_HEIGHT * size))
        };
        // The selection, line by line; a line break selected shows as a
        // little more past its line's end.
        let mut selection = Vec::new();
        if let Some((lo, hi)) = t.range() {
            let mut start = 0;
            for (i, line) in t.text.split('\n').enumerate() {
                let cs: Vec<char> = line.chars().collect();
                let end = start + cs.len();
                let (s, e) = (lo.max(start), hi.min(end));
                let past = hi > end && lo <= end;
                if s < e || past {
                    let width = |n: usize| self.measure.of(&cs[..n].iter().collect::<String>());
                    let x0 = ax + width(s.min(end) - start) * size;
                    let x1 = ax + (width(e.max(s) - start) + if past { 0.3 } else { 0.0 }) * size;
                    let top = ay + i as f32 * LINE_HEIGHT * size;
                    selection.push((
                        self.picture(x0, top),
                        self.picture(x1, top + LINE_HEIGHT * size),
                    ));
                }
                start = end + 1;
            }
        }
        Some(TextCursor {
            corners,
            caret: (self.picture(x, y), self.picture(x, y + LINE_HEIGHT * size)),
            selection,
            size,
            colour: t.look.colour,
        })
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
        self.finish_typing(board, now);
        self.selection.clear();
    }

    pub fn set_tool(&mut self, tool: DrawTool, board: &mut BoardView, now: Instant) {
        self.release(None, false, board, now);
        self.finish_typing(board, now);
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
        // The box being typed in takes it too, as part of its edit.
        if let Some(t) = &mut self.typing {
            if let Some(c) = colour.filter(|c| usize::from(*c) < PALETTE.len()) {
                t.look.colour = c;
            }
            if let Some(w) = width.filter(|w| usize::from(*w) < WIDTHS) {
                t.look.width = w;
            }
            if let Some(f) = fades {
                t.look.fades = f;
            }
            self.show_typing(board, now);
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
                // A text box's size changes its box (the review's finding).
                self.remeasured(e)
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
        if let Some(t) = &mut self.typing {
            t.look.fades = true;
            t.look.stay = stay;
            self.show_typing(board, now);
        }
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
        if self.tool == DrawTool::Text {
            self.press_text(at, shift, board, now);
            return;
        }
        self.finish_typing(board, now);
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
            DrawTool::Text => None,
            DrawTool::Select if let Some((original, end, fixed)) = self.handle_at(at, board) => {
                Some(Gesture::Resize {
                    original,
                    end,
                    fixed,
                })
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
            Gesture::Resize {
                original,
                end,
                fixed,
            } => {
                // Shown resizing here only; sent once let go.
                let resized = self.resized(original, *end, *fixed, at, shift);
                board.apply(self.me, &[BoardOp::Put(resized)], now);
            }
            Gesture::TextSelect => {
                let caret = self.typing.as_ref().map(|t| self.caret_at(t, at));
                if let (Some(caret), Some(t)) = (caret, &mut self.typing) {
                    t.move_to(caret, true);
                }
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
            Gesture::Laser { .. } | Gesture::Marquee { .. } | Gesture::TextSelect => {}
            Gesture::Resize {
                original,
                end,
                fixed,
            } => match at {
                Some(at) => {
                    let resized = self.resized(&original, end, fixed, at, shift);
                    self.replace_from(vec![original], vec![resized], board, now);
                }
                // Back as it was, as shown.
                None => board.apply(self.me, &[BoardOp::Put(original)], now),
            },
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

    /// A press with the Text tool: inside the box being typed in, the caret
    /// goes there; elsewhere that box is finished (as Excalidraw: a click
    /// away is done). Not typing, it edits our text box under it, or starts
    /// a new one there (nothing goes out until it has text).
    fn press_text(&mut self, at: Point, shift: bool, board: &mut BoardView, now: Instant) {
        if self.typing.is_some() {
            if let Some(cursor) = self.text_cursor()
                && self.near_box(cursor.corners, at)
                && let Some(caret) = self.typing.as_ref().map(|t| self.caret_at(t, at))
                && let Some(t) = &mut self.typing
            {
                // The caret there (Shift: the selection to there), and a
                // drag from here selects.
                t.move_to(caret, shift);
                self.gesture = Some(Gesture::TextSelect);
            } else {
                self.finish_typing(board, now);
            }
            return;
        }
        match self.hit_text(at, board) {
            Some(id) => {
                self.edit(id, Some(at), board, now);
                self.gesture = Some(Gesture::TextSelect);
            }
            None => {
                let Some(id) = self.new_id(board) else {
                    return;
                };
                self.selection.clear();
                self.typing = Some(Typing {
                    id,
                    look: self.element(id, ElementKind::Text, Vec::new(), false),
                    anchor: at,
                    text: String::new(),
                    caret: 0,
                    mark: None,
                    before: None,
                    shown: false,
                });
            }
        }
    }

    /// A double click (after its two presses): inside the text box being
    /// typed in, the word there is selected; with Select, on our text box,
    /// typing in it, the caret where it was. Other tools drew with the
    /// clicks; the Text tool edits on one.
    pub fn double_click(&mut self, at: Point, board: &mut BoardView, now: Instant) {
        if let Some(cursor) = self.text_cursor()
            && self.near_box(cursor.corners, at)
            && let Some(caret) = self.typing.as_ref().map(|t| self.caret_at(t, at))
            && let Some(t) = &mut self.typing
        {
            self.gesture = None;
            let cs: Vec<char> = t.text.chars().collect();
            let (start, end) = word_at(&cs, caret);
            t.mark = Some(start);
            t.caret = end;
            if start == end {
                t.mark = None;
            }
            return;
        }
        if self.tool != DrawTool::Select || self.typing.is_some() {
            return;
        }
        self.release(None, false, board, now);
        if let Some(id) = self.hit_text(at, board) {
            self.edit(id, Some(at), board, now);
        }
    }

    // ---- Typing ----

    /// Text typed (or composed by an input method) into the box being typed
    /// in, at the caret: no control characters but line breaks, a tab as
    /// spaces, and only as much as the box holds (characters and lines).
    pub fn type_text(&mut self, text: &str, board: &mut BoardView, now: Instant) {
        let Some(t) = &mut self.typing else {
            return;
        };
        // What it types replaces the selection: line breaks there make room
        // for new ones.
        let (from, to) = t.range().unwrap_or((t.caret, t.caret));
        let mut old: Vec<char> = t.text.chars().collect();
        let mut breaks = old.iter().filter(|c| **c == '\n').count()
            - old[from..to].iter().filter(|c| **c == '\n').count();
        let mut cut = false;
        let mut chars: Vec<char> = Vec::new();
        for c in text.chars() {
            match c {
                '\t' => chars.extend([' '; 4]),
                '\n' | '\r' if breaks + 1 >= MAX_TEXT_LINES => cut = true,
                '\n' | '\r' => {
                    breaks += 1;
                    chars.push('\n');
                }
                c if !c.is_control() => chars.push(c),
                _ => {}
            }
        }
        let room = MAX_TEXT_CHARS.saturating_sub(old.len() - (to - from));
        if chars.len() > room {
            chars.truncate(room);
            cut = true;
        }
        if cut {
            self.note = Some(TOO_LONG);
        }
        if chars.is_empty() {
            return;
        }
        let n = chars.len();
        old.splice(from..to, chars);
        t.text = old.into_iter().collect();
        t.caret = from + n;
        t.mark = None;
        self.show_typing(board, now);
    }

    /// A key while typing: with `shift` a move extends the selection, with
    /// `word` (Ctrl) it goes by word or to the box's ends.
    pub fn text_key(
        &mut self,
        key: TextKey,
        shift: bool,
        word: bool,
        board: &mut BoardView,
        now: Instant,
    ) {
        if key == TextKey::Finish {
            if let Some(id) = self.finish_typing(board, now) {
                self.selection = BTreeSet::from([id]);
            }
            self.tool = DrawTool::Select;
            return;
        }
        if key == TextKey::Enter {
            self.type_text("\n", board, now);
            return;
        }
        let Some(t) = self.typing.as_ref() else {
            return;
        };
        let cs: Vec<char> = t.text.chars().collect();
        let len = cs.len();
        let (line, col) = line_col(&t.text, t.caret);
        let lines: Vec<usize> = t.text.split('\n').map(|l| l.chars().count()).collect();
        let start = t.caret - col;
        let range = t.range();
        match key {
            TextKey::SelectAll => {
                if let Some(t) = &mut self.typing {
                    t.mark = Some(0);
                    t.caret = len;
                    if len == 0 {
                        t.mark = None;
                    }
                }
            }
            TextKey::Backspace | TextKey::Delete => {
                // The selection, or a character (Ctrl: a word) on that side.
                let (from, to) = match range {
                    Some(r) => r,
                    None if key == TextKey::Backspace => {
                        let from = if word {
                            prev_word(&cs, t.caret)
                        } else {
                            t.caret.saturating_sub(1)
                        };
                        (from, t.caret)
                    }
                    None => {
                        let to = if word {
                            next_word(&cs, t.caret)
                        } else {
                            (t.caret + 1).min(len)
                        };
                        (t.caret, to)
                    }
                };
                if from == to {
                    return;
                }
                if let Some(t) = &mut self.typing {
                    t.text = cs[..from].iter().chain(&cs[to..]).collect();
                    t.caret = from;
                    t.mark = None;
                }
                self.show_typing(board, now);
            }
            _ => {
                let to = match key {
                    // Without Shift, a selection collapses to its side.
                    TextKey::Left if !shift && !word && range.is_some() => range.map_or(0, |r| r.0),
                    TextKey::Right if !shift && !word && range.is_some() => {
                        range.map_or(0, |r| r.1)
                    }
                    TextKey::Left if word => prev_word(&cs, t.caret),
                    TextKey::Right if word => next_word(&cs, t.caret),
                    TextKey::Left => t.caret.saturating_sub(1),
                    TextKey::Right => (t.caret + 1).min(len),
                    TextKey::Home if word => 0,
                    TextKey::End if word => len,
                    TextKey::Home => start,
                    TextKey::End => start + lines[line],
                    // Up on the first line goes to its start, Down on the
                    // last to its end, as text fields do.
                    TextKey::Up if line == 0 => 0,
                    TextKey::Down if line + 1 == lines.len() => len,
                    TextKey::Up | TextKey::Down => {
                        let size = text_size(t.look.width);
                        let line_text: Vec<&str> = t.text.split('\n').collect();
                        let x = self
                            .measure
                            .of(&cs[start..t.caret].iter().collect::<String>())
                            * size;
                        let target = if key == TextKey::Up {
                            line - 1
                        } else {
                            line + 1
                        };
                        let first: usize = lines[..target].iter().map(|n| n + 1).sum();
                        first + self.nearest(line_text[target], x, size)
                    }
                    _ => return,
                };
                if let Some(t) = &mut self.typing {
                    t.move_to(to, shift);
                }
            }
        }
    }

    /// The selected text of the box being typed in, for Ctrl+C there.
    pub fn copy_text(&self) -> Option<String> {
        let t = self.typing.as_ref()?;
        let (lo, hi) = t.range()?;
        Some(t.text.chars().skip(lo).take(hi - lo).collect())
    }

    /// Ctrl+X in the box being typed in: its selected text, taken out.
    pub fn cut_text(&mut self, board: &mut BoardView, now: Instant) -> Option<String> {
        let text = self.copy_text()?;
        self.text_key(TextKey::Delete, false, false, board, now);
        Some(text)
    }

    /// What our copy puts on the system clipboard: the selection is ours to
    /// paste, and its text boxes' text (or nothing) goes on the clipboard,
    /// so a paste can tell our copy from text copied elsewhere since.
    /// Nothing selected, nothing is copied (`None`): the clipboard keeps
    /// what it had.
    pub fn copy(&mut self, board: &BoardView) -> Option<String> {
        if self.selection.is_empty() {
            return None;
        }
        self.clipboard = self.selected(board);
        let text: Vec<&str> = self
            .clipboard
            .iter()
            .filter(|e| e.kind == ElementKind::Text)
            .map(|e| e.text.as_str())
            .collect();
        let text = text.join("\n\n");
        self.copied_text = Some(text.clone());
        Some(text)
    }

    /// Ctrl+V, with the system clipboard's text (if any) and the pointer
    /// (if over the picture). While typing, the text goes into the box.
    /// Otherwise text copied elsewhere since our last copy becomes a new
    /// text box there; else what we copied is pasted, as ever.
    pub fn paste(
        &mut self,
        system: Option<&str>,
        at: Option<Point>,
        board: &mut BoardView,
        now: Instant,
    ) {
        let text = system.map(clean_text).unwrap_or_default();
        if self.typing.is_some() {
            self.type_text(&text, board, now);
            return;
        }
        let ours = self.copied_text.as_deref().map(clean_text);
        if !text.trim().is_empty() && ours.as_deref() != Some(text.as_str()) {
            self.paste_text(text, at.unwrap_or(TEXT_AT), board, now);
        } else {
            let copied = self.clipboard.clone();
            self.pastes += 1;
            self.paste_elements(copied, self.pastes, board, now);
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
        // The window's own: nothing of the editor's changes.
        if action == DrawAction::ToggleToolbar {
            return Keyed::ToggleToolbar;
        }
        // A drawing key while typing (the toolbar's undo, say): the box is
        // finished first; Esc finishes it alone (`TextKey::Finish`).
        if self.typing.is_some() {
            if action == DrawAction::Leave {
                self.text_key(TextKey::Finish, false, false, board, now);
                return Keyed::Done;
            }
            self.finish_typing(board, now);
        }
        match action {
            DrawAction::Undo => self.undo(board, now),
            DrawAction::Redo => self.redo(board, now),
            // Without the system clipboard (the window passes it through
            // `copy` and `paste`): our own copy alone.
            DrawAction::Copy => {
                self.copy(board);
            }
            DrawAction::Paste => self.paste(None, None, board, now),
            DrawAction::Duplicate => {
                let selected = self.selected(board);
                self.paste_elements(selected, 1, board, now);
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
            // The window's (above).
            DrawAction::ToggleToolbar => {}
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
        // The box being typed in stays being typed in: a clear of ours is
        // also the host's resync after a refused batch (a full board, too
        // fast), and its text is the user's. It goes out again whole at the
        // next key or when finished; what it was before is history, gone.
        if let Some(t) = &mut self.typing {
            t.before = None;
        }
        self.sync(board);
    }

    /// Finishes our strokes left open that we aren't drawing: a window
    /// started again after a crash mid-stroke gets that stroke back open,
    /// and nobody else would ever finish it (a fading one would never fade).
    /// After a clear of ours (a snapshot among them).
    pub fn finish_strays(&mut self, board: &mut BoardView, now: Instant) {
        let drawing = match &self.gesture {
            Some(Gesture::Stroke { id, .. }) => Some(*id),
            _ => self.typing.as_ref().map(|t| t.id),
        };
        let strays: Vec<Element> = board
            .of(self.me)
            .filter(|s| !s.element.done && Some(s.element.id) != drawing)
            .map(|s| s.element.clone())
            .collect();
        for e in strays {
            // A text box left being typed in: the box again, finished (an
            // append is a stroke's alone).
            let op = if e.kind.is_stroke() {
                BoardOp::Append {
                    id: e.id,
                    points: Vec::new(),
                    done: true,
                }
            } else {
                BoardOp::Put(Element { done: true, ..e })
            };
            self.emit(op, board, now);
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
        // The box being typed in is shown as the board has it: cleared by
        // the host, it is put again at the next key (`cleared`).
        if let Some(t) = &mut self.typing {
            t.shown = board.get(self.me, t.id).is_some();
        }
    }

    // ---- Sending ----

    /// The next batch to send, if one is due (at most every
    /// [`BATCH_EVERY`]), with the generation it was made in.
    pub fn take_batch(&mut self, now: Instant) -> Option<(u32, Vec<BoardOp>)> {
        let early = self
            .sent_at
            .is_some_and(|t| now.saturating_duration_since(t) < BATCH_EVERY);
        // The operations before the first picture: it goes after them.
        let ops = self
            .outbox
            .iter()
            .take_while(|o| matches!(o, Out::Op(_)))
            .count();
        if ops == 0 || early {
            return None;
        }
        self.sent_at = Some(now);
        let rest = self.outbox.split_off(ops);
        let taken: Vec<BoardOp> = std::mem::take(&mut self.outbox)
            .into_iter()
            .filter_map(|o| match o {
                Out::Op(op) => Some(op),
                Out::Picture(_) => None,
            })
            .collect();
        let mut parts = batches(taken);
        let first = parts.remove(0);
        self.outbox = parts.concat().into_iter().map(Out::Op).collect();
        self.outbox.extend(rest);
        Some((self.outbox_generation, first))
    }

    /// The next picture to send (M27e), with the generation it was made in,
    /// once every operation before it has gone: its bytes are the window's,
    /// by its id.
    pub fn take_picture(&mut self) -> Option<(u32, Element)> {
        if !matches!(self.outbox.first(), Some(Out::Picture(_))) {
            return None;
        }
        match self.outbox.remove(0) {
            Out::Picture(e) => Some((self.outbox_generation, e)),
            Out::Op(_) => None,
        }
    }

    /// Whether operations wait to go out.
    #[cfg(test)]
    pub fn pending(&self) -> bool {
        !self.outbox.is_empty()
    }

    // ---- Within ----

    fn element(&self, id: u32, kind: ElementKind, points: Vec<Point>, done: bool) -> Element {
        Element {
            picture: None,
            id,
            kind,
            colour: self.colour,
            width: self.width,
            fades: self.fades,
            stay: self.stay,
            done,
            points,
            text: String::new(),
        }
    }

    /// The box being typed in as an element: from its corner, as wide as
    /// its longest line and as tall as its lines, measured by our font.
    fn text_box(&self, t: &Typing, done: bool) -> Element {
        let size = text_size(t.look.width);
        let lines: Vec<&str> = t.text.split('\n').collect();
        let wide = lines.iter().map(|l| self.measure.of(l)).fold(0.0, f32::max) * size;
        let tall = lines.len() as f32 * LINE_HEIGHT * size;
        let (ax, ay) = self.uniform(t.anchor);
        Element {
            kind: ElementKind::Text,
            done,
            points: vec![t.anchor, self.picture(ax + wide, ay + tall)],
            text: t.text.clone(),
            ..t.look.clone()
        }
    }

    /// A text box of ours as its look now makes it: its corner kept, its
    /// other one measured again (a new size). Anything else as it is.
    fn remeasured(&self, e: Element) -> Element {
        if e.kind != ElementKind::Text || e.points.len() != 2 {
            return e;
        }
        let t = Typing {
            id: e.id,
            anchor: e.points[0],
            text: e.text.clone(),
            caret: 0,
            mark: None,
            before: None,
            shown: true,
            look: e.clone(),
        };
        self.text_box(&t, e.done)
    }

    /// The box being typed in, on our board and to the host as it now is:
    /// put while it has text, removed while it has none.
    fn show_typing(&mut self, board: &mut BoardView, now: Instant) {
        let Some(t) = self.typing.take() else {
            return;
        };
        let shown = valid_text(&t.text);
        if shown {
            let e = self.text_box(&t, false);
            self.emit(BoardOp::Put(e), board, now);
        } else if t.shown {
            self.emit(BoardOp::Remove(vec![t.id]), board, now);
        }
        self.typing = Some(Typing { shown, ..t });
    }

    /// Typing in our text box `id`, the caret where `at` is (or at the
    /// end): it isn't finished meanwhile, so it doesn't fade.
    fn edit(&mut self, id: u32, at: Option<Point>, board: &mut BoardView, now: Instant) {
        let Some(e) = board.get(self.me, id).map(|s| s.element.clone()) else {
            return;
        };
        if e.kind != ElementKind::Text || e.points.len() != 2 {
            return;
        }
        let mut t = Typing {
            id,
            look: e.clone(),
            anchor: e.points[0],
            caret: e.text.chars().count(),
            mark: None,
            text: e.text.clone(),
            before: Some(Element { done: true, ..e }),
            shown: true,
        };
        if let Some(at) = at {
            t.caret = self.caret_at(&t, at);
        }
        self.selection.clear();
        self.tool = DrawTool::Text;
        self.typing = Some(t);
        self.show_typing(board, now);
    }

    /// Typing ends: the box goes out finished, and the edit is one step of
    /// the history (a new box, a changed one, or one emptied away). Returns
    /// the box, if it has text.
    fn finish_typing(&mut self, board: &mut BoardView, now: Instant) -> Option<u32> {
        let t = self.typing.take()?;
        // By its text, not by `shown`: a box the host cleared while typed in
        // goes out again, with all its text.
        let after = valid_text(&t.text).then(|| self.text_box(&t, true));
        if let Some(e) = &after {
            self.emit(BoardOp::Put(e.clone()), board, now);
        }
        if t.before != after {
            self.record(vec![Change {
                id: t.id,
                before: t.before,
                after: after.clone(),
            }]);
        }
        after.map(|e| e.id)
    }

    /// Pasted text as a new box of ours with its corner at `at`, moved in
    /// as far as it must to fit the picture, finished and selected.
    fn paste_text(&mut self, text: String, at: Point, board: &mut BoardView, now: Instant) {
        let lines: Vec<&str> = text.split('\n').collect();
        if lines.len() > MAX_TEXT_LINES {
            self.note = Some(TOO_LONG);
        }
        let text = lines[..lines.len().min(MAX_TEXT_LINES)].join("\n");
        let mut chars: Vec<char> = text.chars().collect();
        if chars.len() > MAX_TEXT_CHARS {
            chars.truncate(MAX_TEXT_CHARS);
            self.note = Some(TOO_LONG);
        }
        let text: String = chars.into_iter().collect();
        if !valid_text(&text) {
            return;
        }
        let Some(id) = self.new_id(board) else {
            return;
        };
        let t = Typing {
            id,
            look: self.element(id, ElementKind::Text, Vec::new(), true),
            anchor: at,
            caret: 0,
            mark: None,
            text,
            before: None,
            shown: true,
        };
        // As it would be with room to grow, then moved back inside.
        let size = text_size(t.look.width);
        let lines: Vec<&str> = t.text.split('\n').collect();
        let wide = lines.iter().map(|l| self.measure.of(l)).fold(0.0, f32::max) * size;
        let tall = lines.len() as f32 * LINE_HEIGHT * size;
        let (ax, ay) = self.uniform(at);
        let x = ax.min(self.aspect - wide).max(0.0);
        let y = ay.min(1.0 - tall).max(0.0);
        let t = Typing {
            anchor: self.picture(x, y),
            ..t
        };
        let e = self.text_box(&t, true);
        self.selection = BTreeSet::from([id]);
        self.tool = DrawTool::Select;
        self.perform(
            vec![Change {
                id,
                before: None,
                after: Some(e),
            }],
            board,
            now,
        );
    }

    /// Where the caret goes for `at` in the box `t`: the nearest line, then
    /// the nearest place between its characters.
    fn caret_at(&self, t: &Typing, at: Point) -> usize {
        let size = text_size(t.look.width);
        let (ax, ay) = self.uniform(t.anchor);
        let (px, py) = self.uniform(at);
        let lines: Vec<&str> = t.text.split('\n').collect();
        let line =
            (((py - ay) / (LINE_HEIGHT * size)).floor().max(0.0) as usize).min(lines.len() - 1);
        let before: usize = lines[..line].iter().map(|l| l.chars().count() + 1).sum();
        before + self.nearest(lines[line], px - ax, size)
    }

    /// The place between `line`'s characters nearest `x` across it (picture
    /// heights, its text `size`).
    /// Prefixes only grow, so a binary search: the last place at or before
    /// `x`, or the one after it if nearer (a tie keeps the first). A few
    /// layouts per drag move, not one per character (the review's finding).
    fn nearest(&self, line: &str, x: f32, size: f32) -> usize {
        let chars: Vec<char> = line.chars().collect();
        let width = |k: usize| self.measure.of(&chars[..k].iter().collect::<String>()) * size;
        let (mut lo, mut hi) = (0, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if width(mid) <= x {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        if lo < chars.len() && width(lo + 1) - x < x - width(lo) {
            lo + 1
        } else {
            lo
        }
    }

    /// Whether `at` is within a box, or near enough to pick it.
    fn near_box(&self, (a, b): (Point, Point), at: Point) -> bool {
        let p = self.uniform(at);
        let (a, b) = (self.uniform(a), self.uniform(b));
        p.0 >= a.0.min(b.0) - TOUCH
            && p.0 <= a.0.max(b.0) + TOUCH
            && p.1 >= a.1.min(b.1) - TOUCH
            && p.1 <= a.1.max(b.1) + TOUCH
    }

    /// The topmost of our text boxes under `at`.
    fn hit_text(&self, at: Point, board: &BoardView) -> Option<u32> {
        board
            .of(self.me)
            .filter(|s| s.element.kind == ElementKind::Text && self.touches(&s.element, at, true))
            .last()
            .map(|s| s.element.id)
    }

    /// A number for a new element, unless we have all the elements we may.
    fn new_id(&mut self, board: &BoardView) -> Option<u32> {
        if board.of(self.me).count() >= MAX_MEMBER_ELEMENTS {
            self.note = Some(TOO_MANY);
            return None;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        Some(id)
    }

    /// Applies `op` to our board and queues it, merged with the one before
    /// when that is the same element's.
    fn emit(&mut self, op: BoardOp, board: &mut BoardView, now: Instant) {
        // A picture the board doesn't show as this element yet goes with
        // its bytes (M27e); one moved or resized, as any element.
        let upload = match &op {
            BoardOp::Put(e) if e.picture.is_some() => board
                .get(self.me, e.id)
                .is_none_or(|s| s.element.picture != e.picture),
            _ => false,
        };
        board.apply(self.me, std::slice::from_ref(&op), now);
        if self.outbox.is_empty() {
            self.outbox_generation = self.generation;
        }
        if upload {
            let BoardOp::Put(e) = op else { unreachable!() };
            self.outbox.push(Out::Picture(e));
            return;
        }
        if let Some(Out::Op(last)) = self.outbox.last_mut()
            && merge(last, &op)
        {
            return;
        }
        self.outbox.push(Out::Op(op));
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
            self.note = Some(TOO_MANY);
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
    fn paste_elements(
        &mut self,
        elements: Vec<Element>,
        times: i32,
        board: &mut BoardView,
        now: Instant,
    ) {
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
            // Its box, as its author measured it: the eraser too touches
            // text anywhere in it.
            ElementKind::Text => self.near_box((e.points[0], e.points[1]), at),
            // A picture is all of its box, for the eraser too (M27e).
            ElementKind::Picture => {
                let (lo, hi) = bounds(e);
                at.x >= lo.x && at.x <= hi.x && at.y >= lo.y && at.y <= hi.y
            }
        }
    }

    // ---- Pictures (M27e) ----

    /// A picture pasted or dropped: a new picture of ours, `width` by
    /// `height` pixels, centred at `at` (the picture's middle without it),
    /// fitting [`PICTURE_FITS`] of the picture each way, finished and
    /// selected. Its bytes are the window's, by `picture`.
    #[allow(clippy::too_many_arguments)]
    pub fn paste_picture(
        &mut self,
        picture: PictureId,
        width: u32,
        height: u32,
        at: Option<Point>,
        board: &mut BoardView,
        now: Instant,
    ) {
        if width == 0 || height == 0 {
            return;
        }
        self.release(None, false, board, now);
        self.finish_typing(board, now);
        let Some(id) = self.new_id(board) else {
            return;
        };
        let ratio = width as f32 / height as f32;
        let (mut w, mut h) = (PICTURE_FITS * ratio, PICTURE_FITS);
        if w > PICTURE_FITS * self.aspect {
            w = PICTURE_FITS * self.aspect;
            h = w / ratio;
        }
        let (cx, cy) = self.uniform(at.unwrap_or(Point::new(32_768, 32_768)));
        let x = (cx - w / 2.0).min(self.aspect - w).max(0.0);
        let y = (cy - h / 2.0).min(1.0 - h).max(0.0);
        let mut e = self.element(
            id,
            ElementKind::Picture,
            vec![self.picture(x, y), self.picture(x + w, y + h)],
            true,
        );
        e.picture = Some(picture);
        if !e.is_valid() {
            return;
        }
        self.selection = BTreeSet::from([id]);
        self.tool = DrawTool::Select;
        self.perform(
            vec![Change {
                id,
                before: None,
                after: Some(e),
            }],
            board,
            now,
        );
    }

    /// The resize handles of our one selected element: a picture's, a
    /// rectangle's or an ellipse's corners (top-left, top-right, bottom-left,
    /// bottom-right), a line's or an arrow's two ends; none for anything else
    /// (a stroke, a text box) or a selection of several.
    pub fn handles(&self, board: &BoardView) -> Vec<Point> {
        if self.selection.len() != 1 {
            return Vec::new();
        }
        let Some(e) = self
            .selection
            .first()
            .and_then(|id| board.get(self.me, *id))
            .map(|s| &s.element)
        else {
            return Vec::new();
        };
        match e.kind {
            ElementKind::Picture | ElementKind::Rectangle | ElementKind::Ellipse => {
                let (lo, hi) = bounds(e);
                vec![lo, Point::new(hi.x, lo.y), Point::new(lo.x, hi.y), hi]
            }
            ElementKind::Line | ElementKind::Arrow => e.points.clone(),
            _ => Vec::new(),
        }
    }

    /// A press on one of those handles: the element, which handle, and the
    /// point that stays (the corner across, or the line's other end).
    fn handle_at(&self, at: Point, board: &BoardView) -> Option<(Element, usize, Point)> {
        let handles = self.handles(board);
        let pressed = handles
            .iter()
            .position(|c| self.distance(*c, at) <= HANDLE)?;
        let id = *self.selection.first()?;
        let original = board.get(self.me, id)?.element.clone();
        let fixed = handles[handles.len() - 1 - pressed];
        Some((original, pressed, fixed))
    }

    /// `original` with handle `end` dragged to `at`, `fixed` staying: a
    /// picture keeps its shape ([`Self::resized_picture`]); a rectangle or
    /// an ellipse takes the box from `fixed` to `at`, a line or an arrow its
    /// new end, Shift making squares, circles and 15° steps as when drawn.
    fn resized(
        &self,
        original: &Element,
        end: usize,
        fixed: Point,
        at: Point,
        shift: bool,
    ) -> Element {
        let mut e = original.clone();
        match original.kind {
            ElementKind::Picture => return self.resized_picture(original, fixed, at),
            ElementKind::Rectangle | ElementKind::Ellipse => {
                e.points = vec![fixed, self.constrained(e.kind, fixed, at, shift)];
            }
            ElementKind::Line | ElementKind::Arrow => {
                let moved = self.constrained(e.kind, fixed, at, shift);
                e.points = if end == 0 {
                    vec![moved, fixed]
                } else {
                    vec![fixed, moved]
                };
            }
            _ => {}
        }
        e
    }

    /// A picture with the corner across from `fixed` dragged to `at`, its
    /// shape kept, at least [`PICTURE_LEAST`] each way and inside the
    /// picture.
    fn resized_picture(&self, original: &Element, fixed: Point, at: Point) -> Element {
        let (lo, hi) = bounds(original);
        let (l, h) = (self.uniform(lo), self.uniform(hi));
        let ratio = ((h.0 - l.0) / (h.1 - l.1).max(f32::EPSILON)).max(f32::EPSILON);
        let (fx, fy) = self.uniform(fixed);
        let (ax, ay) = self.uniform(at);
        let sx = if ax < fx { -1.0 } else { 1.0 };
        let sy = if ay < fy { -1.0 } else { 1.0 };
        // As tall as the pointer asks, or as wide, whichever is more.
        let mut tall = (ay - fy).abs().max((ax - fx).abs() / ratio);
        tall = tall.max(PICTURE_LEAST).max(PICTURE_LEAST / ratio);
        // Inside the picture: no taller, nor wider, than there is room for.
        let room_y = if sy < 0.0 { fy } else { 1.0 - fy };
        let room_x = if sx < 0.0 { fx } else { self.aspect - fx };
        tall = tall.min(room_y).min(room_x / ratio);
        let (ex, ey) = (fx + sx * tall * ratio, fy + sy * tall);
        let mut e = original.clone();
        e.points = vec![
            self.picture(fx.min(ex), fy.min(ey)),
            self.picture(fx.max(ex), fy.max(ey)),
        ];
        // Too small to be a box (against the picture's edge): as it was.
        if !e.is_valid() {
            return original.clone();
        }
        e
    }
}

/// The caret's line and its place in that line, both counted from 0, for
/// `caret` characters into `text`.
fn line_col(text: &str, caret: usize) -> (usize, usize) {
    let mut line = 0;
    let mut col = 0;
    for c in text.chars().take(caret) {
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// What a character is for moving by word: a space, part of a word
/// (letters, digits, `_`), anything else (a run of punctuation is a word of
/// its own), or a line break, which word moves stop at, as text fields do.
/// The board overlay's editor alike.
const SPACE: u8 = 0;
const BREAK: u8 = 3;
fn class(c: char) -> u8 {
    if c == '\n' {
        BREAK
    } else if c.is_whitespace() {
        SPACE
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// Ctrl+Right from `at`: past the word there, then the spaces after it, up
/// to a line's end; at a line's end, just past the break.
fn next_word(cs: &[char], at: usize) -> usize {
    let mut i = at;
    match cs.get(i).map(|c| class(*c)) {
        Some(BREAK) => return i + 1,
        Some(k) if k != SPACE => {
            while i < cs.len() && class(cs[i]) == k {
                i += 1;
            }
        }
        _ => {}
    }
    while i < cs.len() && class(cs[i]) == SPACE {
        i += 1;
    }
    i
}

/// Ctrl+Left from `at`: back past the spaces, then the word before them,
/// down to a line's start; at a line's start, just before the break.
fn prev_word(cs: &[char], at: usize) -> usize {
    let mut i = at;
    while i > 0 && class(cs[i - 1]) == SPACE {
        i -= 1;
    }
    if i > 0 {
        let k = class(cs[i - 1]);
        if k == BREAK {
            return if i == at { i - 1 } else { i };
        }
        while i > 0 && class(cs[i - 1]) == k {
            i -= 1;
        }
    }
    i
}

/// The word (or run of spaces, or of punctuation) at `at`, for a double
/// click: the run of one kind of character around it, never a line break
/// (an empty line selects nothing; the review's finding).
fn word_at(cs: &[char], at: usize) -> (usize, usize) {
    if cs.is_empty() {
        return (0, 0);
    }
    // At the end, or on a space or break just past a word: that word.
    let after_word = at > 0 && matches!(class(cs[at - 1]), 1 | 2);
    let i = if at == cs.len() || (matches!(class(cs[at]), SPACE | BREAK) && after_word) {
        at - 1
    } else {
        at
    };
    let k = class(cs[i]);
    if k == BREAK {
        return (at, at);
    }
    let (mut lo, mut hi) = (i, i + 1);
    while lo > 0 && class(cs[lo - 1]) == k {
        lo -= 1;
    }
    while hi < cs.len() && class(cs[hi]) == k {
        hi += 1;
    }
    (lo, hi)
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
