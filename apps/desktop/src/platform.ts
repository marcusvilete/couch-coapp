// Everything the main window needs from outside the page: the engine (one
// command, one event channel), the window, restarting the app, and the host
// overlay's window. The app's entry installs the Tauri platform
// (`platform/tauri.ts`); the preview's entry installs a fake one
// (`preview/`). Chosen by the entry at build time, never as a fallback: the
// app never runs on the fake, and the preview never reaches Tauri.
import type { Command } from "./bindings/Command";
import type { Event as EngineEvent } from "./bindings/Event";
import type { Response } from "./bindings/Response";
import type { ScreenArea } from "./bindings/ScreenArea";
import type { Toast } from "./model/overlay";

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
  overlay: OverlayHost;
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
}

export function platform(): Platform {
  if (installed == null) throw new Error("No platform installed: the page's entry must install one.");
  return installed;
}
