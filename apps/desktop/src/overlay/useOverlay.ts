// The host overlay (M8), run from the main window: it works out what to show
// from the rooms it already follows, opens the overlay over the game when
// there is something to show, sends it the list, and closes it when the list
// empties. The window itself (never focused, closed rather than hidden) is
// the platform's (`platform/tauri.ts`). On a Linux host the same list goes
// to the desktop's notifications instead (M16c): no window of ours can stay
// over the game there without taking the focus.

import { useEffect, useRef, useState } from "react";
import type { NoticeView } from "../bindings/NoticeView";
import type { RoomView } from "../bindings/RoomView";
import type { SessionView } from "../bindings/SessionView";
import {
  desktopBanner,
  hosted,
  noticesOf,
  overGame,
  requestToasts,
  sessionEvents,
  type Toast,
} from "../model/overlay";
import { platform } from "../platform";
import { overlaySettings } from "../state/overlaySettings";

/** How long a note stays. */
const EVENT_MS = 4500;
/** How often the engine is asked whether a game is in exclusive full screen. */
const FULL_SCREEN_POLL_MS = 1000;

/** Only a convenience: the console shows the same. */
function showNotices(notices: NoticeView[]) {
  void platform()
    .command({ type: "showNotices", notices })
    .catch(() => {});
}

/** Whether a game in exclusive full screen is in front (Windows); no if
 * the engine can't say. */
function askFullScreen(): Promise<boolean> {
  return platform()
    .command({ type: "fullScreenGame" })
    .then((r) => r.type === "fullScreenGame" && r.on)
    .catch(() => false);
}

/** `notices`: as desktop notifications, not the overlay (a Linux host,
 * M16c). Known once the engine's info has come. */
export function useOverlay(rooms: RoomView[], shown: "overlay" | "notices" = "overlay") {
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
      platform().stopFlashing();
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

  // A game in exclusive full screen (Windows) leaves it, often minimizing,
  // when any window comes up over it: asked every second while we host.
  // Unknown counts as full screen, so nothing comes up before the answer;
  // an engine that can't answer (stopping) counts as none, the overlay as
  // before. The poll alone may be a second old (or slower, the window
  // hidden in the tray), so the overlay also asks again before it opens.
  const hosting = host != null && shown === "overlay";
  const [fullScreen, setFullScreen] = useState<boolean | null>(null);
  useEffect(() => {
    if (!hosting) {
      setFullScreen(null);
      return;
    }
    let live = true;
    const ask = () =>
      void askFullScreen().then((on) => {
        if (live) setFullScreen(on);
      });
    ask();
    const timer = window.setInterval(ask, FULL_SCREEN_POLL_MS);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [hosting]);

  // Desktop control's banner (M25b) shows whatever the setting, and over the
  // app too: whoever is at this PC must see it.
  const banner = host ? desktopBanner(host.room, host.session) : [];
  const all: Toast[] = host ? [...banner, ...requestToasts(host.room, host.session), ...events] : [];
  const toasts = shown === "overlay" ? overGame(all, fullScreen !== false) : all;
  const show = host != null && toasts.length > 0 && (banner.length > 0 || (!appInFront && overlaySettings().show));
  const area = host?.session.overlayArea ?? null;

  // The overlay page asks for the list when it has loaded.
  const latest = useRef(toasts);
  latest.current = toasts;
  useEffect(() => {
    if (shown === "notices") return;
    const unlisten = platform().overlay.onReady(() => platform().overlay.send(latest.current));
    return () => {
      void unlisten.then((f) => f());
    };
  }, [shown]);

  const key = JSON.stringify(toasts);
  const wish = useRef(0);
  useEffect(() => {
    if (shown === "notices") {
      showNotices(show ? noticesOf(latest.current) : []);
      return;
    }
    const mine = ++wish.current;
    // More than the banner: a game may have gone full screen since the last
    // poll, so ask again first. Only the latest wish acts on its answer.
    const check = show && latest.current.some((t) => t.kind !== "banner") ? askFullScreen() : Promise.resolve(false);
    void check.then((on) => {
      if (mine !== wish.current) return;
      if (on) {
        setFullScreen(true); // filters the toasts: this runs again
        return;
      }
      void platform()
        .overlay.set(show, area)
        .then(() => {
          if (show) platform().overlay.send(latest.current);
        });
    });
    // `key` stands for the toasts' contents.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [show, key, shown]);

  // Gone with the app's window, or when this unmounts.
  const mode = useRef(shown);
  mode.current = shown;
  useEffect(
    () => () => {
      wish.current++; // a check still on its way opens nothing
      if (mode.current === "notices") showNotices([]);
      else void platform().overlay.set(false, null);
    },
    [],
  );
}
