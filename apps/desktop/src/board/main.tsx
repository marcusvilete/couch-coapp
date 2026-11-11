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
import {
  BOARD_DONE,
  BOARD_NEWS,
  BOARD_OPS,
  BOARD_PICTURE,
  BOARD_PICTURE_UP,
  BOARD_READY,
  BOARD_STATE,
  type BoardBatch,
  type BoardPicture,
  type BoardPictureUp,
  type BoardState,
} from "./events";
import "./board.css";
import "./page.css";

/** News that came before the surface listened, kept in order. */
let pending: BoardNewsView[] = [];
let handler: ((news: BoardNewsView) => void) | null = null;
let showState: (state: BoardState) => void = () => {};
let lastState: BoardState | null = null;

/** Pictures that came before the surface listened (M27e). */
let pendingPictures: BoardPicture[] = [];
let pictureHandler: ((picture: BoardPicture) => void) | null = null;

function subscribePictures(next: (picture: BoardPicture) => void) {
  pictureHandler = next;
  const queued = pendingPictures;
  pendingPictures = [];
  for (const picture of queued) next(picture);
  return () => {
    if (pictureHandler === next) pictureHandler = null;
  };
}

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
const sendPicture = (up: BoardPictureUp) => void emit(BOARD_PICTURE_UP, up).catch(() => {});

function Board() {
  const [state, setState] = useState<BoardState | null>(lastState);
  useEffect(() => {
    showState = setState;
    if (lastState) setState(lastState);
  }, []);
  const sub = useCallback(subscribe, []);
  const subPictures = useCallback(subscribePictures, []);
  return (
    <BoardSurface
      state={state}
      subscribe={sub}
      send={send}
      done={done}
      subscribePictures={subPictures}
      sendPicture={sendPicture}
    />
  );
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
  listen<BoardPicture>(BOARD_PICTURE, (e) => {
    if (pictureHandler) pictureHandler(e.payload);
    else pendingPictures.push(e.payload);
  }),
]).then(() => {
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <StrictMode>
      <Board />
    </StrictMode>,
  );
  void emit(BOARD_READY).catch(() => {});
});
