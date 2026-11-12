// Everything the main window needs from outside the page: the engine (one
// command, one event channel), the window, restarting the app, and the host
// overlay's window. The app's entry installs the Tauri platform
// (`platform/tauri.ts`); the preview's entry installs a fake one
// (`preview/`). Chosen by the entry at build time, never as a fallback: the
// app never runs on the fake, and the preview never reaches Tauri.
import type { BoardNewsView } from "./bindings/BoardNewsView";
import type { Command } from "./bindings/Command";
import type { Event as EngineEvent } from "./bindings/Event";
import type { Response } from "./bindings/Response";
import type { ScreenArea } from "./bindings/ScreenArea";
import type { Toast } from "./model/overlay";
import type { BoardBatch, BoardPicture, BoardPictureUp, BoardState } from "./board/events";

export type Unlisten = () => void;

export type Theme = "dark" | "light";

export interface Platform {
  /** Sends a command; rejects with the engine's error text. */
  command(command: Command): Promise<Response>;
  /** Engine events; resolves once listening. */
  onEvent(handler: (event: EngineEvent) => void): Promise<Unlisten>;
  /** Flashes the taskbar button until the window is focused. */
  flashTaskbar(): void;
  /** Stops flashing: nothing left to call the user for. */
  stopFlashing(): void;
  /** Closes the app and starts it again (release builds). */
  restartApp(): Promise<void>;
  /** `false` in a development build, which `npm run tauri dev` started:
   * it stops with the app and can't start itself again. */
  canRestartItself: boolean;
  /** The window's own theme (the title bar), or Windows' with `null`. */
  setTheme(theme: Theme | null): Promise<void>;
  /** The splash at start; the preview's screenshots go without it. */
  splash: boolean;
  /** The window's own title bar (P15, Windows), or `null` where the
   * system draws it (Linux). */
  titleBar: TitleBarHost | null;
  overlay: OverlayHost;
  board: BoardHost;
}

/** The main window's own title bar (P15): the page draws it, the window has
 * no frame of the system's. Its maximize button lies under a window of the
 * app's (Windows' snap layouts), which takes the pointer there and says how
 * to draw it (`hover`, `pressed`). */
export interface TitleBarHost {
  /** The window's title (with the profile's name beside the default's). */
  title: string;
  /** Logical pixels: the bar's height and each button's width, which the
   * app's maximize window is laid out by too. */
  height: number;
  buttonWidth: number;
  minimize(): void;
  toggleMaximize(): void;
  close(): void;
  /** The window's state, at once and on each change. */
  onState(handler: (state: WindowState) => void): Promise<Unlisten>;
}

export interface WindowState {
  maximized: boolean;
  focused: boolean;
  /** How to draw the maximize button. */
  hover: boolean;
  pressed: boolean;
}

/** The host's board overlay over the game (M27b): the engine makes, places
 * and focuses its window; the main window only talks to its page. */
export interface BoardHost {
  /** Its page has loaded: it wants the state and the board. */
  onReady(handler: () => void): Promise<Unlisten>;
  sendState(state: BoardState): void;
  sendNews(news: BoardNewsView): void;
  /** The host's own drawing, to pass on to the engine. */
  onOps(handler: (batch: BoardBatch) => void): Promise<Unlisten>;
  /** A picture on the board, for the page (M27e). */
  sendPicture(picture: BoardPicture): void;
  /** The host's own picture, to pass on to the engine (M27e). */
  onPicture(handler: (up: BoardPictureUp) => void): Promise<Unlisten>;
  /** Esc or Done in the page: back to the game. */
  onDone(handler: () => void): Promise<Unlisten>;
}

/** The host overlay's window (M8). It must never take the focus from the
 * game: the Tauri implementation creates it unfocusable, off screen until
 * drawn, and closes rather than hides it. */
export interface OverlayHost {
  /** Opens or closes it, on the monitor showing `area`; steps run one at a
   * time and the latest wish wins. Resolves once settled. */
  set(show: boolean, area: ScreenArea | null): Promise<void>;
  /** What it shows. */
  send(toasts: Toast[]): void;
  /** Its page has loaded and wants the list. */
  onReady(handler: () => void): Promise<Unlisten>;
}

let installed: Platform | null = null;

/** Called once, by the page's entry, before anything renders. */
export function installPlatform(platform: Platform) {
  installed = platform;
  // Before the first render: the bar's room is the page's from the start,
  // and the layers over the whole window (dialogs, the splash) leave it.
  const root = document.documentElement.style;
  const bar = platform.titleBar;
  root.setProperty("--titlebar-h", bar ? `${bar.height}px` : "0px");
  if (bar) root.setProperty("--caption-w", `${bar.buttonWidth}px`);
}

export function platform(): Platform {
  if (installed == null) throw new Error("No platform installed: the page's entry must install one.");
  return installed;
}
