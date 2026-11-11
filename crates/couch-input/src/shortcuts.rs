//! Every keyboard shortcut the app reacts to, in one table. The viewer and
//! the host's panic-key hook read their keys from here, and the app lists
//! them, so a shortcut is never described in one place and bound in another.
//!
//! Shortcuts are physical keys (HID usages), like forwarded keys. The chord
//! ones hold Ctrl, Alt and Shift (either side of each); those are never
//! forwarded to the host.
//!
//! [`SHORTCUTS`] holds the defaults. The user may move each row to another
//! key (never its modifiers): [`Shortcuts`] is the table in use, the defaults
//! with the user's keys laid over them. Only keys from [`KEYS`] are allowed,
//! no two rows share one, and the panic key can be moved but never removed.

use std::collections::BTreeMap;

use crate::HidUsage;

/// Something a shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shortcut {
    /// Viewer: show or hide the overlay.
    ToggleHud,
    /// Viewer: free the mouse, or capture it again.
    FreeMouse,
    /// Viewer: ask for the controller, withdraw the request, or give it back.
    ToggleControl,
    /// Viewer: full screen, or back to a window.
    Fullscreen,
    /// Viewer: the stream quieter (P6).
    VolumeDown,
    /// Viewer: the stream louder, up to as the host sent it (P6).
    VolumeUp,
    /// Viewer: mute the stream, or unmute it (P6).
    Mute,
    /// Viewer: drawing mode on or off (M27a): the mouse and keys go to the
    /// board instead of the game.
    ToggleDrawing,
    /// Viewer: show or hide the drawings, for this window alone (M27a).
    ToggleBoard,
    /// Host: take the controller back from everyone and pause control.
    Panic,
    /// Host: accept the oldest request for a seat.
    AcceptRequest,
    /// Host: decline the oldest request for a seat.
    DeclineRequest,
    /// Host: drawing over the game on or off (M27b, Windows): the board
    /// overlay takes the mouse and the focus, then gives the game back.
    HostDrawing,
}

impl Shortcut {
    /// The host's shortcuts, which the host-keys hook watches for while
    /// streaming (the panic key first).
    pub const HOST: [Self; 3] = [Self::Panic, Self::AcceptRequest, Self::DeclineRequest];

    /// Two rows that may share a key: the same thing in two places. The
    /// host's drawing key and the stream window's both mean "draw" (M27b):
    /// one works anywhere while hosting, the other in a stream window.
    fn same_meaning(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::HostDrawing, Self::ToggleDrawing) | (Self::ToggleDrawing, Self::HostDrawing)
        )
    }
}

/// Where a shortcut works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// In the stream window, while it has the focus.
    Viewer,
    /// Anywhere on the host's PC while streaming, even inside the game.
    Host,
}

/// One row of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// Stable, for saving the user's key: never renamed.
    pub id: &'static str,
    pub shortcut: Shortcut,
    /// Ctrl, Alt and Shift must be held.
    pub chord: bool,
    pub key: HidUsage,
    pub action: &'static str,
    pub scope: Scope,
    /// When it applies, if not always; empty otherwise.
    pub condition: &'static str,
}

impl Binding {
    /// The key as printed on a US keyboard, such as "Backspace".
    pub fn key_name(&self) -> &'static str {
        key(self.key).map_or("?", |k| k.name)
    }

    /// The keys to press, such as "Ctrl+Alt+Shift+H".
    pub fn keys(&self) -> String {
        if self.chord {
            format!("Ctrl+Alt+Shift+{}", self.key_name())
        } else {
            self.key_name().to_string()
        }
    }
}

/// A key a shortcut may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub usage: HidUsage,
    /// As printed on a US keyboard.
    pub name: &'static str,
    /// The browser's `KeyboardEvent.code`, also a physical key.
    pub code: &'static str,
    /// Allowed on a row that works without the chord: only F-keys, which
    /// nothing types.
    pub alone: bool,
}

const fn k(usage: u16, name: &'static str, code: &'static str) -> Key {
    Key {
        usage: HidUsage(usage),
        name,
        code,
        alone: false,
    }
}

const fn f(usage: u16, name: &'static str) -> Key {
    Key {
        usage: HidUsage(usage),
        name,
        code: name,
        alone: true,
    }
}

/// Every key a shortcut may use. Left out on purpose:
/// - Esc (Ctrl+Shift+Esc is Task Manager), Delete (Ctrl+Alt+Del), Tab;
/// - F4 (Alt+F4 closes the stream window while keys aren't sent);
/// - the gray navigation keys (arrows, Insert, Home, End, Page Up and Down):
///   depending on NumLock, keyboards wrap them in "fake Shift" key-ups and
///   key-downs, which the panic-key hook can't tell from the real Shift, so
///   a chord on one of them could never fire;
/// - Enter, Space, the modifiers, the Windows keys, Pause, Print Screen and
///   the numpad.
///
/// Every key here has a plain, non-extended scancode, which the panic-key
/// hook needs (`couch_platform_win::host_keys`, tested there).
pub const KEYS: &[Key] = &[
    k(0x04, "A", "KeyA"),
    k(0x05, "B", "KeyB"),
    k(0x06, "C", "KeyC"),
    k(0x07, "D", "KeyD"),
    k(0x08, "E", "KeyE"),
    k(0x09, "F", "KeyF"),
    k(0x0A, "G", "KeyG"),
    k(0x0B, "H", "KeyH"),
    k(0x0C, "I", "KeyI"),
    k(0x0D, "J", "KeyJ"),
    k(0x0E, "K", "KeyK"),
    k(0x0F, "L", "KeyL"),
    k(0x10, "M", "KeyM"),
    k(0x11, "N", "KeyN"),
    k(0x12, "O", "KeyO"),
    k(0x13, "P", "KeyP"),
    k(0x14, "Q", "KeyQ"),
    k(0x15, "R", "KeyR"),
    k(0x16, "S", "KeyS"),
    k(0x17, "T", "KeyT"),
    k(0x18, "U", "KeyU"),
    k(0x19, "V", "KeyV"),
    k(0x1A, "W", "KeyW"),
    k(0x1B, "X", "KeyX"),
    k(0x1C, "Y", "KeyY"),
    k(0x1D, "Z", "KeyZ"),
    k(0x1E, "1", "Digit1"),
    k(0x1F, "2", "Digit2"),
    k(0x20, "3", "Digit3"),
    k(0x21, "4", "Digit4"),
    k(0x22, "5", "Digit5"),
    k(0x23, "6", "Digit6"),
    k(0x24, "7", "Digit7"),
    k(0x25, "8", "Digit8"),
    k(0x26, "9", "Digit9"),
    k(0x27, "0", "Digit0"),
    k(0x2A, "Backspace", "Backspace"),
    k(0x2D, "-", "Minus"),
    k(0x2E, "=", "Equal"),
    k(0x2F, "[", "BracketLeft"),
    k(0x30, "]", "BracketRight"),
    k(0x31, "\\", "Backslash"),
    k(0x33, ";", "Semicolon"),
    k(0x34, "'", "Quote"),
    k(0x35, "`", "Backquote"),
    k(0x36, ",", "Comma"),
    k(0x37, ".", "Period"),
    k(0x38, "/", "Slash"),
    f(0x3A, "F1"),
    f(0x3B, "F2"),
    f(0x3C, "F3"),
    f(0x3E, "F5"),
    f(0x3F, "F6"),
    f(0x40, "F7"),
    f(0x41, "F8"),
    f(0x42, "F9"),
    f(0x43, "F10"),
    f(0x44, "F11"),
    f(0x45, "F12"),
];

/// The allowed key with this usage.
pub fn key(usage: HidUsage) -> Option<&'static Key> {
    KEYS.iter().find(|k| k.usage == usage)
}

/// The allowed key with this browser code (`KeyboardEvent.code`).
pub fn key_for_code(code: &str) -> Option<&'static Key> {
    KEYS.iter().find(|k| k.code == code)
}

const KEY_0: HidUsage = HidUsage(0x27);
const KEY_MINUS: HidUsage = HidUsage(0x2D);
const KEY_EQUAL: HidUsage = HidUsage(0x2E);
const KEY_B: HidUsage = HidUsage(0x05);
const KEY_C: HidUsage = HidUsage(0x06);
const KEY_D: HidUsage = HidUsage(0x07);
const KEY_F: HidUsage = HidUsage(0x09);
const KEY_H: HidUsage = HidUsage(0x0B);
const KEY_M: HidUsage = HidUsage(0x10);
const KEY_N: HidUsage = HidUsage(0x11);
const KEY_Y: HidUsage = HidUsage(0x1C);
const KEY_BACKSPACE: HidUsage = HidUsage(0x2A);
const KEY_F3: HidUsage = HidUsage(0x3C);

/// The defaults, in the order the app lists them.
pub const SHORTCUTS: &[Binding] = &[
    Binding {
        id: "panic",
        shortcut: Shortcut::Panic,
        chord: true,
        key: KEY_BACKSPACE,
        action: "Take the controller back from everyone and pause control",
        scope: Scope::Host,
        condition: "while you stream; the game never sees it",
    },
    Binding {
        id: "acceptRequest",
        shortcut: Shortcut::AcceptRequest,
        chord: true,
        key: KEY_Y,
        action: "Accept the oldest request for a seat",
        scope: Scope::Host,
        condition: "while you stream and someone asks; the game never sees it",
    },
    Binding {
        id: "declineRequest",
        shortcut: Shortcut::DeclineRequest,
        chord: true,
        key: KEY_N,
        action: "Decline the oldest request for a seat",
        scope: Scope::Host,
        condition: "while you stream and someone asks; the game never sees it",
    },
    Binding {
        id: "hostDrawing",
        shortcut: Shortcut::HostDrawing,
        chord: true,
        key: KEY_D,
        action: "Draw over the game, or back to the game",
        scope: Scope::Host,
        condition: "while you stream on Windows with drawing on; the game never sees it",
    },
    Binding {
        id: "toggleControl",
        shortcut: Shortcut::ToggleControl,
        chord: true,
        key: KEY_C,
        action: "Ask for the controller, withdraw the request, or give it back",
        scope: Scope::Viewer,
        condition: "",
    },
    Binding {
        id: "freeMouse",
        shortcut: Shortcut::FreeMouse,
        chord: true,
        key: KEY_M,
        action: "Free the mouse (with desktop control, the keyboard too), or capture it again",
        scope: Scope::Viewer,
        condition: "while you may use the mouse",
    },
    Binding {
        id: "fullscreen",
        shortcut: Shortcut::Fullscreen,
        chord: true,
        key: KEY_F,
        action: "Full screen, or back to a window",
        scope: Scope::Viewer,
        condition: "",
    },
    Binding {
        id: "volumeDown",
        shortcut: Shortcut::VolumeDown,
        chord: true,
        key: KEY_MINUS,
        action: "Turn the stream down",
        scope: Scope::Viewer,
        condition: "",
    },
    Binding {
        id: "volumeUp",
        shortcut: Shortcut::VolumeUp,
        chord: true,
        key: KEY_EQUAL,
        action: "Turn the stream up, to as loud as the host sends it",
        scope: Scope::Viewer,
        condition: "",
    },
    Binding {
        id: "mute",
        shortcut: Shortcut::Mute,
        chord: true,
        key: KEY_0,
        action: "Mute the stream, or unmute it",
        scope: Scope::Viewer,
        condition: "",
    },
    Binding {
        id: "toggleDrawing",
        shortcut: Shortcut::ToggleDrawing,
        chord: true,
        key: KEY_D,
        action: "Draw on the stream, or back to the game",
        scope: Scope::Viewer,
        condition: "while the host lets you draw, and you don't have the keyboard and mouse",
    },
    Binding {
        id: "toggleBoard",
        shortcut: Shortcut::ToggleBoard,
        chord: true,
        key: KEY_B,
        action: "Show or hide the drawings",
        scope: Scope::Viewer,
        condition: "",
    },
    Binding {
        id: "toggleHud",
        shortcut: Shortcut::ToggleHud,
        chord: true,
        key: KEY_H,
        action: "Show or hide the overlay",
        scope: Scope::Viewer,
        condition: "",
    },
    Binding {
        id: "toggleHudAlone",
        shortcut: Shortcut::ToggleHud,
        chord: false,
        key: KEY_F3,
        action: "Show or hide the overlay",
        scope: Scope::Viewer,
        condition: "while you aren't sending keys to the game",
    },
];

// ---- The drawing editor's keys (M27a) ----

/// A drawing tool (M27a; text boxes M27c).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DrawTool {
    Select,
    Pen,
    Highlighter,
    Laser,
    Arrow,
    Line,
    Rectangle,
    Ellipse,
    Text,
    Eraser,
}

impl DrawTool {
    /// In the toolbar's order.
    pub const ALL: [Self; 10] = [
        Self::Select,
        Self::Pen,
        Self::Highlighter,
        Self::Laser,
        Self::Arrow,
        Self::Line,
        Self::Rectangle,
        Self::Ellipse,
        Self::Text,
        Self::Eraser,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Pen => "Pen",
            Self::Highlighter => "Highlighter",
            Self::Laser => "Laser pointer",
            Self::Arrow => "Arrow",
            Self::Line => "Line",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Text => "Text",
            Self::Eraser => "Eraser",
        }
    }
}

/// What a drawing key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawAction {
    Undo,
    Redo,
    Copy,
    Paste,
    Duplicate,
    SelectAll,
    Delete,
    /// Moves the selection by a step (Shift: further), by direction.
    Nudge(i8, i8),
    Tool(DrawTool),
    /// Leaves the tool, then drawing mode.
    Leave,
    /// Hides the toolbar to a small pill, or shows it again: the window's
    /// own, not the editor's (M27c, the author's request).
    ToggleToolbar,
}

/// One of the drawing editor's keys. They are each editor's own (the stream
/// window's and, from M27b, the host's board overlay's), like a text box's:
/// fixed, listed in Settings but never moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrawingKey {
    pub ctrl: bool,
    pub shift: bool,
    pub key: HidUsage,
    /// As printed on a US keyboard.
    pub name: &'static str,
    /// The browser's `KeyboardEvent.code`.
    pub code: &'static str,
    pub action: DrawAction,
    /// What the app lists for it; empty for a row listed with another.
    pub description: &'static str,
}

impl DrawingKey {
    /// The keys to press, such as "Ctrl+Shift+Z".
    pub fn keys(&self) -> String {
        let mut keys = String::new();
        if self.ctrl {
            keys.push_str("Ctrl+");
        }
        if self.shift {
            keys.push_str("Shift+");
        }
        keys.push_str(self.name);
        keys
    }
}

const fn dk(
    ctrl: bool,
    shift: bool,
    usage: u16,
    name: &'static str,
    code: &'static str,
    action: DrawAction,
    description: &'static str,
) -> DrawingKey {
    DrawingKey {
        ctrl,
        shift,
        key: HidUsage(usage),
        name,
        code,
        action,
        description,
    }
}

/// Every drawing key (Excalidraw's), in the order the app lists them.
pub const DRAWING_KEYS: &[DrawingKey] = &[
    dk(
        true,
        false,
        0x1D,
        "Z",
        "KeyZ",
        DrawAction::Undo,
        "Undo what you drew last",
    ),
    dk(true, true, 0x1D, "Z", "KeyZ", DrawAction::Redo, "Redo"),
    dk(true, false, 0x1C, "Y", "KeyY", DrawAction::Redo, ""),
    dk(
        true,
        false,
        0x06,
        "C",
        "KeyC",
        DrawAction::Copy,
        "Copy what you selected",
    ),
    dk(
        true,
        false,
        0x19,
        "V",
        "KeyV",
        DrawAction::Paste,
        "Paste it",
    ),
    dk(
        true,
        false,
        0x07,
        "D",
        "KeyD",
        DrawAction::Duplicate,
        "Duplicate what you selected",
    ),
    dk(
        true,
        false,
        0x04,
        "A",
        "KeyA",
        DrawAction::SelectAll,
        "Select all you drew",
    ),
    dk(
        false,
        false,
        0x4C,
        "Delete",
        "Delete",
        DrawAction::Delete,
        "Delete what you selected",
    ),
    dk(
        false,
        false,
        0x2A,
        "Backspace",
        "Backspace",
        DrawAction::Delete,
        "",
    ),
    dk(
        false,
        false,
        0x52,
        "Up",
        "ArrowUp",
        DrawAction::Nudge(0, -1),
        "Nudge what you selected (Shift: further)",
    ),
    dk(
        false,
        false,
        0x51,
        "Down",
        "ArrowDown",
        DrawAction::Nudge(0, 1),
        "",
    ),
    dk(
        false,
        false,
        0x50,
        "Left",
        "ArrowLeft",
        DrawAction::Nudge(-1, 0),
        "",
    ),
    dk(
        false,
        false,
        0x4F,
        "Right",
        "ArrowRight",
        DrawAction::Nudge(1, 0),
        "",
    ),
    dk(
        false,
        false,
        0x19,
        "V",
        "KeyV",
        DrawAction::Tool(DrawTool::Select),
        "Select",
    ),
    dk(
        false,
        false,
        0x13,
        "P",
        "KeyP",
        DrawAction::Tool(DrawTool::Pen),
        "Pen",
    ),
    dk(
        false,
        false,
        0x0B,
        "H",
        "KeyH",
        DrawAction::Tool(DrawTool::Highlighter),
        "Highlighter",
    ),
    dk(
        false,
        false,
        0x0E,
        "K",
        "KeyK",
        DrawAction::Tool(DrawTool::Laser),
        "Laser pointer",
    ),
    dk(
        false,
        false,
        0x04,
        "A",
        "KeyA",
        DrawAction::Tool(DrawTool::Arrow),
        "Arrow",
    ),
    dk(
        false,
        false,
        0x0F,
        "L",
        "KeyL",
        DrawAction::Tool(DrawTool::Line),
        "Line",
    ),
    dk(
        false,
        false,
        0x15,
        "R",
        "KeyR",
        DrawAction::Tool(DrawTool::Rectangle),
        "Rectangle",
    ),
    dk(
        false,
        false,
        0x12,
        "O",
        "KeyO",
        DrawAction::Tool(DrawTool::Ellipse),
        "Ellipse",
    ),
    dk(
        false,
        false,
        0x17,
        "T",
        "KeyT",
        DrawAction::Tool(DrawTool::Text),
        "Text",
    ),
    dk(
        false,
        false,
        0x08,
        "E",
        "KeyE",
        DrawAction::Tool(DrawTool::Eraser),
        "Eraser",
    ),
    dk(
        false,
        false,
        0x29,
        "Esc",
        "Escape",
        DrawAction::Leave,
        "Leave the tool, then drawing",
    ),
    dk(
        false,
        false,
        0x2B,
        "Tab",
        "Tab",
        DrawAction::ToggleToolbar,
        "Hide or show the drawing tools",
    ),
];

/// What a key does while drawing, if anything: Ctrl and Shift as held (an
/// arrow nudges whether Shift is held or not; Alt is never part of one).
pub fn drawing_key(usage: HidUsage, ctrl: bool, shift: bool) -> Option<DrawAction> {
    let exact = DRAWING_KEYS
        .iter()
        .find(|k| k.key == usage && k.ctrl == ctrl && k.shift == shift);
    let nudge = || {
        DRAWING_KEYS
            .iter()
            .find(|k| k.key == usage && !ctrl && matches!(k.action, DrawAction::Nudge(..)))
    };
    exact.or_else(nudge).map(|k| k.action)
}

/// The tool a key picks, for the toolbar's tooltips ("Pen (P)").
pub fn tool_key(tool: DrawTool) -> &'static str {
    DRAWING_KEYS
        .iter()
        .find(|k| k.action == DrawAction::Tool(tool))
        .map_or("", |k| k.name)
}

/// Why a key can't go on a row.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShortcutError {
    #[error("there is no shortcut {0:?}")]
    Unknown(String),
    #[error("that key can't be used for a shortcut")]
    KeyNotAllowed,
    #[error("only F1 to F12 (but F4) work without Ctrl+Alt+Shift")]
    NotAnFKey,
    #[error("{keys} already does this: {action}")]
    Taken { keys: String, action: &'static str },
}

/// The table in use: the defaults, with the user's keys laid over them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcuts {
    rows: Vec<Binding>,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            rows: SHORTCUTS.to_vec(),
        }
    }
}

impl Shortcuts {
    /// The defaults with `keys` (row id to HID usage, as [`Self::changed`]
    /// gives them) laid over them. Entries that don't fit (an unknown row, a
    /// key not allowed, a clash) are left out and returned, so the caller
    /// can log them; those rows keep their default keys.
    pub fn with_keys(keys: &BTreeMap<String, u16>) -> (Self, Vec<(String, ShortcutError)>) {
        let mut skipped = Vec::new();
        // First the entries wrong on their own: an unknown row, a key not
        // allowed there.
        let mut fitting = Vec::new();
        for (id, &usage) in keys {
            match SHORTCUTS.iter().find(|r| r.id == id) {
                None => skipped.push((id.clone(), ShortcutError::Unknown(id.clone()))),
                Some(row) => match Self::allowed(row, HidUsage(usage)) {
                    Ok(()) => fitting.push((id.as_str(), HidUsage(usage))),
                    Err(e) => skipped.push((id.clone(), e)),
                },
            }
        }
        // The rest all at once: a valid set may still clash halfway through
        // (two rows that swapped keys).
        let mut all = Self::default();
        for &(id, usage) in &fitting {
            if let Some(row) = all.rows.iter_mut().find(|r| r.id == id) {
                row.key = usage;
            }
        }
        if all.valid() {
            return (all, skipped);
        }
        // Clashes: one at a time, each one that still fits.
        let mut table = Self::default();
        for (id, usage) in fitting {
            if let Err(e) = table.set(id, usage) {
                skipped.push((id.to_string(), e));
            }
        }
        (table, skipped)
    }

    /// Every row is allowed its key, and no two rows share one.
    fn valid(&self) -> bool {
        self.rows.iter().enumerate().all(|(i, row)| {
            Self::allowed(row, row.key).is_ok()
                && self.rows[..i]
                    .iter()
                    .all(|other| !Self::clash(other, row, row.key))
        })
    }

    fn allowed(row: &Binding, usage: HidUsage) -> Result<(), ShortcutError> {
        let key = key(usage).ok_or(ShortcutError::KeyNotAllowed)?;
        if !row.chord && !key.alone {
            return Err(ShortcutError::NotAnFKey);
        }
        Ok(())
    }

    /// Two rows clash when they would answer the same key press, unless
    /// they mean the same thing ([`Shortcut::same_meaning`]): `other` on
    /// `usage` against `row`.
    fn clash(other: &Binding, row: &Binding, usage: HidUsage) -> bool {
        other.chord == row.chord && other.key == usage && !other.shortcut.same_meaning(row.shortcut)
    }

    /// Moves row `id` to `usage`.
    pub fn set(&mut self, id: &str, usage: HidUsage) -> Result<(), ShortcutError> {
        let i = self
            .rows
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| ShortcutError::Unknown(id.to_string()))?;
        let row = self.rows[i];
        Self::allowed(&row, usage)?;
        if let Some(other) = self
            .rows
            .iter()
            .enumerate()
            .find(|&(j, other)| j != i && Self::clash(other, &row, usage))
            .map(|(_, other)| other)
        {
            return Err(ShortcutError::Taken {
                keys: other.keys(),
                action: other.action,
            });
        }
        self.rows[i].key = usage;
        Ok(())
    }

    /// Puts row `id` back on its default key.
    pub fn reset(&mut self, id: &str) -> Result<(), ShortcutError> {
        let default = SHORTCUTS
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| ShortcutError::Unknown(id.to_string()))?;
        self.set(id, default.key)
    }

    /// The rows, in the order the app lists them.
    pub fn rows(&self) -> &[Binding] {
        &self.rows
    }

    /// Whether row `id` is on its default key.
    pub fn is_default(&self, id: &str) -> bool {
        self.rows
            .iter()
            .zip(SHORTCUTS)
            .any(|(row, default)| row.id == id && row.key == default.key)
    }

    /// The rows moved off their defaults, for saving.
    pub fn changed(&self) -> BTreeMap<String, u16> {
        self.rows
            .iter()
            .zip(SHORTCUTS)
            .filter(|(row, default)| row.key != default.key)
            .map(|(row, _)| (row.id.to_string(), row.key.0))
            .collect()
    }

    /// The viewer's shortcut for `key`, pressed with the chord held or not.
    /// Conditions are the caller's to check.
    pub fn viewer_shortcut(&self, key: HidUsage, chord: bool) -> Option<Shortcut> {
        self.rows
            .iter()
            .find(|b| b.scope == Scope::Viewer && b.key == key && b.chord == chord)
            .map(|b| b.shortcut)
    }

    /// The (first) row for `shortcut`.
    pub fn binding(&self, shortcut: Shortcut) -> &Binding {
        self.rows
            .iter()
            .find(|b| b.shortcut == shortcut)
            .expect("every shortcut is in the table")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn the_defaults_are_valid_and_ids_unique() {
        assert!(Shortcuts::default().valid());
        let ids: HashSet<_> = SHORTCUTS.iter().map(|b| b.id).collect();
        assert_eq!(ids.len(), SHORTCUTS.len());
    }

    #[test]
    fn every_shortcut_has_a_row() {
        let table = Shortcuts::default();
        for s in [
            Shortcut::ToggleHud,
            Shortcut::FreeMouse,
            Shortcut::ToggleControl,
            Shortcut::Fullscreen,
            Shortcut::VolumeDown,
            Shortcut::VolumeUp,
            Shortcut::Mute,
            Shortcut::ToggleDrawing,
            Shortcut::ToggleBoard,
            Shortcut::Panic,
            Shortcut::AcceptRequest,
            Shortcut::DeclineRequest,
            Shortcut::HostDrawing,
        ] {
            assert_eq!(table.binding(s).shortcut, s);
        }
    }

    #[test]
    fn the_two_drawing_keys_may_share_a_key_and_nothing_else_may() {
        let mut table = Shortcuts::default();
        let host = *table.binding(Shortcut::HostDrawing);
        assert_eq!(host.keys(), "Ctrl+Alt+Shift+D");
        assert_eq!(host.scope, Scope::Host);
        assert_eq!(table.binding(Shortcut::ToggleDrawing).key, host.key);
        // Each moves alone, and back onto the other's key.
        table.set("hostDrawing", HidUsage(0x0A)).unwrap(); // G
        assert_eq!(table.binding(Shortcut::ToggleDrawing).key, KEY_D);
        table.set("toggleDrawing", HidUsage(0x0A)).unwrap();
        assert_eq!(table.changed().len(), 2);
        // Any other row on a drawing key is refused.
        assert!(matches!(
            table.set("mute", HidUsage(0x0A)),
            Err(ShortcutError::Taken { .. })
        ));
        assert!(matches!(
            table.set("hostDrawing", KEY_B),
            Err(ShortcutError::Taken { .. })
        ));
        let (loaded, skipped) = Shortcuts::with_keys(&table.changed());
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(loaded, table);
    }

    #[test]
    fn the_hosts_keys_are_chords_and_never_the_viewers() {
        let table = Shortcuts::default();
        for s in Shortcut::HOST {
            let b = table.binding(s);
            assert_eq!(b.scope, Scope::Host, "{}", b.id);
            assert!(b.chord, "{}", b.id);
            assert_eq!(table.viewer_shortcut(b.key, true), None, "{}", b.id);
        }
        assert_eq!(
            table.binding(Shortcut::AcceptRequest).keys(),
            "Ctrl+Alt+Shift+Y"
        );
        assert_eq!(
            table.binding(Shortcut::DeclineRequest).keys(),
            "Ctrl+Alt+Shift+N"
        );
    }

    #[test]
    fn unchorded_rows_are_only_the_viewers_and_conditional() {
        // Only F3 goes without the chord, and only while keys aren't sent.
        for b in SHORTCUTS.iter().filter(|b| !b.chord) {
            assert_eq!(b.scope, Scope::Viewer);
            assert!(!b.condition.is_empty());
        }
    }

    #[test]
    fn the_panic_key_is_the_hosts() {
        let table = Shortcuts::default();
        let panic = table.binding(Shortcut::Panic);
        assert_eq!(panic.keys(), "Ctrl+Alt+Shift+Backspace");
        assert_eq!(panic.scope, Scope::Host);
        assert_eq!(table.viewer_shortcut(panic.key, true), None);
    }

    #[test]
    fn drawing_keys_are_excalidraws() {
        use DrawAction::*;
        assert_eq!(drawing_key(HidUsage(0x1D), true, false), Some(Undo));
        assert_eq!(drawing_key(HidUsage(0x1D), true, true), Some(Redo));
        assert_eq!(drawing_key(HidUsage(0x1C), true, false), Some(Redo));
        assert_eq!(drawing_key(HidUsage(0x19), true, false), Some(Paste));
        assert_eq!(
            drawing_key(HidUsage(0x19), false, false),
            Some(Tool(DrawTool::Select))
        );
        assert_eq!(
            drawing_key(HidUsage(0x04), false, false),
            Some(Tool(DrawTool::Arrow))
        );
        assert_eq!(drawing_key(HidUsage(0x04), true, false), Some(SelectAll));
        // An arrow nudges with Shift too: further.
        assert_eq!(drawing_key(HidUsage(0x50), false, true), Some(Nudge(-1, 0)));
        assert_eq!(drawing_key(HidUsage(0x1D), false, false), None);
        // Each key and modifiers once; every tool has its key.
        for (i, a) in DRAWING_KEYS.iter().enumerate() {
            for b in &DRAWING_KEYS[..i] {
                assert!(
                    (a.key, a.ctrl, a.shift) != (b.key, b.ctrl, b.shift),
                    "{}",
                    a.keys()
                );
            }
        }
        for tool in DrawTool::ALL {
            assert!(!tool_key(tool).is_empty(), "{tool:?}");
        }
        // None is a chord the window's own shortcuts use.
        assert!(
            DRAWING_KEYS
                .iter()
                .all(|k| !(k.ctrl && k.shift && k.key == KEY_D))
        );
    }

    #[test]
    fn viewer_lookup() {
        let table = Shortcuts::default();
        assert_eq!(
            table.viewer_shortcut(KEY_H, true),
            Some(Shortcut::ToggleHud)
        );
        assert_eq!(
            table.viewer_shortcut(KEY_F3, false),
            Some(Shortcut::ToggleHud)
        );
        assert_eq!(table.viewer_shortcut(KEY_H, false), None);
        assert_eq!(
            table.viewer_shortcut(KEY_C, true),
            Some(Shortcut::ToggleControl)
        );
    }

    #[test]
    fn keys_are_unique_and_codes_round_trip() {
        let usages: HashSet<_> = KEYS.iter().map(|k| k.usage).collect();
        let codes: HashSet<_> = KEYS.iter().map(|k| k.code).collect();
        assert_eq!(usages.len(), KEYS.len());
        assert_eq!(codes.len(), KEYS.len());
        for k in KEYS {
            assert_eq!(key_for_code(k.code), Some(k));
            assert_eq!(key(k.usage), Some(k));
        }
        for b in SHORTCUTS {
            assert!(key(b.key).is_some(), "{} uses a key not allowed", b.id);
        }
    }

    #[test]
    fn risky_keys_are_refused() {
        let mut table = Shortcuts::default();
        for usage in [
            0x29, // Esc
            0x2B, // Tab
            0x4C, // Delete
            0x3D, // F4
            0x28, // Enter
            0x2C, // Space
            0xE0, // Left Ctrl
            0xE3, // Left Windows
            0x48, // Pause
            0x46, // Print Screen
            0x58, // Numpad Enter
            0x49, // Insert, and the other gray navigation keys
            0x4A, // Home
            0x4B, // Page Up
            0x4D, // End
            0x4E, // Page Down
            0x4F, // Right
            0x50, // Left
            0x51, // Down
            0x52, // Up
        ] {
            assert_eq!(
                table.set("panic", HidUsage(usage)),
                Err(ShortcutError::KeyNotAllowed),
                "{usage:#x}"
            );
        }
        assert!(table.is_default("panic"));
    }

    #[test]
    fn moving_a_key() {
        let mut table = Shortcuts::default();
        table.set("panic", HidUsage(0x13)).unwrap(); // P
        assert_eq!(table.binding(Shortcut::Panic).keys(), "Ctrl+Alt+Shift+P");
        assert!(!table.is_default("panic"));
        assert_eq!(
            table.changed(),
            BTreeMap::from([("panic".to_string(), 0x13)])
        );
        table.reset("panic").unwrap();
        assert!(table.changed().is_empty());
    }

    #[test]
    fn two_rows_never_share_a_key() {
        let mut table = Shortcuts::default();
        // The panic key onto the viewer's C: on one PC its hook would
        // swallow that chord.
        assert_eq!(
            table.set("panic", KEY_C),
            Err(ShortcutError::Taken {
                keys: "Ctrl+Alt+Shift+C".into(),
                action: "Ask for the controller, withdraw the request, or give it back",
            })
        );
        // F3 alone and Ctrl+Alt+Shift+F3 are different presses.
        table.set("fullscreen", KEY_F3).unwrap();
        assert!(table.set("toggleHudAlone", KEY_F3).is_ok());
        assert_eq!(
            table.viewer_shortcut(KEY_F3, true),
            Some(Shortcut::Fullscreen)
        );
        // Setting a row to its own key is fine.
        table.set("toggleHud", KEY_H).unwrap();
    }

    #[test]
    fn the_unchorded_row_takes_only_f_keys() {
        let mut table = Shortcuts::default();
        assert_eq!(
            table.set("toggleHudAlone", KEY_H),
            Err(ShortcutError::NotAnFKey)
        );
        table.set("toggleHudAlone", HidUsage(0x3B)).unwrap(); // F2
        assert_eq!(
            table.viewer_shortcut(HidUsage(0x3B), false),
            Some(Shortcut::ToggleHud)
        );
        assert_eq!(table.viewer_shortcut(KEY_F3, false), None);
    }

    #[test]
    fn saved_keys_are_laid_over_the_defaults() {
        // A swap is valid as a whole, though not one row at a time.
        let swapped = BTreeMap::from([
            ("toggleHud".to_string(), KEY_F.0),
            ("fullscreen".to_string(), KEY_H.0),
        ]);
        let (table, skipped) = Shortcuts::with_keys(&swapped);
        assert!(skipped.is_empty());
        assert_eq!(
            table.viewer_shortcut(KEY_F, true),
            Some(Shortcut::ToggleHud)
        );
        assert_eq!(table.changed(), swapped);

        // What doesn't fit is left out and reported; the rest applies.
        let messy = BTreeMap::from([
            ("gone".to_string(), 0x13),
            ("panic".to_string(), 0x29),
            ("toggleControl".to_string(), KEY_M.0),
            ("fullscreen".to_string(), 0x0D),
        ]);
        let (table, skipped) = Shortcuts::with_keys(&messy);
        assert_eq!(table.binding(Shortcut::Fullscreen).key, HidUsage(0x0D));
        assert!(table.is_default("panic"));
        assert!(table.is_default("toggleControl"));
        assert_eq!(skipped.len(), 3);
        assert!(table.valid());

        // One bad entry doesn't cost a swap its other half.
        let mut swap_and_bad = swapped.clone();
        swap_and_bad.insert("panic".to_string(), 0x29); // Esc
        let (table, skipped) = Shortcuts::with_keys(&swap_and_bad);
        assert_eq!(table.changed(), swapped);
        assert_eq!(
            skipped,
            vec![("panic".to_string(), ShortcutError::KeyNotAllowed)]
        );
    }
}
