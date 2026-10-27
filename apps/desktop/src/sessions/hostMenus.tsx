// While we host: what can be done for a viewer (pass them a seat, take one
// back) and for a seat (pass it, take it back, play it with one of our
// controllers). The console's buttons, its right-click menus and the
// members panel's menu all use these lists. The engine decides; the hints
// ("added", "moves them", "swaps…") say what it will do.
import { Gamepad2, Hand, Keyboard, Undo2 } from "lucide-react";
import type { RoomView } from "../bindings/RoomView";
import type { SessionView } from "../bindings/SessionView";
import { keeps, nameOf, numbered, seatsOf } from "../model/sessions";
import type { MenuEntry } from "../ui/Menu";
import type { Run } from "./useRun";

export const PAUSED_REASON = "Allow control first";

export function controllersOf(s: SessionView, member: string): number {
  return s.viewerControllers[s.viewers.indexOf(member)] ?? 0;
}

/** The seats `member` could be passed, as menu entries. */
export function passEntries(room: RoomView, s: SessionView, member: string, run: Run): MenuEntry[] {
  const held = seatsOf(s, member);
  const controllers = controllersOf(s, member);
  return s.seats.flatMap((seat, i): MenuEntry[] => {
    if (held.includes(i)) return [];
    const pass = () => void run(`pass-${member}`, { type: "passControl", member, seat: i });
    // With two controllers or more, which one plays it (P12).
    const pads = guestPadEntries(room, s, member, i, run);
    return [
      {
        label: seat.label,
        icon: seat.pad ? <Gamepad2 /> : <Keyboard />,
        hint: [
          seat.host ? "yours" : seat.holder ? `${nameOf(room, seat.holder)}'s` : "free",
          held.length > 0 ? (keeps(s, held, controllers, i) ? "added" : "moves them") : null,
        ]
          .filter(Boolean)
          .join(" · "),
        ...(pads.length > 0
          ? { items: [{ label: "Whichever they pick up", onSelect: pass }, "separator" as const, ...pads] }
          : { onSelect: pass }),
      },
    ];
  });
}

/** A viewer's entries: pass a seat, take theirs back. */
export function viewerEntries(room: RoomView, s: SessionView, member: string, run: Run): MenuEntry[] {
  if (!s.viewers.includes(member)) return [];
  const held = seatsOf(s, member);
  const entries: MenuEntry[] = [];
  if (s.seats.length === 1) {
    if (held.length === 0) {
      const pass = () => void run(`pass-${member}`, { type: "passControl", member, seat: 0 });
      const pads = guestPadEntries(room, s, member, 0, run);
      entries.push({
        label: `Pass control to ${nameOf(room, member)}`,
        icon: <Hand />,
        disabled: s.controlPaused && PAUSED_REASON,
        ...(pads.length > 0
          ? { items: [{ label: "Whichever they pick up", onSelect: pass }, "separator" as const, ...pads] }
          : { onSelect: pass }),
      });
    }
  } else {
    entries.push({
      label: held.length > 0 ? "Pass another seat" : "Pass a seat",
      icon: <Hand />,
      disabled: s.controlPaused && PAUSED_REASON,
      items: passEntries(room, s, member, run),
    });
  }
  // A seat they hold, to another of their controllers (P12).
  for (const i of held) {
    const pads = guestPadEntries(room, s, member, i, run);
    if (pads.length > 0) {
      entries.push({
        label: s.seats.length === 1 ? "Play it with" : `Play ${s.seats[i].label} with`,
        icon: <Gamepad2 />,
        items: pads,
      });
    }
  }
  for (const i of held) {
    entries.push({
      label: s.seats.length === 1 ? "Take back the controller" : `Take back ${s.seats[i].label}`,
      icon: <Undo2 />,
      onSelect: () => void run(`take-${i}`, { type: "takeBackControl", seat: i }),
    });
  }
  return entries;
}

/** Our controllers' names, numbered where two are of one model. */
export function ourPads(s: SessionView): string[] {
  return numbered(s.hostControllers);
}

/** A viewer's controllers (P12), by slot, their names numbered where two
 * are of one model (`model`, the name as it came). */
export function guestPads(s: SessionView, member: string): { slot: number; name: string; model: string }[] {
  const pads = s.viewerPads[s.viewers.indexOf(member)] ?? [];
  const names = numbered(pads.map((p) => p.name));
  return pads.map((p, k) => ({ slot: p.slot, name: names[k], model: p.name }));
}

/** The plug a seat's cable runs from: the controller driving it (ours, or a
 * guest's, P12), else its holder. */
export function cableFrom(s: SessionView, i: number): string | undefined {
  const seat = s.seats[i];
  if (!seat?.holder) return undefined;
  if (seat.host) return `pad:${seat.hostController ?? 0}`;
  const slot = seat.guestController;
  if (slot != null && guestPads(s, seat.holder).some((p) => p.slot === slot)) return `gpad:${seat.holder}:${slot}`;
  return `viewer:${seat.holder}`;
}

/** What putting `member`'s controller `slot` on seat `i` does (P12), or why
 * it can't. */
export function guestPadHint(room: RoomView, s: SessionView, member: string, slot: number, i: number): { ok: boolean; hint: string } {
  const seat = s.seats[i];
  if (!seat) return { ok: false, hint: "" };
  if (!seat.pad) return { ok: false, hint: "No controller on this seat" };
  const held = seatsOf(s, member);
  // Another seat of theirs this controller drives: it takes another one.
  const other = held.find((j) => j !== i && s.seats[j].guestController === slot);
  if (held.includes(i)) {
    // A pad seat of theirs alone follows the controller touched last; the
    // one driving it now may still be chosen, to stay.
    const alone = held.filter((j) => s.seats[j].pad).length === 1;
    if (seat.guestController === slot) return alone ? { ok: true, hint: "Only this one plays it" } : { ok: false, hint: "Already here" };
    return { ok: true, hint: other != null ? `Swaps with ${s.seats[other].label}` : "Plays here" };
  }
  if (s.controlPaused) return { ok: false, hint: PAUSED_REASON };
  const adds = held.length > 0 && keeps(s, held, controllersOf(s, member), i);
  const what = held.length === 0 ? "Hands it over" : adds ? "Added" : "Moves them";
  const swaps = adds && other != null ? ` · ${s.seats[other].label} takes another of theirs` : "";
  const loses = seat.host ? " · yours" : seat.holder ? ` · ${nameOf(room, seat.holder)} loses it` : "";
  const kbm = seat.keyboardMouse ? " · control of your PC" : "";
  return { ok: true, hint: what + swaps + loses + kbm };
}

/** Passing seat `i` to `member` with one of their controllers (P12), as menu
 * entries; none while they have fewer than two to choose from. */
function guestPadEntries(room: RoomView, s: SessionView, member: string, i: number, run: Run): MenuEntry[] {
  const pads = guestPads(s, member);
  if (!s.seats[i]?.pad || pads.length < 2) return [];
  return pads.flatMap((p): MenuEntry[] => {
    const { ok, hint } = guestPadHint(room, s, member, p.slot, i);
    return ok
      ? [
          {
            label: p.name,
            icon: <Gamepad2 />,
            hint: hint.charAt(0).toLowerCase() + hint.slice(1),
            onSelect: () => void run(`pass-${member}`, { type: "passControl", member, seat: i, controller: p.slot }),
          },
        ]
      : [];
  });
}

/** What "Play here" with controller `k` on seat `i` does. */
function playHint(room: RoomView, s: SessionView, pads: string[], k: number, i: number): string {
  const seat = s.seats[i];
  const own = s.hostHomes[k];
  const other = s.hostHomes.findIndex((home, j) => j !== k && home === i);
  const ownSeat = own != null ? s.seats[own] : undefined;
  const what =
    own === i
      ? "back on its seat"
      : other >= 0
        ? `swaps seats with your ${pads[other]}`
        : ownSeat?.host
          ? `leaves ${ownSeat.label}`
          : "";
  const loses = !seat.host && seat.holder ? `${nameOf(room, seat.holder)} loses it` : "";
  return [what, loses].filter(Boolean).join(" · ");
}

/** A seat's entries: take it back (or leave it, ours), play it with one of
 * our controllers, pass it to a viewer. */
export function seatEntries(room: RoomView, s: SessionView, i: number, run: Run): MenuEntry[] {
  const seat = s.seats[i];
  const pads = ourPads(s);
  const entries: MenuEntry[] = [];
  if (seat.holder) {
    entries.push({
      label: seat.host ? "Leave this seat" : `Take back from ${nameOf(room, seat.holder)}`,
      icon: <Undo2 />,
      hint: "It stays empty",
      onSelect: () => void run(`take-${i}`, { type: "takeBackControl", seat: i }),
    });
  }
  if (seat.pad && pads.length === 1 && !seat.host) {
    entries.push({
      label: "Play here",
      icon: <Gamepad2 />,
      hint: playHint(room, s, pads, 0, i),
      onSelect: () => void run(`play-${i}`, { type: "playSeat", seat: i, controller: 0 }),
    });
  }
  if (seat.pad && pads.length > 1) {
    const choices = pads.flatMap((name, k): MenuEntry[] =>
      // Each controller's own seat, held or not: onto another's own seat,
      // the two swap (HostSession::play).
      s.hostHomes[k] === i && seat.host
        ? []
        : [
            {
              label: name,
              hint: playHint(room, s, pads, k, i),
              onSelect: () => void run(`play-${i}`, { type: "playSeat", seat: i, controller: k }),
            },
          ],
    );
    if (choices.length > 0) entries.push({ label: "Play here with", icon: <Gamepad2 />, items: choices });
  }
  const viewers = s.viewers.filter((v) => !seatsOf(s, v).includes(i));
  if (viewers.length > 0) {
    entries.push({
      label: "Pass to",
      icon: <Hand />,
      disabled: s.controlPaused && PAUSED_REASON,
      items: viewers.map((v) => {
        const held = seatsOf(s, v);
        return {
          label: nameOf(room, v),
          hint: held.length > 0 ? (keeps(s, held, controllersOf(s, v), i) ? "added" : "moves them") : undefined,
          onSelect: () => void run(`pass-${v}`, { type: "passControl", member: v, seat: i }),
        };
      }),
    });
  }
  return entries;
}
