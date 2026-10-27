import { describe, expect, it } from "vitest";
import { viaRemote, waitText } from "./remote";

const target = { room: "r1", member: "pc" };

describe("commands for a home PC run from afar", () => {
  it("wraps the host's own commands", () => {
    expect(viaRemote(target, { type: "endSession" })).toEqual({
      type: "remote",
      room: "r1",
      member: "pc",
      action: { type: "endSession" },
    });
    expect(viaRemote(target, { type: "passControl", member: "bia", seat: 2, controller: 1 })).toEqual({
      type: "remote",
      room: "r1",
      member: "pc",
      action: { type: "passControl", member: "bia", seat: 2, controller: 1 },
    });
  });

  it("starts a session without the home PC's controllers", () => {
    const wrapped = viaRemote(target, {
      type: "startSession",
      room: "ignored",
      title: "Movie night",
      max_viewers: 4,
      input: { controllers: 0, keyboardMouse: "none" },
      controllers: [{ name: "DualSense", vendor: 1, product: 2, kind: "physical" }],
    });
    expect(wrapped).toEqual({
      type: "remote",
      room: "r1",
      member: "pc",
      action: { type: "startSession", title: "Movie night", max_viewers: 4, input: { controllers: 0, keyboardMouse: "none" } },
    });
  });

  it("leaves ours alone, and everything without a target", () => {
    const watch = { type: "watch", room: "r1", host: "pc" } as const;
    expect(viaRemote(target, watch)).toBe(watch);
    expect(viaRemote(target, { type: "setStreamVolume", percent: 50, muted: false })).toEqual({
      type: "setStreamVolume",
      percent: 50,
      muted: false,
    });
    const end = { type: "endSession" } as const;
    expect(viaRemote(null, end)).toBe(end);
  });

  it("says how long to wait", () => {
    expect(waitText(45)).toBe("45 s");
    expect(waitText(600)).toBe("10 min");
  });
});
