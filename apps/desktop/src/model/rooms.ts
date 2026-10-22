// What the UI works out from the engine's views of rooms: their order, a
// line saying what's going on in each, who is online. Pure functions.
import type { MemberLink } from "../bindings/MemberLink";
import type { MemberView } from "../bindings/MemberView";
import type { OtherVersion } from "../bindings/OtherVersion";
import type { RoomView } from "../bindings/RoomView";
import { isLive, nameOf } from "./sessions";

/** `rooms` in the user's order; rooms not placed yet go last, in the
 * order given (by name). */
export function ordered(rooms: RoomView[], order: string[]): RoomView[] {
  const rank = new Map(order.map((id, i) => [id, i]));
  const placed = rooms.filter((r) => rank.has(r.id)).sort((a, b) => rank.get(a.id)! - rank.get(b.id)!);
  return [...placed, ...rooms.filter((r) => !rank.has(r.id))];
}

/** The order after moving `id` to place `to`. */
export function moved(ids: string[], id: string, to: number): string[] {
  const rest = ids.filter((r) => r !== id);
  rest.splice(Math.max(0, Math.min(to, rest.length)), 0, id);
  return rest;
}

/** "Ana", "Ana and Bia", "Ana, Bia and Caio". */
export function namesListed(names: string[]): string {
  if (names.length <= 1) return names[0] ?? "the others";
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

export const LINKS: Record<MemberLink, { label: string; title: string }> = {
  me: { label: "you", title: "This is you" },
  direct: { label: "direct", title: "Connected directly" },
  relay: { label: "relayed", title: "Connected through a relay server: works, with more delay" },
  online: { label: "online", title: "Online, but not connected to you yet" },
  offline: { label: "offline", title: "Not running Couch Co-App right now" },
};

/** Running Couch Co-App now: us, and those connected or seen online. */
export function isOnline(m: MemberView): boolean {
  return m.link !== "offline";
}

/** Connected to us now: the owner can offer them the room (M13). */
export function isConnected(m: MemberView): boolean {
  return m.link === "direct" || m.link === "relay";
}

/** Why two members can't talk: their Couch Co-App versions differ (M10a). */
export function otherVersionText(name: string, other: OtherVersion): string {
  return other.newer
    ? `${name} has Couch Co-App ${other.appVersion}, newer than yours: install the new version to play together.`
    : `${name} has Couch Co-App ${other.appVersion}, older than yours: they need to install the new version to play together.`;
}

export function owner(room: RoomView): MemberView | undefined {
  return room.members.find((m) => m.isOwner);
}

/** Something waits for us in the room: a request in the session we host,
 * or an offer of the room to answer. */
export function needsUs(room: RoomView): boolean {
  return room.offered === false || room.sessions.some((s) => s.role === "host" && s.requests.length > 0);
}

/** One line under a room's name: what's going on there. */
export function roomLine(room: RoomView): string {
  const ours = room.sessions.find((s) => s.role !== "none");
  if (ours?.role === "host") {
    const n = ours.requests.length;
    if (n > 0) return n === 1 ? "1 asks to play" : `${n} ask to play`;
    return isLive(ours) ? "You're live" : "Your session";
  }
  if (ours) return `Watching ${nameOf(room, ours.host)}`;
  if (room.offered === false) return "Offered to you";
  if (room.catchingUp) return "Catching up…";
  const live = room.sessions.find(isLive);
  if (live) return `${nameOf(room, live.host)} is live`;
  const session = room.sessions[0];
  if (session) return `${nameOf(room, session.host)} has a session`;
  const others = room.members.filter((m) => m.link !== "me");
  const online = others.filter(isOnline).length;
  if (others.length === 0) return "Only you";
  return online === 0 ? "Nobody else online" : `${online} online`;
}
