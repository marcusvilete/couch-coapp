import { describe, expect, it } from "vitest";
import { seatsFor } from "./seats";

const labels = (controllers: number, keyboardMouse: "none" | "ownSeat") =>
  seatsFor({ controllers, keyboardMouse }).map((s) => s.label);

describe("seat labels, as the engine makes them", () => {
  it("numbers controllers only when there are several", () => {
    expect(labels(1, "none")).toEqual(["Controller"]);
    expect(labels(2, "ownSeat")).toEqual(["Controller 1", "Controller 2", "Keyboard and mouse"]);
  });

  it("makes a keyboard-only seat", () => {
    expect(seatsFor({ controllers: 0, keyboardMouse: "ownSeat" })).toMatchObject([
      { label: "Keyboard and mouse", pad: false, keyboardMouse: true },
    ]);
  });

  it("makes no seats to watch together", () => {
    expect(seatsFor({ controllers: 0, keyboardMouse: "none" })).toEqual([]);
  });
});
