import { describe, expect, it } from "vitest";
import type { NetworkView } from "../bindings/NetworkView";
import { names, networkLine, relayState, removeWarning, troubleStrip } from "./network";

const online: NetworkView = {
  relays: [],
  publicInUse: true,
  keepPublic: false,
  publicFallback: false,
  fallingBack: false,
  public: { type: "home" },
  homeRelay: null,
  internet: "online",
  udp: true,
  trouble: null,
  testBuild: false,
  changeable: true,
  askForRelay: false,
};

describe("our own connection in plain words", () => {
  it("says each relay's state, with the details behind it", () => {
    expect(relayState({ type: "home" }).tone).toBe("ok");
    expect(relayState({ type: "unreachable", details: "timed out" })).toMatchObject({
      tone: "warn",
      details: "timed out",
    });
    const refused = relayState({ type: "refused", reason: "bad token" });
    expect(refused.text).toMatch(/ask its owner for a new one/);
    expect(refused.details).toBe("bad token");
    expect(relayState({ type: "refused", reason: "" }).details).toBeNull();
  });

  it("says what is wrong with this network first", () => {
    expect(networkLine({ ...online, internet: "offline" }).text).toMatch(/not connected to the internet/);
    expect(networkLine({ ...online, internet: "signIn" }).text).toMatch(/sign in/);
    expect(networkLine({ ...online, udp: false }).text).toMatch(/blocks direct connections/);
    expect(networkLine({ ...online, udp: null }).text).toBe("Online.");
    expect(networkLine({ ...online, internet: "unknown" }).tone).toBe("pending");
  });

  it("gives each trouble the button that fixes it", () => {
    expect(troubleStrip({ type: "relayRefused", url: "https://r.example/", host: "r.example" })).toMatchObject({
      fix: "pasteCode",
      text: expect.stringContaining("r.example"),
    });
    expect(troubleStrip({ type: "noInternet" }).fix).toBe("check");
    expect(troubleStrip({ type: "noRelay" }).fix).toBe("settings");
  });

  it("warns before removing a relay others reach us through", () => {
    expect(names(["Bia"])).toBe("Bia");
    expect(names(["Bia", "Caio", "Dani"])).toBe("Bia, Caio and Dani");
    expect(removeWarning({ rooms: [], remotePlay: false })).toEqual([]);
    const lines = removeWarning({
      rooms: [{ room: "r1", name: "Friday games", members: ["Bia", "Caio"] }],
      remotePlay: true,
    });
    expect(lines[0]).toBe("Without it, you'll reach Bia and Caio in “Friday games” only over a direct path, if one works.");
    expect(lines[1]).toMatch(/Remote play is on/);
  });
});
