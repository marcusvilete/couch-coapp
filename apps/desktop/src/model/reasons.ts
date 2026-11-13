// What the app says about why someone or something can't be reached (M26a-2,
// ADR 0014 "Showing what's wrong"): a member, a room, an invite's relay, a
// join that failed, links gone stale. Hints, never verdicts: a direct path
// may still connect. Pure functions.
import type { InviteRelayView } from "../bindings/InviteRelayView";
import type { JoinProblem } from "../bindings/JoinProblem";
import type { MemberReason } from "../bindings/MemberReason";
import type { MemberView } from "../bindings/MemberView";
import type { RoomView } from "../bindings/RoomView";
import type { StaleInvites } from "../bindings/StaleInvites";
import type { UnreachableView } from "../bindings/UnreachableView";
import { namesListed } from "./rooms";

/** A relay whose code would fix it: "Paste a code" offers it. */
export type CodeFix = { host: string };

export type Said = {
  /** A few words, beside "offline" in the member list. */
  short: string;
  /** The sentence, in a tooltip or a callout. */
  text: string;
  fix: CodeFix | null;
};

/** Why `name` can't be reached. */
export function memberReason(name: string, reason: MemberReason): Said {
  switch (reason.type) {
    case "ourConnection":
      return {
        short: "your connection",
        text: "Your own connection isn't working: the strip at the bottom says why.",
        fix: null,
      };
    case "relayUnreachable":
      return {
        short: "relay down",
        text: reason.ours
          ? `${name} is on ${reason.host}, one of your relays, which isn't answering: Settings → Network shows it.`
          : `${name} is on ${reason.host}, which isn't answering.`,
        fix: null,
      };
    case "relayRefused":
      return {
        short: "code refused",
        text: `${name} is on ${reason.host}, which no longer accepts your code: ask its owner for a new one.`,
        fix: { host: reason.host },
      };
    case "needsCode":
      return {
        short: "needs a code",
        text: `${name} is on ${reason.host}, which needs a code you don't have: ask ${name} for it. Either of you holding the other's relay is enough.`,
        fix: { host: reason.host },
      };
    case "relayNotAllowed":
      return {
        short: "relay not used",
        text: `${name} is on ${reason.host}, a relay this app doesn't use (only https to a named server): they need another relay.`,
        fix: null,
      };
  }
}

/** The member's reason, if the engine gave one. */
export function unreachableOf(m: MemberView): Said | null {
  return m.unreachable ? memberReason(m.name, m.unreachable.reason) : null;
}

function key(u: UnreachableView): string {
  const r = u.reason;
  return r.type === "ourConnection" ? r.type : `${r.type} ${r.host}`;
}

/** The room's one line (ADR 0014): when nobody else can be reached and
 * they share a cause, it is said once at the top. Our own connection is the
 * shell's strip's to say, with its button: no line for it here. */
export function roomReason(room: RoomView): Said | null {
  const others = room.members.filter((m) => m.link !== "me");
  if (others.length === 0) return null;
  if (others.some((m) => m.link === "direct" || m.link === "relay")) return null;
  const reasons = others.map((m) => m.unreachable);
  if (reasons.some((u) => u == null)) return null;
  const first = reasons[0]!;
  if (reasons.some((u) => key(u!) !== key(first))) return null;
  const everyone = others.length === 1 ? others[0]!.name : namesListed(others.map((m) => m.name));
  const said = memberReason(everyone, first.reason);
  const r = first.reason;
  switch (r.type) {
    case "ourConnection":
      return null;
    case "needsCode":
      return {
        ...said,
        text: `${everyone} ${others.length === 1 ? "is" : "are"} on ${r.host}, which needs a code you don't have: ask for it.`,
      };
    default:
      return said;
  }
}

/** The join dialog's relay row, before the password. `null`: nothing worth
 * saying (ours, public, or open to all). */
export function inviteRelayText(view: InviteRelayView, checking: boolean): Said | null {
  const r = view.relay;
  switch (r.type) {
    case "ours":
    case "public":
    case "open":
      return null;
    case "unchecked":
      return checking ? { short: "checking", text: `Checking ${view.host}, the owner's relay…`, fix: null } : null;
    case "needsCode":
      return {
        short: "needs a code",
        text: `This room's owner uses ${view.host}, which needs a code: ask them for it. You can still try.`,
        fix: { host: view.host },
      };
    case "unreachable":
      return {
        short: "not answering",
        text: `${view.host}, the owner's relay, isn't answering right now.`,
        fix: null,
      };
    case "notAllowed":
      return {
        short: "not used",
        text: `This link names ${view.host}, a relay this app doesn't use: ask for a new link.`,
        fix: null,
      };
  }
}

/** A join that didn't reach the owner: which step failed. */
export function joinProblemText(p: JoinProblem): Said {
  switch (p.type) {
    case "ourConnection":
      return {
        short: "your connection",
        text: "Couldn't reach the owner: your own connection isn't working (the strip at the bottom says why).",
        fix: null,
      };
    case "relayUnreachable":
      return {
        short: "relay not answering",
        text: `Couldn't reach ${p.host}, the owner's relay. Try again in a moment.`,
        fix: null,
      };
    case "relayRefused":
      return {
        short: "code refused",
        text: `${p.host}, the owner's relay, no longer accepts your code: ask its owner for a new one.`,
        fix: { host: p.host },
      };
    case "needsCode":
      return {
        short: "needs a code",
        text: `The owner uses ${p.host}, which needs a code: ask them for it, then join.`,
        fix: { host: p.host },
      };
    case "ownerNotOnRelay":
      return {
        short: "owner not there",
        text: `${p.host} answered, but the owner isn't on it: they may be offline, or the link may be old (ask for a new one).`,
        fix: null,
      };
  }
}

/** For the owner: links made before their relay moved. `here`: in the
 * invite dialog, beside the new link. */
export function staleInvitesText(s: StaleInvites, here = false): string {
  const where = s.hosts.length === 1 ? s.hosts[0] : namesListed(s.hosts);
  const links = s.count === 1 ? "An invite link you made points" : `${s.count} invite links you made point`;
  const them = s.count === 1 ? "it" : "them";
  const then = here ? `Send this one to anyone who has ${them}.` : "Send a new link.";
  return `${links} to ${where}, but you're on ${s.now} now: people with ${them} can't join. ${then}`;
}

/** The line at the foot of Create a room and Invite while only the public
 * relays are in use (the author's hint, M26a-2). */
export const PUBLIC_RELAYS_HINT =
  "Your connections go through the public relays. Settings → Network adds a relay of your own.";

/** Why a code is personal (the author's hint, M26a-2). */
export const PERSONAL_WHY =
  "Given to you alone, so the relay's owner can take it back without changing everyone's code. It isn't shared, here or with your rooms.";
