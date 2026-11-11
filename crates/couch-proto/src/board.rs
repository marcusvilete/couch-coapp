//! Drawing on the stream (M27, ADR 0013; docs/protocol.md, Drawing on the
//! stream): the board a session's host keeps, and the operations its members
//! send to draw on it.
//!
//! **The host is the hub.** A member sends only its own operations
//! ([`BoardOps`]); the host checks each batch whole and sends what it took to
//! every viewer with its author ([`BoardUpdate`]), straight, never relayed,
//! like every session fact. Elements are keyed by their author, the
//! connection's member, and a number the author picked: nobody names another
//! member's element.
//!
//! Points are in **the picture's own coordinates**, 0 to 65,535 on each axis,
//! so every window places them alike whatever its size; widths are fractions
//! of the picture's height.

use serde::{Deserialize, Serialize};

use crate::ids::{MemberId, SessionId};

/// The fixed palette, by colour index: bright enough over any game. The
/// stream window and the host's board overlay draw from this one table.
pub const PALETTE: [[u8; 3]; 8] = [
    [0xff, 0x4d, 0x4f], // red
    [0xff, 0x9f, 0x1a], // orange
    [0xff, 0xd6, 0x0a], // yellow
    [0x34, 0xc7, 0x59], // green
    [0x32, 0xd4, 0xf5], // cyan
    [0x3d, 0x7b, 0xff], // blue
    [0xa8, 0x55, 0xf7], // violet
    [0xff, 0xff, 0xff], // white
];
/// What each colour is called, for tooltips.
pub const COLOUR_NAMES: [&str; 8] = [
    "Red", "Orange", "Yellow", "Green", "Cyan", "Blue", "Violet", "White",
];
/// Widths a line may have: thin, medium, bold.
pub const WIDTHS: usize = 3;
/// What each width is called.
pub const WIDTH_NAMES: [&str; WIDTHS] = ["Thin", "Medium", "Bold"];

/// The most points one stroke may have; the sender simplifies to stay under.
pub const MAX_STROKE_POINTS: usize = 2_000;
/// Elements one member may have on the board.
pub const MAX_MEMBER_ELEMENTS: usize = 300;
/// Elements on the board, everyone's.
pub const MAX_BOARD_ELEMENTS: usize = 1_500;
/// Points on the board, everyone's, a text box's text a point a byte
/// ([`Element::load`]).
pub const MAX_BOARD_POINTS: usize = 200_000;
/// A batch's operations, and its points and removed ids together: keeps a
/// batch, and the host's update carrying it, well inside a control frame.
pub const MAX_BATCH_OPS: usize = 64;
pub const MAX_BATCH_ITEMS: usize = 4_000;
/// Laser points in one operation.
pub const MAX_LASER_POINTS: usize = 64;
/// Batches a member may send a second, and laser points and pings.
pub const MAX_BATCHES_PER_SEC: u32 = 40;
pub const MAX_MARKS_PER_SEC: u32 = 60;

/// A picture's encoded bytes at most (M27e): the sender re-encodes to stay
/// under ([`MAX_PICTURE_SIDE`], PNG or JPEG).
pub const MAX_PICTURE_BYTES: usize = 1024 * 1024;
/// A picture's longer side at most, in pixels; the sender scales down to it.
pub const MAX_PICTURE_SIDE: u32 = 1_280;
/// Pictures' bytes on the board, everyone's.
pub const MAX_BOARD_PICTURE_BYTES: usize = 8 * 1024 * 1024;
/// Pictures one member may have on the board.
pub const MAX_MEMBER_PICTURES: usize = 3;
/// Pictures on the board, everyone's: what every window holds decoded (at
/// most 6.5 MB each).
pub const MAX_BOARD_PICTURES: usize = 16;
/// A picture stream's header at most ([`write_header`]).
pub const MAX_PICTURE_HEADER: usize = 512;

/// How long a fading element may stay after it is finished, in seconds: its
/// author picks one per element (wire revision 16, the author's request in
/// M27b's run)...
pub const FADE_STAYS: [u8; 4] = [2, 4, 8, 15];
/// ...this one unless they pick another (M27a's fixed stay).
pub const DEFAULT_STAY: u8 = 4;
/// ...then it fades out over this long. Each window times it from when the
/// element arrived there: no clocks to agree on.
pub const FADE_OUT_MS: u64 = 1_000;
/// The host drops a finished fading element this long after it arrived, a
/// little after every window has faded it, so a newcomer never gets it.
pub fn fade_drop_ms(stay: u8) -> u64 {
    u64::from(stay) * 1_000 + FADE_OUT_MS + 2_000
}
/// A laser trail's points fade within this long.
pub const LASER_MS: u64 = 1_000;
/// A ping pulses this long.
pub const PING_MS: u64 = 2_000;

/// A point in the picture's own coordinates (0 to 65,535 on each axis).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Point {
    pub x: u16,
    pub y: u16,
}

impl Point {
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

/// A picture's id: the SHA-256 of its encoded bytes (M27e). Windows keep
/// pictures by it, so the bytes and the elements showing them may arrive in
/// any order, and whoever receives the bytes checks them against it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PictureId(pub [u8; 32]);

impl std::fmt::Debug for PictureId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PictureId(")?;
        for b in &self.0[..4] {
            write!(f, "{b:02x}")?;
        }
        write!(f, "..)")
    }
}

/// What an element is (M27a; text boxes M27c, wire revision 17; pictures
/// M27e, wire revision 18).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ElementKind {
    /// A freehand line, drawn as it goes: points may follow ([`BoardOp::Append`]).
    Pen,
    /// A wide, see-through freehand line.
    Highlighter,
    /// The shapes: exactly two points, start and end (a box's corners).
    Line,
    Arrow,
    Rectangle,
    Ellipse,
    /// A text box (M27c): its [`Element::text`], lines broken only where
    /// typed, from its first point, the top-left corner. Its second point
    /// is the bottom-right corner as its author's window measured the text:
    /// other windows' fonts may differ a little, so it says where the box
    /// is for picking and moving, never how the text is laid out.
    Text,
    /// A picture (M27e, wire revision 18): its [`Element::picture`], shown
    /// stretched from its first point, the top-left corner, to its second,
    /// the bottom-right one. Its bytes travel on picture streams of their
    /// own ([`PictureUpload`], [`PictureHeader`]), never the control stream.
    Picture,
}

impl ElementKind {
    /// Drawn freehand, point after point.
    pub fn is_stroke(self) -> bool {
        matches!(self, Self::Pen | Self::Highlighter)
    }
}

/// The most characters (Unicode scalar values) a text box holds.
pub const MAX_TEXT_CHARS: usize = 500;
/// The most lines a text box holds: each line is laid out on its own, so
/// this bounds what a full board costs every window to paint.
pub const MAX_TEXT_LINES: usize = 50;
/// A text box's lines, one under the next, this many times its text size
/// apart.
pub const LINE_HEIGHT: f32 = 1.25;

/// A text box's size (its font's em) as a fraction of the picture's height,
/// by width index: small, medium, large.
pub fn text_size(width: u8) -> f32 {
    const SIZE: [f32; WIDTHS] = [0.025, 0.04, 0.065];
    SIZE[usize::from(width).min(WIDTHS - 1)]
}
/// What each text size is called.
pub const TEXT_SIZE_NAMES: [&str; WIDTHS] = ["Small", "Medium", "Large"];

/// A text box's text as the board takes it: 1 to [`MAX_TEXT_CHARS`]
/// characters on at most [`MAX_TEXT_LINES`] lines, something besides
/// spaces, and no control characters but line breaks.
pub fn valid_text(text: &str) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= MAX_TEXT_CHARS
        && text.split('\n').count() <= MAX_TEXT_LINES
        && text.chars().all(|c| c == '\n' || !c.is_control())
}

/// Text from elsewhere (the clipboard) as a text box may hold it, but for
/// its length: line breaks of every kind as `\n`, tabs as four spaces,
/// other control characters gone, and no blank lines or spaces at its end.
/// The board overlay's editor does the same (`cleanText`).
pub fn clean_text(text: &str) -> String {
    let unified = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(unified.len());
    for c in unified.chars() {
        match c {
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.trim_end().to_string()
}

/// A line's width as a fraction of the picture's height, by width index.
pub fn line_width(kind: ElementKind, width: u8) -> f32 {
    const WIDTH: [f32; WIDTHS] = [0.003, 0.006, 0.012];
    let w = WIDTH[usize::from(width).min(WIDTHS - 1)];
    if kind == ElementKind::Highlighter {
        w * 4.0
    } else {
        w
    }
}

/// One element on the board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Element {
    /// The author's own number for it: with the author, its key.
    pub id: u32,
    pub kind: ElementKind,
    /// An index into [`PALETTE`].
    pub colour: u8,
    /// An index into the widths ([`WIDTHS`]).
    pub width: u8,
    /// It fades away once finished, rather than staying.
    pub fades: bool,
    /// How long it stays before it fades, in seconds: one of
    /// [`FADE_STAYS`], whether it fades or not (wire revision 16).
    pub stay: u8,
    /// Finished: no more points come. A shape is always finished; a text
    /// box isn't while its author types (it fades only once finished).
    pub done: bool,
    pub points: Vec<Point>,
    /// A text box's text ([`valid_text`]); empty for every other kind
    /// (wire revision 17).
    pub text: String,
    /// A picture's bytes, by their hash; `None` for every other kind (wire
    /// revision 18).
    pub picture: Option<PictureId>,
}

impl Element {
    /// Well formed on its own: a colour, width and stay that exist, a stroke
    /// of 1 to [`MAX_STROKE_POINTS`] points, a shape of exactly two,
    /// finished, a text box of two with its text; text only in a text box;
    /// a picture of two corners, top-left then bottom-right, finished, with
    /// its picture, and a picture only there.
    pub fn is_valid(&self) -> bool {
        let shape = match self.kind {
            ElementKind::Pen | ElementKind::Highlighter => {
                (1..=MAX_STROKE_POINTS).contains(&self.points.len()) && self.text.is_empty()
            }
            ElementKind::Text => self.points.len() == 2 && valid_text(&self.text),
            ElementKind::Picture => {
                self.points.len() == 2
                    && self.points[0].x < self.points[1].x
                    && self.points[0].y < self.points[1].y
                    && self.done
                    && self.text.is_empty()
            }
            _ => self.points.len() == 2 && self.done && self.text.is_empty(),
        };
        usize::from(self.colour) < PALETTE.len()
            && usize::from(self.width) < WIDTHS
            && FADE_STAYS.contains(&self.stay)
            && (self.kind == ElementKind::Picture) == self.picture.is_some()
            && shape
    }

    /// How long it stays once finished, before fading, in milliseconds.
    pub fn stay_ms(&self) -> u64 {
        u64::from(self.stay) * 1_000
    }

    /// What it weighs on the board, in points: its points, and a text
    /// box's text a point a byte (a point takes up to six on the wire), so
    /// the limits on points bound text too.
    pub fn load(&self) -> usize {
        self.points.len() + self.text.len()
    }
}

/// One thing a member does on the board, to its own elements only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoardOp {
    /// A new element, or one of its own replaced (the same id).
    Put(Element),
    /// More points for its stroke in progress, about every 30 ms while
    /// drawing, so the others see the line as it is drawn; `done` finishes it.
    Append {
        id: u32,
        points: Vec<Point>,
        done: bool,
    },
    /// Some of its own elements gone (erased, undone, deleted).
    Remove(Vec<u32>),
    /// The laser pointer: points of its trail, `start` beginning a new one,
    /// in a colour of the palette (the author's pick). Never on the board:
    /// each window draws the trail fading.
    Laser {
        points: Vec<Point>,
        start: bool,
        colour: u8,
    },
    /// A ring pulsing for a moment where it middle-clicked, with its name.
    /// Never on the board.
    Ping(Point),
}

impl BoardOp {
    /// A passing mark (laser, ping): never stored, counted against
    /// [`MAX_MARKS_PER_SEC`].
    pub fn is_mark(&self) -> bool {
        matches!(self, Self::Laser { .. } | Self::Ping(_))
    }

    /// Points, text bytes and ids it carries, for [`MAX_BATCH_ITEMS`].
    pub fn items(&self) -> usize {
        match self {
            Self::Put(e) => e.load(),
            Self::Append { points, .. } | Self::Laser { points, .. } => points.len(),
            Self::Remove(ids) => ids.len(),
            Self::Ping(_) => 1,
        }
    }

    /// Laser points and pings it carries.
    pub fn marks(&self) -> usize {
        match self {
            Self::Laser { points, .. } => points.len(),
            Self::Ping(_) => 1,
            _ => 0,
        }
    }
}

/// Whether a batch is well formed on its own (not whether the board takes
/// it): 1 to [`MAX_BATCH_OPS`] operations, at most [`MAX_BATCH_ITEMS`]
/// points and ids, valid elements, and laser operations of at most
/// [`MAX_LASER_POINTS`].
pub fn valid_batch(ops: &[BoardOp]) -> bool {
    !ops.is_empty()
        && ops.len() <= MAX_BATCH_OPS
        && ops.iter().map(BoardOp::items).sum::<usize>() <= MAX_BATCH_ITEMS
        && ops.iter().all(|op| match op {
            BoardOp::Put(e) => e.is_valid(),
            BoardOp::Append { points, .. } => points.len() <= MAX_STROKE_POINTS,
            BoardOp::Remove(ids) => !ids.is_empty(),
            BoardOp::Laser { points, colour, .. } => {
                !points.is_empty()
                    && points.len() <= MAX_LASER_POINTS
                    && usize::from(*colour) < PALETTE.len()
            }
            BoardOp::Ping(_) => true,
        })
}

/// `ops` in batches the checks allow ([`valid_batch`]), in order: for the
/// host sending a member's own elements back, and a window pasting many.
pub fn batches(ops: Vec<BoardOp>) -> Vec<Vec<BoardOp>> {
    let mut out: Vec<Vec<BoardOp>> = Vec::new();
    let mut items = 0;
    for op in ops {
        let n = op.items();
        let full = out
            .last()
            .is_none_or(|b| b.len() >= MAX_BATCH_OPS || items + n > MAX_BATCH_ITEMS);
        if full {
            out.push(Vec::new());
            items = 0;
        }
        items += n;
        out.last_mut().expect("one").push(op);
    }
    out
}

/// Member to host, optional: its own operations, in order. The host takes
/// a batch whole or not at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardOps {
    pub session: SessionId,
    /// The board's generation the member last heard of ([`BoardCleared`],
    /// [`BoardSnapshot`]): a batch made before a clear it hadn't heard of
    /// yet is refused, so nothing drawn before a clear lands after it.
    pub generation: u32,
    pub ops: Vec<BoardOp>,
}

/// Host to each viewer, optional: a batch it took, and whose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardUpdate {
    pub session: SessionId,
    pub author: MemberId,
    pub ops: Vec<BoardOp>,
}

/// An element with its author, in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authored {
    pub author: MemberId,
    pub element: Element,
}

/// Host to a viewer, optional: the board whole, in parts that each fit a
/// control frame. The `first` replaces whatever the viewer had; the rest add
/// to it. Sent when it starts watching and when drawing is turned on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardSnapshot {
    pub session: SessionId,
    /// The board's generation, for the member's next batches.
    pub generation: u32,
    pub first: bool,
    pub elements: Vec<Authored>,
}

/// What one part of a snapshot may carry: elements weigh their points and
/// a little more, and a part holds at most this much.
const SNAPSHOT_PART_WEIGHT: usize = 6_000;

fn weight(element: &Element) -> usize {
    // An author (32 bytes) and the element's own fields weigh about as much
    // as eight points; a text box's text, a point a byte.
    element.load() + 8
}

impl BoardSnapshot {
    /// The board in parts, in order; at least one (`first`), even empty.
    pub fn parts(
        session: SessionId,
        generation: u32,
        elements: impl IntoIterator<Item = Authored>,
    ) -> Vec<Self> {
        let mut parts = vec![Self {
            session,
            generation,
            first: true,
            elements: Vec::new(),
        }];
        let mut load = 0;
        for authored in elements {
            let w = weight(&authored.element);
            if load + w > SNAPSHOT_PART_WEIGHT && !parts.last().expect("one").elements.is_empty() {
                parts.push(Self {
                    session,
                    generation,
                    first: false,
                    elements: Vec::new(),
                });
                load = 0;
            }
            load += w;
            parts.last_mut().expect("one").elements.push(authored);
        }
        parts
    }

    /// Well formed: each element valid on its own.
    pub fn is_valid(&self) -> bool {
        self.elements.len() <= MAX_BOARD_ELEMENTS
            && self.elements.iter().all(|a| a.element.is_valid())
    }
}

/// Host to each viewer, optional: elements gone at the host's word: one
/// member's (stopped) or, with `None`, everyone's (the board cleared,
/// drawing turned off, the stream ended). Also to a member alone, its own,
/// right before they are sent back to it after a batch it refused: what the
/// member shows of its own is then the host's again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardCleared {
    pub session: SessionId,
    /// The board's generation from now on: every clear starts a new one.
    pub generation: u32,
    pub author: Option<MemberId>,
}

/// Host to each viewer, optional: whether drawing is on, and whom the host
/// stopped. Sent when a viewer starts watching and on every change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardState {
    pub session: SessionId,
    pub on: bool,
    pub stopped: Vec<MemberId>,
    /// The host allows pictures on the board (M27e, wire revision 18).
    pub pictures: bool,
}

impl BoardState {
    pub fn is_valid(&self) -> bool {
        self.stopped.len() <= crate::room::MAX_ROSTER
    }
}

/// Why the host dropped a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    /// Drawing is off in this session.
    Off,
    /// The host stopped this member drawing.
    Stopped,
    /// This member holds the keyboard-and-mouse seat: its keys and mouse are
    /// the game's.
    KeyboardSeat,
    /// Not watching the session.
    NotWatching,
    /// The board holds all the elements or points it takes.
    BoardFull,
    /// This member has all the elements it may.
    TooMany,
    /// Too many batches, laser points or pings this second.
    TooFast,
    /// Something it may not do: another's element, a finished stroke, a
    /// malformed batch.
    Invalid,
    /// Made before a clear it hadn't heard of yet. Never sent: the member
    /// hears of the clear itself.
    Stale,
    /// Nothing is streamed: no picture to draw on.
    NoStream,
    /// The host doesn't allow pictures (M27e).
    PicturesOff,
    /// A picture that isn't one the board takes: not PNG or JPEG, too
    /// large, or not what its id says.
    BadPicture,
    /// This member has all the pictures it may, or the board all it takes.
    TooManyPictures,
}

impl Refusal {
    /// What the stream window says.
    pub fn message(self) -> &'static str {
        match self {
            Self::Off => "Drawing is off in this session",
            Self::Stopped => "The host stopped your drawing",
            Self::KeyboardSeat => "You can't draw while you have the keyboard and mouse",
            Self::NotWatching => "You aren't watching this session",
            Self::BoardFull => "The board is full: the host can clear it",
            Self::TooMany => "You've drawn all you may: erase some first",
            Self::TooFast => "Drawing too fast: some of it was lost",
            Self::Invalid => "The host couldn't take that drawing",
            Self::Stale => "The board was cleared",
            Self::NoStream => "Nothing is streamed to draw on",
            Self::PicturesOff => "The host doesn't allow pictures",
            Self::BadPicture => "That picture can't go on the board",
            Self::TooManyPictures => "No room for more pictures: erase one first",
        }
    }
}

/// Host to the member whose batch it dropped, optional: why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardRefused {
    pub session: SessionId,
    pub why: Refusal,
}

/// The header of a member's picture stream to the host
/// (`media::stream_kind::PICTURE_UP`, M27e), its bytes after it: a picture
/// put on the board whole, the element with its bytes. The host takes it as
/// a batch of that one `Put`, under the same checks and more, and answers
/// with [`PictureAnswer`] on the control stream; the member's later
/// batches wait for that answer, so they come after it in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PictureUpload {
    pub session: SessionId,
    /// As a batch's ([`BoardOps::generation`]).
    pub generation: u32,
    /// A picture ([`ElementKind::Picture`]) naming the bytes that follow.
    pub element: Element,
    /// How many bytes follow: then the stream ends.
    pub len: u32,
}

impl PictureUpload {
    /// Well formed: a valid picture element, and bytes within the limit.
    pub fn is_valid(&self) -> bool {
        self.element.kind == ElementKind::Picture
            && self.element.is_valid()
            && (1..=MAX_PICTURE_BYTES).contains(&(self.len as usize))
    }
}

/// The header of the host's picture stream to a viewer
/// (`media::stream_kind::PICTURE`, M27e), the picture's bytes after it.
/// The host sends each picture on the board to each viewer once, as it is
/// put there or the viewer starts watching; the elements showing it come on
/// the control stream as any other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PictureHeader {
    pub session: SessionId,
    pub picture: PictureId,
    /// How many bytes follow: then the stream ends.
    pub len: u32,
}

impl PictureHeader {
    pub fn is_valid(&self) -> bool {
        (1..=MAX_PICTURE_BYTES).contains(&(self.len as usize))
    }
}

/// A picture stream's header as it goes on the stream, after the kind byte:
/// its length (`u16`, big-endian, at most [`MAX_PICTURE_HEADER`]) and the
/// header in postcard.
pub fn write_header<T: Serialize>(header: &T) -> Vec<u8> {
    let body = postcard::to_stdvec(header).expect("a header encodes");
    debug_assert!(body.len() <= MAX_PICTURE_HEADER);
    let mut out = Vec::with_capacity(2 + body.len());
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// The header's length from its first two bytes, if within the limit.
pub fn header_len(prefix: [u8; 2]) -> Option<usize> {
    let len = usize::from(u16::from_be_bytes(prefix));
    (1..=MAX_PICTURE_HEADER).contains(&len).then_some(len)
}

/// Host to the member who sent a picture, optional (M27e): taken, or why
/// not. The member's engine sends its next batches only after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PictureAnswer {
    pub session: SessionId,
    /// The picture element's id.
    pub id: u32,
    /// `None`: on the board.
    pub refusal: Option<Refusal>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(id: u32, points: usize) -> Element {
        Element {
            picture: None,
            id,
            kind: ElementKind::Pen,
            colour: 7,
            width: 2,
            fades: true,
            stay: 4,
            done: true,
            points: (0..points)
                .map(|i| Point::new(u16::MAX - (i as u16), u16::MAX))
                .collect(),
            text: String::new(),
        }
    }

    /// A text box of `chars` characters, each `c`.
    fn text_box(id: u32, c: char, chars: usize) -> Element {
        Element {
            kind: ElementKind::Text,
            done: false,
            text: std::iter::repeat_n(c, chars).collect(),
            ..stroke(id, 2)
        }
    }

    /// A picture of the whole board.
    fn picture(id: u32) -> Element {
        Element {
            kind: ElementKind::Picture,
            points: vec![Point::new(0, 0), Point::new(u16::MAX, u16::MAX)],
            picture: Some(PictureId([0xff; 32])),
            ..stroke(id, 2)
        }
    }

    #[test]
    fn pictures_are_checked() {
        assert!(picture(1).is_valid());
        assert!(
            !Element {
                picture: None,
                ..picture(1)
            }
            .is_valid(),
            "a picture names its bytes"
        );
        assert!(
            !Element {
                picture: Some(PictureId([0; 32])),
                ..stroke(1, 2)
            }
            .is_valid(),
            "and only a picture does"
        );
        assert!(
            !Element {
                done: false,
                ..picture(1)
            }
            .is_valid()
        );
        assert!(
            !Element {
                text: "x".into(),
                ..picture(1)
            }
            .is_valid()
        );
        for points in [
            vec![Point::new(0, 0)],
            vec![Point::new(9, 0), Point::new(3, 5)],
            vec![Point::new(0, 5), Point::new(3, 5)],
            vec![Point::new(0, 0), Point::new(1, 1), Point::new(2, 2)],
        ] {
            let e = Element {
                points: points.clone(),
                ..picture(1)
            };
            assert!(!e.is_valid(), "{points:?}: top-left, then bottom-right");
        }
        let upload = |len: usize| PictureUpload {
            session: SessionId(1),
            generation: 0,
            element: picture(1),
            len: len as u32,
        };
        assert!(upload(MAX_PICTURE_BYTES).is_valid());
        assert!(!upload(MAX_PICTURE_BYTES + 1).is_valid());
        assert!(!upload(0).is_valid());
        assert!(
            !PictureUpload {
                element: stroke(1, 2),
                ..upload(1)
            }
            .is_valid(),
            "only a picture comes on a picture stream"
        );
        let header = |len: usize| PictureHeader {
            session: SessionId(1),
            picture: PictureId([1; 32]),
            len: len as u32,
        };
        assert!(header(1).is_valid() && header(MAX_PICTURE_BYTES).is_valid());
        assert!(!header(0).is_valid() && !header(MAX_PICTURE_BYTES + 1).is_valid());
    }

    /// A picture stream's header at its fullest fits its limit, reads back
    /// as written, and a header with bytes after it is refused.
    #[test]
    fn picture_headers_roundtrip_within_their_limit() {
        let upload = PictureUpload {
            session: SessionId(u64::MAX),
            generation: u32::MAX,
            element: Element {
                id: u32::MAX,
                points: vec![
                    Point::new(u16::MAX - 1, u16::MAX - 1),
                    Point::new(u16::MAX, u16::MAX),
                ],
                ..picture(1)
            },
            len: MAX_PICTURE_BYTES as u32,
        };
        let bytes = write_header(&upload);
        let len = header_len([bytes[0], bytes[1]]).expect("within the limit");
        assert_eq!(len, bytes.len() - 2);
        assert!(len <= MAX_PICTURE_HEADER / 2, "{len} bytes: room to spare");
        let back: PictureUpload = crate::media::decode_payload(&bytes[2..]).unwrap();
        assert_eq!(back, upload);
        let mut longer = bytes[2..].to_vec();
        longer.push(0);
        assert!(crate::media::decode_payload::<PictureUpload>(&longer).is_none());

        let header = PictureHeader {
            session: SessionId(u64::MAX),
            picture: PictureId([0xff; 32]),
            len: MAX_PICTURE_BYTES as u32,
        };
        let bytes = write_header(&header);
        let back: PictureHeader = crate::media::decode_payload(&bytes[2..]).unwrap();
        assert_eq!(back, header);

        assert_eq!(header_len([0, 0]), None, "empty");
        assert_eq!(
            header_len(((MAX_PICTURE_HEADER + 1) as u16).to_be_bytes()),
            None
        );
    }

    #[test]
    fn text_boxes_are_checked() {
        assert!(text_box(1, 'a', 1).is_valid());
        assert!(
            text_box(1, 'ç', MAX_TEXT_CHARS).is_valid(),
            "characters, not bytes"
        );
        assert!(!text_box(1, 'a', MAX_TEXT_CHARS + 1).is_valid());
        assert!(!text_box(1, 'a', 0).is_valid(), "empty");
        assert!(!text_box(1, ' ', 3).is_valid(), "spaces alone");
        let lines = |n: usize| Element {
            text: vec!["x"; n].join("\n"),
            ..text_box(1, 'a', 1)
        };
        assert!(lines(MAX_TEXT_LINES).is_valid());
        assert!(!lines(MAX_TEXT_LINES + 1).is_valid(), "too many lines");
        assert!(
            Element {
                text: "\u{feff}".into(),
                ..text_box(1, 'a', 1)
            }
            .is_valid(),
            "a byte order mark isn't a space"
        );
        let lines = Element {
            text: "two\nlines".into(),
            ..text_box(1, 'a', 1)
        };
        assert!(lines.is_valid());
        for bad in ["a\tb", "a\rb", "a\u{7f}", "a\u{85}", "a\u{0}"] {
            let e = Element {
                text: bad.into(),
                ..text_box(1, 'a', 1)
            };
            assert!(!e.is_valid(), "{bad:?}");
        }
        assert!(
            !Element {
                points: vec![Point::new(0, 0)],
                ..text_box(1, 'a', 1)
            }
            .is_valid(),
            "a box has two corners"
        );
        assert!(
            !Element {
                text: "x".into(),
                ..stroke(1, 3)
            }
            .is_valid(),
            "text only in a text box"
        );
        assert!(
            !Element {
                kind: ElementKind::Arrow,
                text: "x".into(),
                ..stroke(1, 2)
            }
            .is_valid()
        );
        assert_eq!(text_box(1, 'ç', 10).load(), 2 + 20, "a point a byte");
    }

    #[test]
    fn pasted_text_is_cleaned() {
        assert_eq!(clean_text("a\r\nb\rc\n"), "a\nb\nc");
        assert_eq!(clean_text("\tx\u{7}y \n\n  "), "    xy");
        assert_eq!(clean_text("é\u{85}ü"), "éü");
        assert_eq!(clean_text(" \n "), "");
        assert!(valid_text(&clean_text("  indented\r\n\tline\u{0} ")));
    }

    #[test]
    fn text_sizes_grow() {
        assert!(text_size(0) < text_size(1) && text_size(1) < text_size(2));
        assert_eq!(text_size(9), text_size(2));
    }

    #[test]
    fn elements_are_checked() {
        assert!(stroke(1, 1).is_valid());
        assert!(stroke(1, MAX_STROKE_POINTS).is_valid());
        assert!(!stroke(1, 0).is_valid());
        assert!(!stroke(1, MAX_STROKE_POINTS + 1).is_valid());
        assert!(
            !Element {
                colour: 8,
                ..stroke(1, 3)
            }
            .is_valid()
        );
        assert!(
            !Element {
                width: 3,
                ..stroke(1, 3)
            }
            .is_valid()
        );
        let arrow = Element {
            kind: ElementKind::Arrow,
            ..stroke(2, 2)
        };
        assert!(arrow.is_valid());
        assert!(
            !Element {
                done: false,
                ..arrow.clone()
            }
            .is_valid(),
            "a shape is finished"
        );
        assert!(
            !Element {
                points: vec![Point::new(0, 0); 3],
                ..arrow
            }
            .is_valid()
        );
    }

    #[test]
    fn batches_are_checked() {
        assert!(!valid_batch(&[]));
        assert!(valid_batch(&[BoardOp::Ping(Point::new(1, 2))]));
        assert!(!valid_batch(&vec![
            BoardOp::Ping(Point::new(1, 2));
            MAX_BATCH_OPS + 1
        ]));
        assert!(!valid_batch(&[BoardOp::Remove(vec![])]));
        assert!(!valid_batch(&[BoardOp::Laser {
            points: vec![Point::new(0, 0); MAX_LASER_POINTS + 1],
            start: true,
            colour: 0,
        }]));
        let laser = |colour| BoardOp::Laser {
            points: vec![Point::new(0, 0)],
            start: true,
            colour,
        };
        assert!(valid_batch(&[laser(7)]));
        assert!(!valid_batch(&[laser(8)]), "a colour outside the palette");
        assert!(!valid_batch(&[
            BoardOp::Put(stroke(1, MAX_STROKE_POINTS)),
            BoardOp::Put(stroke(2, MAX_STROKE_POINTS)),
            BoardOp::Put(stroke(3, 1)),
        ]));
        assert!(!valid_batch(&[BoardOp::Put(stroke(1, 0))]));
    }

    /// The fullest batch the checks allow fits a control frame, and so does
    /// the host's update carrying it.
    #[test]
    fn the_fullest_batch_fits_a_control_frame() {
        let mut ops: Vec<BoardOp> = vec![
            BoardOp::Put(stroke(u32::MAX, MAX_STROKE_POINTS)),
            BoardOp::Append {
                id: u32::MAX,
                points: vec![Point::new(u16::MAX, u16::MAX); MAX_STROKE_POINTS - 62 * 4],
                done: true,
            },
        ];
        for _ in 0..62 {
            ops.push(BoardOp::Remove(vec![u32::MAX; 4]));
        }
        assert!(valid_batch(&ops), "{}", ops.len());
        let update = BoardUpdate {
            session: SessionId(u64::MAX),
            author: MemberId([0xff; 32]),
            ops,
        };
        let len = postcard::to_stdvec(&update).unwrap().len();
        assert!(len + 2 <= crate::envelope::MAX_FRAME_LEN, "{len} bytes");

        // Text: the widest characters, as many as the items allow, beside
        // ids up to the batch's operations.
        let mut ops = vec![
            BoardOp::Put(text_box(1, '😀', MAX_TEXT_CHARS)),
            BoardOp::Put(text_box(2, '😀', (MAX_BATCH_ITEMS - 4 - 2_000 - 62) / 4)),
        ];
        for _ in 0..62 {
            ops.push(BoardOp::Remove(vec![u32::MAX]));
        }
        assert!(valid_batch(&ops), "{}", ops.len());
        let update = BoardUpdate {
            session: SessionId(u64::MAX),
            author: MemberId([0xff; 32]),
            ops,
        };
        let len = postcard::to_stdvec(&update).unwrap().len();
        assert!(len + 2 <= crate::envelope::MAX_FRAME_LEN, "{len} bytes");
    }

    /// Every part of the fullest board fits a control frame, and the parts
    /// carry the board whole, in order.
    #[test]
    fn a_snapshot_comes_in_parts_that_fit() {
        let author = MemberId([0xff; 32]);
        let mut elements = Vec::new();
        // Many small elements, then long strokes: both kinds of full.
        for id in 0..1_400 {
            elements.push(Authored {
                author,
                element: Element {
                    kind: ElementKind::Rectangle,
                    ..stroke(id, 2)
                },
            });
        }
        for id in 1_400..1_450 {
            elements.push(Authored {
                author,
                element: stroke(id, MAX_STROKE_POINTS),
            });
        }
        // And text boxes of the widest characters.
        for id in 1_450..1_500 {
            elements.push(Authored {
                author,
                element: text_box(id, '😀', MAX_TEXT_CHARS),
            });
        }
        let parts = BoardSnapshot::parts(SessionId(u64::MAX), u32::MAX, elements.clone());
        assert!(parts[0].first && parts[1..].iter().all(|p| !p.first));
        for part in &parts {
            assert!(part.is_valid());
            let len = postcard::to_stdvec(part).unwrap().len();
            assert!(len + 2 <= crate::envelope::MAX_FRAME_LEN, "{len} bytes");
        }
        let whole: Vec<Authored> = parts.into_iter().flat_map(|p| p.elements).collect();
        assert_eq!(whole, elements);
        let empty = BoardSnapshot::parts(SessionId(1), 3, []);
        assert_eq!(empty.len(), 1);
        assert!(empty[0].first && empty[0].elements.is_empty());
        assert_eq!(empty[0].generation, 3);
    }

    #[test]
    fn many_operations_go_in_batches_the_checks_allow() {
        let ops: Vec<BoardOp> = (0..300)
            .map(|id| BoardOp::Put(stroke(id, if id % 7 == 0 { 1_500 } else { 3 })))
            .collect();
        let batches = batches(ops.clone());
        assert!(batches.iter().all(|b| valid_batch(b)));
        assert_eq!(batches.concat(), ops);
        assert!(super::batches(Vec::new()).is_empty());
    }

    #[test]
    fn highlighters_are_wider() {
        assert!(line_width(ElementKind::Highlighter, 0) > line_width(ElementKind::Pen, 1));
        assert_eq!(
            line_width(ElementKind::Pen, 9),
            line_width(ElementKind::Pen, 2)
        );
    }
}
