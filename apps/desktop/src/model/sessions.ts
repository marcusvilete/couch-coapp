// What the UI works out from the engine's view of sessions: who plays, what
// a pass would do, how to name seats and controllers. Pure functions; the
// engine decides everything, these only read its state.
import type { HostController } from "../bindings/HostController";
import type { RoomView } from "../bindings/RoomView";
import type { SessionView } from "../bindings/SessionView";
import type { StreamStateView } from "../bindings/StreamStateView";

/** What this profile is doing in any room: one session at a time, and not
 * while watching one. */
export type Busy = { hosting: boolean; watching: boolean };

const WATCHING_ROLES: SessionView["role"][] = ["joining", "watching", "asking", "controlling", "reconnecting"];

export function busyIn(rooms: RoomView[]): Busy {
  const all = rooms.flatMap((r) => r.sessions);
  return {
    hosting: all.some((s) => s.role === "host"),
    watching: all.some((s) => WATCHING_ROLES.includes(s.role)),
  };
}

export function nameOf(room: RoomView, member: string | null | undefined): string {
  return room.members.find((m) => m.id === member)?.name ?? "someone";
}

/** Who plays: each seat's holder once (the host's own controller included;
 * a guest may hold several seats, M18), and the host while a seat is free if
 * they play outside the seats. */
export function playersOf(session: SessionView): string[] {
  const holders = session.seats.flatMap((s) => (s.holder ? [s.holder] : []));
  const outside = !session.hostOnSeats && holders.length < session.seats.length;
  const players = [...new Set(holders)];
  return outside && !players.includes(session.host) ? [...players, session.host] : players;
}

/** A seat's label in a sentence: "the controller", "Controller 2". */
export function seatPhrase(label: string): string {
  return /\d$/.test(label) ? label : `the ${label.toLowerCase()}`;
}

/** The seats `member` holds, by index (M18: a guest may hold several). */
export function seatsOf(s: SessionView, member: string | undefined): number[] {
  return s.seats.flatMap((seat, i) => (member != null && seat.holder === member ? [i] : []));
}

/** "Controller 1", "Controller 1 and Controller 2", "A, B and C". */
export function listed(labels: string[]): string {
  return labels.length <= 1 ? (labels[0] ?? "") : `${labels.slice(0, -1).join(", ")} and ${labels[labels.length - 1]}`;
}

/** Whether a guest holding `held`, with `controllers` controllers, keeps them
 * on being passed seat `seat`, rather than moving to it: as the host's session
 * decides (`HostSession::keeps` in couch-room). A seat without a pad needs no
 * controller; one with a pad is added while they hold fewer pad seats than
 * they have controllers. */
export function keeps(s: SessionView, held: number[], controllers: number, seat: number): boolean {
  if (!s.seats[seat]?.pad) {
    return true;
  }
  const pads = held.filter((i) => i !== seat && s.seats[i]?.pad).length;
  return pads < controllers;
}

/** Whether a session streams something now. */
export function isLive(session: SessionView): boolean {
  return session.stream.state === "live";
}

/** "Bia is playing" (the host while nobody holds the one seat and they play
 * outside it), or per seat: "Controller 1: Ana · Controller 2: free". A host
 * playing outside the seats, on whatever the game sees directly, is listed
 * beside them: "Ana: own controller". */
export function whoPlays(room: RoomView, s: SessionView): string {
  if (s.seats.length === 1) {
    const holder = s.seats[0].holder ?? (s.hostOnSeats ? null : s.host);
    return holder ? `${nameOf(room, holder)} is playing` : "Nobody is playing";
  }
  const seats = s.seats.map((seat) => `${seat.label}: ${seat.holder ? nameOf(room, seat.holder) : "free"}`);
  const outside = s.hostOnSeats ? [] : [`${nameOf(room, s.host)}: own controller`];
  return [...seats, ...outside].join(" · ");
}

export function allowsKbm(s: SessionView): boolean {
  return s.seats.some((seat) => seat.keyboardMouse);
}

/** Why a session streams nothing, for those watching it; `null` while live. */
export function idleText(host: string, stream: StreamStateView): string | null {
  switch (stream.state) {
    case "live":
      return null;
    case "starting":
      return `${host} is starting a stream…`;
    case "waiting":
      return `${host} isn't streaming anything yet.`;
    case "stopped":
      switch (stream.reason) {
        case "gameExited":
          return "The game closed; nothing is streamed now.";
        case "sourceLost":
          return "The window or screen being streamed went away; nothing is streamed now.";
        case "error":
          return "The stream stopped after an error; nothing is streamed now.";
        default:
          return `${host} stopped streaming for now.`;
      }
  }
}

/** What the host's console says while nothing is streamed. */
export function hostIdleText(stream: StreamStateView): string {
  switch (stream.state) {
    case "stopped":
      switch (stream.reason) {
        case "gameExited":
          return "The game closed, so the stream stopped.";
        case "sourceLost":
          return "The window or screen you were streaming went away, so the stream stopped.";
        case "error":
          return "The stream stopped after an error.";
        default:
          return "Nothing is streamed now.";
      }
    default:
      return "Nothing is streamed yet. Start the game, then Start streaming.";
  }
}

// ---- Streams ----

export const QUALITIES: { mbps: number; label: string }[] = [
  { mbps: 5, label: "5 Mbit/s: slow connections" },
  { mbps: 10, label: "10 Mbit/s" },
  { mbps: 20, label: "20 Mbit/s: sharp at 1080p (recommended)" },
  { mbps: 30, label: "30 Mbit/s" },
  { mbps: 50, label: "50 Mbit/s: fast networks only" },
];
export const FRAME_RATES = [120, 60, 30];

/** An upload limit as typed: whole Mbit/s only ("50abc" or "1e9" isn't
 * one), `NaN` if it isn't one, `null` without a limit. */
export function uploadLimit(limit: boolean, typed: string): number | null {
  return limit ? (/^\s*\d{1,5}\s*$/.test(typed) ? Number(typed) : Number.NaN) : null;
}

/** Whether a limit from `uploadLimit` is one the engine takes. */
export function uploadLimitOk(mbps: number | null): boolean {
  return mbps == null || (mbps >= 2 && mbps <= 10000);
}

// ---- The host's own controllers (M22, M23) ----

/** A controller's model: its ids, and for a remapper's pad (M22), which has
 * a real Xbox 360 controller's ids, that it is one. */
export function padKey(c: HostController): string {
  return c.kind === "otherVirtual" ? `${c.vendor}:${c.product}:remapper` : `${c.vendor}:${c.product}`;
}

/** How the app names a controller: a remapper's pad says so (M22). */
export function padName(c: HostController): string {
  return c.kind === "otherVirtual" ? `${c.name} (a remapper's virtual pad)` : c.name;
}

/** Each listed controller's key: its ids, and for a second of one model its
 * place among them ("1356:3302#2"), so two of one model are two choices,
 * told apart by the order they are found (M23). */
export function padKeys(list: HostController[]): string[] {
  const seen = new Map<string, number>();
  return list.map((c) => {
    const n = (seen.get(padKey(c)) ?? 0) + 1;
    seen.set(padKey(c), n);
    return n === 1 ? padKey(c) : `${padKey(c)}#${n}`;
  });
}

/** A `padKeys` key's model (its ids). */
export function modelOf(key: string): string {
  return key.split("#")[0];
}

/** A `padKeys` key's place among controllers of its model (1 for the first). */
export function ordinalOf(key: string): number {
  return Number(key.split("#")[1] ?? 1);
}

/** Names with a number on those that repeat ("DualSense 1", "DualSense 2"):
 * two controllers of one model, in the order found (M23). */
export function numbered(names: string[]): string[] {
  const seen = new Map<string, number>();
  return names.map((name) => {
    const n = (seen.get(name) ?? 0) + 1;
    seen.set(name, n);
    return names.filter((other) => other === name).length > 1 ? `${name} ${n}` : name;
  });
}

/** Ticking or unticking controller `key` among `keys`, the ones ticked being
 * `ticked`. Two of one model are told apart by the order found (the reader
 * takes the first free one): ticking "DualSense 2" ticks "DualSense 1" too,
 * and unticking the first unticks the second. */
export function togglePad(keys: string[], ticked: string[], key: string, on: boolean): string[] {
  const same = (k: string) => modelOf(k) === modelOf(key);
  return on
    ? keys.filter((k) => k === key || ticked.includes(k) || (same(k) && ordinalOf(k) < ordinalOf(key)))
    : ticked.filter((k) => k !== key && !(same(k) && ordinalOf(k) > ordinalOf(key)));
}

/** How many controllers ticking `key` adds: itself and the earlier ones of
 * its model not ticked yet. */
export function padsAdded(keys: string[], ticked: string[], key: string): number {
  return (
    1 +
    keys.filter((k) => modelOf(k) === modelOf(key) && ordinalOf(k) < ordinalOf(key) && !ticked.includes(k)).length
  );
}
