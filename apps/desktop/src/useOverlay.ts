// The host overlay's window (M8), run from the main window: it works out
// what to show from the rooms it already follows, opens the overlay over
// the game when there is something to show, sends it the list, and closes
// it when the list empties.
//
// The overlay must never take the focus from the game: keyboard and mouse
// reach the game only while it is in front, and some games pause without
// it. So the window is created unfocused and unfocusable (no-activate), and
// closed rather than hidden when empty (a later show could activate it).
// Clicks pass through it, and it is left out of screen captures.

import { emitTo, listen } from "@tauri-apps/api/event";
import { availableMonitors, getCurrentWindow, PhysicalPosition, primaryMonitor } from "@tauri-apps/api/window";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { useEffect, useRef, useState } from "react";
import type { RoomView } from "./bindings/RoomView";
import type { ScreenArea } from "./bindings/ScreenArea";
import type { SessionView } from "./bindings/SessionView";
import { OVERLAY_READY, OVERLAY_TOASTS } from "./overlayEvents";
import { hosted, requestToasts, sessionEvents, type Toast } from "./overlayModel";
import { overlaySettings } from "./overlaySettings";

const LABEL = "overlay";
/** Logical pixels. */
const WIDTH = 380;
const HEIGHT = 420;
const MARGIN = 16;
/** How long a note stays. */
const EVENT_MS = 4500;

export function useOverlay(rooms: RoomView[]) {
  const host = hosted(rooms);
  const [events, setEvents] = useState<Toast[]>([]);
  const previous = useRef<SessionView | null>(null);
  const nextId = useRef(1);
  const [appInFront, setAppInFront] = useState(() => document.hasFocus());

  // Notes from each change of our session.
  const session = host?.session ?? null;
  const previousRoom = useRef<RoomView | null>(null);
  useEffect(() => {
    const before = previous.current;
    const roomBefore = previousRoom.current;
    previous.current = session;
    previousRoom.current = host?.room ?? null;
    // Answered (with the keys, say), withdrawn, or the stream ended:
    // nothing left to call the host for, so the taskbar stops flashing.
    if (before && before.requests.length > 0 && (session?.requests.length ?? 0) === 0) {
      void getCurrentWindow()
        .requestUserAttention(null)
        .catch(() => {});
    }
    if (!session || !host) {
      setEvents([]);
      return;
    }
    if (!before) return;
    // Someone who just left the room is named from how it was.
    const name = (member: string) =>
      [host.room, roomBefore].flatMap((r) => r?.members ?? []).find((m) => m.id === member)?.name ??
      "someone";
    const notes = sessionEvents(name, before, session);
    if (notes.length === 0) return;
    const fresh = notes.map((text): Toast => ({ kind: "event", id: `event:${nextId.current++}`, text }));
    setEvents((all) => [...all, ...fresh]);
    const ids = new Set(fresh.map((t) => t.id));
    window.setTimeout(() => setEvents((all) => all.filter((t) => !ids.has(t.id))), EVENT_MS);
    // Only a new view of the session matters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session]);

  // The app's own window shows the same in its notices: no overlay over it.
  useEffect(() => {
    const focus = () => setAppInFront(true);
    const blur = () => setAppInFront(false);
    window.addEventListener("focus", focus);
    window.addEventListener("blur", blur);
    return () => {
      window.removeEventListener("focus", focus);
      window.removeEventListener("blur", blur);
    };
  }, []);

  const toasts: Toast[] = host ? [...requestToasts(host.room, host.session), ...events] : [];
  const show = host != null && toasts.length > 0 && !appInFront && overlaySettings().show;
  const area = host?.session.overlayArea ?? null;

  // The overlay page asks for the list when it has loaded.
  const latest = useRef(toasts);
  latest.current = toasts;
  useEffect(() => {
    const unlisten = listen(OVERLAY_READY, () => {
      void emitTo(LABEL, OVERLAY_TOASTS, latest.current).catch(() => {});
    });
    return () => {
      void unlisten.then((f) => f());
    };
  }, []);

  const key = JSON.stringify(toasts);
  useEffect(() => {
    void setOverlay(show, area).then(() => {
      if (show) void emitTo(LABEL, OVERLAY_TOASTS, latest.current).catch(() => {});
    });
    // `key` stands for the toasts' contents.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [show, key]);

  // Gone with the app's window, or when this unmounts.
  useEffect(() => () => void setOverlay(false, null), []);
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
