import { describe, expect, it } from "vitest";
import { seatsFor } from "./seats";

const labels = (controllers: number, keyboardMouse: "none" | "ownSeat" | "withController1") =>
  seatsFor({ controllers, keyboardMouse }).map((s) => s.label);

describe("seat labels, as the engine makes them", () => {
  it("numbers controllers only when there are several", () => {
    expect(labels(1, "none")).toEqual(["Controller"]);
    expect(labels(2, "ownSeat")).toEqual(["Controller 1", "Controller 2", "Keyboard and mouse"]);
  });

  it("puts keyboard and mouse with the first controller when asked", () => {
    const seats = seatsFor({ controllers: 2, keyboardMouse: "withController1" });
    expect(seats.map((s) => s.label)).toEqual(["Controller, keyboard and mouse", "Controller 1"]);
    expect(seats[0]).toMatchObject({ pad: true, keyboardMouse: true });
  });

  it("makes a keyboard-only seat", () => {
    expect(seatsFor({ controllers: 0, keyboardMouse: "ownSeat" })).toMatchObject([
      { label: "Keyboard and mouse", pad: false, keyboardMouse: true },
    ]);
  });
});
