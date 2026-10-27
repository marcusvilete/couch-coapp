// What the UI remembers in this profile's web storage: conveniences only
// (the last session setup, the room order), so a failure to read or write
// loses nothing that matters. The keys keep their names across versions, so
// people keep their last choices.
import type { InputSetup } from "../bindings/InputSetup";
import type { KeyboardMouseSeat } from "../bindings/KeyboardMouseSeat";

export const KEYS = {
  /** The room shown last. */
  selectedRoom: "selectedRoom",
  /** Room ids in the order the user dragged them into. */
  roomOrder: "roomOrder",
  /** The single controller chosen before M19: still read when no list (M23)
   * is stored, so the choice stays. */
  myPad: "couch.goLive.controller",
  /** The controllers last chosen to play on seats, as `padKeys` (M23). */
  myPads: "couch.session.controllers",
  sessionSetup: "couch.session.setup",
  sessionTitle: "couch.session.title",
  streamFps: "couch.stream.fps",
  streamQuality: "couch.stream.quality",
  /** Settings → Overlay. */
  overlay: "overlay",
  /** Settings → Appearance (M21). */
  theme: "couch.theme",
} as const;

export function readStored(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

export function writeStored(key: string, value: string) {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    // A convenience only.
  }
}

/** A remembered number, if it is one of `allowed`. */
export function storedNumber(key: string, allowed: number[], fallback: number): number {
  const n = Number(readStored(key));
  return allowed.includes(n) ? n : fallback;
}

/** The controllers last chosen to play on seats, as keys: the list (M23),
 * or the single choice from before it; `null` if none was ever made. */
export function storedPads(): string[] | null {
  try {
    const list = JSON.parse(readStored(KEYS.myPads) ?? "null");
    if (Array.isArray(list) && list.every((k) => typeof k === "string")) return list;
  } catch {
    // Unreadable: as if none.
  }
  const one = readStored(KEYS.myPad);
  return one == null ? null : one === "" ? [] : [one];
}

/** The setup last used, or one controller. Keyboard and mouse "with
 * Controller 1", from before M24, comes back as a seat of their own. */
export function storedSetup(): InputSetup {
  const fallback: InputSetup = { controllers: 1, keyboardMouse: "none" };
  try {
    const s = JSON.parse(readStored(KEYS.sessionSetup) ?? "null");
    const kbm: KeyboardMouseSeat[] = ["none", "ownSeat"];
    const keyboardMouse = s?.keyboardMouse === "withController1" ? "ownSeat" : s?.keyboardMouse;
    if (Number.isInteger(s?.controllers) && s.controllers >= 0 && s.controllers <= 4 && kbm.includes(keyboardMouse)) {
      return { controllers: s.controllers, keyboardMouse };
    }
  } catch {
    // Not one: the default.
  }
  return fallback;
}

/** Room ids in the user's order. */
export function storedOrder(): string[] {
  try {
    const order: unknown = JSON.parse(readStored(KEYS.roomOrder) ?? "[]");
    return Array.isArray(order) ? order.filter((id): id is string => typeof id === "string") : [];
  } catch {
    return [];
  }
}
