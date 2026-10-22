import { describe, expect, it } from "vitest";
import type { RoomView } from "../bindings/RoomView";
import { moved, namesListed, needsUs, ordered, roomLine } from "./rooms";

function room(id: string, over: Partial<RoomView> = {}): RoomView {
  return {
    id,
    revision: 1,
    name: id,
    isOwner: true,
    members: [{ id: "me", name: "Ana", isOwner: true, link: "me", otherVersion: null }],
    joinsPausedForSecs: null,
    sessions: [],
    catchingUp: false,
    waitingFor: [],
    needsPassword: false,
    offeringTo: null,
    offered: null,
    canOffer: true,
    ...over,
  };
}

describe("room order", () => {
  it("keeps the user's order and puts new rooms last", () => {
    const rooms = [room("a"), room("b"), room("c")];
    expect(ordered(rooms, ["c", "a"]).map((r) => r.id)).toEqual(["c", "a", "b"]);
  });

  it("moves a room to a place", () => {
    expect(moved(["a", "b", "c"], "a", 2)).toEqual(["b", "c", "a"]);
    expect(moved(["a", "b", "c"], "c", 0)).toEqual(["c", "a", "b"]);
    expect(moved(["a", "b"], "a", 9)).toEqual(["b", "a"]);
  });
});

describe("a room's line", () => {
  it("says what waits for us first", () => {
    expect(roomLine(room("a", { offered: false }))).toBe("Offered to you");
    expect(needsUs(room("a", { offered: false }))).toBe(true);
    expect(needsUs(room("a", { offered: true }))).toBe(false);
  });

  it("counts who else is online", () => {
    const members = [
      { id: "me", name: "Ana", isOwner: true, link: "me" as const, otherVersion: null },
      { id: "b", name: "Bia", isOwner: false, link: "relay" as const, otherVersion: null },
      { id: "c", name: "Caio", isOwner: false, link: "offline" as const, otherVersion: null },
    ];
    expect(roomLine(room("a", { members }))).toBe("1 online");
    expect(roomLine(room("a"))).toBe("Only you");
  });

  it("lists names", () => {
    expect(namesListed([])).toBe("the others");
    expect(namesListed(["Ana", "Bia", "Caio"])).toBe("Ana, Bia and Caio");
  });
});
