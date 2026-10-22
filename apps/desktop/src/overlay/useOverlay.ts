// The host overlay (M8), run from the main window: it works out what to show
// from the rooms it already follows, opens the overlay over the game when
// there is something to show, sends it the list, and closes it when the list
// empties. The window itself (never focused, closed rather than hidden) is
// the platform's (`platform/tauri.ts`).

import { useEffect, useRef, useState } from "react";
import type { RoomView } from "../bindings/RoomView";
import type { SessionView } from "../bindings/SessionView";
import { hosted, requestToasts, sessionEvents, type Toast } from "../model/overlay";
import { platform } from "../platform";
import { overlaySettings } from "../state/overlaySettings";

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

  const toasts: Toast[] = host ? [...requestToasts(host.room, host.session), ...events] : [];
  const show = host != null && toasts.length > 0 && !appInFront && overlaySettings().show;
  const area = host?.session.overlayArea ?? null;

  // The overlay page asks for the list when it has loaded.
  const latest = useRef(toasts);
  latest.current = toasts;
  useEffect(() => {
    const unlisten = platform().overlay.onReady(() => platform().overlay.send(latest.current));
    return () => {
      void unlisten.then((f) => f());
    };
  }, []);

  const key = JSON.stringify(toasts);
  useEffect(() => {
    void platform()
      .overlay.set(show, area)
      .then(() => {
        if (show) platform().overlay.send(latest.current);
      });
    // `key` stands for the toasts' contents.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [show, key]);

  // Gone with the app's window, or when this unmounts.
  useEffect(() => () => void platform().overlay.set(false, null), []);
}
