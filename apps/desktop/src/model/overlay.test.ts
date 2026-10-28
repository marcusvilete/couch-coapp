import { describe, expect, it } from "vitest";
import type { RoomView } from "../bindings/RoomView";
import type { SessionView } from "../bindings/SessionView";
import { desktopBanner, sessionEvents } from "./overlay";

const names: Record<string, string> = { a: "Ana", b: "Bia" };
const name = (m: string) => names[m] ?? "someone";

function view(over: Partial<SessionView>): SessionView {
  return {
    host: "h",
    title: "t",
    maxViewers: 4,
    viewers: [],
    viewerControllers: [],
    viewerPads: [],
    ourControllers: [],
    seats: [
      { label: "Controller 1", pad: true, keyboardMouse: false, holder: null, host: false, hostController: null, guestController: null },
      { label: "Controller 2", pad: true, keyboardMouse: false, holder: null, host: false, hostController: null, guestController: null },
    ],
    hostOnSeats: false,
    controlPaused: false,
    role: "host",
    requests: [],
    askedSeat: null,
    panicKey: null,
    answerKeys: null,
    overlayArea: null,
    hostControllers: [],
    hostHomes: [],
    hostControllerProblem: null,
    stream: { state: "live" },
    streaming: null,
    soundLevel: null,
    padsSettingUp: [],
    padsSettingUpSecs: 0,
    padsMissing: [],
    streamWindow: null,
    gameInFront: null,
    note: null,
    operator: false,
    operatedBy: [],
    desktop: null,
    ...over,
  };
}

const held = (holders: (string | null)[]) =>
  holders.map((holder, i) => ({
    label: `Controller ${i + 1}`,
    pad: true,
    keyboardMouse: false,
    holder,
    host: false,
    hostController: null,
    guestController: null,
  }));

describe("notes over the game", () => {
  it("says who started and stopped watching", () => {
    expect(sessionEvents(name, view({ viewers: ["a"] }), view({ viewers: ["b"] }))).toEqual([
      "Bia started watching",
      "Ana stopped watching",
    ]);
  });

  it("says who got a seat, and who lost one without moving", () => {
    const before = view({ viewers: ["a", "b"], seats: held(["a", null]) });
    expect(sessionEvents(name, before, view({ viewers: ["a", "b"], seats: held(["b", null]) }))).toEqual([
      "Bia now plays Controller 1",
      "Ana no longer plays Controller 1",
    ]);
    // Moving to another seat is only the new seat.
    expect(sessionEvents(name, before, view({ viewers: ["a", "b"], seats: held([null, "a"]) }))).toEqual([
      "Ana now plays Controller 2",
    ]);
  });

  it("says a request ended unless it was granted", () => {
    const before = view({ viewers: ["a"], requests: [{ member: "a", seat: 1, controller: null }] });
    expect(sessionEvents(name, before, view({ viewers: ["a"] }))).toEqual(["Ana's request ended"]);
    expect(sessionEvents(name, before, view({ viewers: ["a"], seats: held([null, "a"]) }))).toEqual([
      "Ana now plays Controller 2",
    ]);
  });

  it("says control was paused", () => {
    expect(sessionEvents(name, view({}), view({ controlPaused: true }))).toEqual([
      "Control paused: allow it again in the app",
    ]);
  });
});

describe("the desktop control banner (M25b)", () => {
  const room = { members: [{ id: "b", name: "Ana's laptop" }] } as unknown as RoomView;

  it("names the device and the key that ends it, while it lasts", () => {
    expect(desktopBanner(room, view({}))).toEqual([]);
    expect(desktopBanner(room, view({ desktop: "b", panicKey: "Ctrl+Alt+Shift+Backspace" }))).toEqual([
      {
        kind: "banner",
        id: "desktop:b",
        text: "Ana's laptop is controlling this PC",
        panic: "Ctrl+Alt+Shift+Backspace",
      },
    ]);
  });
});
