// What the UI works out from the engine's view of sessions: who plays, what
// a pass would do, how to name seats and controllers. Pure functions; the
// engine decides everything, these only read its state.
import type { DrawingView } from "../bindings/DrawingView";
import type { GameWindowView } from "../bindings/GameWindowView";
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
        case "nothingShared":
          return "Nothing was chosen to share; nothing is streamed now.";
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
        case "nothingShared":
          return "Nothing was chosen in the PC's screen-sharing dialog, so the stream stopped. Started from afar, a stream shares only what the desktop remembers: share a screen at the PC once.";
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

/** The choice of everything but the app, among program names: a menu's
 * values can't be empty, and no program's name holds a NUL. */
export const EVERYTHING = "\u0000";

/** A Linux host's sound choices (M16c): everything but the app
 * (`EVERYTHING`), or one program playing now (`programs`, `null` while
 * unknown). The program picked before (`remembered`, `""` for everything)
 * stays a choice while it isn't playing, so it can be kept. */
export function soundChoices(
  programs: string[] | null,
  remembered: string,
): { value: string; label: string; detail?: string }[] {
  const choices: { value: string; label: string; detail?: string }[] = [
    { value: EVERYTHING, label: "Everything but Couch Co-App" },
  ];
  for (const p of programs ?? []) {
    if (p !== "" && p !== EVERYTHING) choices.push({ value: p, label: p });
  }
  if (remembered !== "" && !choices.some((c) => c.value === remembered)) {
    choices.push({ value: remembered, label: remembered, detail: programs ? "not playing now" : undefined });
  }
  return choices;
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

/** The game's window to offer first on a Linux host (M16c-2): the one window
 * with the shared picture's size, if exactly one has it. */
export function likelyGame(windows: GameWindowView[]): GameWindowView | null {
  const matching = windows.filter((w) => w.matches);
  return matching.length === 1 ? matching[0] : null;
}

/** A game window as menus and the console name it: "Title · program". */
export function gameWindowName(w: GameWindowView): string {
  const title = w.title.trim() || "Untitled window";
  return w.app.trim() ? `${title} · ${w.app.trim()}` : title;
}

/** Whether the console asks the host to name the game's window (M16c-2): a
 * keyboard seat on a Linux host's shared screen, unless desktop control
 * holds that seat (its keys reach the whole desktop, no window needed). */
export function asksForGame(s: SessionView): boolean {
  return s.namesGame && s.desktop == null;
}

/** Whether the stream picker refuses a screen: a session with a
 * keyboard-and-mouse seat needs the game's window, except a stream started
 * from afar (the seat is desktop control's, M25b) and a Linux host's one
 * source, the desktop's dialog, listed as a screen: its host names the game
 * once it streams (M16c-2; refusing it there was found in the VM run). */
export function screenNeedsWindow(
  kbmSeats: boolean,
  picked: "window" | "screen" | undefined,
  options: { remote: boolean; shared: boolean },
): boolean {
  return kbmSeats && picked === "screen" && !options.remote && !options.shared;
}

/** For a home PC's session run from a device (M16c-2): a guest holds the
 * keyboard seat on a streamed screen, and no game window is named at the PC
 * yet (its focus is unknown), so their keys reach nothing. */
export function waitsForGameAtPc(s: SessionView): boolean {
  return (
    isLive(s) &&
    s.streaming?.screen === true &&
    s.desktop == null &&
    s.gameInFront == null &&
    s.seats.some((seat) => seat.keyboardMouse && seat.holder != null && !seat.host)
  );
}

// ---- Drawing on the stream (M27a) ----

/** A board with nothing on it, drawing off: a session's view before any. */
export function noDrawing(): DrawingView {
  return { on: false, elements: 0, drawers: [], stopped: [], pictures: false };
}

/** What the console says of the board: "Nothing drawn yet", "4 drawings by
 * Bia and Caio". */
export function drawingSummary(room: RoomView, d: DrawingView): string {
  if (d.elements === 0) return "Nothing drawn yet.";
  // The host draws too, over the game (M27b): "you" then.
  const me = room.members.find((m) => m.link === "me")?.id;
  const who = listed(d.drawers.map((x) => (x.member === me ? "you" : nameOf(room, x.member))));
  return `${d.elements} ${d.elements === 1 ? "drawing" : "drawings"} by ${who}.`;
}

/** Whether the host stopped `member` drawing. */
export function drawingStopped(s: SessionView, member: string): boolean {
  return s.drawing.stopped.includes(member);
}
