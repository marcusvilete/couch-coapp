// The drawing cases (ADR 0013's "shared cases"): what each tool and key does
// to our own elements and what goes to the host. The same cases as the
// stream window's editor (`couch-viewer/src/editor/tests.rs`), case for case.

import { describe, expect, it } from "vitest";
import type { BoardElement } from "../bindings/BoardElement";
import type { BoardOpView } from "../bindings/BoardOpView";
import type { BoardPoint } from "../bindings/BoardPoint";
import { DEFAULT_STAY, type DrawAction, type DrawTool, MAX_MEMBER_ELEMENTS, MAX_STROKE_POINTS } from "../bindings/boardRules";
import { BoardView } from "./board";
import { Editor, marks, validBatch } from "./editor";

const ME = "01".repeat(32);
const BIA = "02".repeat(32);

const p = (x: number, y: number): BoardPoint => ({ x, y });

class Desk {
  editor = new Editor(ME);
  board = new BoardView();
  now = 1_000;
  sent: BoardOpView[] = [];

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
      } else if (!this.editor.pending()) {
        break;
      }
    }
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
      id: 1,
      kind: "rectangle",
      colour: 0,
      width: 0,
      fades: false,
      stay: 4,
      done: true,
      points: [p(10_000, 10_000), p(20_000, 20_000)],
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
      id: 5,
      kind: "pen",
      colour: 0,
      width: 0,
      fades: true,
      stay: 4,
      done: false,
      points: [p(1, 1)],
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
          element: { id: 41, kind: "line", colour: 0, width: 0, fades: false, stay: 4, done: true, points: [p(0, 0), p(1, 1)] },
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
