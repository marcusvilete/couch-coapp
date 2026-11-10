import { describe, expect, it } from "vitest";
import type { HostController } from "../bindings/HostController";
import type { SeatView } from "../bindings/SeatView";
import type { SessionView } from "../bindings/SessionView";
import {
  asksForGame,
  gameWindowName,
  screenNeedsWindow,
  keeps,
  listed,
  likelyGame,
  numbered,
  padKeys,
  padsAdded,
  playersOf,
  seatPhrase,
  seatsOf,
  EVERYTHING,
  soundChoices,
  togglePad,
  uploadLimit,
  uploadLimitOk,
} from "./sessions";
import { drawingStopped, drawingSummary, noDrawing, waitsForGameAtPc } from "./sessions";
import type { RoomView } from "../bindings/RoomView";

const seat = (label: string, pad: boolean, holder: string | null = null, host = false): SeatView => ({
  label,
  pad,
  keyboardMouse: !pad,
  holder,
  host,
  hostController: null,
  guestController: null,
});

function session(seats: SeatView[], hostOnSeats = true): SessionView {
  return {
    host: "h",
    title: "Game night",
    maxViewers: 4,
    viewers: ["a", "b"],
    viewerControllers: [2, 1],
    viewerPads: [
      [
        { slot: 0, name: "DualSense" },
        { slot: 1, name: "DualSense" },
      ],
      [{ slot: 0, name: "Xbox Controller" }],
    ],
    ourControllers: [],
    seats,
    hostOnSeats,
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
    stream: { state: "waiting" },
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
    guestsKeyboard: true,
    drawing: noDrawing(),
    namesGame: false,
    warnHotCorner: false,
    gameWindow: null,
  };
}

describe("passing a seat", () => {
  const s = session([seat("Controller 1", true, "a"), seat("Controller 2", true), seat("Keyboard and mouse", false)]);

  it("adds a pad seat while the guest has a controller to spare", () => {
    expect(keeps(s, [0], 2, 1)).toBe(true);
    expect(keeps(s, [0], 1, 1)).toBe(false);
  });

  it("always adds a seat without a pad", () => {
    expect(keeps(s, [0], 1, 2)).toBe(true);
    expect(keeps(s, [0], 0, 2)).toBe(true);
  });

  it("finds a guest's seats", () => {
    expect(seatsOf(s, "a")).toEqual([0]);
    expect(seatsOf(s, undefined)).toEqual([]);
  });
});

describe("who plays", () => {
  it("lists each holder once and the host beside the seats", () => {
    const s = session([seat("Controller 1", true, "a"), seat("Controller 2", true, "a"), seat("Controller 3", true)], false);
    expect(playersOf(s)).toEqual(["a", "h"]);
  });

  it("leaves out a host whose controllers play through seats", () => {
    const s = session([seat("Controller 1", true, "h", true), seat("Controller 2", true)]);
    expect(playersOf(s)).toEqual(["h"]);
  });
});

describe("words", () => {
  it("names seats in sentences", () => {
    expect(seatPhrase("Controller 2")).toBe("Controller 2");
    expect(seatPhrase("Controller")).toBe("the controller");
    expect(listed(["A"])).toBe("A");
    expect(listed(["A", "B", "C"])).toBe("A, B and C");
  });

  it("numbers repeated controller names", () => {
    expect(numbered(["DualSense", "Xbox", "DualSense"])).toEqual(["DualSense 1", "Xbox", "DualSense 2"]);
  });
});

describe("the host's controllers", () => {
  const pad = (name: string, product: number, kind: HostController["kind"] = "physical"): HostController => ({
    name,
    vendor: 1356,
    product,
    kind,
  });
  const list = [pad("DualSense", 3302), pad("DualSense", 3302), pad("Xbox 360", 654, "otherVirtual")];
  const keys = padKeys(list);

  it("tells two of one model apart, and a remapper's pad from a real one", () => {
    expect(keys).toEqual(["1356:3302", "1356:3302#2", "1356:654:remapper"]);
  });

  it("ticks the earlier ones of a model with a later one, and unticks the later ones with an earlier one", () => {
    expect(togglePad(keys, [], keys[1], true)).toEqual([keys[0], keys[1]]);
    expect(padsAdded(keys, [], keys[1])).toBe(2);
    expect(togglePad(keys, [keys[0], keys[1], keys[2]], keys[0], false)).toEqual([keys[2]]);
  });
});

describe("the upload limit", () => {
  it("takes whole Mbit/s from 2 to 10000 only", () => {
    expect(uploadLimit(false, "abc")).toBeNull();
    expect(uploadLimit(true, " 50 ")).toBe(50);
    expect(uploadLimit(true, "1e9")).toBeNaN();
    expect(uploadLimitOk(uploadLimit(true, "1"))).toBe(false);
    expect(uploadLimitOk(uploadLimit(true, "10000"))).toBe(true);
    expect(uploadLimitOk(uploadLimit(true, "50abc"))).toBe(false);
    expect(uploadLimitOk(null)).toBe(true);
  });
});

describe("soundChoices (a Linux host's sound, M16c)", () => {
  it("offers everything but us, then each program playing", () => {
    expect(soundChoices(["Firefox", "Steam"], "").map((c) => c.value)).toEqual([EVERYTHING, "Firefox", "Steam"]);
  });

  it("keeps the program picked before while it isn't playing", () => {
    expect(soundChoices(["Firefox"], "Hollow Knight")[2]).toEqual({
      value: "Hollow Knight",
      label: "Hollow Knight",
      detail: "not playing now",
    });
    expect(soundChoices(["Hollow Knight"], "Hollow Knight")).toHaveLength(2);
    // Not yet known: not said to be silent.
    expect(soundChoices(null, "Hollow Knight")[1].detail).toBeUndefined();
  });
});

describe("naming the game's window (a Linux host, M16c-2)", () => {
  const w = (id: number, title: string, app: string, matches = false) => ({ id, title, app, matches });

  it("offers first the one window with the shared size, and only one", () => {
    expect(likelyGame([w(1, "Editor", "kate"), w(2, "Game", "game", true)])?.id).toBe(2);
    expect(likelyGame([w(1, "A", "a", true), w(2, "B", "b", true)])).toBeNull();
    expect(likelyGame([w(1, "Editor", "kate")])).toBeNull();
    expect(likelyGame([])).toBeNull();
  });

  it("names a window by its title and program", () => {
    expect(gameWindowName(w(1, "Celeste", "celeste"))).toBe("Celeste · celeste");
    expect(gameWindowName(w(1, " ", ""))).toBe("Untitled window");
  });

  it("asks the host only where a game is named, and not for desktop control", () => {
    const s = session([seat("Keyboard and mouse", false)]);
    expect(asksForGame(s)).toBe(false);
    expect(asksForGame({ ...s, namesGame: true })).toBe(true);
    expect(asksForGame({ ...s, namesGame: true, desktop: "d" })).toBe(false);
  });

  it("refuses a screen with a keyboard seat, but not from afar nor a Linux host's dialog", () => {
    const here = { remote: false, shared: false };
    expect(screenNeedsWindow(true, "screen", here)).toBe(true);
    expect(screenNeedsWindow(true, "window", here)).toBe(false);
    expect(screenNeedsWindow(false, "screen", here)).toBe(false);
    expect(screenNeedsWindow(true, undefined, here)).toBe(false);
    // From afar: desktop control's seat.
    expect(screenNeedsWindow(true, "screen", { remote: true, shared: false })).toBe(false);
    // Linux: the desktop's dialog, listed as a screen; the game is named after.
    expect(screenNeedsWindow(true, "screen", { remote: false, shared: true })).toBe(false);
  });

  it("tells a device running the PC when a guest's keys wait for the game there", () => {
    const live: Partial<SessionView> = {
      stream: { state: "live" },
      streaming: { name: "Shared", fps: 60, maxMbps: 20, uploadMbps: null, screen: true },
    };
    const s = { ...session([seat("Keyboard and mouse", false, "a")]), ...live } as SessionView;
    expect(waitsForGameAtPc(s)).toBe(true);
    // Named there: its focus is known.
    expect(waitsForGameAtPc({ ...s, gameInFront: false })).toBe(false);
    // Desktop control, or nobody on the seat, or a window streamed.
    expect(waitsForGameAtPc({ ...s, desktop: "a" })).toBe(false);
    expect(waitsForGameAtPc({ ...s, seats: [seat("Keyboard and mouse", false)] })).toBe(false);
    expect(waitsForGameAtPc({ ...s, streaming: { ...s.streaming!, screen: false } })).toBe(false);
  });
});

describe("drawing on the stream", () => {
  const room = { members: [{ id: "a", name: "Ana" }, { id: "b", name: "Bia" }] } as unknown as RoomView;

  it("says what is on the board", () => {
    expect(drawingSummary(room, noDrawing())).toBe("Nothing drawn yet.");
    const d = {
      ...noDrawing(),
      on: true,
      elements: 4,
      drawers: [
        { member: "a", elements: 1 },
        { member: "b", elements: 3 },
      ],
    };
    expect(drawingSummary(room, d)).toBe("4 drawings by Ana and Bia.");
    expect(drawingSummary(room, { ...d, elements: 1, drawers: [{ member: "b", elements: 1 }] })).toBe(
      "1 drawing by Bia.",
    );
  });

  it("knows whom the host stopped", () => {
    const s = { ...session([]), drawing: { ...noDrawing(), stopped: ["b"] } };
    expect(drawingStopped(s, "b")).toBe(true);
    expect(drawingStopped(s, "a")).toBe(false);
  });
});
