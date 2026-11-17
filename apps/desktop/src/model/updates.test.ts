import { describe, expect, it } from "vitest";
import { newVersionText } from "./updates";

describe("newVersionText", () => {
  it("names the version", () => {
    expect(newVersionText({ version: "0.40.0", wireChanged: false })).toBe("Couch Co-App 0.40.0 is out.");
  });

  it("says everyone needs it when it plays only with itself", () => {
    expect(newVersionText({ version: "0.40.0", wireChanged: true })).toBe(
      "Couch Co-App 0.40.0 is out. Everyone in your rooms needs it to play together.",
    );
  });
});
