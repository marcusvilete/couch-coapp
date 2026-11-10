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
  LINE_WIDTHS,
  MAX_BATCH_ITEMS,
  MAX_BATCH_OPS,
  MAX_LASER_POINTS,
  MAX_MEMBER_ELEMENTS,
  MAX_STROKE_POINTS,
  PALETTE,
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
export const TOO_MANY = "You've drawn all you may: erase some first";

/** How the toolbar's one fade button steps: a click goes round, the
 * wheel goes longer or shorter. */
export type FadeStep = "next" | "longer" | "shorter";

/** What a key did. */
export type Keyed = "done" | "leaveDrawing";

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
  | { kind: "marquee"; from: BoardPoint; to: BoardPoint; kept: Set<number> };

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
      return op.element.points.length;
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
  const points = isStroke(e.kind)
    ? e.points.length >= 1 && e.points.length <= MAX_STROKE_POINTS
    : e.points.length === 2 && e.done;
  return e.colour < PALETTE.length && e.width < WIDTH_NAMES.length && FADE_STAYS.includes(e.stay) && points;
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
  private pastes = 0;
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

  /** Drawing begins: with the tool it last drew with. */
  begin() {
    if (this.tool === "select" && this.selected.size === 0) this.tool = this.lastTool;
  }

  /** Drawing ends: whatever was half done is finished or dropped. */
  end(board: BoardView, now: number) {
    this.release(null, false, board, now);
    this.selected.clear();
  }

  setTool(tool: DrawTool, board: BoardView, now: number) {
    this.release(null, false, board, now);
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
    const changed = this.selectedElements(board).map((e) => ({
      ...e,
      colour: colour ?? e.colour,
      width: width ?? e.width,
      fades: fades ?? e.fades,
    }));
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
    const changed = this.selectedElements(board).map((e) => ({ ...e, fades: true, stay }));
    this.replaceFrom(this.selectedElements(board), changed, board, now);
  }

  // ---- The pointer ----

  /** A press of the main button at `at`. */
  press(at: BoardPoint, shift: boolean, board: BoardView, now: number) {
    this.release(null, false, board, now);
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

  /** A middle click: a ping where it was. */
  ping(at: BoardPoint, board: BoardView, now: number) {
    if (this.pingedAt !== null && now - this.pingedAt < PING_EVERY_MS) return;
    this.pingedAt = now;
    this.emit({ type: "ping", at }, board, now);
  }

  // ---- Keys ----

  key(action: DrawAction, shift: boolean, board: BoardView, now: number): Keyed {
    switch (action.type) {
      case "undo":
        this.undo(board, now);
        break;
      case "redo":
        this.redo(board, now);
        break;
      case "copy":
        this.clipboard = this.selectedElements(board);
        break;
      case "paste":
        this.pastes += 1;
        this.paste(this.clipboard, this.pastes, board, now);
        break;
      case "duplicate":
        this.paste(this.selectedElements(board), 1, board, now);
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
    this.sync(board);
  }

  /** Finishes our strokes left open that we aren't drawing (a page loaded
   * again mid-stroke gets that stroke back open). After a clear of ours. */
  finishStrays(board: BoardView, now: number) {
    const drawing = this.gesture?.kind === "stroke" ? this.gesture.id : null;
    const strays = board
      .of(this.me)
      .filter((s) => !s.element.done && s.element.id !== drawing)
      .map((s) => s.element.id);
    for (const id of strays) this.emit({ type: "append", id, points: [], done: true }, board, now);
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
    return { id, kind, colour: this.colour, width: this.width, fades: this.fades, stay: this.stay, done, points: [...points] };
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
  private paste(elements: BoardElement[], times: number, board: BoardView, now: number) {
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
    }
  }
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
    a.points.length === b.points.length &&
    a.points.every((p, i) => p.x === b.points[i].x && p.y === b.points[i].y)
  );
}
