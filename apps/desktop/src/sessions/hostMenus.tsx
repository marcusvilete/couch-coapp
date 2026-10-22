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
  return s.seats.flatMap((seat, i) =>
    held.includes(i)
      ? []
      : [
          {
            label: seat.label,
            icon: seat.pad ? <Gamepad2 /> : <Keyboard />,
            hint: [
              seat.host ? "yours" : seat.holder ? `${nameOf(room, seat.holder)}'s` : "free",
              held.length > 0 ? (keeps(s, held, controllers, i) ? "added" : "moves them") : null,
            ]
              .filter(Boolean)
              .join(" · "),
            onSelect: () => void run(`pass-${member}`, { type: "passControl", member, seat: i }),
          },
        ],
  );
}

/** A viewer's entries: pass a seat, take theirs back. */
export function viewerEntries(room: RoomView, s: SessionView, member: string, run: Run): MenuEntry[] {
  if (!s.viewers.includes(member)) return [];
  const held = seatsOf(s, member);
  const entries: MenuEntry[] = [];
  if (s.seats.length === 1) {
    if (held.length === 0) {
      entries.push({
        label: `Pass control to ${nameOf(room, member)}`,
        icon: <Hand />,
        disabled: s.controlPaused && PAUSED_REASON,
        onSelect: () => void run(`pass-${member}`, { type: "passControl", member, seat: 0 }),
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
