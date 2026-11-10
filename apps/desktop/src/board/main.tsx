// The host's board overlay page (M27b): what's drawn on the stream, over the
// game, in a transparent window the engine makes, lays over the captured
// window and gives the focus only while the host draws (its own key at the
// PC). The page hears the board from the main window and hands it the host's
// drawing: it calls no engine command, and loads nothing of the main
// window's (no theme, no global styles).
import { emit, listen } from "@tauri-apps/api/event";
import { StrictMode, useCallback, useEffect, useState } from "react";
import ReactDOM from "react-dom/client";
import type { BoardNewsView } from "../bindings/BoardNewsView";
import { BoardSurface } from "./BoardSurface";
import { BOARD_DONE, BOARD_NEWS, BOARD_OPS, BOARD_READY, BOARD_STATE, type BoardBatch, type BoardState } from "./events";
import "./board.css";
import "./page.css";

/** News that came before the surface listened, kept in order. */
let pending: BoardNewsView[] = [];
let handler: ((news: BoardNewsView) => void) | null = null;
let showState: (state: BoardState) => void = () => {};
let lastState: BoardState | null = null;

function subscribe(next: (news: BoardNewsView) => void) {
  handler = next;
  const queued = pending;
  pending = [];
  for (const news of queued) next(news);
  return () => {
    if (handler === next) handler = null;
  };
}

const send = (batch: BoardBatch) => void emit(BOARD_OPS, batch).catch(() => {});
const done = () => void emit(BOARD_DONE).catch(() => {});

function Board() {
  const [state, setState] = useState<BoardState | null>(lastState);
  useEffect(() => {
    showState = setState;
    if (lastState) setState(lastState);
  }, []);
  const sub = useCallback(subscribe, []);
  return <BoardSurface state={state} subscribe={sub} send={send} done={done} />;
}

// Listening first, then asking: what the main window sends isn't missed.
void Promise.all([
  listen<BoardState>(BOARD_STATE, (e) => {
    lastState = e.payload;
    showState(e.payload);
  }),
  listen<BoardNewsView>(BOARD_NEWS, (e) => {
    if (handler) handler(e.payload);
    else pending.push(e.payload);
  }),
]).then(() => {
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <StrictMode>
      <Board />
    </StrictMode>,
  );
  void emit(BOARD_READY).catch(() => {});
});
