// Events between the main window and the host's board overlay page (M27b),
// in a module of their own so the board's page loads nothing else. The page
// calls no engine command: the main window passes its drawing on.

import type { BoardOpView } from "../bindings/BoardOpView";

/** Board to main: loaded, send the state and ask for the board. */
export const BOARD_READY = "board-ready";
/** Main to board: who we are and what drawing is doing (`BoardState`). */
export const BOARD_STATE = "board-state";
/** Main to board: what the board says (`BoardNewsView`). */
export const BOARD_NEWS = "board-news";
/** Board to main: the host's own operations (`BoardBatch`). */
export const BOARD_OPS = "board-ops";
/** Board to main: Esc or Done, back to the game. */
export const BOARD_DONE = "board-done";

/** What the board's page needs to know besides the board. */
export interface BoardState {
  /** Our member id: our own elements are ours to change. */
  me: string;
  /** Drawing over the game now: the page has the mouse and the keys. */
  drawing: boolean;
  /** Who may have drawn, by member id, for a ping's name. */
  names: [string, string][];
}

/** The host's own operations, made in the board's `generation`. */
export interface BoardBatch {
  generation: number;
  ops: BoardOpView[];
}
