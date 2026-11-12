// The preview's platform: the fake engine, and stand-ins for the window,
// restarting and the overlay window (drawn inside the page instead).
import type { ScreenArea } from "../bindings/ScreenArea";
import type { Toast } from "../model/overlay";
import type { Platform, Theme, TitleBarHost, WindowState } from "../platform";
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

/** The title bar as Windows has it (P15), in the page alone: the preview's
 * window is the browser's, so its buttons only change how the bar looks. */
function previewTitleBar(): TitleBarHost {
  let state: WindowState = { maximized: false, focused: true, hover: false, pressed: false };
  const watchers = new Set<(state: WindowState) => void>();
  const put = (next: Partial<WindowState>) => {
    state = { ...state, ...next };
    watchers.forEach((w) => w(state));
  };
  return {
    title: "Couch Co-App",
    height: 32,
    buttonWidth: 46,
    minimize: () => {},
    toggleMaximize: () => put({ maximized: !state.maximized }),
    close: () => {},
    onState: async (handler) => {
      watchers.add(handler);
      handler(state);
      return () => watchers.delete(handler);
    },
  };
}

export function previewPlatform(options: { release: boolean; splash: boolean; titleBar: boolean }): Platform {
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
    titleBar: options.titleBar ? previewTitleBar() : null,
    overlay: {
      set: async (show, area) => setOverlayState({ show, area }),
      send: (toasts) => setOverlayState({ toasts }),
      onReady: async () => () => {},
    },
    // The board overlay's page isn't in the preview's app (its surface has
    // a mode of its own, `?board`).
    board: {
      onReady: async () => () => {},
      sendState: () => {},
      sendNews: () => {},
      onOps: async () => () => {},
      sendPicture: () => {},
      onPicture: async () => () => {},
      onDone: async () => () => {},
    },
  };
}
