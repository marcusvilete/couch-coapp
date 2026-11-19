import { afterEach, describe, expect, it, vi } from "vitest";
import { KEYS, storedSetup } from "./storage";

/** The web storage holding `items`, as the webview's would. */
function stored(items: Record<string, string>) {
  vi.stubGlobal("window", { localStorage: { getItem: (key: string) => items[key] ?? null } });
}

afterEach(() => vi.unstubAllGlobals());

describe("the last session setup", () => {
  it("comes back as it was saved", () => {
    stored({ [KEYS.sessionSetup]: JSON.stringify({ controllers: 0, keyboardMouse: "none" }) });
    expect(storedSetup()).toEqual({ controllers: 0, keyboardMouse: "none" });
  });

  it("falls back to one controller for anything else", () => {
    stored({ [KEYS.sessionSetup]: JSON.stringify({ controllers: 5, keyboardMouse: "none" }) });
    expect(storedSetup()).toEqual({ controllers: 1, keyboardMouse: "none" });
    stored({ [KEYS.sessionSetup]: JSON.stringify({ controllers: 2, keyboardMouse: "everything" }) });
    expect(storedSetup()).toEqual({ controllers: 1, keyboardMouse: "none" });
    stored({});
    expect(storedSetup()).toEqual({ controllers: 1, keyboardMouse: "none" });
  });
});
