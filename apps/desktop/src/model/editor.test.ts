// The drawing cases (ADR 0013's "shared cases"): what each tool and key does
// to our own elements and what goes to the host. The same cases as the
// stream window's editor (`couch-viewer/src/editor/tests.rs`), case for case.

import { describe, expect, it } from "vitest";
import type { BoardElement } from "../bindings/BoardElement";
import type { BoardOpView } from "../bindings/BoardOpView";
import type { BoardPoint } from "../bindings/BoardPoint";
import {
  DEFAULT_STAY,
  type DrawAction,
  type DrawTool,
  MAX_MEMBER_ELEMENTS,
  MAX_STROKE_POINTS,
  MAX_TEXT_CHARS,
  MAX_TEXT_LINES,
} from "../bindings/boardRules";
import { BoardView } from "./board";
import { Editor, type TextKey, TOO_LONG, cleanText, marks, validBatch, validText } from "./editor";

const ME = "01".repeat(32);
const BIA = "02".repeat(32);

const p = (x: number, y: number): BoardPoint => ({ x, y });

class Desk {
  editor = new Editor(ME);
  board = new BoardView();
  now = 1_000;
  sent: BoardOpView[] = [];
  /** Pictures that went with their bytes (M27e), in order, and where each
   * fell among the batches (the number of operations sent before it). */
  uploaded: [number, BoardElement][] = [];

  constructor() {
    this.editor.setAspect(1920, 1080);
  }

  tool(tool: DrawTool) {
    this.editor.setTool(tool, this.board, this.now);
  }

  later(ms: number) {
    this.now += ms;
  }

  /** A press, drags through the rest, a release at the last. */
  stroke(points: BoardPoint[], shift: boolean) {
    const [first, ...rest] = points;
    this.editor.press(first, shift, this.board, this.now);
    for (const at of rest) {
      this.later(10);
      this.editor.drag(at, shift, this.board, this.now);
    }
    this.editor.release(rest.length > 0 ? rest[rest.length - 1] : null, shift, this.board, this.now);
  }

  key(action: DrawAction) {
    return this.editor.key(action, false, this.board, this.now);
  }

  /** Everything queued goes out, batch by batch, as the page sends it. */
  send() {
    for (;;) {
      this.later(40);
      const batch = this.editor.takeBatch(this.now);
      if (batch) {
        expect(validBatch(batch.ops)).toBe(true);
        this.sent.push(...JSON.parse(JSON.stringify(batch.ops)));
        continue;
      }
      // On the host it is a `put`, taken before what follows.
      const picture = this.editor.takePicture();
      if (picture) {
        const e = JSON.parse(JSON.stringify(picture.element)) as BoardElement;
        this.uploaded.push([this.sent.length, e]);
        this.sent.push({ type: "put", element: e });
      } else if (!this.editor.pending()) {
        break;
      }
    }
  }

  /** A picture `w` by `h` pixels pasted at `at` (M27e). */
  pastePicture(seed: number, w: number, h: number, at: BoardPoint) {
    const id = seed.toString(16).padStart(2, "0").repeat(32);
    this.editor.pastePicture(id, w, h, at, this.board, this.now);
  }

  mine(): BoardElement[] {
    return this.board.of(ME).map((s) => s.element);
  }

  /** What the host would hold of ours, from what we sent. */
  hostsCopy(): BoardElement[] {
    const elements: BoardElement[] = [];
    for (const op of JSON.parse(JSON.stringify(this.sent)) as BoardOpView[]) {
      if (op.type === "put") {
        const i = elements.findIndex((e) => e.id === op.element.id);
        if (i >= 0) elements[i] = op.element;
        else elements.push(op.element);
      } else if (op.type === "append") {
        const e = elements.find((x) => x.id === op.id);
        expect(e).toBeDefined();
        expect(e?.done).toBe(false);
        e?.points.push(...op.points);
        if (e) e.done = op.done;
      } else if (op.type === "remove") {
        for (const id of op.ids) {
          const i = elements.findIndex((e) => e.id === id);
          if (i >= 0) elements.splice(i, 1);
        }
      }
    }
    return elements;
  }
}

/** Elements compared as plain data. */
const plain = (es: BoardElement[]) => JSON.parse(JSON.stringify(es));

/** A picture's box in picture heights, at 16:9: its width over its height. */
function shape(e: BoardElement): number {
  const w = ((e.points[1].x - e.points[0].x) / 65535) * (16 / 9);
  const h = (e.points[1].y - e.points[0].y) / 65535;
  return w / h;
}

describe("the drawing editor's shared cases: pictures (M27e)", () => {
  it("a picture goes with its bytes once and moves as any element", () => {
    const d = new Desk();
    d.tool("pen");
    d.stroke([p(1_000, 1_000), p(2_000, 1_500)], false);
    d.pastePicture(7, 400, 200, p(32_768, 32_768));
    const mine = d.mine();
    const pic = plain([mine[mine.length - 1]])[0] as BoardElement;
    expect(pic.kind).toBe("picture");
    expect(Math.abs(shape(pic) - 2)).toBeLessThan(0.01);
    // Wide: 40% of the picture's width, centred where it was pasted.
    expect(pic.points[1].x - pic.points[0].x).toBe(26_214);
    expect(pic.points[1].y - pic.points[0].y).toBe(23_302);
    expect([...d.editor.selection()]).toEqual([pic.id]);
    expect(d.editor.tool).toBe("select");
    d.send();
    expect(d.uploaded.length).toBe(1);
    expect(d.uploaded[0][1]).toEqual(pic);
    expect(d.uploaded[0][0]).toBeGreaterThan(0);
    // Moved: an ordinary `put`, no bytes again.
    d.stroke([p(32_768, 32_768), p(36_768, 30_768)], false);
    d.send();
    expect(d.uploaded.length).toBe(1);
    expect(d.mine()[d.mine().length - 1].points).not.toEqual(pic.points);
    // Undone: back where it was, still no bytes; undone again: gone; then
    // redone: it comes back with its bytes.
    d.key({ type: "undo" });
    d.key({ type: "undo" });
    expect(d.mine().every((e) => e.kind !== "picture")).toBe(true);
    d.key({ type: "redo" });
    d.send();
    expect(d.uploaded.length).toBe(2);
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });

  it("a box's corner resizes it freely and Shift makes it square", () => {
    const d = new Desk();
    d.tool("rectangle");
    d.stroke([p(10_000, 10_000), p(20_000, 16_000)], false);
    d.tool("select");
    d.stroke([p(15_000, 10_000)], false);
    expect(d.editor.handles(d.board).length).toBe(4);
    // The bottom-right corner, dragged: the top-left stays, any shape.
    d.stroke([p(20_000, 16_000), p(30_000, 18_000)], false);
    expect(plain(d.mine())[0].points).toEqual([p(10_000, 10_000), p(30_000, 18_000)]);
    // The top-left, with Shift: a square from the bottom-right.
    d.stroke([p(10_000, 10_000), p(25_000, 5_000)], true);
    const e = plain(d.mine())[0] as BoardElement;
    expect(e.points[0]).toEqual(p(30_000, 18_000));
    const w = ((e.points[0].x - e.points[1].x) / 65535) * (16 / 9);
    const h = (e.points[0].y - e.points[1].y) / 65535;
    expect(Math.abs(w - h)).toBeLessThan(0.001);
    // A stroke and a text box have none; nor have several selected.
    d.tool("pen");
    d.stroke([p(40_000, 40_000), p(42_000, 41_000)], false);
    d.key({ type: "selectAll" });
    expect(d.editor.handles(d.board)).toEqual([]);
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });

  it("a line's ends move by their handles and Shift steps the angle", () => {
    const d = new Desk();
    d.tool("arrow");
    d.stroke([p(10_000, 30_000), p(30_000, 30_000)], false);
    d.tool("select");
    d.stroke([p(20_000, 30_000)], false);
    expect(d.editor.handles(d.board)).toEqual([p(10_000, 30_000), p(30_000, 30_000)]);
    // Its end moved: its start stays, and it is still an arrow from it.
    d.stroke([p(30_000, 30_000), p(31_000, 20_000)], false);
    expect(plain(d.mine())[0].points).toEqual([p(10_000, 30_000), p(31_000, 20_000)]);
    // Its start, with Shift: level with its end (0° from it).
    d.stroke([p(10_000, 30_000), p(12_000, 20_500)], true);
    const e = plain(d.mine())[0] as BoardElement;
    expect(e.points[1]).toEqual(p(31_000, 20_000));
    expect(Math.abs(e.points[0].y - 20_000)).toBeLessThanOrEqual(1);
    // One step each.
    d.key({ type: "undo" });
    expect(plain(d.mine())[0].points).toEqual([p(10_000, 30_000), p(31_000, 20_000)]);
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });

  it("a picture's corner resizes it keeping its shape inside the picture", () => {
    const d = new Desk();
    d.pastePicture(3, 300, 300, p(32_768, 32_768));
    const pic = plain(d.mine())[0] as BoardElement;
    const [lo, hi] = pic.points;
    // Its bottom-right corner, dragged out: the top-left stays.
    d.stroke([hi, p(hi.x + 3_000, hi.y + 9_000)], false);
    const big = plain(d.mine())[0] as BoardElement;
    expect(big.points[0]).toEqual(lo);
    expect(big.points[1].y).toBeGreaterThan(hi.y + 8_000);
    expect(Math.abs(shape(big) - 1)).toBeLessThan(0.01);
    // Its top-left, dragged past the picture's corner: only as far as fits.
    const bigHi = big.points[1];
    d.stroke([big.points[0], p(0, 0)], false);
    const most = plain(d.mine())[0] as BoardElement;
    expect(most.points[1]).toEqual(bigHi);
    expect(most.points[0].x === 0 || most.points[0].y === 0).toBe(true);
    expect(Math.abs(shape(most) - 1)).toBeLessThan(0.01);
    // Dragged through its fixed corner: as small as a picture may be, never
    // turned inside out.
    d.stroke([most.points[0], p(bigHi.x + 2_000, bigHi.y + 2_000)], false);
    const small = d.mine()[0];
    expect(small.points[0].x < small.points[1].x && small.points[0].y < small.points[1].y).toBe(true);
    d.send();
    expect(d.uploaded.length).toBe(1);
    // One step each.
    d.key({ type: "undo" });
    expect(plain(d.mine())[0]).toEqual(most);
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });

  it("the eraser takes a picture anywhere in it and an undo brings its bytes", () => {
    const d = new Desk();
    d.pastePicture(5, 640, 480, p(20_000, 20_000));
    const pic = plain(d.mine())[0] as BoardElement;
    d.send();
    d.tool("eraser");
    const inside = p(Math.floor((pic.points[0].x + pic.points[1].x) / 2), Math.floor((pic.points[0].y + pic.points[1].y) / 2));
    d.stroke([inside], false);
    expect(d.mine()).toEqual([]);
    d.key({ type: "undo" });
    expect(plain(d.mine())).toEqual([pic]);
    d.send();
    expect(d.uploaded.length).toBe(2);
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });
});

describe("the drawing editor's shared cases", () => {
  it("a pen stroke goes as it is drawn and undoes whole", () => {
    const d = new Desk();
    const points = Array.from({ length: 20 }, (_, i) => p(1000 + i * 400, 2000));
    d.editor.press(points[0], false, d.board, d.now);
    d.later(40);
    // The start goes out at once, while drawing.
    const first = d.editor.takeBatch(d.now);
    expect(first?.ops.length).toBe(1);
    expect(first?.ops[0].type === "put" && !first.ops[0].element.done).toBe(true);
    d.sent.push(...JSON.parse(JSON.stringify(first?.ops)));
    for (const at of points.slice(1)) {
      d.later(10);
      d.editor.drag(at, false, d.board, d.now);
    }
    d.editor.release(points[19], false, d.board, d.now);
    d.send();
    const mine = plain(d.mine());
    expect(mine.length).toBe(1);
    expect(mine[0].done && mine[0].points.length === 20).toBe(true);
    expect(plain(d.hostsCopy())).toEqual(mine);
    d.key({ type: "undo" });
    d.send();
    expect(d.mine()).toEqual([]);
    expect(plain(d.hostsCopy())).toEqual([]);
    d.key({ type: "redo" });
    d.send();
    expect(plain(d.mine())).toEqual(mine);
    expect(plain(d.hostsCopy())).toEqual(mine);
  });

  it("Shift draws squares and lines at fifteen degrees", () => {
    const d = new Desk();
    d.tool("rectangle");
    d.stroke([p(10_000, 10_000), p(20_000, 14_000)], true);
    const [a, b] = d.mine()[0].points;
    // Square on screen: as wide as tall in picture heights (16:9).
    const w = ((b.x - a.x) / 65_535) * (16 / 9);
    const h = (b.y - a.y) / 65_535;
    expect(Math.abs(w - h)).toBeLessThan(0.001);
    d.tool("line");
    d.stroke([p(10_000, 30_000), p(30_000, 30_900)], true);
    const line = d.mine()[1];
    expect(line.points[0].y).toBe(line.points[1].y);
    // Without Shift, as drawn.
    d.tool("arrow");
    d.stroke([p(1, 2), p(3_000, 4_000)], false);
    expect(d.mine()[2].points).toEqual([p(1, 2), p(3_000, 4_000)]);
    expect(d.mine()[2].kind).toBe("arrow");
  });

  it("the eraser takes only ours and undoes", () => {
    const d = new Desk();
    const theirs: BoardElement = {
      picture: null,
      id: 1,
      kind: "rectangle",
      colour: 0,
      width: 0,
      fades: false,
      stay: 4,
      done: true,
      points: [p(10_000, 10_000), p(20_000, 20_000)],
      text: "",
    };
    d.board.apply(BIA, [{ type: "put", element: theirs }], d.now);
    d.tool("rectangle");
    d.stroke([p(10_000, 10_000), p(20_000, 20_000)], false);
    d.tool("eraser");
    d.stroke([p(10_000, 15_000), p(10_050, 15_000)], false);
    expect(d.mine()).toEqual([]);
    expect(d.board.get(BIA, 1)).toBeDefined();
    d.send();
    expect(d.hostsCopy()).toEqual([]);
    d.key({ type: "undo" });
    expect(d.mine().length).toBe(1);
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });

  it("selecting, moving and nudging keep inside the picture", () => {
    const d = new Desk();
    d.tool("rectangle");
    d.stroke([p(1_000, 1_000), p(5_000, 5_000)], false);
    d.tool("select");
    d.stroke([p(3_000, 3_000), p(13_000, 3_000)], false);
    expect(d.mine()[0].points[0]).toEqual(p(11_000, 1_000));
    expect(d.editor.selection().size).toBe(1);
    // Nudged up, past the edge: stops at it.
    d.editor.key({ type: "nudge", dx: 0, dy: -1 }, true, d.board, d.now);
    expect(d.mine()[0].points[0].y).toBe(0);
    d.key({ type: "undo" });
    expect(d.mine()[0].points[0]).toEqual(p(11_000, 1_000));
    d.key({ type: "undo" });
    expect(d.mine()[0].points[0]).toEqual(p(1_000, 1_000));
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
    // A marquee around it selects it; one around nothing, nothing.
    d.stroke([p(60_000, 60_000), p(61_000, 61_000)], false);
    expect(d.editor.selection().size).toBe(0);
    d.stroke([p(0, 0), p(6_000, 6_000)], false);
    expect(d.editor.selection().size).toBe(1);
    // Esc: the selection, then the tool, then drawing.
    expect(d.key({ type: "leave" })).toBe("done");
    expect(d.editor.selection().size).toBe(0);
    expect(d.key({ type: "leave" })).toBe("leaveDrawing");
  });

  it("copy, paste and duplicate make new ones of ours", () => {
    const d = new Desk();
    d.tool("ellipse");
    d.stroke([p(1_000, 1_000), p(4_000, 3_000)], false);
    d.key({ type: "selectAll" });
    d.key({ type: "copy" });
    d.key({ type: "paste" });
    d.key({ type: "paste" });
    const mine = d.mine();
    expect(mine.length).toBe(3);
    expect(new Set(mine.map((e) => e.id)).size).toBe(3);
    expect(mine[1].points).not.toEqual(mine[2].points);
    expect(d.editor.selection().size).toBe(1);
    d.key({ type: "duplicate" });
    expect(d.mine().length).toBe(4);
    d.key({ type: "selectAll" });
    d.key({ type: "delete" });
    expect(d.mine()).toEqual([]);
    d.key({ type: "undo" });
    expect(d.mine().length).toBe(4);
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });

  it("a colour picked recolours the selection", () => {
    const d = new Desk();
    d.tool("line");
    d.stroke([p(1_000, 1_000), p(4_000, 3_000)], false);
    d.key({ type: "selectAll" });
    d.editor.setStyle({ colour: 5, width: 2, fades: true }, d.board, d.now);
    const e = d.mine()[0];
    expect([e.colour, e.width, e.fades]).toEqual([5, 2, true]);
    d.key({ type: "undo" });
    expect(d.mine()[0].colour).toBe(0);
    d.tool("line");
    d.stroke([p(1_000, 1_000), p(4_000, 3_000)], false);
    expect(d.mine()[1].colour).toBe(5);
  });

  it("the laser and pings keep to the host's rates", () => {
    const d = new Desk();
    d.tool("laser");
    d.editor.press(p(0, 0), false, d.board, d.now);
    // A drag every 5 ms for a second: at most 50 points.
    for (let i = 0; i < 200; i++) {
      d.now += 5;
      d.editor.drag(p(i * 100, 0), false, d.board, d.now);
    }
    d.editor.release(p(0, 0), false, d.board, d.now);
    d.send();
    expect(d.sent.reduce((n, op) => n + marks(op), 0)).toBeLessThanOrEqual(51);
    expect(d.sent[0].type === "laser" && d.sent[0].start).toBe(true);
    expect(d.mine()).toEqual([]);
    expect(d.sent.every((op) => op.type === "laser" && op.colour === 0)).toBe(true);
    d.sent = [];
    d.editor.setStyle({ colour: 4 }, d.board, d.now);
    d.editor.press(p(0, 0), false, d.board, d.now);
    d.editor.release(p(0, 0), false, d.board, d.now);
    d.send();
    expect(d.sent.length).toBe(1);
    expect(d.sent[0].type === "laser" && d.sent[0].colour === 4).toBe(true);
    d.sent = [];
    for (let i = 0; i < 10; i++) {
      d.later(100);
      d.editor.ping(p(5, 5), d.board, d.now);
    }
    d.send();
    expect(d.sent.filter((op) => op.type === "ping").length).toBe(2);
  });

  it("a long stroke goes on in a new element", () => {
    const d = new Desk();
    const points = Array.from({ length: MAX_STROKE_POINTS + 100 }, (_, i) =>
      p((i % 600) * 100, Math.floor(i / 600) * 3_000),
    );
    d.editor.press(points[0], false, d.board, d.now);
    for (const at of points.slice(1)) d.editor.drag(at, false, d.board, d.now);
    d.editor.release(null, false, d.board, d.now);
    d.send();
    const mine = plain(d.mine());
    expect(mine.length).toBe(2);
    expect(mine.every((e: BoardElement) => e.done && e.points.length <= MAX_STROKE_POINTS)).toBe(true);
    expect(plain(d.hostsCopy())).toEqual(mine);
    d.key({ type: "undo" });
    expect(d.mine()).toEqual([]);
  });

  it("we stop at our limit", () => {
    const d = new Desk();
    d.tool("line");
    for (let i = 0; i < MAX_MEMBER_ELEMENTS; i++) d.stroke([p(i, 0), p(i, 100)], false);
    expect(d.editor.takeNote()).toBeNull();
    d.stroke([p(0, 0), p(0, 100)], false);
    expect(d.mine().length).toBe(MAX_MEMBER_ELEMENTS);
    expect(d.editor.takeNote()).not.toBeNull();
    d.send();
    expect(d.hostsCopy().length).toBe(MAX_MEMBER_ELEMENTS);
  });

  it("what was made before a clear goes with the generation it was made in", () => {
    const d = new Desk();
    const cleared = (author: string | null, generation: number) =>
      ({ type: "cleared", author, generation }) as const;
    d.tool("line");
    d.stroke([p(0, 0), p(9, 9)], false);
    // A clear of someone else's: ours goes on in our generation.
    d.board.news(cleared(BIA, 7), ME, d.now);
    d.editor.sync(d.board);
    d.later(40);
    expect(d.editor.takeBatch(d.now)?.generation).toBe(0);
    // A clear of ours while a line waits to go: the line goes with it.
    d.stroke([p(0, 0), p(19, 19)], false);
    d.board.news(cleared(ME, 8), ME, d.now);
    d.editor.cleared(d.board);
    d.later(40);
    expect(d.editor.takeBatch(d.now)).toBeNull();
    expect(d.mine()).toEqual([]);
    d.stroke([p(0, 0), p(29, 29)], false);
    d.later(40);
    expect(d.editor.takeBatch(d.now)?.generation).toBe(8);
  });

  it("a stroke left open by a reload is finished", () => {
    const d = new Desk();
    const open: BoardElement = {
      picture: null,
      id: 5,
      kind: "pen",
      colour: 0,
      width: 0,
      fades: true,
      stay: 4,
      done: false,
      points: [p(1, 1)],
      text: "",
    };
    d.board.news(
      { type: "snapshot", generation: 3, first: true, elements: [{ author: ME, element: open }] },
      ME,
      d.now,
    );
    d.editor.cleared(d.board);
    d.editor.finishStrays(d.board, d.now);
    expect(d.mine()[0].done).toBe(true);
    d.later(40);
    const batch = d.editor.takeBatch(d.now);
    expect(batch?.generation).toBe(3);
    expect(batch?.ops).toEqual([{ type: "append", id: 5, points: [], done: true }]);
    // A stroke being drawn now is left alone.
    d.tool("pen");
    d.editor.press(p(0, 0), false, d.board, d.now);
    d.editor.finishStrays(d.board, d.now);
    expect(d.mine().some((e) => !e.done)).toBe(true);
  });

  it("what the host cleared leaves the history", () => {
    const d = new Desk();
    d.tool("pen");
    d.editor.press(p(0, 0), false, d.board, d.now);
    d.editor.drag(p(500, 500), false, d.board, d.now);
    // Cleared mid-stroke: the stroke stops, nothing more of it goes.
    d.board.news({ type: "cleared", author: null, generation: 1 }, ME, d.now);
    d.editor.cleared(d.board);
    d.editor.drag(p(900, 900), false, d.board, d.now);
    d.editor.release(p(900, 900), false, d.board, d.now);
    expect(d.mine()).toEqual([]);
    d.key({ type: "undo" });
    expect(d.mine()).toEqual([]);
    // Our numbers stay new after a snapshot holding ours.
    d.board.apply(
      ME,
      [
        {
          type: "put",
          element: { id: 41, kind: "line", colour: 0, width: 0, fades: false, stay: 4, done: true, points: [p(0, 0), p(1, 1)], text: "", picture: null },
        },
      ],
      d.now,
    );
    d.editor.sync(d.board);
    d.tool("line");
    d.stroke([p(0, 0), p(9, 9)], false);
    expect(d.board.get(ME, 42)).toBeDefined();
  });

  it("picking how long turns fading on and restyles the selection", () => {
    // Wire revision 16 (the author's request in M27b's run).
    const d = new Desk();
    expect(d.editor.stay).toBe(DEFAULT_STAY);
    d.tool("line");
    d.stroke([p(1_000, 1_000), p(4_000, 3_000)], false);
    d.key({ type: "selectAll" });
    d.editor.setStay(15, d.board, d.now);
    expect([d.mine()[0].fades, d.mine()[0].stay]).toEqual([true, 15]);
    d.editor.setStay(5, d.board, d.now);
    expect(d.editor.stay).toBe(15);
    d.key({ type: "undo" });
    expect([d.mine()[0].fades, d.mine()[0].stay]).toEqual([false, 4]);
    d.tool("line");
    d.stroke([p(1_000, 1_000), p(4_000, 3_000)], false);
    expect([d.mine()[1].fades, d.mine()[1].stay]).toEqual([true, 15]);
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });

  it("the fade button goes round and the wheel steps", () => {
    // The author's choice in M27b's run: one fade button.
    const d = new Desk();
    const state = () => [d.editor.fades, d.editor.stay];
    expect(state()).toEqual([false, 4]);
    for (const want of [2, 4, 8, 15]) {
      d.editor.stepFade("next", d.board, d.now);
      expect(state()).toEqual([true, want]);
    }
    d.editor.stepFade("next", d.board, d.now);
    expect(d.editor.fades).toBe(false);
    d.editor.stepFade("shorter", d.board, d.now);
    expect(d.editor.fades).toBe(false);
    d.editor.stepFade("longer", d.board, d.now);
    expect(state()).toEqual([true, 2]);
    for (let i = 0; i < 6; i++) d.editor.stepFade("longer", d.board, d.now);
    expect(state()).toEqual([true, 15]);
    d.editor.stepFade("shorter", d.board, d.now);
    expect(state()).toEqual([true, 8]);
    d.tool("line");
    d.stroke([p(1_000, 1_000), p(4_000, 3_000)], false);
    d.key({ type: "selectAll" });
    d.editor.stepFade("shorter", d.board, d.now);
    expect([d.mine()[0].fades, d.mine()[0].stay]).toEqual([true, 4]);
    d.editor.stepFade("shorter", d.board, d.now);
    d.editor.stepFade("shorter", d.board, d.now);
    expect(d.mine()[0].fades).toBe(false);
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });
});

// ---- Text boxes (M27c): the same cases as `editor/tests.rs`'s ----

class TextDesk extends Desk {
  /** A click with the Text tool. */
  clickText(at: BoardPoint) {
    this.tool("text");
    this.editor.press(at, false, this.board, this.now);
    this.editor.release(at, false, this.board, this.now);
  }

  typeText(text: string) {
    this.editor.typeText(text, this.board, this.now);
  }

  textKey(key: TextKey) {
    this.editor.textKey(key, this.board, this.now);
  }

  /** A key with Shift and Ctrl as given. */
  textKeyWith(key: TextKey, shift: boolean, ctrl: boolean) {
    this.editor.textKey(key, this.board, this.now, shift, ctrl);
  }

  /** The point over character `col` of `line` in the box at `anchor`
   * (medium text, the estimate's half an em a character). */
  overChar(anchor: BoardPoint, line: number, col: number): BoardPoint {
    const size = 0.04;
    const x = Math.fround(((col * 0.5 * size) / (16 / 9)) * 65_535);
    const y = Math.fround((line + 0.5) * 1.25 * size * 65_535);
    return p(anchor.x + Math.round(x), anchor.y + Math.round(y));
  }

  textNow(): string {
    const mine = this.board.of(ME);
    return mine.length > 0 ? mine[mine.length - 1].element.text : "";
  }
}

/** Within one of `want`: the two editors round alike but for halves. */
function near(got: BoardPoint, want: BoardPoint): boolean {
  return Math.abs(got.x - want.x) <= 1 && Math.abs(got.y - want.y) <= 1;
}

/** Rust's f32 arithmetic for the expected corners. */
const f32 = Math.fround;

describe("the drawing editor's shared cases: text boxes", () => {
  it("a text box goes as it is typed and finishes as one step", () => {
    const d = new TextDesk();
    d.clickText(p(10_000, 20_000));
    expect(d.editor.typing()).toBe(true);
    d.send();
    expect(d.sent).toEqual([]);
    d.typeText("Hi");
    d.send();
    expect(d.sent.length).toBe(1);
    const first = d.sent[0];
    expect(first.type === "put" && first.element.text === "Hi" && !first.element.done).toBe(true);
    d.textKey("enter");
    d.typeText("there");
    d.textKey("finish");
    expect(d.editor.typing()).toBe(false);
    d.send();
    const mine = plain(d.mine());
    expect(mine.length).toBe(1);
    const e = mine[0];
    expect(e.done && e.kind === "text" && e.text === "Hi\nthere").toBe(true);
    // Its box: as wide as "there" (half an em a character, medium: 0.04 of
    // the height) and two lines tall, from where it was clicked.
    expect(e.points[0]).toEqual(p(10_000, 20_000));
    const wide = f32(((5 * 0.5 * 0.04) / (16 / 9)) * 65_535);
    const tall = f32(2 * 1.25 * 0.04 * 65_535);
    expect(near(e.points[1], p(10_000 + Math.round(wide), 20_000 + Math.round(tall)))).toBe(true);
    expect(plain(d.hostsCopy())).toEqual(mine);
    // Finished with Esc: selected, to move at once.
    expect(d.editor.tool).toBe("select");
    expect(d.editor.selection().size).toBe(1);
    // One step: undone whole, and back.
    d.key({ type: "undo" });
    expect(d.mine()).toEqual([]);
    d.key({ type: "redo" });
    expect(plain(d.mine())).toEqual(mine);
    d.send();
    expect(plain(d.hostsCopy())).toEqual(mine);
  });

  it("the caret moves and keys edit where it is", () => {
    const d = new TextDesk();
    d.clickText(p(1_000, 1_000));
    d.typeText("abc\nde");
    const text = () => d.board.of(ME)[0].element.text;
    d.textKey("up");
    d.typeText("X");
    expect(text()).toBe("abXc\nde");
    d.textKey("end");
    d.textKey("down");
    d.typeText("Y");
    expect(text()).toBe("abXc\ndeY");
    d.textKey("home");
    d.textKey("backspace");
    expect(text()).toBe("abXcdeY");
    d.textKey("delete");
    d.textKey("left");
    d.textKey("left");
    d.textKey("delete");
    expect(text()).toBe("abceY");
    // Tabs as spaces, control characters never.
    d.textKey("end");
    d.typeText("\t!\u0007");
    expect(text()).toBe("abceY    !");
    // Accents and wide characters are one character each.
    d.textKey("home");
    d.typeText("é😀");
    d.textKey("left");
    d.textKey("backspace");
    expect(text()).toBe("😀abceY    !");
    d.textKey("finish");
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });

  it("a box emptied goes and spaces alone never go", () => {
    const d = new TextDesk();
    d.clickText(p(1_000, 1_000));
    d.typeText("  ");
    d.send();
    expect(d.sent).toEqual([]);
    d.typeText("x");
    d.send();
    expect(d.hostsCopy().length).toBe(1);
    for (let i = 0; i < 3; i++) d.textKey("backspace");
    d.send();
    expect(d.hostsCopy()).toEqual([]);
    expect(d.mine()).toEqual([]);
    d.textKey("finish");
    // A new box that came to nothing leaves no step to undo.
    d.tool("rectangle");
    d.stroke([p(1_000, 1_000), p(2_000, 2_000)], false);
    d.key({ type: "undo" });
    d.key({ type: "undo" });
    expect(d.mine()).toEqual([]);
    d.key({ type: "redo" });
    expect(d.mine().length).toBe(1);
  });

  it("editing a box changes it as one step", () => {
    const d = new TextDesk();
    d.clickText(p(10_000, 10_000));
    d.typeText("Hello");
    d.textKey("finish");
    const before = plain(d.mine());
    // Double-clicked with Select, at the end of its line: typing in it, the
    // caret there, unfinished meanwhile.
    d.tool("select");
    const at = p(before[0].points[1].x, 10_100);
    d.stroke([at], false);
    d.stroke([at], false);
    d.editor.doubleClick(at, d.board, d.now);
    expect(d.editor.typing()).toBe(true);
    expect(d.mine()[0].done).toBe(false);
    d.typeText("!");
    expect(d.mine()[0].text).toBe("Hello!");
    // A click inside it moves the caret: to the start here.
    d.editor.press(p(10_000, 10_100), false, d.board, d.now);
    d.typeText(">");
    expect(d.mine()[0].text).toBe(">Hello!");
    // A click away finishes it, the tool staying.
    d.editor.press(p(60_000, 60_000), false, d.board, d.now);
    expect(d.editor.typing()).toBe(false);
    expect(d.editor.tool).toBe("text");
    expect(d.mine()[0].done).toBe(true);
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
    d.key({ type: "undo" });
    expect(plain(d.mine())).toEqual(before);
    d.key({ type: "redo" });
    expect(d.mine()[0].text).toBe(">Hello!");
    // The Text tool's click on our box edits it too; on theirs, a new one.
    const theirs: BoardElement = {
      picture: null,
      id: 9,
      kind: "text",
      colour: 0,
      width: 1,
      fades: false,
      stay: 4,
      done: true,
      points: [p(40_000, 40_000), p(50_000, 45_000)],
      text: "Bia",
    };
    d.board.apply(BIA, [{ type: "put", element: theirs }], d.now);
    d.clickText(p(45_000, 42_000));
    d.typeText("mine");
    d.textKey("finish");
    expect(d.board.get(BIA, 9)?.element.text).toBe("Bia");
    expect(d.mine().length).toBe(2);
  });

  it("text stops at its limit", () => {
    const d = new TextDesk();
    d.clickText(p(1_000, 1_000));
    d.typeText("a".repeat(MAX_TEXT_CHARS - 1));
    d.typeText("bcd");
    const e = d.mine()[0];
    expect(Array.from(e.text).length).toBe(MAX_TEXT_CHARS);
    expect(e.text.endsWith("b")).toBe(true);
    expect(d.editor.takeNote()).toBe(TOO_LONG);
    expect(TOO_LONG).toContain(String(MAX_TEXT_CHARS));
    d.textKey("enter");
    expect(Array.from(d.mine()[0].text).length).toBe(MAX_TEXT_CHARS);
    d.textKey("finish");
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });

  it("pasted text becomes a box and our own copy stays ours", () => {
    const d = new TextDesk();
    d.tool("rectangle");
    d.stroke([p(1_000, 1_000), p(4_000, 3_000)], false);
    d.key({ type: "selectAll" });
    // No text boxes in it: the clipboard gets nothing, and a paste of
    // nothing is our copy.
    expect(d.editor.copy(d.board)).toBe("");
    d.editor.paste("", null, d.board, d.now);
    expect(d.mine().length).toBe(2);
    expect(d.mine()[1].kind).toBe("rectangle");
    // Text copied elsewhere since: a box at the pointer, cleaned, finished
    // and selected, one step.
    d.editor.paste("hello\r\nworld\t!\n\n", p(20_000, 20_000), d.board, d.now);
    const e = plain(d.mine())[2];
    expect(e.text).toBe("hello\nworld    !");
    expect(e.done).toBe(true);
    expect(e.points[0]).toEqual(p(20_000, 20_000));
    expect([...d.editor.selection()]).toEqual([e.id]);
    expect(d.editor.tool).toBe("select");
    d.key({ type: "undo" });
    expect(d.mine().length).toBe(2);
    d.key({ type: "redo" });
    // Copied, its text goes on the clipboard; that coming back (the
    // clipboard's line breaks aside) pastes our copy, not a new box.
    d.key({ type: "selectAll" });
    expect(d.editor.copy(d.board)).toBe("hello\nworld    !");
    d.editor.paste("hello\r\nworld    !", null, d.board, d.now);
    expect(d.mine().length).toBe(6);
    // Near the edge it moves in, whole.
    d.editor.paste("far right", p(65_000, 65_000), d.board, d.now);
    const far = d.mine()[d.mine().length - 1];
    expect(far.text).toBe("far right");
    // Its whole size (nine characters, one line), ending at the edges.
    const wide = Math.round(f32(((9 * 0.5 * 0.04) / (16 / 9)) * 65_535));
    const tall = Math.round(f32(1.25 * 0.04 * 65_535));
    expect(near(far.points[0], p(65_535 - wide, 65_535 - tall))).toBe(true);
    // Too long: cut, and said.
    d.editor.paste("z".repeat(600), null, d.board, d.now);
    expect(d.mine()[d.mine().length - 1].text.length).toBe(MAX_TEXT_CHARS);
    expect(d.editor.takeNote()).toBe(TOO_LONG);
    // While typing, it goes into the box, at the caret.
    d.clickText(p(1_000, 30_000));
    d.typeText("ab");
    d.textKey("left");
    d.editor.paste("X\r\nY", null, d.board, d.now);
    expect(d.mine()[d.mine().length - 1].text).toBe("aX\nYb");
    d.textKey("finish");
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });

  it("a text box left open is finished whole and a clear ends typing", () => {
    const d = new TextDesk();
    // As a page loaded again finds it: being typed.
    const open: BoardElement = {
      picture: null,
      id: 3,
      kind: "text",
      colour: 0,
      width: 1,
      fades: true,
      stay: 4,
      done: false,
      points: [p(1, 1), p(5_000, 5_000)],
      text: "half",
    };
    d.board.apply(ME, [{ type: "put", element: open }], d.now);
    d.sent.push(JSON.parse(JSON.stringify({ type: "put", element: open })));
    d.editor.cleared(d.board);
    d.editor.finishStrays(d.board, d.now);
    d.send();
    const last = d.sent[d.sent.length - 1];
    // The box again, finished (an append is a stroke's alone).
    expect(last.type === "put" && last.element.done && last.element.text === "half").toBe(true);
    expect(d.mine()[0].done).toBe(true);
    // The host clears ours while we type (its resync after a refused batch,
    // say): typing goes on, and the box comes back whole at the next key,
    // its old self gone from the history (the review's finding).
    d.clickText(p(30_000, 30_000));
    d.typeText("not gone");
    d.board.news({ type: "cleared", generation: 9, author: ME }, ME, d.now);
    d.editor.cleared(d.board);
    expect(d.editor.typing()).toBe(true);
    expect(d.mine()).toEqual([]);
    d.typeText("!");
    expect(d.mine()[0].text).toBe("not gone!");
    // Cleared again, then finished at once: it goes out with all its text.
    d.board.news({ type: "cleared", generation: 10, author: ME }, ME, d.now);
    d.editor.cleared(d.board);
    d.textKey("finish");
    expect(d.mine()[0].done && d.mine()[0].text === "not gone!").toBe(true);
    d.send();
    expect(d.hostsCopy()[d.hostsCopy().length - 1].text).toBe("not gone!");
    // One step, new: undone, it goes; nothing older comes back.
    d.key({ type: "undo" });
    expect(d.mine()).toEqual([]);
  });

  it("a size picked for a selected box measures it again", () => {
    const d = new TextDesk();
    d.clickText(p(1_000, 1_000));
    d.typeText("Hello");
    d.textKey("finish");
    const small = { ...d.mine()[0].points[1] };
    d.editor.setStyle({ width: 2 }, d.board, d.now);
    const e = plain(d.mine())[0];
    expect(e.points[0]).toEqual(p(1_000, 1_000));
    const wide = Math.round(f32(((5 * 0.5 * 0.065) / (16 / 9)) * 65_535));
    const tall = Math.round(f32(1.25 * 0.065 * 65_535));
    expect(e.points[1].x > small.x).toBe(true);
    expect(near(e.points[1], p(1_000 + wide, 1_000 + tall))).toBe(true);
    d.key({ type: "undo" });
    expect(d.mine()[0].points[1]).toEqual(small);
  });

  it("lines stop at their limit and only select double clicks into a box", () => {
    const d = new TextDesk();
    d.clickText(p(1_000, 1_000));
    d.typeText("a\n".repeat(MAX_TEXT_LINES + 5));
    const text = d.mine()[0].text;
    expect(text.split("\n").length).toBe(MAX_TEXT_LINES);
    expect(d.editor.takeNote()).toBe(TOO_LONG);
    d.textKey("enter");
    expect(d.mine()[0].text).toBe(text);
    d.textKey("finish");
    // Pasted, the lines past the last are cut.
    d.editor.paste("b\n".repeat(80), p(30_000, 1_000), d.board, d.now);
    expect(d.mine()[d.mine().length - 1].text.split("\n").length).toBe(MAX_TEXT_LINES);
    // A double click with the pen draws; it doesn't edit.
    d.tool("pen");
    const at = p(1_200, 1_500);
    d.stroke([at], false);
    d.stroke([at], false);
    d.editor.doubleClick(at, d.board, d.now);
    expect(d.editor.typing()).toBe(false);
    expect(d.editor.tool).toBe("pen");
    // Ctrl+C with nothing selected leaves the clipboard alone.
    d.key({ type: "leave" });
    expect(d.editor.copy(d.board)).toBe(null);
  });

  it("a style picked while typing restyles the box", () => {
    const d = new TextDesk();
    d.clickText(p(1_000, 1_000));
    d.typeText("big");
    const small = { ...d.mine()[0].points[1] };
    d.editor.setStyle({ colour: 4, width: 2 }, d.board, d.now);
    const e = plain(d.mine())[0];
    expect([e.colour, e.width]).toEqual([4, 2]);
    expect(e.points[1].x > small.x && e.points[1].y > small.y).toBe(true);
    d.editor.stepFade("next", d.board, d.now);
    expect(d.mine()[0].fades && !d.mine()[0].done).toBe(true);
    d.textKey("finish");
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
    // The eraser takes a box anywhere in it.
    d.tool("eraser");
    const [a, b] = e.points;
    const mid = p(Math.floor(a.x / 2) + Math.floor(b.x / 2), Math.floor(a.y / 2) + Math.floor(b.y / 2));
    d.stroke([mid, mid], false);
    expect(d.mine()).toEqual([]);
  });

  it("text boxes are checked and pasted text cleaned as the host does", () => {
    // `couch_proto::board`'s `text_boxes_are_checked` and
    // `pasted_text_is_cleaned`, for this editor's copies.
    expect(validText("a")).toBe(true);
    expect(validText("ç".repeat(MAX_TEXT_CHARS))).toBe(true);
    expect(validText("a".repeat(MAX_TEXT_CHARS + 1))).toBe(false);
    expect(validText("")).toBe(false);
    expect(validText("   ")).toBe(false);
    expect(validText("two\nlines")).toBe(true);
    for (const bad of ["a\tb", "a\rb", "a\u007f", "a\u0085", "a\u0000"]) expect(validText(bad)).toBe(false);
    expect(cleanText("a\r\nb\rc\n")).toBe("a\nb\nc");
    expect(cleanText("\tx\u0007y \n\n  ")).toBe("    xy");
    expect(cleanText("é\u0085ü")).toBe("éü");
    expect(cleanText(" \n ")).toBe("");
    expect(validText(cleanText("  indented\r\n\tline\u0000 "))).toBe(true);
    expect(validText(Array(MAX_TEXT_LINES).fill("x").join("\n"))).toBe(true);
    expect(validText(Array(MAX_TEXT_LINES + 1).fill("x").join("\n"))).toBe(false);
    // As Rust's: a byte order mark isn't a space (JavaScript's own `trim`
    // says it is), and a lone surrogate, which no Rust string holds, goes.
    const bom = String.fromCharCode(0xfeff);
    expect(validText(bom)).toBe(true);
    expect(cleanText(`a${bom}`)).toBe(`a${bom}`);
    expect(cleanText(`a${String.fromCharCode(0xd800)}b${String.fromCharCode(0xdc00)}`)).toBe("ab");
  });
});

// ---- Editing text as text fields do (the author's request, M27c): the
// same cases as `editor/tests.rs`'s ----

describe("the drawing editor's shared cases: editing text", () => {
  it("words and the selection move as in text fields", () => {
    const d = new TextDesk();
    d.clickText(p(1_000, 1_000));
    d.typeText("Hello big world");
    // Ctrl+Left and Ctrl+Right by word.
    d.textKeyWith("left", false, true);
    d.textKeyWith("left", false, true);
    d.typeText("<");
    expect(d.textNow()).toBe("Hello <big world");
    d.textKey("backspace");
    d.textKeyWith("right", false, true);
    d.typeText(">");
    expect(d.textNow()).toBe("Hello big >world");
    d.textKey("backspace");
    // Shift+Ctrl+Right selects the word; typing replaces it.
    d.textKeyWith("right", true, true);
    expect(d.editor.copyText()).toBe("world");
    d.typeText("there");
    expect(d.textNow()).toBe("Hello big there");
    // Ctrl+Backspace takes a word, then the spaces and the word before.
    d.textKeyWith("backspace", false, true);
    expect(d.textNow()).toBe("Hello big ");
    d.textKeyWith("backspace", false, true);
    expect(d.textNow()).toBe("Hello ");
    // Ctrl+Home, then Ctrl+Delete: a word and the spaces after it, then
    // punctuation and its spaces (as Windows' text fields).
    d.typeText("a, b");
    d.textKeyWith("home", false, true);
    d.textKeyWith("delete", false, true);
    expect(d.textNow()).toBe("a, b");
    d.textKeyWith("end", false, true);
    d.textKeyWith("left", false, true);
    d.textKeyWith("left", false, true);
    d.textKeyWith("delete", false, true);
    expect(d.textNow()).toBe("ab");
    // Shift+Home selects to the line's start; Left collapses to its start.
    d.textKeyWith("end", false, false);
    d.textKeyWith("home", true, false);
    expect(d.editor.copyText()).toBe("ab");
    d.textKey("left");
    expect(d.editor.copyText()).toBe(null);
    d.typeText("[");
    expect(d.textNow()).toBe("[ab");
    // Ctrl+A and Ctrl+X: all of it out, and the box with it.
    d.textKey("selectAll");
    expect(d.editor.cutText(d.board, d.now)).toBe("[ab");
    expect(d.mine()).toEqual([]);
    d.typeText("again");
    d.textKey("finish");
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });

  it("up and down keep the place across and selections replace", () => {
    const d = new TextDesk();
    d.clickText(p(1_000, 1_000));
    d.typeText("abcdef\nab");
    // Up from the end of "ab": the same place across, after "ab".
    d.textKey("up");
    d.typeText("|");
    expect(d.textNow()).toBe("ab|cdef\nab");
    d.textKey("backspace");
    // Down from the end of the long line: the short one's end.
    d.textKey("end");
    d.textKey("down");
    d.typeText("!");
    expect(d.textNow()).toBe("abcdef\nab!");
    // Down on the last line: its end; Up on the first: the start.
    d.textKey("up");
    d.textKey("up");
    d.typeText("^");
    expect(d.textNow()).toBe("^abcdef\nab!");
    // Shift+Down selects across the line break; typing replaces it all.
    d.textKeyWith("down", true, false);
    d.textKeyWith("end", true, false);
    expect(d.editor.copyText()).toBe("abcdef\nab!");
    d.typeText("one");
    expect(d.textNow()).toBe("^one");
    d.textKey("finish");
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });

  it("dragging and double clicking select text", () => {
    const d = new TextDesk();
    const anchor = p(1_000, 1_000);
    d.clickText(anchor);
    d.typeText("Go left now\nthen jump");
    // A drag inside the box selects, from where it began.
    const [from, to] = [d.overChar(anchor, 0, 3), d.overChar(anchor, 0, 7)];
    d.editor.press(from, false, d.board, d.now);
    d.editor.drag(to, false, d.board, d.now);
    d.editor.release(to, false, d.board, d.now);
    expect(d.editor.copyText()).toBe("left");
    // Shift and a click extend it.
    const end = d.overChar(anchor, 1, 4);
    d.editor.press(end, true, d.board, d.now);
    d.editor.release(end, true, d.board, d.now);
    expect(d.editor.copyText()).toBe("left now\nthen");
    // A double click selects the word there; a click alone none.
    const jump = d.overChar(anchor, 1, 6);
    d.editor.press(jump, false, d.board, d.now);
    d.editor.doubleClick(jump, d.board, d.now);
    d.editor.release(jump, false, d.board, d.now);
    expect(d.editor.copyText()).toBe("jump");
    // Painted: one box for the word, on the second line.
    const cursor = d.editor.textCursor();
    expect(cursor?.selection.length).toBe(1);
    expect((cursor?.selection[0][0].y ?? 0) > anchor.y + 2_000).toBe(true);
    d.typeText("run");
    expect(d.textNow()).toBe("Go left now\nthen run");
    d.textKey("finish");
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });
});

describe("the drawing editor's shared cases: line breaks", () => {
  it("word moves stop at line breaks and an empty line selects nothing", () => {
    const d = new TextDesk();
    const anchor = p(1_000, 1_000);
    d.clickText(anchor);
    d.typeText("abc\n\n  def");
    const mark = () => {
      d.typeText("|");
      const text = d.textNow();
      d.textKey("backspace");
      return text;
    };
    // Ctrl+Right: the word's end, then past each break, then the indent.
    d.textKeyWith("home", false, true);
    d.textKeyWith("right", false, true);
    expect(mark()).toBe("abc|\n\n  def");
    d.textKeyWith("right", false, true);
    expect(mark()).toBe("abc\n|\n  def");
    d.textKeyWith("right", false, true);
    d.textKeyWith("right", false, true);
    expect(mark()).toBe("abc\n\n  |def");
    // Ctrl+Left: the line's start, then back over each break.
    d.textKeyWith("left", false, true);
    expect(mark()).toBe("abc\n\n|  def");
    d.textKeyWith("left", false, true);
    expect(mark()).toBe("abc\n|\n  def");
    // A double click on the empty line selects nothing; at a line's end, the
    // word before it.
    const empty = d.overChar(anchor, 1, 0);
    d.editor.press(empty, false, d.board, d.now);
    d.editor.doubleClick(empty, d.board, d.now);
    d.editor.release(empty, false, d.board, d.now);
    expect(d.editor.copyText()).toBe(null);
    const end = d.overChar(anchor, 0, 3);
    d.editor.press(end, false, d.board, d.now);
    d.editor.doubleClick(end, d.board, d.now);
    d.editor.release(end, false, d.board, d.now);
    expect(d.editor.copyText()).toBe("abc");
    // Ctrl+Delete at a line's end takes the break alone.
    d.textKey("end");
    d.textKeyWith("delete", false, true);
    expect(d.textNow()).toBe("abc\n  def");
    d.textKey("finish");
    d.send();
    expect(plain(d.hostsCopy())).toEqual(plain(d.mine()));
  });
});
