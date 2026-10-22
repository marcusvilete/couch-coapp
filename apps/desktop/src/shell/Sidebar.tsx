// The rooms, in the order the user dragged them into (Alt+Up and Alt+Down
// move the focused one; Ctrl+1…9 opens one), each with a line saying what's
// going on there; at the bottom, you. Below 900 px wide it narrows to a
// rail of room icons.
import { Pencil, Plus, Settings } from "lucide-react";
import { useState } from "react";
import type { EngineInfo } from "../bindings/EngineInfo";
import type { NetworkView } from "../bindings/NetworkView";
import type { ProfileView } from "../bindings/ProfileView";
import type { RoomView } from "../bindings/RoomView";
import { needsUs, roomLine } from "../model/rooms";
import { isLive } from "../model/sessions";
import { Avatar } from "../ui/Avatar";
import { IconButton } from "../ui/Button";
import { ContextMenu, type MenuEntry } from "../ui/Menu";
import { Tooltip } from "../ui/Tooltip";
import { useNarrow } from "./useNarrow";
import "./Sidebar.css";

export function Sidebar(props: {
  rooms: RoomView[];
  selected: string | null;
  /** Settings is open: no room is shown as selected. */
  inSettings: boolean;
  onSelect: (room: string) => void;
  onMove: (room: string, to: number) => void;
  onAdd: () => void;
  menuOf: (room: RoomView, index: number) => MenuEntry[];
  profile: ProfileView | null;
  info: EngineInfo | null;
  network: NetworkView | null;
  onEditName: () => void;
  onSettings: () => void;
}) {
  const [dragging, setDragging] = useState<string | null>(null);
  // Where a drop would land: before the room at this index.
  const [dropAt, setDropAt] = useState<number | null>(null);

  const finish = () => {
    setDragging(null);
    setDropAt(null);
  };
  const drop = () => {
    if (dragging != null && dropAt != null) {
      const from = props.rooms.findIndex((r) => r.id === dragging);
      // Taking the dragged room out first shifts later places down.
      props.onMove(dragging, from < dropAt ? dropAt - 1 : dropAt);
    }
    finish();
  };

  // A rail of icons in narrow windows: the names only in tooltips.
  const rail = useNarrow(900);
  const name = props.profile?.displayName ?? "…";
  const relay = props.network?.homeRelay ? hostOf(props.network.homeRelay) : null;

  return (
    <nav className="sidebar" aria-label="Rooms">
      <header className="sidebar-header">
        <span className="brand" aria-hidden="true">
          <span className="brand-mark">
            <CouchMark />
          </span>
          <span className="brand-name">Couch Co-App</span>
        </span>
        <IconButton label="Create or join a room" onClick={props.onAdd} className="sidebar-add">
          <Plus />
        </IconButton>
      </header>

      <div
        className="sidebar-rooms"
        onDragOver={(e) => {
          if (dragging == null) return;
          e.preventDefault();
          e.dataTransfer.dropEffect = "move";
          // Before the first room whose middle is below the pointer.
          const rows = Array.from(e.currentTarget.querySelectorAll(".room-row"));
          const at = rows.findIndex((row) => {
            const box = row.getBoundingClientRect();
            return e.clientY < box.top + box.height / 2;
          });
          setDropAt(at === -1 ? props.rooms.length : at);
        }}
        onDrop={(e) => {
          e.preventDefault();
          drop();
        }}
      >
        {props.rooms.map((room, i) => {
          const selected = !props.inSettings && room.id === props.selected;
          const live = room.sessions.some(isLive);
          const marker =
            dragging != null && dropAt === i
              ? " drop-before"
              : dragging != null && dropAt === i + 1 && i === props.rooms.length - 1
                ? " drop-after"
                : "";
          return (
            <ContextMenu key={room.id} items={props.menuOf(room, i)}>
              <Tooltip label={rail || room.name.length > 22 ? room.name : null} side="right">
                <button
                  className={`room-row${selected ? " selected" : ""}${room.id === dragging ? " dragging" : ""}${marker}`}
                  aria-current={selected ? "page" : undefined}
                  aria-label={`${room.name}: ${roomLine(room)}${needsUs(room) ? " (needs you)" : ""}`}
                  draggable
                  onClick={() => props.onSelect(room.id)}
                  onDragStart={(e) => {
                    setDragging(room.id);
                    e.dataTransfer.effectAllowed = "move";
                    e.dataTransfer.setData("text/plain", room.name);
                  }}
                  onDragEnd={finish}
                  onKeyDown={(e) => {
                    if (!e.altKey) return;
                    if (e.key === "ArrowUp" && i > 0) {
                      e.preventDefault();
                      props.onMove(room.id, i - 1);
                    } else if (e.key === "ArrowDown" && i < props.rooms.length - 1) {
                      e.preventDefault();
                      props.onMove(room.id, i + 1);
                    }
                  }}
                >
                  <span className="room-icon">
                    <Avatar id={room.id} name={room.name} square size={36} />
                    {live && <span className="room-live" aria-hidden="true" />}
                  </span>
                  <span className="room-text">
                    <span className="room-name">{room.name}</span>
                    <span className="room-line">{roomLine(room)}</span>
                  </span>
                  {needsUs(room) && <span className="room-attention" aria-hidden="true" />}
                </button>
              </Tooltip>
            </ContextMenu>
          );
        })}
      </div>

      <footer className="you">
        <Tooltip label={relay ? `Connected through ${relay}` : "Not connected to a relay yet"} side="top">
          <span className="you-avatar">
            <Avatar id={props.profile?.memberId ?? "me"} name={name} presence={relay ? "direct" : "online"} />
          </span>
        </Tooltip>
        <span className="you-text">
          <span className="you-name">{name}</span>
          <span className="you-line">
            {props.info?.profile && props.info.profile !== "default" ? `profile ${props.info.profile} · ` : ""}
            {relay ? `via ${relay}` : "connecting…"}
          </span>
        </span>
        <IconButton label="Change your name" size="sm" onClick={props.onEditName} className="you-edit">
          <Pencil />
        </IconButton>
        <IconButton label="Settings (Ctrl+,)" onClick={props.onSettings} className="you-settings">
          <Settings />
        </IconButton>
      </footer>
    </nav>
  );
}

/** "couchrelay.duckdns.org" from its URL. */
export function hostOf(url: string): string {
  try {
    return new URL(url).host.replace(/\.$/, "");
  } catch {
    return url;
  }
}

/** The app's mark: a couch, two seats. */
export function CouchMark() {
  return (
    <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
      <path
        d="M5 10V8a3 3 0 0 1 3-3h8a3 3 0 0 1 3 3v2M3 12a2 2 0 0 1 4 0v2h10v-2a2 2 0 0 1 4 0v5a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1zM6 18v1.5M18 18v1.5M12 9.5v4.5"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.9"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}
