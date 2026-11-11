// The board model's cases, as the stream window's (`couch-viewer/src/board.rs`).

import { describe, expect, it } from "vitest";
import type { BoardElement } from "../bindings/BoardElement";
import { FADE_OUT_MS, LASER_MS, PING_MS } from "../bindings/boardRules";

/** The default stay, as `stroke` makes it. */
const FADE_STAY_MS = 4000;
import { alpha, BoardView } from "./board";

const ANA = "01".repeat(32);
const BIA = "02".repeat(32);

const stroke = (id: number, done: boolean, fades: boolean): BoardElement => ({
  id,
  kind: "pen",
  colour: 0,
  width: 0,
  fades,
  stay: 4,
  done,
  points: [{ x: 1, y: 1 }],
  text: "",
});

describe("the board as the overlay shows it", () => {
  it("a fading element stays, then fades from when it finished here", () => {
    const t0 = 1000;
    const board = new BoardView();
    board.apply(ANA, [{ type: "put", element: stroke(1, false, true) }], t0);
    expect(board.moving(t0 + 60_000)).toBe(false);
    const done = t0 + 2000;
    board.apply(ANA, [{ type: "append", id: 1, points: [{ x: 2, y: 2 }], done: true }], done);
    const s = board.elements[0];
    expect(alpha(s, done + FADE_STAY_MS)).toBe(1);
    expect(alpha(s, done + FADE_STAY_MS + FADE_OUT_MS / 2)).toBeCloseTo(0.5, 2);
    expect(board.moving(done + FADE_STAY_MS)).toBe(false);
    // Nothing moves while it stays: the page must wake itself when it
    // starts to fade (the host's board fading, from the author's run).
    expect(board.nextFade(done + 1000)).toBe(done + FADE_STAY_MS);
    expect(board.nextFade(done + FADE_STAY_MS)).toBeNull();
    expect(board.moving(done + FADE_STAY_MS + FADE_OUT_MS / 2)).toBe(true);
    board.prune(done + FADE_STAY_MS + FADE_OUT_MS);
    expect(board.elements).toEqual([]);
  });

  it("the host replaces, clears and refuses", () => {
    const now = 1000;
    const board = new BoardView();
    board.apply(ANA, [{ type: "put", element: stroke(1, true, false) }], now);
    board.apply(BIA, [{ type: "put", element: stroke(1, true, false) }], now);
    expect(board.elements.length).toBe(2);
    board.news({ type: "cleared", author: BIA, generation: 4 }, ANA, now);
    expect(board.generation).toBe(0);
    board.apply(BIA, [{ type: "put", element: stroke(1, true, false) }], now);
    board.news({ type: "cleared", author: ANA, generation: 4 }, ANA, now);
    expect(board.elements.length).toBe(1);
    expect(board.generation).toBe(4);
    board.news(
      { type: "snapshot", generation: 5, first: true, elements: [{ author: ANA, element: stroke(7, true, false) }] },
      ANA,
      now,
    );
    expect(board.elements.length).toBe(1);
    expect(board.get(ANA, 7)).toBeDefined();
    board.news({ type: "update", author: BIA, ops: [{ type: "ping", at: { x: 3, y: 3 } }] }, ANA, now);
    expect(board.pings.length).toBe(1);
    expect(board.moving(now)).toBe(true);
    board.prune(now + PING_MS);
    expect(board.moving(now + PING_MS)).toBe(false);
    expect(board.news({ type: "refused", message: "Drawing too fast" }, ANA, now)).toBe("Drawing too fast");
    board.news({ type: "cleared", author: null, generation: 6 }, ANA, now);
    expect(board.elements).toEqual([]);
  });

  it("a laser trail fades within its time", () => {
    const now = 1000;
    const board = new BoardView();
    board.apply(
      ANA,
      [
        {
          type: "laser",
          points: [
            { x: 1, y: 1 },
            { x: 2, y: 2 },
          ],
          start: true,
          colour: 5,
        },
      ],
      now,
    );
    const trail = board.lasers.get(ANA) ?? [];
    expect(trail.every((p) => p.colour === 5)).toBe(true);
    expect(trail[0].start && !trail[1].start).toBe(true);
    board.prune(now + LASER_MS);
    expect(board.lasers.size).toBe(0);
  });
});
