import { describe, expect, it } from "vitest";
import type { HostController } from "../bindings/HostController";
import type { SeatView } from "../bindings/SeatView";
import type { SessionView } from "../bindings/SessionView";
import {
  keeps,
  listed,
  numbered,
  padKeys,
  padsAdded,
  playersOf,
  seatPhrase,
  seatsOf,
  togglePad,
  uploadLimit,
  uploadLimitOk,
} from "./sessions";

const seat = (label: string, pad: boolean, holder: string | null = null, host = false): SeatView => ({
  label,
  pad,
  keyboardMouse: !pad,
  holder,
  host,
  hostController: null,
});

function session(seats: SeatView[], hostOnSeats = true): SessionView {
  return {
    host: "h",
    title: "Game night",
    maxViewers: 4,
    viewers: ["a", "b"],
    viewerControllers: [2, 1],
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
