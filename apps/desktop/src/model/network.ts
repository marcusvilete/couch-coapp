// Our own connection in plain words (M26a, ADR 0014 "Showing what's wrong"):
// each relay's state, this network's, the trouble the shell's strip says,
// and the warning before removing a relay. "Relay" and "relay code", never
// the network library's own terms.
import type { NetworkTrouble } from "../bindings/NetworkTrouble";
import type { NetworkView } from "../bindings/NetworkView";
import type { RelayStateView } from "../bindings/RelayStateView";
import type { RelayUseView } from "../bindings/RelayUseView";

/** The one line that says what a relay is, the first time it appears. */
export const RELAY_LINE = "A relay passes your connection along when your PCs can't connect directly.";

export type Tone = "ok" | "idle" | "pending" | "warn" | "danger";

export type Said = { text: string; tone: Tone };

/** A relay's state, with iroh's error or the relay's own words behind
 * Details. */
export function relayState(state: RelayStateView): Said & { details: string | null } {
  switch (state.type) {
    case "checking":
      return { text: "Checking…", tone: "pending", details: null };
    case "home":
      return { text: "Connected: others reach you through it", tone: "ok", details: null };
    case "connected":
      return { text: "Connected", tone: "ok", details: null };
    case "idle":
      return { text: "Answers, not needed right now", tone: "idle", details: null };
    case "unreachable":
      return { text: "Not answering", tone: "warn", details: state.details };
    case "refused":
      return {
        text: "No longer accepts your code: ask its owner for a new one",
        tone: "danger",
        details: state.reason || null,
      };
  }
}

/** This network: online, offline, a sign-in page, direct connections. */
export function networkLine(view: NetworkView): Said {
  switch (view.internet) {
    case "offline":
      return { text: "You're not connected to the internet.", tone: "danger" };
    case "signIn":
      return { text: "This network wants you to sign in first: open a web page to sign in.", tone: "warn" };
    case "unknown":
      return { text: "Looking at this network…", tone: "pending" };
    case "online":
      if (view.udp === false) {
        return {
          text: "This network blocks direct connections: everything goes through a relay, with more delay and a lower picture.",
          tone: "warn",
        };
      }
      return { text: view.udp ? "Online. Direct connections work here." : "Online.", tone: "ok" };
  }
}

export type TroubleFix = "check" | "pasteCode" | "settings";

/** What the shell's strip says, and the button that fixes it. */
export function troubleStrip(
  trouble: NetworkTrouble,
  fallingBack: boolean,
): { text: string; fix: TroubleFix; label: string } {
  switch (trouble.type) {
    case "noInternet":
      return { text: "You're not connected to the internet: nobody can reach you.", fix: "check", label: "Check again" };
    case "signIn":
      return {
        text: "This network wants you to sign in first: open a web page to sign in.",
        fix: "check",
        label: "Check again",
      };
    case "relayRefused":
      return {
        text: `${trouble.host} no longer accepts your relay code: ask its owner for a new one.`,
        fix: "pasteCode",
        label: "Paste a new code",
      };
    case "noRelay":
      return {
        text: fallingBack
          ? "None of your relays answers: the public relays stand in for now."
          : "None of your relays answers: friends may not reach you.",
        fix: "settings",
        label: "Network settings",
      };
  }
}

/** "Bia", "Bia and Caio", "Bia, Caio and Dani". */
export function names(list: string[]): string {
  if (list.length <= 1) return list[0] ?? "";
  return `${list.slice(0, -1).join(", ")} and ${list[list.length - 1]}`;
}

/** The warning before removing a relay: who is reached through it, and
 * remote play. Empty when nothing depends on it. */
export function removeWarning(use: RelayUseView): string[] {
  const lines = use.rooms.map(
    (r) => `Without it, you'll reach ${names(r.members)} in “${r.name}” only over a direct path, if one works.`,
  );
  if (use.remotePlay) {
    lines.push(
      "Remote play is on: a device of yours that reaches this PC through this relay may be cut off, with nobody here to fix it.",
    );
  }
  return lines;
}
