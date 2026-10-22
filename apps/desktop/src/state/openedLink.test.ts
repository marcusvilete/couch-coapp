import { describe, expect, it } from "vitest";
import { linkOpened, opensJoin, takesOfferItself } from "./openedLink";

describe("an opened invite link", () => {
  it("opens the join screen only when nothing else is up", () => {
    expect(opensJoin("none")).toBe(true);
    expect(opensJoin("add")).toBe(true);
    expect(opensJoin("join")).toBe(false);
    expect(opensJoin("other")).toBe(false);
  });

  it("tells the user it waits behind anything else", () => {
    expect(linkOpened("other").notify).toBe(true);
    expect(linkOpened("none").notify).toBe(false);
    expect(linkOpened("add").notify).toBe(false);
    // The join dialog offers it instead.
    expect(linkOpened("join").notify).toBe(false);
  });

  it("never slips in under a typed password or a join under way", () => {
    expect(takesOfferItself({ busy: false, password: "" })).toBe(true);
    expect(takesOfferItself({ busy: false, password: "x" })).toBe(false);
    expect(takesOfferItself({ busy: true, password: "" })).toBe(false);
  });
});
