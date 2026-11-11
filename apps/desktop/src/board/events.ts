// Events between the main window and the host's board overlay page (M27b),
// in a module of their own so the board's page loads nothing else. The page
// calls no engine command: the main window passes its drawing on.

import type { BoardElement } from "../bindings/BoardElement";
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
  /** The host allows pictures on the board (M27e). */
  pictures: boolean;
}

/** The host's own operations, made in the board's `generation`. */
export interface BoardBatch {
  generation: number;
  ops: BoardOpView[];
}

/** Main to board: a picture on the board (`BoardPicture`, M27e). */
export const BOARD_PICTURE = "board-picture";
/** Board to main: the host's own picture (`BoardPictureUp`, M27e). */
export const BOARD_PICTURE_UP = "board-picture-up";

/** A picture's bytes, by its id (SHA-256 in hex), in base64. */
export interface BoardPicture {
  picture: string;
  data: string;
}

/** The host's own picture, made in the board's `generation`: its element
 * and its bytes in base64. */
export interface BoardPictureUp {
  generation: number;
  element: BoardElement;
  data: string;
}
