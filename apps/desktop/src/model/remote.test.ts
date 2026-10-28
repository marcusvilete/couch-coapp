import { describe, expect, it } from "vitest";
import type { SeatView } from "../bindings/SeatView";
import type { SessionView } from "../bindings/SessionView";
import { desktopBlocked, desktopCommand, viaRemote, waitText } from "./remote";

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

  it("brings the home PC's window forward, and controls its desktop (M25b)", () => {
    expect(viaRemote(target, { type: "bringToFront" })).toEqual({
      type: "remote",
      room: "r1",
      member: "pc",
      action: { type: "bringToFront" },
    });
    // Already the home PC's: passed through as it is.
    const desktop = desktopCommand(target, true);
    expect(desktop).toEqual({ type: "remote", room: "r1", member: "pc", action: { type: "desktop", on: true } });
    expect(viaRemote(target, desktop)).toBe(desktop);
  });
});

describe("when desktop control can start (M25b)", () => {
  const seat = (keyboardMouse: boolean): SeatView => ({
    label: keyboardMouse ? "Keyboard and mouse" : "Controller 1",
    pad: !keyboardMouse,
    keyboardMouse,
    holder: null,
    host: false,
    hostController: null,
    guestController: null,
  });
  const session = (over: Partial<SessionView>) =>
    ({
      seats: [seat(false), seat(true)],
      role: "watching",
      streaming: { name: "Screen 1", fps: 30, maxMbps: 20, uploadMbps: null, screen: true },
      controlPaused: false,
      ...over,
    }) as SessionView;

  it("needs the keyboard seat, watching, a stream, and control allowed", () => {
    expect(desktopBlocked(session({}))).toBeNull();
    expect(desktopBlocked(session({ role: "controlling" }))).toBeNull();
    expect(desktopBlocked(session({ seats: [seat(false)] }))).toMatch(/no keyboard-and-mouse seat/);
    expect(desktopBlocked(session({ role: "none" }))).toMatch(/Watch/);
    expect(desktopBlocked(session({ streaming: null }))).toMatch(/Stream/);
    expect(desktopBlocked(session({ controlPaused: true }))).toMatch(/paused/);
  });
});
