import { describe, expect, it } from "vitest";
import type { MemberView } from "../bindings/MemberView";
import type { RoomView } from "../bindings/RoomView";
import type { UnreachableView } from "../bindings/UnreachableView";
import { inviteRelayText, joinProblemText, memberReason, roomReason, staleInvitesText } from "./reasons";

const needsCode = (host: string): UnreachableView => ({
  reason: { type: "needsCode", url: `https://${host}/`, host },
  details: "timed out",
});

function member(name: string, over: Partial<MemberView> = {}): MemberView {
  return { id: name, name, isOwner: false, link: "offline", otherVersion: null, remote: null, unreachable: null, ...over };
}

function room(members: MemberView[]): RoomView {
  return {
    id: "r",
    revision: 1,
    name: "Sofá",
    isOwner: true,
    members: [member("Me", { link: "me" }), ...members],
    joinsPausedForSecs: null,
    sessions: [],
    catchingUp: false,
    waitingFor: [],
    needsPassword: false,
    offeringTo: null,
    offered: null,
    canOffer: true,
    staleInvites: null,
  };
}

describe("why a member can't be reached", () => {
  it("names the relay, and offers a code where one would fix it", () => {
    const said = memberReason("Bia", needsCode("relay.example.com").reason);
    expect(said.short).toBe("needs a code");
    expect(said.text).toContain("ask Bia for it");
    expect(said.fix).toEqual({ host: "relay.example.com" });
    expect(memberReason("Bia", { type: "ourConnection" }).fix).toBeNull();
    expect(
      memberReason("Bia", { type: "relayUnreachable", url: "u", host: "r.example", ours: true }).text,
    ).toContain("Settings → Network");
  });

  it("is said once for the room when everyone shares it", () => {
    const bia = member("Bia", { unreachable: needsCode("relay.example.com") });
    const caio = member("Caio", { unreachable: needsCode("relay.example.com") });
    expect(roomReason(room([bia, caio]))?.text).toBe(
      "Bia and Caio are on relay.example.com, which needs a code you don't have: ask for it.",
    );
    // Different causes, someone reached, or someone with no reason: nothing.
    expect(roomReason(room([bia, member("Caio", { unreachable: needsCode("other.example") })]))).toBeNull();
    expect(roomReason(room([bia, member("Caio", { link: "direct" })]))).toBeNull();
    expect(roomReason(room([bia, member("Caio")]))).toBeNull();
    expect(roomReason(room([]))).toBeNull();
    const ours = { reason: { type: "ourConnection" as const }, details: null };
    expect(roomReason(room([member("Bia", { unreachable: ours })])), "the strip says it").toBeNull();
  });
});

describe("joining", () => {
  const view = (relay: Parameters<typeof inviteRelayText>[0]["relay"]) => ({
    url: "https://relay.example.com/",
    host: "relay.example.com",
    relay,
  });

  it("says before the password when the owner's relay needs a code", () => {
    expect(inviteRelayText(view({ type: "ours" }), false)).toBeNull();
    expect(inviteRelayText(view({ type: "unchecked" }), false)).toBeNull();
    expect(inviteRelayText(view({ type: "unchecked" }), true)?.text).toContain("Checking");
    const said = inviteRelayText(view({ type: "needsCode" }), false);
    expect(said?.fix).toEqual({ host: "relay.example.com" });
    expect(said?.text).toContain("You can still try");
  });

  it("names the step that failed", () => {
    expect(joinProblemText({ type: "ownerNotOnRelay", host: "relay.example.com" }).text).toContain("link may be old");
    expect(joinProblemText({ type: "needsCode", url: "u", host: "relay.example.com" }).fix).toEqual({
      host: "relay.example.com",
    });
  });
});

describe("stale links", () => {
  it("count the old links and name where to now", () => {
    expect(staleInvitesText({ count: 1, hosts: ["old.example"], now: "new.example" })).toBe(
      "An invite link you made points to old.example, but you're on new.example now: people with it can't join. Send a new link.",
    );
    expect(staleInvitesText({ count: 2, hosts: ["a.example", "b.example"], now: "c.example" })).toContain(
      "2 invite links you made point to a.example and b.example",
    );
  });
});
