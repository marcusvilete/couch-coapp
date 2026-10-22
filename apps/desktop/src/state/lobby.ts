// Lobby state: this profile, its rooms, notices, and invite links the app
// was opened with. Kept current by engine events.
import { useCallback, useEffect, useRef, useState } from "react";
import type { EngineInfo } from "../bindings/EngineInfo";
import type { Event as EngineEvent } from "../bindings/Event";
import type { ProfileView } from "../bindings/ProfileView";
import type { RoomGone } from "../bindings/RoomGone";
import type { RoomView } from "../bindings/RoomView";
import type { SessionEnd } from "../bindings/SessionEnd";
import type { WatchEnd } from "../bindings/WatchEnd";
import { errorText, onEngineEvent, request } from "../engine";
import { playChime } from "../chime";
import { seatPhrase } from "../model/sessions";
import { platform } from "../platform";

export type Notice = { id: number; text: string; tone: "info" | "warn" };

/** An invite link the app was opened with. `seq` tells two openings of the
 * same link apart. */
export type OpenedInvite = { link: string; seq: number };

export type Lobby = {
  info: EngineInfo | null;
  profile: ProfileView | null;
  /** By name. */
  rooms: RoomView[];
  /** When joining reopens in each room we own, from `Date.now()`. */
  pausedUntil: Record<string, number>;
  loaded: boolean;
  failure: string | null;
  notices: Notice[];
  opened: OpenedInvite | null;
  /** Forgets the opened link once the UI has it. */
  clearOpened: () => void;
  setProfile: (profile: ProfileView) => void;
  /** Shows a room the engine just answered with (created or joined). */
  putRoom: (room: RoomView) => void;
  notify: (text: string, tone?: Notice["tone"]) => void;
  dismiss: (id: number) => void;
  /** While the pointer is on the notices, none goes by itself. */
  hold: (held: boolean) => void;
};

const NOTICE_MS = 8000;

function goneText(room: RoomView, reason: RoomGone): Notice {
  const name = room.name;
  switch (reason) {
    case "left":
      return { id: 0, text: room.isOwner ? `You closed “${name}”.` : `You left “${name}”.`, tone: "info" };
    case "banned":
      return { id: 0, text: `You were banned from “${name}”.`, tone: "warn" };
    case "removed":
      return {
        id: 0,
        text: `“${name}” was closed, or its owner took you off the member list.`,
        tone: "warn",
      };
  }
}

/** Why our stream ended, unless we ended it ourselves. */
function sessionEndText(reason: SessionEnd): string | null {
  switch (reason) {
    case "error":
      return "Your session stopped after an error.";
    case "gameExited":
    case "sourceLost":
    case "hostStopped":
    case "hostLeftRoom":
    case "hostShutdown":
      return null;
  }
}

/** Why our stream ended inside our session (M19), unless we stopped it. */
function streamEndText(reason: SessionEnd): string | null {
  switch (reason) {
    case "gameExited":
      return "The game closed, so the stream stopped. The session goes on: stream again when you're ready.";
    case "sourceLost":
      return "The window or screen you were streaming went away, so the stream stopped. The session goes on.";
    case "error":
      return "The stream stopped after an error. The session goes on.";
    case "hostStopped":
    case "hostLeftRoom":
    case "hostShutdown":
      return null;
  }
}

/** Why watching ended, unless we stopped it ourselves. */
function watchEndText(host: string, reason: WatchEnd): string | null {
  switch (reason) {
    case "ended":
      return `${host}'s stream ended.`;
    case "connectionLost":
      return `Lost the connection to ${host}'s stream, and it didn't come back.`;
    case "unavailable":
      return `${host} can't stream to you right now.`;
    case "viewerFailed":
      return "The stream window couldn't open, or it crashed twice. Try Watch again.";
    case "left":
      return null;
  }
}

/** Flashes the taskbar button until the window is focused. */
function flashTaskbar() {
  platform().flashTaskbar();
}

export function useLobby(): Lobby {
  const [info, setInfo] = useState<EngineInfo | null>(null);
  const [profile, setProfile] = useState<ProfileView | null>(null);
  const [rooms, setRooms] = useState<Record<string, RoomView>>({});
  const [pausedUntil, setPausedUntil] = useState<Record<string, number>>({});
  const [loaded, setLoaded] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [notices, setNotices] = useState<Notice[]>([]);
  const [opened, setOpened] = useState<OpenedInvite | null>(null);
  const roomsRef = useRef(rooms);
  roomsRef.current = rooms;
  /** Counts room events, so a room list fetched meanwhile is known stale. */
  const roomEvents = useRef(0);
  const nextId = useRef(1);

  /** The pointer is on the notices: none goes by itself meanwhile. */
  const held = useRef(false);
  const notify = useCallback((text: string, tone: Notice["tone"] = "info") => {
    const id = nextId.current++;
    setNotices((all) => [...all, { id, text, tone }]);
    const expire = () => {
      if (held.current) {
        window.setTimeout(expire, 1500);
        return;
      }
      setNotices((all) => all.filter((n) => n.id !== id));
    };
    window.setTimeout(expire, NOTICE_MS);
  }, []);
  const hold = useCallback((on: boolean) => {
    held.current = on;
  }, []);

  const dismiss = useCallback((id: number) => {
    setNotices((all) => all.filter((n) => n.id !== id));
  }, []);

  // A command's reply and the engine's events arrive by different paths, in
  // either order: of two views of a room, the one the engine made later (the
  // higher revision) wins. A join's reply once hid a session announced just
  // after it.
  const putRoom = useCallback((room: RoomView) => {
    const newer = (known: RoomView | undefined) => known != null && known.revision > room.revision;
    if (newer(roomsRef.current[room.id])) return;
    setRooms((all) => (newer(all[room.id]) ? all : { ...all, [room.id]: room }));
    setPausedUntil((all) => {
      const next = { ...all };
      if (room.joinsPausedForSecs != null) {
        next[room.id] = Date.now() + room.joinsPausedForSecs * 1000;
      } else {
        delete next[room.id];
      }
      return next;
    });
  }, []);

  const refreshRooms = useCallback(async () => {
    // A room event that lands while the list is on its way is newer than
    // the list: ask again rather than bring back a room just removed.
    let reply;
    for (let tries = 0; ; tries++) {
      const seen = roomEvents.current;
      reply = await request({ type: "listRooms" }, "rooms");
      if (roomEvents.current === seen || tries === 3) break;
    }
    setRooms((all) =>
      Object.fromEntries(
        reply.rooms.map((r) => {
          const known = all[r.id];
          return [r.id, known != null && known.revision > r.revision ? known : r];
        }),
      ),
    );
    setPausedUntil(
      Object.fromEntries(
        reply.rooms
          .filter((r) => r.joinsPausedForSecs != null)
          .map((r) => [r.id, Date.now() + (r.joinsPausedForSecs ?? 0) * 1000]),
      ),
    );
  }, []);

  const takeOpened = useCallback(async () => {
    const reply = await request({ type: "takeOpenedInvite" }, "openedInvite");
    if (reply.link != null) {
      const link = reply.link;
      setOpened((prev) => ({ link, seq: (prev?.seq ?? 0) + 1 }));
    }
  }, []);

  useEffect(() => {
    let live = true;
    const handle = (event: EngineEvent) => {
      if (!live) return;
      switch (event.type) {
        case "roomChanged":
          roomEvents.current++;
          putRoom(event.room);
          break;
        case "roomRemoved": {
          roomEvents.current++;
          const room = roomsRef.current[event.room];
          setRooms((all) => {
            const next = { ...all };
            delete next[event.room];
            return next;
          });
          if (room != null) {
            const { text, tone } = goneText(room, event.reason);
            notify(text, tone);
          }
          break;
        }
        case "joinsPaused": {
          const name = roomsRef.current[event.room]?.name ?? "your room";
          notify(
            `Too many wrong passwords: nobody can join “${name}” for ${event.minutes} minutes.`,
            "warn",
          );
          refreshRooms().catch(() => {});
          break;
        }
        // M13: the room changed hands, or its owner offers it to us.
        case "ownerChanged": {
          const room = roomsRef.current[event.room];
          const owner = room?.members.find((m) => m.id === event.owner);
          const name = room?.name ?? "the room";
          notify(
            owner?.link === "me" ? `You now own “${name}”.` : `${owner?.name ?? "Someone"} now owns “${name}”.`,
          );
          break;
        }
        case "roomOffered": {
          const room = roomsRef.current[event.room];
          const owner = room?.members.find((m) => m.isOwner)?.name ?? "The owner";
          notify(`${owner} offers you “${room?.name ?? "their room"}”. Answer in the room.`);
          flashTaskbar();
          break;
        }
        case "controlRequested": {
          const room = roomsRef.current[event.room];
          const name = room?.members.find((m) => m.id === event.member)?.name ?? "Someone";
          notify(`${name} asks for ${seatPhrase(event.seat || "Controller")}. Answer in “${room?.name ?? "the room"}”.`);
          flashTaskbar();
          playChime();
          break;
        }
        case "sessionEnded": {
          const text = sessionEndText(event.reason);
          if (text) notify(text, "warn");
          break;
        }
        case "streamEnded": {
          const text = streamEndText(event.reason);
          if (text) notify(text, "warn");
          refreshRooms().catch(() => {});
          break;
        }
        case "watchEnded": {
          const host = roomsRef.current[event.room]?.members.find((m) => m.id === event.host)?.name ?? "The host";
          const text = watchEndText(host, event.reason);
          if (text) notify(text, event.reason === "ended" ? "info" : "warn");
          break;
        }
        case "inviteOpened":
          takeOpened().catch(() => {});
          break;
        case "pong":
          break;
      }
    };
    const unlisten = onEngineEvent(handle);
    // Listen first, then load, so nothing announced in between is missed.
    unlisten
      .then(async () => {
        const [infoReply, profileReply] = await Promise.all([
          request({ type: "getInfo" }, "info"),
          request({ type: "getProfile" }, "profile"),
        ]);
        await refreshRooms();
        if (!live) return;
        setInfo(infoReply);
        setProfile(profileReply);
        setLoaded(true);
        await takeOpened();
      })
      .catch((e) => live && setFailure(errorText(e)));
    // Events can be missed if the UI falls behind; catch up when the
    // window comes back.
    const onFocus = () => {
      refreshRooms().catch(() => {});
    };
    window.addEventListener("focus", onFocus);
    return () => {
      live = false;
      window.removeEventListener("focus", onFocus);
      unlisten.then((stop) => stop());
    };
  }, [notify, putRoom, refreshRooms, takeOpened]);

  const clearOpened = useCallback(() => setOpened(null), []);

  const sorted = Object.values(rooms).sort((a, b) =>
    a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
  );
  return {
    info,
    profile,
    rooms: sorted,
    pausedUntil,
    loaded,
    failure,
    notices,
    opened,
    clearOpened,
    setProfile,
    putRoom,
    notify,
    dismiss,
    hold,
  };
}
