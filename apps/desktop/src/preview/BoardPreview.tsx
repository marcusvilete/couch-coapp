// The host's board overlay in the preview (`preview.html?board`, M27b): its
// surface while the host draws, over a stand-in for the game, with what
// others drew, a laser trail and a ping. The clock stands still, so the
// screenshots are alike every time.

import { useCallback } from "react";
import type { BoardElement } from "../bindings/BoardElement";
import type { BoardNewsView } from "../bindings/BoardNewsView";
import { BoardSurface } from "../board/BoardSurface";
import type { BoardState } from "../board/events";
import "../board/board.css";
import "./preview.css";

const ME = "aa".repeat(32);
const BIA = "bb".repeat(32);
const CAIO = "cc".repeat(32);
const NOW = 10_000;

const el = (id: number, kind: BoardElement["kind"], colour: number, points: [number, number][]): BoardElement => ({
  id,
  kind,
  colour,
  width: 1,
  fades: false,
  stay: 4,
  done: true,
  points: points.map(([x, y]) => ({ x, y })),
});

const NEWS: BoardNewsView[] = [
  {
    type: "snapshot",
    generation: 1,
    first: true,
    elements: [
      { author: BIA, element: el(1, "ellipse", 2, [[38_000, 24_000], [50_000, 40_000]]) },
      { author: BIA, element: el(2, "arrow", 2, [[24_000, 50_000], [37_000, 36_000]]) },
      { author: CAIO, element: { ...el(1, "highlighter", 3, [[8_000, 12_000], [20_000, 12_500], [30_000, 12_000]]) } },
      { author: ME, element: el(1, "rectangle", 0, [[52_000, 44_000], [62_000, 56_000]]) },
      {
        author: ME,
        element: el(2, "pen", 0, Array.from({ length: 30 }, (_, i): [number, number] => [44_000 + i * 500, 20_000 + Math.round(Math.sin(i / 4) * 2_000)])),
      },
    ],
  },
  {
    type: "update",
    author: CAIO,
    ops: [
      {
        type: "laser",
        start: true,
        colour: 4,
        points: Array.from({ length: 12 }, (_, i) => ({ x: 12_000 + i * 1_200, y: 30_000 + i * 600 })),
      },
    ],
  },
  { type: "update", author: BIA, ops: [{ type: "ping", at: { x: 44_000, y: 32_000 } }] },
];

const STATE: BoardState = {
  me: ME,
  drawing: true,
  names: [
    [ME, "Ana"],
    [BIA, "Bia"],
    [CAIO, "Caio"],
  ],
};

export function BoardPreview() {
  const subscribe = useCallback((handler: (news: BoardNewsView) => void) => {
    for (const news of NEWS) handler(news);
    return () => {};
  }, []);
  const clock = useCallback(() => NOW, []);
  return (
    <div className="preview-board-game" aria-label="A game, under the board">
      <BoardSurface state={STATE} subscribe={subscribe} send={() => {}} done={() => {}} clock={clock} />
    </div>
  );
}
