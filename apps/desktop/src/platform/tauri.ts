// The app's platform: Tauri. The only module of the main window that calls
// Tauri's API (the overlay's own page, `overlay/main.tsx`, listens on its
// side).
import { invoke } from "@tauri-apps/api/core";
import { emitTo, listen } from "@tauri-apps/api/event";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  availableMonitors,
  getCurrentWindow,
  PhysicalPosition,
  primaryMonitor,
  UserAttentionType,
} from "@tauri-apps/api/window";
import type { Command } from "../bindings/Command";
import type { Event as EngineEvent } from "../bindings/Event";
import type { Response } from "../bindings/Response";
import type { ScreenArea } from "../bindings/ScreenArea";
import type { Toast } from "../model/overlay";
import { OVERLAY_READY, OVERLAY_TOASTS } from "../overlay/events";
import type { OverlayHost, Platform } from "../platform";

export const tauriPlatform: Platform = {
  command: (command: Command) => invoke<Response>("engine_command", { command }),
  onEvent: (handler: (event: EngineEvent) => void) =>
    listen<EngineEvent>("engine-event", (e) => handler(e.payload)),
  flashTaskbar: () => {
    getCurrentWindow()
      .requestUserAttention(UserAttentionType.Critical)
      .catch(() => {});
  },
  stopFlashing: () => {
    getCurrentWindow()
      .requestUserAttention(null)
      .catch(() => {});
  },
  restartApp: () => invoke("restart_app"),
  canRestartItself: !import.meta.env.DEV,
  setTheme: (theme) => getCurrentWindow().setTheme(theme),
  splash: true,
  overlay: overlayHost(),
};

// ---- The host overlay's window (M8) ----
//
// It must never take the focus from the game: keyboard and mouse reach the
// game only while it is in front, and some games pause without it. So the
// window is created unfocused and unfocusable (no-activate), and closed
// rather than hidden when empty (a later show could activate it). Clicks
// pass through it, and it is left out of screen captures.

const LABEL = "overlay";
/** Logical pixels. */
const WIDTH = 380;
const HEIGHT = 420;
const MARGIN = 16;

function overlayHost(): OverlayHost {
  return {
    set: setOverlay,
    send: (toasts: Toast[]) => {
      void emitTo(LABEL, OVERLAY_TOASTS, toasts).catch(() => {});
    },
    onReady: (handler) => listen(OVERLAY_READY, () => handler()),
  };
}

/** What the overlay should be now: the latest wish wins. */
let wanted: { show: boolean; area: ScreenArea | null } = { show: false, area: null };
/** Opening and closing, one step at a time. */
let queue: Promise<void> = Promise.resolve();

/** Asks for the overlay open or closed. Steps run one after another, and
 * each looks at the latest wish, so an open still loading (up to 3 s) and
 * a close still going can't leave the wrong state behind. */
function setOverlay(show: boolean, area: ScreenArea | null): Promise<void> {
  wanted = { show, area };
  queue = queue.then(settle, settle);
  return queue;
}

async function settle() {
  const existing = await WebviewWindow.getByLabel(LABEL).catch(() => null);
  if (wanted.show && !existing) {
    await create(wanted.area);
  } else if (!wanted.show && existing) {
    // Until it's gone, its label still finds it: wait, so a new one isn't
    // mistaken for it.
    const gone = new Promise<void>((resolve) => {
      void existing.once("tauri://destroyed", () => resolve());
      window.setTimeout(resolve, 2000);
    });
    await existing.close().catch(() => {});
    await gone;
  }
}

/** Where a new overlay waits until its page has drawn: off every screen.
 * A new window shows the system's white before its page loads, and showing
 * it later instead could take the focus; moving it can't. */
const OFF_SCREEN = -20000;
/** How long a new overlay's page may take to load before it is shown
 * anyway. */
const LOAD_WAIT_MS = 3000;

async function create(area: ScreenArea | null): Promise<WebviewWindow | null> {
  const target = await corner(area);
  // Listening before the window exists, so its page's word isn't missed.
  let loaded = () => {};
  const ready = new Promise<void>((resolve) => {
    loaded = resolve;
  });
  const unlisten = await listen(OVERLAY_READY, () => loaded()).catch(() => null);
  try {
    const overlay = new WebviewWindow(LABEL, {
      url: "overlay.html",
      title: "Couch Co-App overlay",
      x: OFF_SCREEN,
      y: OFF_SCREEN,
      width: WIDTH,
      height: HEIGHT,
      transparent: true,
      backgroundColor: [0, 0, 0, 0],
      decorations: false,
      shadow: false,
      resizable: false,
      alwaysOnTop: true,
      skipTaskbar: true,
      focus: false,
      focusable: false,
      contentProtected: true,
    });
    const created = await new Promise<boolean>((resolve) => {
      void overlay.once("tauri://created", () => resolve(true));
      void overlay.once("tauri://error", () => resolve(false));
    });
    if (!created) return null;
    // Careful: after creation, tao applies any style change (this one
    // included) with a plain ShowWindow(SW_SHOW), which could activate a
    // hidden window. It is harmless only because the overlay is already
    // visible (off screen). Never hide the overlay, nor change its style
    // while it is hidden.
    await overlay.setIgnoreCursorEvents(true).catch(() => {});
    await Promise.race([ready, new Promise((r) => window.setTimeout(r, LOAD_WAIT_MS))]);
    // Into its corner, now that it draws transparent (no activation).
    if (target) await overlay.setPosition(target).catch(() => {});
    return overlay;
  } finally {
    unlisten?.();
  }
}

/** The top right of the monitor showing what we stream (the primary one if
 * unknown), in physical pixels. */
async function corner(area: ScreenArea | null): Promise<PhysicalPosition | null> {
  const monitors = await availableMonitors().catch(() => []);
  const monitor = area
    ? monitors.find(
        (m) =>
          area.x >= m.position.x &&
          area.x < m.position.x + m.size.width &&
          area.y >= m.position.y &&
          area.y < m.position.y + m.size.height,
      )
    : await primaryMonitor().catch(() => null);
  const box = area ?? (monitor ? { x: monitor.position.x, y: monitor.position.y, width: monitor.size.width } : null);
  if (!box) return null;
  const scale = monitor?.scaleFactor ?? 1;
  return new PhysicalPosition(
    Math.round(box.x + box.width - (WIDTH + MARGIN) * scale),
    Math.round(box.y + MARGIN * scale),
  );
}
