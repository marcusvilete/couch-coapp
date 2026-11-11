//! Drawing on the stream as the app's front end sees it (M27b, ADR 0013):
//! the host's board overlay draws and edits the board in the webview, so the
//! board's elements, operations and news cross the UI boundary as these
//! types, and the board's rules (palette, widths, limits, timings, the
//! drawing keys) are written for it from `couch_proto` and `couch_input`,
//! one source for both editors ([`rules_ts`]).
//!
//! **The board overlay's window** is the app's to make ([`BoardWindows`]),
//! the engine's to place and switch: the webview never moves it, focuses it,
//! or names it.

use couch_input::shortcuts::{DRAWING_KEYS, DrawAction, DrawTool, tool_key};
use couch_proto::{
    board::{
        Authored, BoardOp, COLOUR_NAMES, DEFAULT_STAY, Element, ElementKind, FADE_OUT_MS,
        FADE_STAYS, LASER_MS, LINE_HEIGHT, MAX_BATCH_ITEMS, MAX_BATCH_OPS, MAX_LASER_POINTS,
        MAX_MEMBER_ELEMENTS, MAX_STROKE_POINTS, MAX_TEXT_CHARS, MAX_TEXT_LINES, PALETTE, PING_MS,
        Point, TEXT_SIZE_NAMES, WIDTH_NAMES, WIDTHS, line_width, text_size,
    },
    ids::MemberId,
    ipc::BoardNews,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::pipeline::NativeId;

/// The app's side of the board overlay's window (M27b, Windows): the engine
/// asks for it while the host has something to see or draw over the game,
/// and lets it go after. Both may block a moment; the engine calls them
/// off its own thread.
pub trait BoardWindows: Send + Sync {
    /// Makes the window: transparent, click-through, never activated, left
    /// out of captures, waiting off every screen. Its native handle, or
    /// `None` if it couldn't be made.
    fn open(&self) -> Option<NativeId>;
    /// Closes the window `open` made as `window`, if it is still open:
    /// that one alone, never a newer one.
    fn close(&self, window: NativeId);
}

/// A point in the picture's own coordinates, 0 to 65,535 each way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BoardPoint {
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum BoardElementKind {
    Pen,
    Highlighter,
    Line,
    Arrow,
    Rectangle,
    Ellipse,
    Text,
}

/// One element on the board (`couch_proto::board::Element`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BoardElement {
    pub id: u32,
    pub kind: BoardElementKind,
    pub colour: u8,
    pub width: u8,
    pub fades: bool,
    /// How long it stays before fading, in seconds: one of `FADE_STAYS`.
    pub stay: u8,
    pub done: bool,
    pub points: Vec<BoardPoint>,
    /// A text box's text; empty for every other kind (M27c).
    pub text: String,
}

/// One thing the host does on the board, to its own elements only
/// (`couch_proto::board::BoardOp`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum BoardOpView {
    Put {
        element: BoardElement,
    },
    Append {
        id: u32,
        points: Vec<BoardPoint>,
        done: bool,
    },
    Remove {
        ids: Vec<u32>,
    },
    Laser {
        points: Vec<BoardPoint>,
        start: bool,
        colour: u8,
    },
    Ping {
        at: BoardPoint,
    },
}

/// An element and its author, by member id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AuthoredElement {
    pub author: String,
    pub element: BoardElement,
}

/// What the board says to the host's overlay, as a stream window hears it
/// (`couch_proto::ipc::BoardNews`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum BoardNewsView {
    /// The board whole, in parts: the first replaces what was shown.
    Snapshot {
        generation: u32,
        first: bool,
        elements: Vec<AuthoredElement>,
    },
    /// Someone else's operations.
    Update {
        author: String,
        ops: Vec<BoardOpView>,
    },
    /// One author's elements gone (the host's own among them: the
    /// generation is then ours), or everyone's.
    Cleared {
        generation: u32,
        author: Option<String>,
    },
    /// Our own batch was refused: what to say.
    Refused { message: String },
}

fn point(p: Point) -> BoardPoint {
    BoardPoint { x: p.x, y: p.y }
}

fn points(ps: &[Point]) -> Vec<BoardPoint> {
    ps.iter().copied().map(point).collect()
}

fn from_points(ps: &[BoardPoint]) -> Vec<Point> {
    ps.iter().map(|p| Point::new(p.x, p.y)).collect()
}

impl From<ElementKind> for BoardElementKind {
    fn from(k: ElementKind) -> Self {
        match k {
            ElementKind::Pen => Self::Pen,
            ElementKind::Highlighter => Self::Highlighter,
            ElementKind::Line => Self::Line,
            ElementKind::Arrow => Self::Arrow,
            ElementKind::Rectangle => Self::Rectangle,
            ElementKind::Ellipse => Self::Ellipse,
            ElementKind::Text => Self::Text,
        }
    }
}

impl From<BoardElementKind> for ElementKind {
    fn from(k: BoardElementKind) -> Self {
        match k {
            BoardElementKind::Pen => Self::Pen,
            BoardElementKind::Highlighter => Self::Highlighter,
            BoardElementKind::Line => Self::Line,
            BoardElementKind::Arrow => Self::Arrow,
            BoardElementKind::Rectangle => Self::Rectangle,
            BoardElementKind::Ellipse => Self::Ellipse,
            BoardElementKind::Text => Self::Text,
        }
    }
}

impl From<&Element> for BoardElement {
    fn from(e: &Element) -> Self {
        Self {
            id: e.id,
            kind: e.kind.into(),
            colour: e.colour,
            width: e.width,
            fades: e.fades,
            stay: e.stay,
            done: e.done,
            points: points(&e.points),
            text: e.text.clone(),
        }
    }
}

impl From<&BoardElement> for Element {
    fn from(e: &BoardElement) -> Self {
        Self {
            id: e.id,
            kind: e.kind.into(),
            colour: e.colour,
            width: e.width,
            fades: e.fades,
            stay: e.stay,
            done: e.done,
            points: from_points(&e.points),
            text: e.text.clone(),
        }
    }
}

impl From<&BoardOp> for BoardOpView {
    fn from(op: &BoardOp) -> Self {
        match op {
            BoardOp::Put(e) => Self::Put { element: e.into() },
            BoardOp::Append {
                id,
                points: p,
                done,
            } => Self::Append {
                id: *id,
                points: points(p),
                done: *done,
            },
            BoardOp::Remove(ids) => Self::Remove { ids: ids.clone() },
            BoardOp::Laser {
                points: p,
                start,
                colour,
            } => Self::Laser {
                points: points(p),
                start: *start,
                colour: *colour,
            },
            BoardOp::Ping(at) => Self::Ping { at: point(*at) },
        }
    }
}

impl From<&BoardOpView> for BoardOp {
    fn from(op: &BoardOpView) -> Self {
        match op {
            BoardOpView::Put { element } => Self::Put(element.into()),
            BoardOpView::Append { id, points, done } => Self::Append {
                id: *id,
                points: from_points(points),
                done: *done,
            },
            BoardOpView::Remove { ids } => Self::Remove(ids.clone()),
            BoardOpView::Laser {
                points,
                start,
                colour,
            } => Self::Laser {
                points: from_points(points),
                start: *start,
                colour: *colour,
            },
            BoardOpView::Ping { at } => Self::Ping(Point::new(at.x, at.y)),
        }
    }
}

fn authored(a: &Authored) -> AuthoredElement {
    AuthoredElement {
        author: a.author.to_hex(),
        element: (&a.element).into(),
    }
}

impl From<&BoardNews> for BoardNewsView {
    fn from(news: &BoardNews) -> Self {
        match news {
            BoardNews::Snapshot(s) => Self::Snapshot {
                generation: s.generation,
                first: s.first,
                elements: s.elements.iter().map(authored).collect(),
            },
            BoardNews::Update(u) => Self::Update {
                author: u.author.to_hex(),
                ops: u.ops.iter().map(Into::into).collect(),
            },
            BoardNews::Cleared(c) => Self::Cleared {
                generation: c.generation,
                author: c.author.as_ref().map(MemberId::to_hex),
            },
            BoardNews::Refused(why) => Self::Refused {
                message: why.message().to_string(),
            },
        }
    }
}

// ---- The board's rules, for the front end ----

fn tool_name(tool: DrawTool) -> &'static str {
    match tool {
        DrawTool::Select => "select",
        DrawTool::Pen => "pen",
        DrawTool::Highlighter => "highlighter",
        DrawTool::Laser => "laser",
        DrawTool::Arrow => "arrow",
        DrawTool::Line => "line",
        DrawTool::Rectangle => "rectangle",
        DrawTool::Ellipse => "ellipse",
        DrawTool::Text => "text",
        DrawTool::Eraser => "eraser",
    }
}

fn action_ts(action: DrawAction) -> String {
    let plain = |t: &str| format!("{{ type: \"{t}\" }}");
    match action {
        DrawAction::Undo => plain("undo"),
        DrawAction::Redo => plain("redo"),
        DrawAction::Copy => plain("copy"),
        DrawAction::Paste => plain("paste"),
        DrawAction::Duplicate => plain("duplicate"),
        DrawAction::SelectAll => plain("selectAll"),
        DrawAction::Delete => plain("delete"),
        DrawAction::Nudge(dx, dy) => format!("{{ type: \"nudge\", dx: {dx}, dy: {dy} }}"),
        DrawAction::Tool(tool) => format!("{{ type: \"tool\", tool: \"{}\" }}", tool_name(tool)),
        DrawAction::Leave => plain("leave"),
        DrawAction::ToggleToolbar => plain("toggleToolbar"),
    }
}

/// `boardRules.ts`: the board's rules as the front end's editor reads
/// them, from the same tables the stream window's editor reads. Written by
/// the tests, beside the other bindings.
pub fn rules_ts() -> String {
    let mut ts = String::from(
        "// Written by couch-engine's tests from couch_proto::board and\n\
         // couch_input::shortcuts (M27b): the board's rules, one source for the\n\
         // stream window's editor and the board overlay's. Don't edit.\n\n",
    );
    let list = |items: Vec<String>| items.join(", ");
    ts += &format!(
        "export const PALETTE: [number, number, number][] = [{}];\n",
        list(
            PALETTE
                .iter()
                .map(|[r, g, b]| format!("[{r}, {g}, {b}]"))
                .collect()
        )
    );
    ts += &format!(
        "export const COLOUR_NAMES: string[] = [{}];\n",
        list(COLOUR_NAMES.iter().map(|n| format!("\"{n}\"")).collect())
    );
    ts += &format!(
        "/** How long a fading element may stay before it fades, in seconds (its author's pick), and the default. */
export const FADE_STAYS: number[] = [{}];
export const DEFAULT_STAY = {DEFAULT_STAY};
",
        list(FADE_STAYS.iter().map(u8::to_string).collect())
    );
    ts += &format!(
        "export const WIDTH_NAMES: string[] = [{}];\n",
        list(WIDTH_NAMES.iter().map(|n| format!("\"{n}\"")).collect())
    );
    let widths = |kind| {
        list(
            (0..WIDTHS as u8)
                .map(|w| format!("{}", line_width(kind, w)))
                .collect(),
        )
    };
    ts += "/** A line's width as a fraction of the picture's height, by width index. */\n";
    ts += &format!(
        "export const LINE_WIDTHS: number[] = [{}];\n",
        widths(ElementKind::Pen)
    );
    ts += &format!(
        "export const HIGHLIGHTER_WIDTHS: number[] = [{}];\n",
        widths(ElementKind::Highlighter)
    );
    ts += "/** A text box's size (its font's em) as a fraction of the picture's height, by width index (M27c). */\n";
    ts += &format!(
        "export const TEXT_SIZES: number[] = [{}];\n",
        list(
            (0..WIDTHS as u8)
                .map(|w| format!("{}", text_size(w)))
                .collect()
        )
    );
    ts += &format!(
        "export const TEXT_SIZE_NAMES: string[] = [{}];\n",
        list(TEXT_SIZE_NAMES.iter().map(|n| format!("\"{n}\"")).collect())
    );
    ts += "/** A text box's lines, this many ems apart. */\n";
    ts += &format!("export const LINE_HEIGHT = {LINE_HEIGHT};\n");
    for (name, value) in [
        ("MAX_TEXT_CHARS", MAX_TEXT_CHARS as u64),
        ("MAX_TEXT_LINES", MAX_TEXT_LINES as u64),
        ("MAX_STROKE_POINTS", MAX_STROKE_POINTS as u64),
        ("MAX_MEMBER_ELEMENTS", MAX_MEMBER_ELEMENTS as u64),
        ("MAX_BATCH_OPS", MAX_BATCH_OPS as u64),
        ("MAX_BATCH_ITEMS", MAX_BATCH_ITEMS as u64),
        ("MAX_LASER_POINTS", MAX_LASER_POINTS as u64),
        ("FADE_OUT_MS", FADE_OUT_MS),
        ("LASER_MS", LASER_MS),
        ("PING_MS", PING_MS),
    ] {
        ts += &format!("export const {name} = {value};\n");
    }
    ts += "\nexport type DrawTool =\n";
    ts += &list(
        DrawTool::ALL
            .iter()
            .map(|t| format!("\"{}\"", tool_name(*t)))
            .collect(),
    )
    .split(", ")
    .map(|t| format!("  | {t}\n"))
    .collect::<String>();
    ts.pop();
    ts += ";\n\n/** The toolbar's tools, in order, with their names and keys. */\n";
    ts += "export const TOOLS: { tool: DrawTool; name: string; key: string }[] = [\n";
    for tool in DrawTool::ALL {
        ts += &format!(
            "  {{ tool: \"{}\", name: \"{}\", key: \"{}\" }},\n",
            tool_name(tool),
            tool.name(),
            tool_key(tool)
        );
    }
    ts += "];\n\nexport type DrawAction =\n\
           \x20 | { type: \"undo\" }\n\
           \x20 | { type: \"redo\" }\n\
           \x20 | { type: \"copy\" }\n\
           \x20 | { type: \"paste\" }\n\
           \x20 | { type: \"duplicate\" }\n\
           \x20 | { type: \"selectAll\" }\n\
           \x20 | { type: \"delete\" }\n\
           \x20 | { type: \"nudge\"; dx: number; dy: number }\n\
           \x20 | { type: \"tool\"; tool: DrawTool }\n\
           \x20 | { type: \"leave\" }\n\
           \x20 | { type: \"toggleToolbar\" };\n\n";
    ts += "/** The drawing editor's keys (Excalidraw's), by `KeyboardEvent.code`. */\n";
    ts += "export const DRAWING_KEYS: { code: string; ctrl: boolean; shift: boolean; action: DrawAction }[] = [\n";
    for key in DRAWING_KEYS {
        ts += &format!(
            "  {{ code: \"{}\", ctrl: {}, shift: {}, action: {} }},\n",
            key.code,
            key.ctrl,
            key.shift,
            action_ts(key.action)
        );
    }
    ts += "];\n";
    ts
}

#[cfg(test)]
mod tests {
    use couch_proto::board::{BoardCleared, BoardUpdate, Refusal};
    use couch_proto::ids::SessionId;

    use super::*;

    #[test]
    fn ops_cross_the_boundary_unchanged() {
        let ops = vec![
            BoardOp::Put(Element {
                id: 3,
                kind: ElementKind::Arrow,
                colour: 4,
                width: 2,
                fades: true,
                stay: 4,
                done: true,
                points: vec![Point::new(1, 2), Point::new(65_535, 0)],
                text: String::new(),
            }),
            BoardOp::Put(Element {
                id: 4,
                kind: ElementKind::Text,
                colour: 7,
                width: 0,
                fades: false,
                stay: 2,
                done: false,
                points: vec![Point::new(1, 2), Point::new(3, 4)],
                text: "Olá\n😀".into(),
            }),
            BoardOp::Append {
                id: 3,
                points: vec![Point::new(5, 6)],
                done: false,
            },
            BoardOp::Remove(vec![1, 2]),
            BoardOp::Laser {
                points: vec![Point::new(7, 8)],
                start: true,
                colour: 6,
            },
            BoardOp::Ping(Point::new(9, 10)),
        ];
        for op in ops {
            let view = BoardOpView::from(&op);
            let json = serde_json::to_string(&view).unwrap();
            let back: BoardOpView = serde_json::from_str(&json).unwrap();
            assert_eq!(BoardOp::from(&back), op, "{json}");
        }
    }

    #[test]
    fn news_names_members_by_id_and_says_why() {
        let ana = MemberId([1; 32]);
        let news = BoardNewsView::from(&BoardNews::Update(BoardUpdate {
            session: SessionId(1),
            author: ana,
            ops: vec![BoardOp::Ping(Point::new(1, 1))],
        }));
        assert!(matches!(news, BoardNewsView::Update { author, .. } if author == ana.to_hex()));
        let cleared = BoardNewsView::from(&BoardNews::Cleared(BoardCleared {
            session: SessionId(1),
            generation: 3,
            author: None,
        }));
        assert_eq!(
            serde_json::to_string(&cleared).unwrap(),
            r#"{"type":"cleared","generation":3,"author":null}"#
        );
        let refused = BoardNewsView::from(&BoardNews::Refused(Refusal::TooFast));
        assert!(
            matches!(refused, BoardNewsView::Refused { message } if message == Refusal::TooFast.message())
        );
    }

    /// Writes `boardRules.ts` beside the bindings, as ts-rs writes its
    /// files, when it changed; fails if it can't.
    #[test]
    fn the_board_rules_are_written_for_the_front_end() {
        let rules = rules_ts();
        assert!(rules.contains("export const DRAWING_KEYS"));
        assert!(
            rules.contains(
                "{ code: \"KeyZ\", ctrl: true, shift: false, action: { type: \"undo\" } }"
            )
        );
        let Some(dir) = std::env::var_os("TS_RS_EXPORT_DIR") else {
            return;
        };
        let path = std::path::Path::new(&dir).join("boardRules.ts");
        if std::fs::read_to_string(&path).ok().as_deref() != Some(rules.as_str()) {
            std::fs::write(&path, rules).unwrap();
        }
    }
}
