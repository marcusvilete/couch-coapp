// Who's in the room: online first, then offline. Each row says how they're
// connected to us, what they're doing (streaming, watching, playing), and
// whether their Couch Co-App is another version; their menu (right-click or
// "⋯") holds what can be done to them. Rows hold still while a menu is open.
import { Crown, Eye, Gamepad2, MoreHorizontal, Radio, Tv, X } from "lucide-react";
import { useRef, useState } from "react";
import type { MemberView } from "../bindings/MemberView";
import type { RoomView } from "../bindings/RoomView";
import { LINKS, isOnline, otherVersionText } from "../model/rooms";
import { isLive, playersOf } from "../model/sessions";
import { Avatar } from "../ui/Avatar";
import { IconButton } from "../ui/Button";
import { Badge } from "../ui/Feedback";
import { ContextMenu, DropdownMenu, type MenuEntry } from "../ui/Menu";
import { Tooltip } from "../ui/Tooltip";
import type { RoomActions } from "./actions";
import { memberMenu } from "./menus";
import "./MembersPanel.css";

export function MembersPanel(props: {
  room: RoomView;
  actions: RoomActions;
  /** While we host: the session's entries for a member (seats). */
  hostEntries?: (member: MemberView) => MenuEntry[];
  /** Shown as a drawer over the room (narrow windows): it can be closed. */
  onClose?: () => void;
}) {
  const { room } = props;
  // The order while a menu is open: presence changing meanwhile moves no row
  // from under the pointer.
  // Each row's place and group (online or not) as the menu opened.
  type Frozen = { order: string[]; online: Set<string> };
  const [frozen, setFrozen] = useState<Frozen | null>(null);
  const live = useRef<Frozen>({ order: [], online: new Set() });
  const sorted = [...room.members.filter(isOnline), ...room.members.filter((m) => !isOnline(m))];
  live.current = { order: sorted.map((m) => m.id), online: new Set(sorted.filter(isOnline).map((m) => m.id)) };
  const members = frozen
    ? [
        ...frozen.order.flatMap((id) => sorted.filter((m) => m.id === id)),
        ...sorted.filter((m) => !frozen.order.includes(m.id)),
      ]
    : sorted;
  const shownOnline = (m: MemberView) => (frozen && frozen.order.includes(m.id) ? frozen.online.has(m.id) : isOnline(m));
  const online = members.filter(shownOnline);
  const offline = members.filter((m) => !shownOnline(m));
  const holdRows = (open: boolean) => setFrozen(open ? live.current : null);

  const row = (m: MemberView) => {
    const items = memberMenu(room, m, props.actions, props.hostEntries?.(m) ?? []);
    const hosting = room.sessions.find((s) => s.host === m.id);
    const plays = room.sessions.some((s) => playersOf(s).includes(m.id));
    const watches = room.sessions.some((s) => s.viewers.includes(m.id));
    return (
      <ContextMenu key={m.id} items={items} onOpenChange={holdRows}>
        <li className={`member${m.link === "offline" ? " away" : ""}`}>
          <Tooltip label={LINKS[m.link].title} side="left">
            <span className="member-avatar">
              <Avatar id={m.id} name={m.name} presence={m.link} dimmed={m.link === "offline"} />
            </span>
          </Tooltip>
          <span className="member-text">
            <span className="member-name">
              <span className="member-name-text">{m.name}</span>
              {m.isOwner && (
                <Tooltip label="Owner: the one who lets people in">
                  <Crown className="member-crown" aria-label="owner" />
                </Tooltip>
              )}
            </span>
            <span className="member-detail">
              {m.link === "me" ? "you" : LINKS[m.link].label}
              {hosting && (isLive(hosting) ? " · streaming" : " · has a session")}
              {!hosting && plays && " · playing"}
              {!hosting && !plays && watches && " · watching"}
            </span>
          </span>
          <span className="member-status">
            {m.otherVersion && (
              <Tooltip label={otherVersionText(m.name, m.otherVersion)}>
                <span>
                  <Badge tone="warn">{m.otherVersion.newer ? "Update yours" : "Needs update"}</Badge>
                </span>
              </Tooltip>
            )}
            {hosting &&
              (isLive(hosting) ? (
                <Tooltip label={`${m.name} is streaming`}>
                  <Radio className="member-icon live" aria-label="streaming" />
                </Tooltip>
              ) : (
                <Tooltip label={`${m.name} has a session; nothing is streamed now`}>
                  <Tv className="member-icon" aria-label="has a session" />
                </Tooltip>
              ))}
            {!hosting && plays && (
              <Tooltip label="Playing">
                <Gamepad2 className="member-icon play" aria-label="playing" />
              </Tooltip>
            )}
            {!hosting && !plays && watches && (
              <Tooltip label="Watching">
                <Eye className="member-icon" aria-label="watching" />
              </Tooltip>
            )}
          </span>
          {items.length > 0 && (
            <DropdownMenu items={items} onOpenChange={holdRows}>
              <IconButton label={`${m.name}: actions`} size="sm" className="member-more">
                <MoreHorizontal />
              </IconButton>
            </DropdownMenu>
          )}
        </li>
      </ContextMenu>
    );
  };

  return (
    <aside className={props.onClose ? "members-panel drawer" : "members-panel"} aria-label="Members">
      <header className="members-header">
        <h2>Members</h2>
        <span className="members-count tabular">{room.members.length}</span>
        {props.onClose && (
          <IconButton label="Close" size="sm" onClick={props.onClose} className="members-close">
            <X />
          </IconButton>
        )}
      </header>
      <div className="members-scroll">
        <h3 className="members-group">
          Online — <span className="tabular">{online.length}</span>
        </h3>
        <ul>{online.map(row)}</ul>
        {offline.length > 0 && (
          <>
            <h3 className="members-group">
              Offline — <span className="tabular">{offline.length}</span>
            </h3>
            <ul>{offline.map(row)}</ul>
          </>
        )}
        {room.members.length === 1 && (
          <p className="members-alone">
            {room.isOwner ? "Nobody else yet. Invite people to get started." : "Waiting for the others."}
          </p>
        )}
      </div>
    </aside>
  );
}
