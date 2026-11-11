// Written by couch-engine's tests from couch_proto::board and
// couch_input::shortcuts (M27b): the board's rules, one source for the
// stream window's editor and the board overlay's. Don't edit.

export const PALETTE: [number, number, number][] = [[255, 77, 79], [255, 159, 26], [255, 214, 10], [52, 199, 89], [50, 212, 245], [61, 123, 255], [168, 85, 247], [255, 255, 255]];
export const COLOUR_NAMES: string[] = ["Red", "Orange", "Yellow", "Green", "Cyan", "Blue", "Violet", "White"];
/** How long a fading element may stay before it fades, in seconds (its author's pick), and the default. */
export const FADE_STAYS: number[] = [2, 4, 8, 15];
export const DEFAULT_STAY = 4;
export const WIDTH_NAMES: string[] = ["Thin", "Medium", "Bold"];
/** A line's width as a fraction of the picture's height, by width index. */
export const LINE_WIDTHS: number[] = [0.003, 0.006, 0.012];
export const HIGHLIGHTER_WIDTHS: number[] = [0.012, 0.024, 0.048];
/** A text box's size (its font's em) as a fraction of the picture's height, by width index (M27c). */
export const TEXT_SIZES: number[] = [0.025, 0.04, 0.065];
export const TEXT_SIZE_NAMES: string[] = ["Small", "Medium", "Large"];
/** A text box's lines, this many ems apart. */
export const LINE_HEIGHT = 1.25;
export const MAX_TEXT_CHARS = 500;
export const MAX_TEXT_LINES = 50;
export const MAX_STROKE_POINTS = 2000;
export const MAX_MEMBER_ELEMENTS = 300;
export const MAX_BATCH_OPS = 64;
export const MAX_BATCH_ITEMS = 4000;
export const MAX_LASER_POINTS = 64;
export const FADE_OUT_MS = 1000;
export const LASER_MS = 1000;
export const PING_MS = 2000;

export type DrawTool =
  | "select"
  | "pen"
  | "highlighter"
  | "laser"
  | "arrow"
  | "line"
  | "rectangle"
  | "ellipse"
  | "text"
  | "eraser";

/** The toolbar's tools, in order, with their names and keys. */
export const TOOLS: { tool: DrawTool; name: string; key: string }[] = [
  { tool: "select", name: "Select", key: "V" },
  { tool: "pen", name: "Pen", key: "P" },
  { tool: "highlighter", name: "Highlighter", key: "H" },
  { tool: "laser", name: "Laser pointer", key: "K" },
  { tool: "arrow", name: "Arrow", key: "A" },
  { tool: "line", name: "Line", key: "L" },
  { tool: "rectangle", name: "Rectangle", key: "R" },
  { tool: "ellipse", name: "Ellipse", key: "O" },
  { tool: "text", name: "Text", key: "T" },
  { tool: "eraser", name: "Eraser", key: "E" },
];

export type DrawAction =
  | { type: "undo" }
  | { type: "redo" }
  | { type: "copy" }
  | { type: "paste" }
  | { type: "duplicate" }
  | { type: "selectAll" }
  | { type: "delete" }
  | { type: "nudge"; dx: number; dy: number }
  | { type: "tool"; tool: DrawTool }
  | { type: "leave" }
  | { type: "toggleToolbar" };

/** The drawing editor's keys (Excalidraw's), by `KeyboardEvent.code`. */
export const DRAWING_KEYS: { code: string; ctrl: boolean; shift: boolean; action: DrawAction }[] = [
  { code: "KeyZ", ctrl: true, shift: false, action: { type: "undo" } },
  { code: "KeyZ", ctrl: true, shift: true, action: { type: "redo" } },
  { code: "KeyY", ctrl: true, shift: false, action: { type: "redo" } },
  { code: "KeyC", ctrl: true, shift: false, action: { type: "copy" } },
  { code: "KeyV", ctrl: true, shift: false, action: { type: "paste" } },
  { code: "KeyD", ctrl: true, shift: false, action: { type: "duplicate" } },
  { code: "KeyA", ctrl: true, shift: false, action: { type: "selectAll" } },
  { code: "Delete", ctrl: false, shift: false, action: { type: "delete" } },
  { code: "Backspace", ctrl: false, shift: false, action: { type: "delete" } },
  { code: "ArrowUp", ctrl: false, shift: false, action: { type: "nudge", dx: 0, dy: -1 } },
  { code: "ArrowDown", ctrl: false, shift: false, action: { type: "nudge", dx: 0, dy: 1 } },
  { code: "ArrowLeft", ctrl: false, shift: false, action: { type: "nudge", dx: -1, dy: 0 } },
  { code: "ArrowRight", ctrl: false, shift: false, action: { type: "nudge", dx: 1, dy: 0 } },
  { code: "KeyV", ctrl: false, shift: false, action: { type: "tool", tool: "select" } },
  { code: "KeyP", ctrl: false, shift: false, action: { type: "tool", tool: "pen" } },
  { code: "KeyH", ctrl: false, shift: false, action: { type: "tool", tool: "highlighter" } },
  { code: "KeyK", ctrl: false, shift: false, action: { type: "tool", tool: "laser" } },
  { code: "KeyA", ctrl: false, shift: false, action: { type: "tool", tool: "arrow" } },
  { code: "KeyL", ctrl: false, shift: false, action: { type: "tool", tool: "line" } },
  { code: "KeyR", ctrl: false, shift: false, action: { type: "tool", tool: "rectangle" } },
  { code: "KeyO", ctrl: false, shift: false, action: { type: "tool", tool: "ellipse" } },
  { code: "KeyT", ctrl: false, shift: false, action: { type: "tool", tool: "text" } },
  { code: "KeyE", ctrl: false, shift: false, action: { type: "tool", tool: "eraser" } },
  { code: "Escape", ctrl: false, shift: false, action: { type: "leave" } },
  { code: "Tab", ctrl: false, shift: false, action: { type: "toggleToolbar" } },
];
