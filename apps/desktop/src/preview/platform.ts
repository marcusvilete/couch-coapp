// The preview's platform: the fake engine, and stand-ins for the window,
// restarting and the overlay window (drawn inside the page instead).
import type { ScreenArea } from "../bindings/ScreenArea";
import type { Toast } from "../model/overlay";
import type { Platform, Theme } from "../platform";
import * as engine from "./fakeEngine";

type OverlayState = { show: boolean; area: ScreenArea | null; toasts: Toast[] };

let overlay: OverlayState = { show: false, area: null, toasts: [] };
const overlayWatchers = new Set<(state: OverlayState) => void>();

function setOverlayState(next: Partial<OverlayState>) {
  overlay = { ...overlay, ...next };
  overlayWatchers.forEach((w) => w(overlay));
}

/** The overlay as the app last asked for it, for the preview's frame. */
export function watchOverlay(watcher: (state: OverlayState) => void): () => void {
  overlayWatchers.add(watcher);
  watcher(overlay);
  return () => overlayWatchers.delete(watcher);
}

export const taskbar = { flashing: false };

export function previewPlatform(options: { release: boolean; splash: boolean }): Platform {
  return {
    command: engine.command,
    onEvent: engine.onEvent,
    flashTaskbar: () => {
      taskbar.flashing = true;
    },
    stopFlashing: () => {
      taskbar.flashing = false;
    },
    restartApp: async () => {
      window.setTimeout(() => window.location.reload(), 1200);
    },
    canRestartItself: options.release,
    setTheme: async (_theme: Theme | null) => {},
    splash: options.splash,
    overlay: {
      set: async (show, area) => setOverlayState({ show, area }),
      send: (toasts) => setOverlayState({ toasts }),
      onReady: async () => () => {},
    },
  };
}
