// The room's header: its name, your part in it, who's online, and its main
// action (Invite, or Set a password where it has none), with the room's menu.
import { Crown, KeyRound, MoreHorizontal, UserPlus, Users } from "lucide-react";
import type { RoomView } from "../bindings/RoomView";
import { isOnline } from "../model/rooms";
import type { RoomActions } from "../rooms/actions";
import { CATCHING_UP_REASON } from "../rooms/menus";
import { Avatar } from "../ui/Avatar";
import { Button, IconButton } from "../ui/Button";
import { DropdownMenu, type MenuEntry } from "../ui/Menu";
import "./RoomHeader.css";

export function RoomHeader(props: {
  room: RoomView;
  actions: RoomActions;
  menu: MenuEntry[];
  /** The members panel is a drawer (narrow windows): this opens it. */
  onMembers?: () => void;
}) {
  const { room, actions: a } = props;
  const online = room.members.filter(isOnline).length;
  return (
    <header className="room-header">
      <Avatar id={room.id} name={room.name} square size={32} />
      <div className="room-titles">
        <h1 className="room-title">{room.name}</h1>
        <span className="room-sub">
          {room.isOwner ? (
            <>
              <Crown aria-hidden="true" /> You own it
            </>
          ) : (
            "Member"
          )}
          <span className="room-sub-sep">·</span>
          <span className="tabular">
            {online} of {room.members.length} online
          </span>
        </span>
      </div>
      <div className="room-actions">
        {room.isOwner &&
          (room.needsPassword ? (
            <Button variant="primary" icon={<KeyRound />} onClick={a.setPassword}>
              Set a password
            </Button>
          ) : (
            <Button
              variant="primary"
              icon={<UserPlus />}
              onClick={a.invite}
              disabled={room.catchingUp}
              reason={room.catchingUp ? CATCHING_UP_REASON : null}
            >
              Invite
            </Button>
          ))}
        {props.onMembers && (
          <IconButton label="Members" onClick={props.onMembers} variant="secondary">
            <Users />
          </IconButton>
        )}
        <DropdownMenu items={props.menu}>
          <IconButton label="Room options" variant="secondary">
            <MoreHorizontal />
          </IconButton>
        </DropdownMenu>
      </div>
    </header>
  );
}
