// The menus of a room and of a member: the same entries on the "⋯" button
// and on right-click. An entry the engine would refuse is disabled with why.
import { ArrowDown, ArrowUp, Ban, Crown, DoorOpen, KeyRound, Link2Off, LogOut, UserPlus } from "lucide-react";
import type { MemberView } from "../bindings/MemberView";
import type { RoomView } from "../bindings/RoomView";
import { isConnected } from "../model/rooms";
import type { MenuEntry } from "../ui/Menu";
import type { RoomActions } from "./actions";

export const CATCHING_UP_REASON = "Catching up with the room: once everyone has been online";
export const ACCEPTED_REASON = "You accepted the offer of this room: wait until it is handed over or withdrawn";

export function roomMenu(room: RoomView, a: RoomActions, place: { index: number; count: number }): MenuEntry[] {
  const entries: MenuEntry[] = [];
  if (room.isOwner) {
    entries.push(
      room.needsPassword
        ? { label: "Set a password…", icon: <KeyRound />, onSelect: a.setPassword }
        : {
            label: "Invite people…",
            icon: <UserPlus />,
            onSelect: a.invite,
            disabled: room.catchingUp && CATCHING_UP_REASON,
          },
    );
    if (!room.needsPassword) entries.push({ label: "Change password…", icon: <KeyRound />, onSelect: a.setPassword });
    entries.push({ label: "Revoke all invites…", icon: <Link2Off />, onSelect: a.revoke }, "separator");
  }
  entries.push(
    { label: "Move up", icon: <ArrowUp />, onSelect: () => a.moveBy(-1), disabled: place.index === 0 },
    { label: "Move down", icon: <ArrowDown />, onSelect: () => a.moveBy(1), disabled: place.index >= place.count - 1 },
    "separator",
    room.isOwner
      ? { label: "Close room for everyone…", icon: <DoorOpen />, danger: true, onSelect: a.leave }
      : {
          label: "Leave room…",
          icon: <LogOut />,
          danger: true,
          onSelect: a.leave,
          disabled: room.offered === true && ACCEPTED_REASON,
        },
  );
  return entries;
}

/** A member's menu; `hosting` adds the session's entries for them (seats)
 * while we host. Empty for ourselves outside a session. */
export function memberMenu(room: RoomView, m: MemberView, a: RoomActions, hosting: MenuEntry[] = []): MenuEntry[] {
  const entries: MenuEntry[] = [...hosting];
  const room_: MenuEntry[] = [];
  if (m.link !== "me" && room.canOffer && isConnected(m)) {
    room_.push({ label: "Make owner…", icon: <Crown />, onSelect: () => a.offer(m) });
  }
  if (room.isOwner && m.link !== "me") {
    room_.push({
      label: `Ban ${m.name}…`,
      icon: <Ban />,
      danger: true,
      onSelect: () => a.ban(m),
      disabled: room.catchingUp && CATCHING_UP_REASON,
    });
  }
  if (entries.length > 0 && room_.length > 0) entries.push("separator");
  return [...entries, ...room_];
}
