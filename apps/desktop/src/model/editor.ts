// The board overlay's drawing editor (M27b, ADR 0013): tools, the
// selection, undo and redo, copy and paste, and the operations they make,
// batched for the host. A port of the stream window's
// `couch-viewer/src/editor.rs`, checked by the same cases
// (`editor.test.ts`, `editor/tests.rs`); keep the two alike. Pure:
// positions in the picture's own coordinates, times in milliseconds.
//
// Every operation it makes is applied to our own board as it is made, then
// sent: what we show of our own is what we sent, and the host's resync
// after a refusal sets it right. It selects, erases, moves and undoes only
// our own elements; the host refuses anything else anyway.
//
// Text boxes (M27c): typed into one at a time, shown to the others as they
// grow (unfinished, so not fading), finished as one step of the history. A
// box's second corner is where our font ends it (`setMeasure`); other
// windows lay its text out with their own.

import type { BoardElement } from "../bindings/BoardElement";
import type { BoardElementKind } from "../bindings/BoardElementKind";
import type { BoardOpView } from "../bindings/BoardOpView";
import type { BoardPoint } from "../bindings/BoardPoint";
import {
  type DrawAction,
  type DrawTool,
  DEFAULT_STAY,
  FADE_STAYS,
  HIGHLIGHTER_WIDTHS,
  LINE_HEIGHT,
  LINE_WIDTHS,
  MAX_BATCH_ITEMS,
  MAX_BATCH_OPS,
  MAX_LASER_POINTS,
  MAX_MEMBER_ELEMENTS,
  MAX_STROKE_POINTS,
  MAX_TEXT_CHARS,
  MAX_TEXT_LINES,
  PALETTE,
  TEXT_SIZES,
  WIDTH_NAMES,
} from "../bindings/boardRules";
import { type BoardView, cloneElement } from "./board";

/** Batches go out at most this often: the host takes 40 a second. */
export const BATCH_EVERY_MS = 30;
/** Laser points at most this often (the host takes 60 marks a second). */
const LASER_EVERY_MS = 20;
/** Pings at most this often. */
const PING_EVERY_MS = 500;
/** A stroke's points at least this far apart, in picture heights. */
const MIN_SPACING = 0.0015;
/** How near the pointer must be to touch a line, in picture heights, beyond
 * half its width. */
const TOUCH = 0.012;
/** A nudge, in picture coordinates; Shift: ten. */
const NUDGE = 131;
/** Where a paste lands from what was copied, in picture coordinates. */
const PASTE_STEP = 1300;
const FULL = 65535;
/** Where pasted text lands without a pointer over the picture. */
const TEXT_AT: BoardPoint = { x: 6554, y: 6554 };
export const TOO_MANY = "You've drawn all you may: erase some first";
export const TOO_LONG = "A text box holds at most 500 characters and 50 lines";

/** How wide a line of text is, in ems of its font: a text box is that times
 * its size wide. The page measures with its own font; until it says, and in
 * the shared cases, an estimate (half an em a character). */
export type Measure = (line: string) => number;
export const estimate: Measure = (line) => chars(line).length * 0.5;

/** A key while typing in a text box: the page's keys go here, never to the
 * drawing keys, until the box is finished. `finish`: Esc or Ctrl+Enter;
 * `selectAll`: Ctrl+A. With Shift, a move extends the selection; with Ctrl
 * (`word`), as text fields do: left and right by word, backspace and delete
 * a word, home and end the box's start and end. */
export type TextKey =
  | "backspace"
  | "delete"
  | "left"
  | "right"
  | "home"
  | "end"
  | "up"
  | "down"
  | "enter"
  | "finish"
  | "selectAll";

/** The text box being typed in. */
interface Typing {
  /** Its number, kept while it has no text (and so isn't on the board). */
  id: number;
  /** Its colour, size and fading. */
  look: BoardElement;
  /** Its top-left corner. */
  anchor: BoardPoint;
  text: string;
  /** Characters before the caret. */
  caret: number;
  /** The selection's other end, in characters: the text between it and the
   * caret is selected. `null`, or the caret's own place: nothing. */
  mark: number | null;
  /** The box as it was before this edit; `null` for a new one. */
  before: BoardElement | null;
  /** On the board: it has text (spaces alone aren't). */
  shown: boolean;
}

/** The selected characters of `t`, start and end, if any. */
function range(t: Typing): [number, number] | null {
  if (t.mark === null || t.mark === t.caret) return null;
  return [Math.min(t.mark, t.caret), Math.max(t.mark, t.caret)];
}

/** The caret moved to `to`: with `extend` (Shift, a drag) the selection
 * grows from where it was; otherwise there is none. */
function moveTo(t: Typing, to: number, extend: boolean) {
  t.mark = extend ? (t.mark ?? t.caret) : null;
  t.caret = to;
  if (t.mark === t.caret) t.mark = null;
}

/** The text box being typed in, for painting it: its corners, the caret's
 * top and bottom, the selection's boxes, one per line (picture
 * coordinates), and its size (picture heights). */
export interface TextCursor {
  corners: [BoardPoint, BoardPoint];
  caret: [BoardPoint, BoardPoint];
  selection: [BoardPoint, BoardPoint][];
  size: number;
  colour: number;
}

/** A string's characters (Unicode scalar values, as Rust's `chars`). */
function chars(s: string): string[] {
  return Array.from(s);
}

const CONTROL = /[\u0000-\u001f\u007f-\u009f]/;
function isControl(c: string) {
  return CONTROL.test(c);
}

/** A text box's size (its font's em) as a fraction of the picture's height. */
export function textSize(width: number): number {
  return TEXT_SIZES[Math.min(width, TEXT_SIZES.length - 1)];
}

/** Rust's `char::is_whitespace` (Unicode's White_Space), which `trim` uses
 * there: JavaScript's `trim` also takes U+FEFF and leaves U+0085, so the
 * two editors would disagree (the review's finding). */
const SPACES = [0x9, 0xa, 0xb, 0xc, 0xd, 0x20, 0x85, 0xa0, 0x1680, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000];
function isSpace(c: string): boolean {
  const n = c.codePointAt(0) ?? 0;
  return SPACES.includes(n) || (n >= 0x2000 && n <= 0x200a);
}
function blank(cs: string[]): boolean {
  return cs.every(isSpace);
}

/** Without lone UTF-16 surrogates, which a Rust string can't hold (the
 * host's JSON would refuse the whole batch). */
function wellFormed(text: string): string {
  return text.replace(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/g, "");
}

/** A text box's text as the board takes it (`couch_proto::board::valid_text`). */
export function validText(text: string): boolean {
  const cs = chars(text);
  return (
    !blank(cs) &&
    cs.length <= MAX_TEXT_CHARS &&
    text.split("\n").length <= MAX_TEXT_LINES &&
    cs.every((c) => c === "\n" || !isControl(c))
  );
}

/** Text from elsewhere as a text box may hold it, but for its length
 * (`couch_proto::board::clean_text`): line breaks as `\n`, tabs as four
 * spaces, other control characters gone, nothing blank at its end. */
export function cleanText(text: string): string {
  const out: string[] = [];
  for (const c of chars(wellFormed(text).replace(/\r\n/g, "\n").replace(/\r/g, "\n"))) {
    if (c === "\n") out.push(c);
    else if (c === "\t") out.push(" ", " ", " ", " ");
    else if (!isControl(c)) out.push(c);
  }
  while (out.length > 0 && isSpace(out[out.length - 1])) out.pop();
  return out.join("");
}

const encoder = new TextEncoder();
/** An element's weight in points: its points, and its text a point a byte. */
function load(e: BoardElement): number {
  return e.points.length + (e.text ? encoder.encode(e.text).length : 0);
}

/** How the toolbar's one fade button steps: a click goes round, the
 * wheel goes longer or shorter. */
export type FadeStep = "next" | "longer" | "shorter";

/** What a key did. */
export type Keyed = "done" | "leaveDrawing" | "toggleToolbar";

/** One change to one of our elements: what it was and what it became. */
interface Change {
  id: number;
  before: BoardElement | null;
  after: BoardElement | null;
}

type Gesture =
  | { kind: "stroke"; id: number; last: BoardPoint; made: number[] }
  | { kind: "shape"; id: number; shape: BoardElementKind; start: BoardPoint }
  | { kind: "laser"; lastAt: number | null; fresh: boolean }
  | { kind: "erase"; removed: BoardElement[] }
  | { kind: "move"; from: BoardPoint; originals: BoardElement[] }
  | { kind: "marquee"; from: BoardPoint; to: BoardPoint; kept: Set<number> }
  /** Dragging inside the text box being typed in: selecting. */
  | { kind: "textSelect" };

/** A line's width as a fraction of the picture's height. */
export function lineWidth(kind: BoardElementKind, width: number): number {
  const i = Math.min(width, WIDTH_NAMES.length - 1);
  return kind === "highlighter" ? HIGHLIGHTER_WIDTHS[i] : LINE_WIDTHS[i];
}

export function isStroke(kind: BoardElementKind) {
  return kind === "pen" || kind === "highlighter";
}

function items(op: BoardOpView): number {
  switch (op.type) {
    case "put":
      return load(op.element);
    case "append":
    case "laser":
      return op.points.length;
    case "remove":
      return op.ids.length;
    case "ping":
      return 1;
  }
}

/** Laser points and pings an operation carries. */
export function marks(op: BoardOpView): number {
  return op.type === "laser" ? op.points.length : op.type === "ping" ? 1 : 0;
}

function validElement(e: BoardElement): boolean {
  const shape = isStroke(e.kind)
    ? e.points.length >= 1 && e.points.length <= MAX_STROKE_POINTS && e.text === ""
    : e.kind === "text"
      ? e.points.length === 2 && validText(e.text)
      : e.points.length === 2 && e.done && e.text === "";
  return e.colour < PALETTE.length && e.width < WIDTH_NAMES.length && FADE_STAYS.includes(e.stay) && shape;
}

/** Whether a batch is well formed on its own (`couch_proto::board::valid_batch`). */
export function validBatch(ops: BoardOpView[]): boolean {
  return (
    ops.length > 0 &&
    ops.length <= MAX_BATCH_OPS &&
    ops.reduce((n, op) => n + items(op), 0) <= MAX_BATCH_ITEMS &&
    ops.every((op) => {
      switch (op.type) {
        case "put":
          return validElement(op.element);
        case "append":
          return op.points.length <= MAX_STROKE_POINTS;
        case "remove":
          return op.ids.length > 0;
        case "laser":
          return op.points.length > 0 && op.points.length <= MAX_LASER_POINTS && op.colour < PALETTE.length;
        case "ping":
          return true;
      }
    })
  );
}

/** `ops` in batches the checks allow, in order (`couch_proto::board::batches`). */
export function batches(ops: BoardOpView[]): BoardOpView[][] {
  const out: BoardOpView[][] = [];
  let count = 0;
  for (const op of ops) {
    const n = items(op);
    const last = out[out.length - 1];
    if (!last || last.length >= MAX_BATCH_OPS || count + n > MAX_BATCH_ITEMS) {
      out.push([]);
      count = 0;
    }
    count += n;
    out[out.length - 1].push(op);
  }
  return out;
}

export class Editor {
  readonly me: string;
  tool: DrawTool = "pen";
  /** The tool before Esc went to Select: drawing starts with it. */
  private lastTool: DrawTool = "pen";
  colour = 0;
  width = 1;
  fades = false;
  /** How long a fading element stays before it fades, in seconds: one of
   * `FADE_STAYS` (wire revision 16). */
  stay = DEFAULT_STAY;
  /** The picture's width over its height: distances in picture heights. */
  private aspect = 16 / 9;
  private nextId = 1;
  private gesture: Gesture | null = null;
  private selected = new Set<number>();
  private undoSteps: Change[][] = [];
  private redoSteps: Change[][] = [];
  private clipboard: BoardElement[] = [];
  /** What our last copy put on the system clipboard (the copied boxes'
   * text): while the clipboard still holds it, a paste is ours. */
  private copiedText: string | null = null;
  private pastes = 0;
  private typingBox: Typing | null = null;
  private measure: Measure = estimate;
  private outbox: BoardOpView[] = [];
  /** The board's generation as last heard, and the one the queued
   * operations were made in. */
  private generation = 0;
  private outboxGeneration = 0;
  private sentAt: number | null = null;
  private pingedAt: number | null = null;
  private note: string | null = null;

  constructor(me: string) {
    this.me = me;
  }

  /** The picture's shape, as shown. */
  setAspect(width: number, height: number) {
    if (width > 0 && height > 0) this.aspect = width / height;
  }

  selection(): ReadonlySet<number> {
    return this.selected;
  }

  /** The marquee being dragged, corner to corner. */
  marquee(): [BoardPoint, BoardPoint] | null {
    return this.gesture?.kind === "marquee" ? [this.gesture.from, this.gesture.to] : null;
  }

  /** Something to tell the user, once. */
  takeNote(): string | null {
    const note = this.note;
    this.note = null;
    return note;
  }

  /** How the page's font measures text, from now on. */
  setMeasure(measure: Measure) {
    this.measure = measure;
  }

  private measured(line: string): number {
    return Math.max(0, this.measure(line));
  }

  /** A text box is being typed in: the page's keys and text are its. */
  typing(): boolean {
    return this.typingBox !== null;
  }

  /** The text box being typed in, for painting its frame and caret. */
  textCursor(): TextCursor | null {
    const t = this.typingBox;
    if (!t) return null;
    const size = textSize(t.look.width);
    const [line, col] = lineCol(t.text, t.caret);
    const prefix = chars(t.text.split("\n")[line] ?? "")
      .slice(0, col)
      .join("");
    const [ax, ay] = this.uniform(t.anchor);
    const x = ax + this.measured(prefix) * size;
    const y = ay + line * LINE_HEIGHT * size;
    const corners: [BoardPoint, BoardPoint] = t.shown
      ? bounds(this.textBox(t, false))
      : [t.anchor, this.picture(ax, ay + LINE_HEIGHT * size)];
    // The selection, line by line; a line break selected shows as a little
    // more past its line's end.
    const selection: [BoardPoint, BoardPoint][] = [];
    const r = range(t);
    if (r) {
      const [lo, hi] = r;
      let start = 0;
      t.text.split("\n").forEach((line, i) => {
        const cs = chars(line);
        const end = start + cs.length;
        const [s, e] = [Math.max(lo, start), Math.min(hi, end)];
        const past = hi > end && lo <= end;
        if (s < e || past) {
          const width = (n: number) => this.measured(cs.slice(0, n).join(""));
          const x0 = ax + width(Math.min(s, end) - start) * size;
          const x1 = ax + (width(Math.max(e, s) - start) + (past ? 0.3 : 0)) * size;
          const top = ay + i * LINE_HEIGHT * size;
          selection.push([this.picture(x0, top), this.picture(x1, top + LINE_HEIGHT * size)]);
        }
        start = end + 1;
      });
    }
    return {
      corners,
      caret: [this.picture(x, y), this.picture(x, y + LINE_HEIGHT * size)],
      selection,
      size,
      colour: t.look.colour,
    };
  }

  /** Drawing begins: with the tool it last drew with. */
  begin() {
    if (this.tool === "select" && this.selected.size === 0) this.tool = this.lastTool;
  }

  /** Drawing ends: whatever was half done is finished or dropped. */
  end(board: BoardView, now: number) {
    this.release(null, false, board, now);
    this.finishTyping(board, now);
    this.selected.clear();
  }

  setTool(tool: DrawTool, board: BoardView, now: number) {
    this.release(null, false, board, now);
    this.finishTyping(board, now);
    if (tool !== "select") {
      this.selected.clear();
      this.lastTool = tool;
    }
    this.tool = tool;
  }

  /** A colour, width or fading picked: what is selected takes it too. */
  setStyle(
    style: { colour?: number; width?: number; fades?: boolean },
    board: BoardView,
    now: number,
  ) {
    const { colour, width, fades } = style;
    if (colour !== undefined && colour < PALETTE.length) this.colour = colour;
    if (width !== undefined && width < WIDTH_NAMES.length) this.width = width;
    if (fades !== undefined) this.fades = fades;
    // The box being typed in takes it too, as part of its edit.
    const t = this.typingBox;
    if (t) {
      if (colour !== undefined && colour < PALETTE.length) t.look.colour = colour;
      if (width !== undefined && width < WIDTH_NAMES.length) t.look.width = width;
      if (fades !== undefined) t.look.fades = fades;
      this.showTyping(board, now);
    }
    // A text box's size changes its box (the review's finding).
    const changed = this.selectedElements(board).map((e) =>
      this.remeasured({
        ...e,
        colour: colour ?? e.colour,
        width: width ?? e.width,
        fades: fades ?? e.fades,
      }),
    );
    this.replaceFrom(this.selectedElements(board), changed, board, now);
  }

  /** The toolbar's one fade button stepped (the author's choice, M27b):
   * through "stays", then each of `FADE_STAYS`. A click goes round
   * (`next`); the wheel goes `longer` or `shorter`, stopping at either end.
   * What is selected takes each step. */
  stepFade(step: FadeStep, board: BoardView, now: number) {
    // 0 stays; 1.. the stays offered, in order.
    const at = this.fades ? FADE_STAYS.indexOf(this.stay) + 1 : 0;
    const last = FADE_STAYS.length;
    const to =
      step === "next" ? (at === last ? 0 : at + 1) : step === "longer" ? Math.min(at + 1, last) : Math.max(at - 1, 0);
    if (to === at) return;
    if (to === 0) this.setStyle({ fades: false }, board, now);
    else this.setStay(FADE_STAYS[to - 1], board, now);
  }

  /** How long fading elements stay before they fade (one of `FADE_STAYS`):
   * fading turns on, and what is selected takes both. */
  setStay(stay: number, board: BoardView, now: number) {
    if (!FADE_STAYS.includes(stay)) return;
    this.stay = stay;
    this.fades = true;
    const t = this.typingBox;
    if (t) {
      t.look.fades = true;
      t.look.stay = stay;
      this.showTyping(board, now);
    }
    const changed = this.selectedElements(board).map((e) => ({ ...e, fades: true, stay }));
    this.replaceFrom(this.selectedElements(board), changed, board, now);
  }

  // ---- The pointer ----

  /** A press of the main button at `at`. */
  press(at: BoardPoint, shift: boolean, board: BoardView, now: number) {
    this.release(null, false, board, now);
    if (this.tool === "text") {
      this.pressText(at, shift, board, now);
      return;
    }
    this.finishTyping(board, now);
    const tool = this.tool;
    switch (tool) {
      case "pen":
      case "highlighter": {
        const id = this.newId(board);
        if (id === null) return;
        this.emit({ type: "put", element: this.element(id, tool, [at], false) }, board, now);
        this.gesture = { kind: "stroke", id, last: at, made: [id] };
        return;
      }
      case "arrow":
      case "line":
      case "rectangle":
      case "ellipse": {
        const id = this.newId(board);
        if (id === null) return;
        this.emit({ type: "put", element: this.element(id, tool, [at, at], true) }, board, now);
        this.gesture = { kind: "shape", id, shape: tool, start: at };
        return;
      }
      case "laser": {
        const g: Gesture = { kind: "laser", lastAt: null, fresh: true };
        this.laser(g, at, board, now);
        this.gesture = g;
        return;
      }
      case "eraser": {
        const removed: BoardElement[] = [];
        this.eraseAt(at, removed, board, now);
        this.gesture = { kind: "erase", removed };
        return;
      }
      case "select": {
        const id = this.hit(at, board);
        if (id !== null) {
          if (shift) {
            if (!this.selected.delete(id)) this.selected.add(id);
          } else if (!this.selected.has(id)) {
            this.selected = new Set([id]);
          }
          this.gesture = { kind: "move", from: at, originals: this.selectedElements(board) };
        } else {
          const kept = shift ? new Set(this.selected) : new Set<number>();
          this.selected = new Set(kept);
          this.gesture = { kind: "marquee", from: at, to: at, kept };
        }
        return;
      }
    }
  }

  /** The pointer moved to `at` with the main button down. */
  drag(at: BoardPoint, shift: boolean, board: BoardView, now: number) {
    const g = this.gesture;
    if (!g) return;
    switch (g.kind) {
      case "stroke": {
        if (this.distance(g.last, at) < MIN_SPACING) return;
        const current = board.get(this.me, g.id);
        if (current && current.element.points.length >= MAX_STROKE_POINTS - 1) {
          // Finished here, and on in a new element.
          this.emit({ type: "append", id: g.id, points: [at], done: true }, board, now);
          const kind = board.get(this.me, g.id)?.element.kind ?? "pen";
          const next = this.newId(board);
          if (next === null) {
            this.gesture = null;
            this.finish(g.made, board);
            return;
          }
          this.emit({ type: "put", element: this.element(next, kind, [at], false) }, board, now);
          g.id = next;
          g.made.push(next);
        } else {
          this.emit({ type: "append", id: g.id, points: [at], done: false }, board, now);
        }
        g.last = at;
        return;
      }
      case "shape": {
        const end = this.constrained(g.shape, g.start, at, shift);
        this.emit({ type: "put", element: this.element(g.id, g.shape, [g.start, end], true) }, board, now);
        return;
      }
      case "laser":
        this.laser(g, at, board, now);
        return;
      case "erase":
        this.eraseAt(at, g.removed, board, now);
        return;
      case "move": {
        // Shown moving here only; sent once let go.
        const moved = shifted(g.originals, this.offset(g.originals, g.from, at));
        board.apply(
          this.me,
          moved.map((element) => ({ type: "put", element })),
          now,
        );
        return;
      }
      case "marquee": {
        g.to = at;
        const inside = board
          .of(this.me)
          .filter((s) => within(bounds(s.element), g.from, g.to))
          .map((s) => s.element.id);
        this.selected = new Set([...g.kept, ...inside]);
        return;
      }
      case "textSelect": {
        const t = this.typingBox;
        if (t) moveTo(t, this.caretAt(t, at), true);
        return;
      }
    }
  }

  /** The main button let go (at `at`, if known), or the gesture ended
   * another way (`null`: drawing ending, a tool picked). */
  release(at: BoardPoint | null, shift: boolean, board: BoardView, now: number) {
    const g = this.gesture;
    if (!g) return;
    this.gesture = null;
    switch (g.kind) {
      case "stroke":
        this.emit({ type: "append", id: g.id, points: [], done: true }, board, now);
        this.finish(g.made, board);
        return;
      case "shape":
        if (at) {
          const end = this.constrained(g.shape, g.start, at, shift);
          this.emit({ type: "put", element: this.element(g.id, g.shape, [g.start, end], true) }, board, now);
        }
        this.finish([g.id], board);
        return;
      case "laser":
      case "marquee":
      case "textSelect":
        return;
      case "erase":
        if (g.removed.length > 0) {
          this.record(g.removed.map((e) => ({ id: e.id, before: e, after: null })));
        }
        return;
      case "move": {
        const offset: [number, number] = at ? this.offset(g.originals, g.from, at) : [0, 0];
        if (offset[0] === 0 && offset[1] === 0) {
          // Back where they were, as shown.
          board.apply(
            this.me,
            g.originals.map((element) => ({ type: "put", element })),
            now,
          );
        } else {
          this.replaceFrom(g.originals, shifted(g.originals, offset), board, now);
        }
        return;
      }
    }
  }

  /** A press with the Text tool: inside the box being typed in, the caret
   * goes there; elsewhere that box is finished (as Excalidraw: a click away
   * is done). Not typing, it edits our text box under it, or starts a new
   * one there (nothing goes out until it has text). */
  private pressText(at: BoardPoint, shift: boolean, board: BoardView, now: number) {
    const t = this.typingBox;
    if (t) {
      const cursor = this.textCursor();
      if (cursor && this.nearBox(cursor.corners, at)) {
        // The caret there (Shift: the selection to there), and a drag from
        // here selects.
        moveTo(t, this.caretAt(t, at), shift);
        this.gesture = { kind: "textSelect" };
      } else {
        this.finishTyping(board, now);
      }
      return;
    }
    const hit = this.hitText(at, board);
    if (hit !== null) {
      this.edit(hit, at, board, now);
      this.gesture = { kind: "textSelect" };
      return;
    }
    const id = this.newId(board);
    if (id === null) return;
    this.selected.clear();
    this.typingBox = {
      id,
      look: this.element(id, "text", [], false),
      anchor: at,
      text: "",
      caret: 0,
      mark: null,
      before: null,
      shown: false,
    };
  }

  /** A double click (after its two presses): inside the text box being typed
   * in, the word there is selected; with Select, on our text box, typing in
   * it, the caret where it was. Other tools drew with the clicks; the Text
   * tool edits on one. */
  doubleClick(at: BoardPoint, board: BoardView, now: number) {
    const t = this.typingBox;
    const cursor = this.textCursor();
    if (t && cursor && this.nearBox(cursor.corners, at)) {
      this.gesture = null;
      const [start, end] = wordAt(chars(t.text), this.caretAt(t, at));
      t.mark = start === end ? null : start;
      t.caret = end;
      return;
    }
    if (this.tool !== "select" || this.typingBox) return;
    this.release(null, false, board, now);
    const hit = this.hitText(at, board);
    if (hit !== null) this.edit(hit, at, board, now);
  }

  // ---- Typing ----

  /** Text typed (or composed by an input method) into the box being typed
   * in, at the caret: no control characters but line breaks, a tab as
   * spaces, and only as much as the box holds (characters and lines). */
  typeText(text: string, board: BoardView, now: number) {
    const t = this.typingBox;
    if (!t) return;
    // What it types replaces the selection: line breaks there make room for
    // new ones.
    const [from, to] = range(t) ?? [t.caret, t.caret];
    const old = chars(t.text);
    let breaks = old.filter((c) => c === "\n").length - old.slice(from, to).filter((c) => c === "\n").length;
    let cut = false;
    let cs: string[] = [];
    for (const c of chars(wellFormed(text))) {
      if (c === "\t") cs.push(" ", " ", " ", " ");
      else if (c === "\n" || c === "\r") {
        if (breaks + 1 >= MAX_TEXT_LINES) {
          cut = true;
        } else {
          breaks += 1;
          cs.push("\n");
        }
      } else if (!isControl(c)) cs.push(c);
    }
    const room = Math.max(0, MAX_TEXT_CHARS - (old.length - (to - from)));
    if (cs.length > room) {
      cs = cs.slice(0, room);
      cut = true;
    }
    if (cut) this.note = TOO_LONG;
    if (cs.length === 0) return;
    old.splice(from, to - from, ...cs);
    t.text = old.join("");
    t.caret = from + cs.length;
    t.mark = null;
    this.showTyping(board, now);
  }

  /** A key while typing: with `shift` a move extends the selection, with
   * `word` (Ctrl) it goes by word or to the box's ends. */
  textKey(key: TextKey, board: BoardView, now: number, shift = false, word = false) {
    if (key === "finish") {
      const id = this.finishTyping(board, now);
      if (id !== null) this.selected = new Set([id]);
      this.tool = "select";
      return;
    }
    if (key === "enter") {
      this.typeText("\n", board, now);
      return;
    }
    const t = this.typingBox;
    if (!t) return;
    const cs = chars(t.text);
    const len = cs.length;
    const [line, col] = lineCol(t.text, t.caret);
    const lines = t.text.split("\n").map((l) => chars(l).length);
    const start = t.caret - col;
    const r = range(t);
    if (key === "selectAll") {
      t.mark = len === 0 ? null : 0;
      t.caret = len;
      return;
    }
    if (key === "backspace" || key === "delete") {
      // The selection, or a character (Ctrl: a word) on that side.
      let [from, to] = r ?? [t.caret, t.caret];
      if (!r) {
        if (key === "backspace") from = word ? prevWord(cs, t.caret) : Math.max(0, t.caret - 1);
        else to = word ? nextWord(cs, t.caret) : Math.min(len, t.caret + 1);
      }
      if (from === to) return;
      t.text = [...cs.slice(0, from), ...cs.slice(to)].join("");
      t.caret = from;
      t.mark = null;
      this.showTyping(board, now);
      return;
    }
    let to: number;
    if (key === "left" && !shift && !word && r) to = r[0];
    else if (key === "right" && !shift && !word && r) to = r[1];
    else if (key === "left") to = word ? prevWord(cs, t.caret) : Math.max(0, t.caret - 1);
    else if (key === "right") to = word ? nextWord(cs, t.caret) : Math.min(len, t.caret + 1);
    else if (key === "home") to = word ? 0 : start;
    else if (key === "end") to = word ? len : start + lines[line];
    // Up on the first line goes to its start, Down on the last to its end,
    // as text fields do.
    else if (key === "up" && line === 0) to = 0;
    else if (key === "down" && line + 1 === lines.length) to = len;
    else {
      const size = textSize(t.look.width);
      const x = this.measured(cs.slice(start, t.caret).join("")) * size;
      const target = key === "up" ? line - 1 : line + 1;
      const first = lines.slice(0, target).reduce((n, l) => n + l + 1, 0);
      to = first + this.nearest(t.text.split("\n")[target], x, size);
    }
    moveTo(t, to, shift);
  }

  /** The selected text of the box being typed in, for Ctrl+C there. */
  copyText(): string | null {
    const t = this.typingBox;
    const r = t ? range(t) : null;
    if (!t || !r) return null;
    return chars(t.text).slice(r[0], r[1]).join("");
  }

  /** Ctrl+X in the box being typed in: its selected text, taken out. */
  cutText(board: BoardView, now: number): string | null {
    const text = this.copyText();
    if (text === null) return null;
    this.textKey("delete", board, now);
    return text;
  }

  /** What our copy puts on the system clipboard: the selection is ours to
   * paste, and its text boxes' text (or nothing) goes on the clipboard, so
   * a paste can tell our copy from text copied elsewhere since. Nothing
   * selected, nothing is copied (`null`): the clipboard keeps what it had. */
  copy(board: BoardView): string | null {
    if (this.selected.size === 0) return null;
    this.clipboard = this.selectedElements(board);
    const text = this.clipboard
      .filter((e) => e.kind === "text")
      .map((e) => e.text)
      .join("\n\n");
    this.copiedText = text;
    return text;
  }

  /** Ctrl+V, with the system clipboard's text (if any) and the pointer (if
   * over the picture). While typing, the text goes into the box. Otherwise
   * text copied elsewhere since our last copy becomes a new text box there;
   * else what we copied is pasted, as ever. */
  paste(system: string | null, at: BoardPoint | null, board: BoardView, now: number) {
    const text = system === null ? "" : cleanText(system);
    if (this.typingBox) {
      this.typeText(text, board, now);
      return;
    }
    const ours = this.copiedText === null ? null : cleanText(this.copiedText);
    if (!blank(chars(text)) && ours !== text) {
      this.pasteText(text, at ?? TEXT_AT, board, now);
    } else {
      this.pastes += 1;
      this.pasteElements(this.clipboard, this.pastes, board, now);
    }
  }

  /** A middle click: a ping where it was. */
  ping(at: BoardPoint, board: BoardView, now: number) {
    if (this.pingedAt !== null && now - this.pingedAt < PING_EVERY_MS) return;
    this.pingedAt = now;
    this.emit({ type: "ping", at }, board, now);
  }

  // ---- Keys ----

  key(action: DrawAction, shift: boolean, board: BoardView, now: number): Keyed {
    // The page's own: nothing of the editor's changes.
    if (action.type === "toggleToolbar") return "toggleToolbar";
    // A drawing key while typing (the toolbar's undo, say): the box is
    // finished first; Esc finishes it alone (`textKey("finish")`).
    if (this.typingBox) {
      if (action.type === "leave") {
        this.textKey("finish", board, now);
        return "done";
      }
      this.finishTyping(board, now);
    }
    switch (action.type) {
      case "undo":
        this.undo(board, now);
        break;
      case "redo":
        this.redo(board, now);
        break;
      // Without the system clipboard (the page passes it through `copy` and
      // `paste`): our own copy alone.
      case "copy":
        this.copy(board);
        break;
      case "paste":
        this.paste(null, null, board, now);
        break;
      case "duplicate":
        this.pasteElements(this.selectedElements(board), 1, board, now);
        break;
      case "selectAll":
        this.release(null, false, board, now);
        this.tool = "select";
        this.selected = new Set(board.of(this.me).map((s) => s.element.id));
        break;
      case "delete": {
        const changes = this.selectedElements(board).map((e) => ({ id: e.id, before: e, after: null }));
        this.selected.clear();
        this.perform(changes, board, now);
        break;
      }
      case "nudge": {
        const step = shift ? NUDGE * 10 : NUDGE;
        const selected = this.selectedElements(board);
        const offset = clampOffset(selected, [action.dx * step, action.dy * step]);
        if (offset[0] !== 0 || offset[1] !== 0) {
          this.replaceFrom(selected, shifted(selected, offset), board, now);
        }
        break;
      }
      case "tool":
        this.setTool(action.tool, board, now);
        break;
      case "leave":
        if (this.gesture) this.release(null, false, board, now);
        else if (this.selected.size > 0) this.selected.clear();
        else if (this.tool !== "select") this.tool = "select";
        else return "leaveDrawing";
        break;
    }
    return "done";
  }

  // ---- What the host said ----

  /** The host cleared our elements, or everyone's: what it cleared is gone
   * from the history, and anything half drawn stops. */
  cleared(board: BoardView) {
    this.undoSteps = [];
    this.redoSteps = [];
    // Made before the clear: the host drops it, and the clear took it from
    // our board too.
    this.outbox = [];
    // The box being typed in stays being typed in: a clear of ours is also
    // the host's resync after a refused batch, and its text is the user's.
    // It goes out again whole at the next key or when finished; what it was
    // before is history, gone.
    if (this.typingBox) this.typingBox.before = null;
    this.sync(board);
  }

  /** Finishes our strokes left open that we aren't drawing (a page loaded
   * again mid-stroke gets that stroke back open). After a clear of ours. */
  finishStrays(board: BoardView, now: number) {
    const drawing = this.gesture?.kind === "stroke" ? this.gesture.id : (this.typingBox?.id ?? null);
    const strays = board
      .of(this.me)
      .filter((s) => !s.element.done && s.element.id !== drawing)
      .map((s) => cloneElement(s.element));
    for (const e of strays) {
      // A text box left being typed in: the box again, finished (an append
      // is a stroke's alone).
      if (isStroke(e.kind)) this.emit({ type: "append", id: e.id, points: [], done: true }, board, now);
      else this.emit({ type: "put", element: { ...e, done: true } }, board, now);
    }
  }

  /** After any news: our numbers stay new, and the selection and a gesture
   * forget what is gone. */
  sync(board: BoardView) {
    this.generation = board.generation;
    const most = Math.max(0, ...board.of(this.me).map((s) => s.element.id));
    this.nextId = Math.max(this.nextId, most + 1);
    this.selected = new Set([...this.selected].filter((id) => board.get(this.me, id)));
    const g = this.gesture;
    if ((g?.kind === "stroke" || g?.kind === "shape") && !board.get(this.me, g.id)) this.gesture = null;
    // The box being typed in is shown as the board has it: cleared by the
    // host, it is put again at the next key (`cleared`).
    const t = this.typingBox;
    if (t) t.shown = board.get(this.me, t.id) !== undefined;
  }

  // ---- Sending ----

  /** The next batch to send, if one is due (at most every
   * `BATCH_EVERY_MS`), with the generation it was made in. */
  takeBatch(now: number): { generation: number; ops: BoardOpView[] } | null {
    const early = this.sentAt !== null && now - this.sentAt < BATCH_EVERY_MS;
    if (this.outbox.length === 0 || early) return null;
    this.sentAt = now;
    const parts = batches(this.outbox);
    const first = parts.shift() ?? [];
    this.outbox = parts.flat();
    return { generation: this.outboxGeneration, ops: first };
  }

  /** Whether operations wait to go out. */
  pending(): boolean {
    return this.outbox.length > 0;
  }

  // ---- Within ----

  private element(id: number, kind: BoardElementKind, points: BoardPoint[], done: boolean): BoardElement {
    return {
      id,
      kind,
      colour: this.colour,
      width: this.width,
      fades: this.fades,
      stay: this.stay,
      done,
      points: [...points],
      text: "",
    };
  }

  /** The box being typed in as an element: from its corner, as wide as its
   * longest line and as tall as its lines, measured by our font. */
  private textBox(t: Typing, done: boolean): BoardElement {
    const size = textSize(t.look.width);
    const lines = t.text.split("\n");
    const wide = Math.max(0, ...lines.map((l) => this.measured(l))) * size;
    const tall = lines.length * LINE_HEIGHT * size;
    const [ax, ay] = this.uniform(t.anchor);
    return {
      ...t.look,
      kind: "text",
      done,
      points: [{ ...t.anchor }, this.picture(ax + wide, ay + tall)],
      text: t.text,
    };
  }

  /** A text box of ours as its look now makes it: its corner kept, its other
   * one measured again (a new size). Anything else as it is. */
  private remeasured(e: BoardElement): BoardElement {
    if (e.kind !== "text" || e.points.length !== 2) return e;
    const t: Typing = {
      id: e.id,
      anchor: { ...e.points[0] },
      text: e.text,
      caret: 0,
      mark: null,
      before: null,
      shown: true,
      look: cloneElement(e),
    };
    return this.textBox(t, e.done);
  }

  /** The box being typed in, on our board and to the host as it now is: put
   * while it has text, removed while it has none. */
  private showTyping(board: BoardView, now: number) {
    const t = this.typingBox;
    if (!t) return;
    const shown = validText(t.text);
    if (shown) this.emit({ type: "put", element: this.textBox(t, false) }, board, now);
    else if (t.shown) this.emit({ type: "remove", ids: [t.id] }, board, now);
    t.shown = shown;
  }

  /** Typing in our text box `id`, the caret where `at` is (or at the end):
   * it isn't finished meanwhile, so it doesn't fade. */
  private edit(id: number, at: BoardPoint | null, board: BoardView, now: number) {
    const s = board.get(this.me, id);
    if (!s || s.element.kind !== "text" || s.element.points.length !== 2) return;
    const e = cloneElement(s.element);
    const t: Typing = {
      id,
      look: cloneElement(e),
      anchor: { ...e.points[0] },
      caret: chars(e.text).length,
      mark: null,
      text: e.text,
      before: { ...e, done: true },
      shown: true,
    };
    if (at) t.caret = this.caretAt(t, at);
    this.selected.clear();
    this.tool = "text";
    this.typingBox = t;
    this.showTyping(board, now);
  }

  /** Typing ends: the box goes out finished, and the edit is one step of
   * the history (a new box, a changed one, or one emptied away). Returns
   * the box, if it has text. */
  private finishTyping(board: BoardView, now: number): number | null {
    const t = this.typingBox;
    if (!t) return null;
    this.typingBox = null;
    // By its text, not by `shown`: a box the host cleared while typed in goes
    // out again, with all its text.
    const after = validText(t.text) ? this.textBox(t, true) : null;
    if (after) this.emit({ type: "put", element: cloneElement(after) }, board, now);
    const same = t.before === null ? after === null : after !== null && sameElement(t.before, after);
    if (!same) this.record([{ id: t.id, before: t.before, after }]);
    return after ? after.id : null;
  }

  /** Pasted text as a new box of ours with its corner at `at`, moved in as
   * far as it must to fit the picture, finished and selected. */
  private pasteText(text: string, at: BoardPoint, board: BoardView, now: number) {
    const pasted = text.split("\n");
    if (pasted.length > MAX_TEXT_LINES) this.note = TOO_LONG;
    let cs = chars(pasted.slice(0, MAX_TEXT_LINES).join("\n"));
    if (cs.length > MAX_TEXT_CHARS) {
      cs = cs.slice(0, MAX_TEXT_CHARS);
      this.note = TOO_LONG;
    }
    text = cs.join("");
    if (!validText(text)) return;
    const id = this.newId(board);
    if (id === null) return;
    const look = this.element(id, "text", [], true);
    // As it would be with room to grow, then moved back inside.
    const size = textSize(look.width);
    const lines = text.split("\n");
    const wide = Math.max(0, ...lines.map((l) => this.measured(l))) * size;
    const tall = lines.length * LINE_HEIGHT * size;
    const [ax, ay] = this.uniform(at);
    const x = Math.max(0, Math.min(ax, this.aspect - wide));
    const y = Math.max(0, Math.min(ay, 1 - tall));
    const e = this.textBox(
      { id, look, anchor: this.picture(x, y), caret: 0, mark: null, text, before: null, shown: true },
      true,
    );
    this.selected = new Set([id]);
    this.tool = "select";
    this.perform([{ id, before: null, after: e }], board, now);
  }

  /** Where the caret goes for `at` in the box `t`: the nearest line, then
   * the nearest place between its characters. */
  private caretAt(t: Typing, at: BoardPoint): number {
    const size = textSize(t.look.width);
    const [ax, ay] = this.uniform(t.anchor);
    const [px, py] = this.uniform(at);
    const lines = t.text.split("\n");
    const line = Math.min(Math.max(0, Math.floor((py - ay) / (LINE_HEIGHT * size))), lines.length - 1);
    const before = lines.slice(0, line).reduce((n, l) => n + chars(l).length + 1, 0);
    return before + this.nearest(lines[line], px - ax, size);
  }

  /** The place between `line`'s characters nearest `x` across it (picture
   * heights, its text `size`). */
  // Prefixes only grow, so a binary search: the last place at or before
  // `x`, or the one after it if nearer (a tie keeps the first). A few
  // layouts per drag move, not one per character (the review's finding).
  private nearest(line: string, x: number, size: number): number {
    const cs = chars(line);
    const width = (k: number) => this.measured(cs.slice(0, k).join("")) * size;
    let [lo, hi] = [0, cs.length];
    while (lo < hi) {
      const mid = Math.ceil((lo + hi) / 2);
      if (width(mid) <= x) lo = mid;
      else hi = mid - 1;
    }
    return lo < cs.length && width(lo + 1) - x < x - width(lo) ? lo + 1 : lo;
  }

  /** Whether `at` is within a box, or near enough to pick it. */
  private nearBox([a, b]: [BoardPoint, BoardPoint], at: BoardPoint): boolean {
    const p = this.uniform(at);
    const [ua, ub] = [this.uniform(a), this.uniform(b)];
    return (
      p[0] >= Math.min(ua[0], ub[0]) - TOUCH &&
      p[0] <= Math.max(ua[0], ub[0]) + TOUCH &&
      p[1] >= Math.min(ua[1], ub[1]) - TOUCH &&
      p[1] <= Math.max(ua[1], ub[1]) + TOUCH
    );
  }

  /** The topmost of our text boxes under `at`. */
  private hitText(at: BoardPoint, board: BoardView): number | null {
    const under = board.of(this.me).filter((s) => s.element.kind === "text" && this.touches(s.element, at, true));
    return under.length > 0 ? under[under.length - 1].element.id : null;
  }

  /** A number for a new element, unless we have all the elements we may. */
  private newId(board: BoardView): number | null {
    if (board.of(this.me).length >= MAX_MEMBER_ELEMENTS) {
      this.note = TOO_MANY;
      return null;
    }
    const id = this.nextId;
    this.nextId = Math.max(1, (this.nextId + 1) % 0x1_0000_0000);
    return id;
  }

  /** Applies `op` to our board and queues it, merged with the one before
   * when that is the same element's. */
  private emit(op: BoardOpView, board: BoardView, now: number) {
    board.apply(this.me, [op], now);
    if (this.outbox.length === 0) this.outboxGeneration = this.generation;
    const last = this.outbox[this.outbox.length - 1];
    if (last && merge(last, op)) return;
    this.outbox.push(copyOp(op));
  }

  private laser(g: Gesture, at: BoardPoint, board: BoardView, now: number) {
    if (g.kind !== "laser") return;
    if (g.lastAt !== null && now - g.lastAt < LASER_EVERY_MS) return;
    g.lastAt = now;
    const start = g.fresh;
    g.fresh = false;
    this.emit({ type: "laser", points: [at], start, colour: this.colour }, board, now);
  }

  private eraseAt(at: BoardPoint, removed: BoardElement[], board: BoardView, now: number) {
    const hit = board
      .of(this.me)
      .filter((s) => this.touches(s.element, at, false))
      .map((s) => cloneElement(s.element));
    if (hit.length === 0) return;
    const ids = hit.map((e) => e.id);
    for (const id of ids) this.selected.delete(id);
    this.emit({ type: "remove", ids }, board, now);
    removed.push(...hit);
  }

  /** The topmost of ours under `at`. */
  private hit(at: BoardPoint, board: BoardView): number | null {
    const under = board.of(this.me).filter((s) => this.touches(s.element, at, true));
    return under.length > 0 ? under[under.length - 1].element.id : null;
  }

  /** Our selected elements, in drawing order. */
  private selectedElements(board: BoardView): BoardElement[] {
    return board
      .of(this.me)
      .filter((s) => this.selected.has(s.element.id))
      .map((s) => cloneElement(s.element));
  }

  /** A finished gesture's new elements, as one step. */
  private finish(made: number[], board: BoardView) {
    const changes: Change[] = [];
    for (const id of made) {
      const s = board.get(this.me, id);
      if (s) changes.push({ id, before: null, after: cloneElement(s.element) });
    }
    if (changes.length > 0) this.record(changes);
  }

  private record(step: Change[]) {
    this.undoSteps.push(step);
    this.redoSteps = [];
    // A long session's history, within reason.
    if (this.undoSteps.length > 200) this.undoSteps.shift();
  }

  /** Makes `changes` and records them as one step. */
  private perform(changes: Change[], board: BoardView, now: number) {
    if (changes.length === 0) return;
    for (const c of changes) this.changeTo(c.id, c.after, board, now);
    this.record(changes);
  }

  /** Our element `id` becomes `to` (or goes). */
  private changeTo(id: number, to: BoardElement | null, board: BoardView, now: number) {
    if (to) this.emit({ type: "put", element: cloneElement(to) }, board, now);
    else this.emit({ type: "remove", ids: [id] }, board, now);
  }

  private replaceFrom(before: BoardElement[], after: BoardElement[], board: BoardView, now: number) {
    const changes: Change[] = [];
    before.forEach((b, i) => {
      const a = after[i];
      if (a && !sameElement(a, b)) changes.push({ id: a.id, before: b, after: a });
    });
    this.perform(changes, board, now);
  }

  private undo(board: BoardView, now: number) {
    this.release(null, false, board, now);
    const step = this.undoSteps.pop();
    if (!step) return;
    for (const c of [...step].reverse()) this.changeTo(c.id, c.before, board, now);
    this.selected.clear();
    this.redoSteps.push(step);
  }

  private redo(board: BoardView, now: number) {
    this.release(null, false, board, now);
    const step = this.redoSteps.pop();
    if (!step) return;
    if (step.some((c) => c.before === null) && board.of(this.me).length >= MAX_MEMBER_ELEMENTS) {
      this.note = TOO_MANY;
      this.redoSteps.push(step);
      return;
    }
    for (const c of step) this.changeTo(c.id, c.after, board, now);
    this.selected.clear();
    this.undoSteps.push(step);
  }

  /** `elements` again as new ones of ours, `times` paste steps away, and
   * selected. */
  private pasteElements(elements: BoardElement[], times: number, board: BoardView, now: number) {
    if (elements.length === 0) return;
    const offset = clampOffset(elements, [PASTE_STEP * times, PASTE_STEP * times]);
    const changes: Change[] = [];
    for (const e of shifted(elements, offset)) {
      const id = this.newId(board);
      if (id === null) break;
      changes.push({ id, before: null, after: { ...e, id, done: true } });
    }
    this.selected = new Set(changes.map((c) => c.id));
    this.tool = "select";
    this.perform(changes, board, now);
  }

  /** Where a move by the pointer from `from` to `at` takes `elements`, kept
   * inside the picture. */
  private offset(elements: BoardElement[], from: BoardPoint, at: BoardPoint): [number, number] {
    return clampOffset(elements, [at.x - from.x, at.y - from.y]);
  }

  /** A shape's end with Shift: lines at 15° steps, squares and circles. */
  private constrained(kind: BoardElementKind, start: BoardPoint, at: BoardPoint, shift: boolean): BoardPoint {
    if (!shift) return at;
    const [sx, sy] = this.uniform(start);
    const [ax, ay] = this.uniform(at);
    const [dx, dy] = [ax - sx, ay - sy];
    let ex: number;
    let ey: number;
    if (kind === "line" || kind === "arrow") {
      const len = Math.hypot(dx, dy);
      const step = Math.PI / 12;
      const angle = Math.round(Math.atan2(dy, dx) / step) * step;
      ex = sx + len * Math.cos(angle);
      ey = sy + len * Math.sin(angle);
    } else {
      // Rust's signum: 1 for zero.
      const side = Math.max(Math.abs(dx), Math.abs(dy));
      ex = sx + side * (dx < 0 ? -1 : 1);
      ey = sy + side * (dy < 0 ? -1 : 1);
    }
    return this.picture(ex, ey);
  }

  /** A point in picture heights: x across `aspect`, y across 1. */
  private uniform(p: BoardPoint): [number, number] {
    return [(p.x / FULL) * this.aspect, p.y / FULL];
  }

  private picture(x: number, y: number): BoardPoint {
    return {
      x: clamp(Math.round((x / this.aspect) * FULL), 0, FULL),
      y: clamp(Math.round(y * FULL), 0, FULL),
    };
  }

  private distance(a: BoardPoint, b: BoardPoint): number {
    const [ax, ay] = this.uniform(a);
    const [bx, by] = this.uniform(b);
    return Math.hypot(ax - bx, ay - by);
  }

  /** Whether `at` touches `e`: near its line, or, picking, inside a box or
   * an ellipse. */
  touches(e: BoardElement, at: BoardPoint, picking: boolean): boolean {
    const reach = TOUCH + lineWidth(e.kind, e.width) / 2;
    const p = this.uniform(at);
    const pts = e.points.map((q) => this.uniform(q));
    switch (e.kind) {
      case "pen":
      case "highlighter":
      case "line":
      case "arrow":
        if (pts.length === 1) return dist(p, pts[0]) <= reach;
        for (let i = 1; i < pts.length; i++) if (segmentDistance(p, pts[i - 1], pts[i]) <= reach) return true;
        return false;
      case "rectangle": {
        const [a, b] = pts;
        const [x0, x1] = [Math.min(a[0], b[0]), Math.max(a[0], b[0])];
        const [y0, y1] = [Math.min(a[1], b[1]), Math.max(a[1], b[1])];
        const inside = p[0] >= x0 && p[0] <= x1 && p[1] >= y0 && p[1] <= y1;
        const corners: [number, number][] = [
          [x0, y0],
          [x1, y0],
          [x1, y1],
          [x0, y1],
          [x0, y0],
        ];
        let edge = false;
        for (let i = 1; i < corners.length; i++) {
          if (segmentDistance(p, corners[i - 1], corners[i]) <= reach) edge = true;
        }
        return edge || (picking && inside);
      }
      case "ellipse": {
        const [a, b] = pts;
        const c = [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2];
        const r = [Math.abs((a[0] - b[0]) / 2), Math.abs((a[1] - b[1]) / 2)];
        if (r[0] < 1e-7 || r[1] < 1e-7) return segmentDistance(p, a, b) <= reach;
        const n = Math.hypot((p[0] - c[0]) / r[0], (p[1] - c[1]) / r[1]);
        const near = Math.abs(n - 1) * Math.min(r[0], r[1]) <= reach;
        return near || (picking && n <= 1);
      }
      // Its box, as its author measured it: the eraser too touches text
      // anywhere in it.
      case "text":
        return this.nearBox([e.points[0], e.points[1]], at);
    }
  }
}

const WORD = /[\p{Alphabetic}\p{N}_]/u;
const SPACE = 0;
const BREAK = 3;
/** What a character is for moving by word: a space, part of a word (Rust's
 * `is_alphanumeric`, or `_`), anything else (a run of punctuation is a word
 * of its own), or a line break, which word moves stop at, as text fields
 * do; as the stream window's `class`. */
function charClass(c: string): number {
  return c === "\n" ? BREAK : isSpace(c) ? SPACE : WORD.test(c) ? 1 : 2;
}

/** Ctrl+Right from `at`: past the word there, then the spaces after it, up
 * to a line's end; at a line's end, just past the break. */
function nextWord(cs: string[], at: number): number {
  let i = at;
  if (i < cs.length) {
    const k = charClass(cs[i]);
    if (k === BREAK) return i + 1;
    if (k !== SPACE) while (i < cs.length && charClass(cs[i]) === k) i++;
  }
  while (i < cs.length && charClass(cs[i]) === SPACE) i++;
  return i;
}

/** Ctrl+Left from `at`: back past the spaces, then the word before them,
 * down to a line's start; at a line's start, just before the break. */
function prevWord(cs: string[], at: number): number {
  let i = at;
  while (i > 0 && charClass(cs[i - 1]) === SPACE) i--;
  if (i > 0) {
    const k = charClass(cs[i - 1]);
    if (k === BREAK) return i === at ? i - 1 : i;
    while (i > 0 && charClass(cs[i - 1]) === k) i--;
  }
  return i;
}

/** The word (or run of spaces, or of punctuation) at `at`, for a double
 * click: the run of one kind of character around it, never a line break
 * (an empty line selects nothing; the review's finding). */
function wordAt(cs: string[], at: number): [number, number] {
  if (cs.length === 0) return [0, 0];
  // At the end, or on a space or break just past a word: that word.
  const afterWord = at > 0 && [1, 2].includes(charClass(cs[at - 1]));
  const i = at === cs.length || ([SPACE, BREAK].includes(charClass(cs[at])) && afterWord) ? at - 1 : at;
  const k = charClass(cs[i]);
  if (k === BREAK) return [at, at];
  let [lo, hi] = [i, i + 1];
  while (lo > 0 && charClass(cs[lo - 1]) === k) lo--;
  while (hi < cs.length && charClass(cs[hi]) === k) hi++;
  return [lo, hi];
}

/** The caret's line and its place in that line, both counted from 0, for
 * `caret` characters into `text`. */
function lineCol(text: string, caret: number): [number, number] {
  let line = 0;
  let col = 0;
  for (const c of chars(text).slice(0, caret)) {
    if (c === "\n") {
      line += 1;
      col = 0;
    } else {
      col += 1;
    }
  }
  return [line, col];
}

function copyOp(op: BoardOpView): BoardOpView {
  switch (op.type) {
    case "put":
      return { type: "put", element: cloneElement(op.element) };
    case "append":
    case "laser":
      return { ...op, points: [...op.points] };
    case "remove":
      return { type: "remove", ids: [...op.ids] };
    case "ping":
      return { ...op };
  }
}

/** Folds `op` into `last`, the queued operation before it, when both are
 * about the same element (or trail); `false` if it must go on its own. */
function merge(last: BoardOpView, op: BoardOpView): boolean {
  if (
    last.type === "put" &&
    op.type === "append" &&
    last.element.id === op.id &&
    !last.element.done &&
    last.element.points.length + op.points.length <= MAX_STROKE_POINTS
  ) {
    last.element.points.push(...op.points);
    last.element.done = op.done;
    return true;
  }
  if (
    last.type === "append" &&
    op.type === "append" &&
    last.id === op.id &&
    !last.done &&
    last.points.length + op.points.length <= MAX_STROKE_POINTS
  ) {
    last.points.push(...op.points);
    last.done = op.done;
    return true;
  }
  if (last.type === "put" && op.type === "put" && last.element.id === op.element.id) {
    last.element = cloneElement(op.element);
    return true;
  }
  if (
    last.type === "laser" &&
    op.type === "laser" &&
    !op.start &&
    last.colour === op.colour &&
    last.points.length + op.points.length <= MAX_LASER_POINTS
  ) {
    last.points.push(...op.points);
    return true;
  }
  return false;
}

function clamp(v: number, lo: number, hi: number) {
  return Math.min(hi, Math.max(lo, v));
}

function dist(a: readonly number[], b: readonly number[]) {
  return Math.hypot(a[0] - b[0], a[1] - b[1]);
}

function segmentDistance(p: readonly number[], a: readonly number[], b: readonly number[]) {
  const [dx, dy] = [b[0] - a[0], b[1] - a[1]];
  const len2 = dx * dx + dy * dy;
  if (len2 < 1e-12) return dist(p, a);
  const t = clamp(((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2, 0, 1);
  return dist(p, [a[0] + t * dx, a[1] + t * dy]);
}

/** An element's box, corner to corner, in picture coordinates. */
export function bounds(e: BoardElement): [BoardPoint, BoardPoint] {
  const xs = e.points.map((p) => p.x);
  const ys = e.points.map((p) => p.y);
  if (xs.length === 0) return [{ x: 0, y: 0 }, { x: 0, y: 0 }];
  return [
    { x: Math.min(...xs), y: Math.min(...ys) },
    { x: Math.max(...xs), y: Math.max(...ys) },
  ];
}

/** Whether the box `[lo, hi]` lies within the one `a` to `b` spans. */
function within([lo, hi]: [BoardPoint, BoardPoint], a: BoardPoint, b: BoardPoint) {
  return (
    lo.x >= Math.min(a.x, b.x) && hi.x <= Math.max(a.x, b.x) && lo.y >= Math.min(a.y, b.y) && hi.y <= Math.max(a.y, b.y)
  );
}

/** `want`, cut short so every one of `elements` stays inside the picture. */
function clampOffset(elements: BoardElement[], want: [number, number]): [number, number] {
  if (elements.length === 0) return [0, 0];
  let lo = [Infinity, Infinity];
  let hi = [-Infinity, -Infinity];
  for (const e of elements) {
    const [a, b] = bounds(e);
    lo = [Math.min(lo[0], a.x), Math.min(lo[1], a.y)];
    hi = [Math.max(hi[0], b.x), Math.max(hi[1], b.y)];
  }
  return [clamp(want[0], -lo[0], FULL - hi[0]), clamp(want[1], -lo[1], FULL - hi[1])];
}

function shifted(elements: BoardElement[], [dx, dy]: [number, number]): BoardElement[] {
  return elements.map((e) => ({
    ...e,
    points: e.points.map((p) => ({ x: clamp(p.x + dx, 0, FULL), y: clamp(p.y + dy, 0, FULL) })),
  }));
}

function sameElement(a: BoardElement, b: BoardElement) {
  return (
    a.id === b.id &&
    a.kind === b.kind &&
    a.colour === b.colour &&
    a.width === b.width &&
    a.fades === b.fades &&
    a.stay === b.stay &&
    a.done === b.done &&
    a.text === b.text &&
    a.points.length === b.points.length &&
    a.points.every((p, i) => p.x === b.points[i].x && p.y === b.points[i].y)
  );
}
