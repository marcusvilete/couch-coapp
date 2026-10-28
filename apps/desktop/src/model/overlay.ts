// What the host overlay shows (M8): the requests waiting for an answer, and
// short notes about the session, worked out by comparing one view of the
// host's session with the next. Sent from the main window to the overlay.

import type { RoomView } from "../bindings/RoomView";
import type { SessionView } from "../bindings/SessionView";
import { nameOf, seatPhrase } from "./sessions";

export type Toast =
  | {
      kind: "request";
      id: string;
      /** "bob asks for Controller 2". */
      text: string;
      /** The keys that answer it, while the hook is in place. */
      accept: string | null;
      decline: string | null;
      /** The oldest request: the one the keys answer. */
      first: boolean;
    }
  | { kind: "event"; id: string; text: string }
  /** Desktop control (M25b): stays while it lasts, whatever the overlay
   * setting, so whoever is at this PC sees it. */
  | { kind: "banner"; id: string; text: string; panic: string | null };

/** The banner while a device of ours controls this PC's desktop. */
export function desktopBanner(room: RoomView, s: SessionView): Toast[] {
  if (s.desktop == null) return [];
  return [
    {
      kind: "banner",
      id: `desktop:${s.desktop}`,
      text: `${nameOf(room, s.desktop)} is controlling this PC`,
      panic: s.panicKey,
    },
  ];
}

/** One toast per pending request, oldest first. */
export function requestToasts(room: RoomView, s: SessionView): Toast[] {
  return s.requests.map((r, i) => ({
    kind: "request",
    id: `request:${r.member}:${r.seat}`,
    text: `${nameOf(room, r.member)} asks for ${seatPhrase(s.seats[r.seat]?.label ?? "a seat")}`,
    accept: s.answerKeys?.accept ?? null,
    decline: s.answerKeys?.decline ?? null,
    first: i === 0,
  }));
}

/** What changed from `before` to `after` worth a note. The host's own seat
 * changes are their own doing and get none. `name` names a member, someone
 * who just left the room included. */
export function sessionEvents(
  name: (member: string) => string,
  before: SessionView,
  after: SessionView,
): string[] {
  const notes: string[] = [];
  for (const v of after.viewers) {
    if (!before.viewers.includes(v)) notes.push(`${name(v)} started watching`);
  }
  for (const v of before.viewers) {
    if (!after.viewers.includes(v)) notes.push(`${name(v)} stopped watching`);
  }
  after.seats.forEach((seat, i) => {
    const old = before.seats[i]?.holder ?? null;
    const now = seat.holder;
    if (old === now) return;
    const phrase = seatPhrase(seat.label);
    if (now != null && now !== after.host) notes.push(`${name(now)} now plays ${phrase}`);
    // Lost it, to nobody or to someone else. Someone who stopped watching
    // was told of above, and someone who moved to another seat plays that
    // (a seat they just got). One who gave back one of several seats (M18)
    // hears of it.
    const moved = after.seats.some((s, j) => s.holder === old && before.seats[j]?.holder !== old);
    if (old != null && old !== after.host && !moved && after.viewers.includes(old)) {
      notes.push(`${name(old)} no longer plays ${phrase}`);
    }
  });
  // Answered (declined, or by the panic key) or withdrawn: which, the view
  // doesn't say, so the note doesn't either.
  for (const r of before.requests) {
    const still = after.requests.some((a) => a.member === r.member);
    const seated = after.seats[r.seat]?.holder === r.member;
    if (!still && !seated && after.viewers.includes(r.member)) {
      notes.push(`${name(r.member)}'s request ended`);
    }
  }
  if (after.controlPaused && !before.controlPaused) {
    notes.push("Control paused: allow it again in the app");
  }
  return notes;
}

/** The session this profile hosts, if any, with its room. */
export function hosted(rooms: RoomView[]): { room: RoomView; session: SessionView } | null {
  for (const room of rooms) {
    const session = room.sessions.find((s) => s.role === "host");
    if (session) return { room, session };
  }
  return null;
}
