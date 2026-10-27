import { describe, expect, it } from "vitest";
import type { RoomView } from "../bindings/RoomView";
import type { SessionView } from "../bindings/SessionView";
import { hostSession } from "../preview/fakeEngine";
import { cableFrom, guestPads } from "./hostMenus";
import { dropOn } from "./PatchBay";

const room = {
  members: [
    { id: "h", name: "Ana", isOwner: true, link: "me", otherVersion: null },
    { id: "b", name: "Bia", isOwner: false, link: "direct", otherVersion: null },
    { id: "c", name: "Caio", isOwner: false, link: "relay", otherVersion: null },
  ],
} as unknown as RoomView;

/** Three pads and a keyboard seat; our two controllers on the first two;
 * Bia (two controllers) on the third, Caio (one) watching. */
function session(over: Partial<SessionView> = {}): SessionView {
  const s = hostSession("h", "t", 4, { controllers: 3, keyboardMouse: "ownSeat" }, ["DualSense", "Xbox"]);
  s.viewers = ["b", "c"];
  s.viewerControllers = [2, 1];
  s.seats[2].holder = "b";
  return Object.assign(s, over);
}

const viewer = (member: string) => ({ kind: "viewer" as const, member, from: `viewer:${member}` });
const pad = (controller: number) => ({ kind: "pad" as const, controller, from: `pad:${controller}` });

describe("dropping on a seat in the patch bay", () => {
  it("hands a seat over, saying what happens to its holder", () => {
    expect(dropOn(room, session(), viewer("c"), 2)).toEqual({ ok: true, hint: "Hands it over · Bia loses it" });
    expect(dropOn(room, session(), viewer("c"), 0)).toEqual({ ok: true, hint: "Hands it over · yours" });
  });

  it("says added or moves them, as the host's session decides", () => {
    expect(dropOn(room, session(), viewer("b"), 1).hint).toMatch(/^Added/);
    const s = session({ viewerControllers: [1, 1] });
    expect(dropOn(room, s, viewer("b"), 1).hint).toMatch(/^Moves them/);
  });

  it("warns that the keyboard seat is control of the PC", () => {
    expect(dropOn(room, session(), viewer("c"), 3).hint).toContain("control of your PC");
  });

  it("refuses every hand-over while control is paused", () => {
    expect(dropOn(room, session({ controlPaused: true }), viewer("c"), 3)).toEqual({ ok: false, hint: "Allow control first" });
  });

  it("puts our controllers only on seats with a pad, swapping two of ours", () => {
    expect(dropOn(room, session(), pad(0), 3)).toEqual({ ok: false, hint: "No controller on this seat" });
    expect(dropOn(room, session(), pad(0), 1).hint).toBe("Swaps with your Xbox");
    expect(dropOn(room, session(), pad(0), 0).ok).toBe(false);
    expect(dropOn(room, session(), pad(1), 2).hint).toBe("Plays here · Bia loses it");
  });

  it("moves a picked-up cable as its holder would go", () => {
    const cable = { kind: "cable" as const, seat: 2, from: "seat:2" };
    expect(dropOn(room, session(), cable, 2).ok).toBe(false);
    expect(dropOn(room, session(), cable, 1).hint).toMatch(/^Added/);
  });
});

/** P12: Bia's two controllers, the second on seat 3; Caio's one. */
function withPads(over: Partial<SessionView> = {}): SessionView {
  const s = session();
  s.viewerPads = [
    [
      { slot: 0, name: "DualSense" },
      { slot: 2, name: "DualSense" },
    ],
    [{ slot: 0, name: "Xbox Controller" }],
  ];
  s.seats[2].guestController = 2;
  return Object.assign(s, over);
}

const guestPad = (member: string, controller: number) => ({
  kind: "guestPad" as const,
  member,
  controller,
  from: `gpad:${member}:${controller}`,
});

describe("a guest's controllers in the patch bay", () => {
  it("names them, numbered when alike, and runs a seat's cable from the one driving it", () => {
    const s = withPads();
    expect(guestPads(s, "b").map((p) => p.name)).toEqual(["DualSense 1", "DualSense 2"]);
    expect(cableFrom(s, 2)).toBe("gpad:b:2");
    // Our own seat, and a guest that hasn't said which drives it.
    expect(cableFrom(s, 0)).toBe("pad:0");
    s.seats[2].guestController = null;
    expect(cableFrom(s, 2)).toBe("viewer:b");
  });

  it("hands a seat over with one controller, as added or a move", () => {
    expect(dropOn(room, withPads(), guestPad("b", 0), 1)).toEqual({ ok: true, hint: "Added · yours" });
    // The one already playing their other seat: that seat takes another.
    expect(dropOn(room, withPads(), guestPad("b", 2), 1)).toEqual({
      ok: true,
      hint: "Added · Controller 3 takes another of theirs · yours",
    });
    expect(dropOn(room, withPads(), guestPad("c", 0), 2)).toEqual({ ok: true, hint: "Hands it over · Bia loses it" });
    expect(dropOn(room, withPads({ viewerControllers: [1, 1] }), guestPad("b", 0), 1).hint).toMatch(/^Moves them/);
  });

  it("moves a seat they hold to their other controller, and swaps two", () => {
    const s = withPads();
    // Their one seat follows the controller touched last: the one on it
    // now may be kept to it.
    expect(dropOn(room, s, guestPad("b", 2), 2)).toEqual({ ok: true, hint: "Only this one plays it" });
    expect(dropOn(room, s, guestPad("b", 0), 2)).toEqual({ ok: true, hint: "Plays here" });
    s.seats[1].holder = "b";
    s.seats[1].host = false;
    s.seats[1].guestController = 0;
    expect(dropOn(room, s, guestPad("b", 0), 2)).toEqual({ ok: true, hint: "Swaps with Controller 2" });
    expect(dropOn(room, s, guestPad("b", 2), 2)).toEqual({ ok: false, hint: "Already here" });
    // Even while control is paused: nothing changes hands.
    s.controlPaused = true;
    expect(dropOn(room, s, guestPad("b", 0), 2).ok).toBe(true);
    expect(dropOn(room, s, guestPad("c", 0), 0)).toEqual({ ok: false, hint: "Allow control first" });
  });

  it("puts a controller only on a seat with a pad", () => {
    expect(dropOn(room, withPads(), guestPad("b", 0), 3)).toEqual({ ok: false, hint: "No controller on this seat" });
  });

  it("drops a picked-up cable as the controller at its end", () => {
    const cable = { kind: "cable" as const, seat: 2, from: "seat:2" };
    expect(dropOn(room, withPads(), cable, 1)).toEqual({ ok: true, hint: "Added · Controller 3 takes another of theirs · yours" });
  });
});
