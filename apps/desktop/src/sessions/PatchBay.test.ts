import { describe, expect, it } from "vitest";
import type { RoomView } from "../bindings/RoomView";
import type { SessionView } from "../bindings/SessionView";
import { hostSession } from "../preview/fakeEngine";
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
