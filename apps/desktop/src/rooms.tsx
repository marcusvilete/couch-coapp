// The lobby's columns: the room rail, the room's column (members and you),
// and the welcome screen. The stage is in sessions.tsx.
import { useEffect, useRef, useState } from "react";
import type { EngineInfo } from "./bindings/EngineInfo";
import type { MemberLink } from "./bindings/MemberLink";
import type { MemberView } from "./bindings/MemberView";
import type { OtherVersion } from "./bindings/OtherVersion";
import type { ProfileView } from "./bindings/ProfileView";
import type { RoomView } from "./bindings/RoomView";
import { SessionBadge, isLive, playersOf } from "./sessions";
import { Avatar, PlusIcon, duration } from "./ui";

/** Rooms as round icons, in the order the user dragged them into (Alt+Up
 * and Alt+Down move the focused one). The add button stays below the list,
 * which scrolls on its own. */
export function RoomRail(props: {
  rooms: RoomView[];
  selected: string | null;
  onSelect: (room: string) => void;
  onAdd: () => void;
  onMove: (room: string, to: number) => void;
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
      // Removing the dragged room first shifts later positions down.
      props.onMove(dragging, from < dropAt ? dropAt - 1 : dropAt);
    }
    finish();
  };

  return (
    <nav className="rail" aria-label="Rooms">
      <div
        className="rail-rooms"
        onDragOver={(e) => {
          if (dragging == null) return;
          e.preventDefault();
          e.dataTransfer.dropEffect = "move";
          // Before the first room whose middle is below the pointer, wherever
          // the pointer is: over a room, a gap or the padding.
          const icons = Array.from(e.currentTarget.querySelectorAll(".rail-room"));
          const at = icons.findIndex((icon) => {
            const box = icon.getBoundingClientRect();
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
          const marker =
            dragging != null && dropAt === i ? " drop-before" : dragging != null && dropAt === i + 1 && i === props.rooms.length - 1 ? " drop-after" : "";
          return (
            <button
              key={room.id}
              className={`rail-room${room.id === props.selected ? " selected" : ""}${room.id === dragging ? " dragging" : ""}${marker}`}
              title={room.name}
              aria-label={room.name}
              aria-current={room.id === props.selected ? "page" : undefined}
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
              <Avatar name={room.name} size="large" />
            </button>
          );
        })}
      </div>
      <div className="rail-footer">
        <button className="rail-add" title="Create or join a room" aria-label="Create or join a room" onClick={props.onAdd}>
          <PlusIcon />
        </button>
      </div>
    </nav>
  );
}


const LINKS: Record<MemberLink, { label: string; title: string }> = {
  me: { label: "you", title: "This is you" },
  direct: { label: "direct", title: "Connected directly" },
  relay: { label: "relayed", title: "Connected through a relay server: works, with more delay" },
  online: { label: "online", title: "Online, but not connected to you yet" },
  offline: { label: "offline", title: "Not running Couch Co-App right now" },
};

export type RoomActions = {
  invite: () => void;
  revoke: () => void;
  changePassword: () => void;
  leave: () => void;
  ban: (member: MemberView) => void;
  /** Owner only, restored from a backup (M13). */
  stopCatchingUp: () => void;
};

export function RoomColumn(props: {
  room: RoomView | null;
  pausedUntil: number | undefined;
  profile: ProfileView | null;
  info: EngineInfo | null;
  actions: RoomActions;
  onEditName: () => void;
  onSettings: () => void;
}) {
  const { room } = props;
  return (
    <aside className="column">
      <header className="column-header">
        <h1 title={room?.name}>{room?.name ?? "Couch Co-App"}</h1>
        {room && <RoomMenu room={room} actions={props.actions} />}
      </header>
      {room && (
        <div className="column-body">
          {room.isOwner &&
            (room.needsPassword ? (
              // Restored from a backup (M13): it came without a password.
              <>
                <p className="banner warn" role="status">
                  This room came back from a backup without its password. Set a new one to invite people again; members
                  already in aren't affected.
                </p>
                <button className="primary wide" onClick={props.actions.changePassword}>
                  Set a new password
                </button>
              </>
            ) : (
              <button className="primary wide" onClick={props.actions.invite}>
                Invite people
              </button>
            ))}
          {room.catchingUp && <CatchingUp room={room} onStop={props.actions.stopCatchingUp} />}
          {props.pausedUntil != null && <PausedBanner until={props.pausedUntil} />}
          <MemberList room={room} onBan={props.actions.ban} />
        </div>
      )}
      {!room && <div className="column-body" />}
      <UserPanel profile={props.profile} info={props.info} onEdit={props.onEditName} onSettings={props.onSettings} />
    </aside>
  );
}

/** A room restored from a backup (M13), until it has caught up: the owner
 * waits for every member (one away may hold the newest state), a member for
 * any one. */
function CatchingUp(props: { room: RoomView; onStop: () => void }) {
  const { room } = props;
  const names = room.waitingFor.map((id) => room.members.find((m) => m.id === id)?.name ?? "a member");
  return (
    <>
      <p className="banner" role="status">
        {room.isOwner
          ? `Restored from a backup: catching up with the room. Until ${listed(names)} ${names.length === 1 ? "has" : "have"} been online, nobody can be invited, let in or banned.`
          : "Restored from a backup: catching up with the room until another member is online."}
      </p>
      {room.isOwner && (
        <button className="wide" onClick={props.onStop}>
          Stop waiting…
        </button>
      )}
    </>
  );
}

/** "Ana", "Ana and Bia", "Ana, Bia and Caio". */
function listed(names: string[]): string {
  if (names.length <= 1) return names[0] ?? "the others";
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

function RoomMenu(props: { room: RoomView; actions: RoomActions }) {
  const [open, setOpen] = useState(false);
  const menu = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!menu.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);
  const pick = (action: () => void) => () => {
    setOpen(false);
    action();
  };
  return (
    <div className="menu" ref={menu}>
      <button className="icon" aria-label="Room options" aria-expanded={open} onClick={() => setOpen(!open)}>
        ⋯
      </button>
      {open && (
        <div className="menu-items" role="menu">
          {props.room.isOwner && (
            <>
              <button role="menuitem" onClick={pick(props.actions.invite)}>
                Invite people
              </button>
              <button role="menuitem" onClick={pick(props.actions.changePassword)}>
                Change password
              </button>
              <button role="menuitem" onClick={pick(props.actions.revoke)}>
                Revoke all invites
              </button>
            </>
          )}
          <button role="menuitem" className="danger" onClick={pick(props.actions.leave)}>
            {props.room.isOwner ? "Close room for everyone" : "Leave room"}
          </button>
        </div>
      )}
    </div>
  );
}

function PausedBanner(props: { until: number }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);
  const left = Math.ceil((props.until - now) / 1000);
  if (left <= 0) return null;
  return (
    <p className="banner warn" role="status">
      Too many wrong passwords: joining is paused for {duration(left)}.
    </p>
  );
}

/** Why two members can't talk: their Couch Co-App versions differ (M10a). */
function otherVersionText(name: string, other: OtherVersion): string {
  return other.newer
    ? `${name} has Couch Co-App ${other.appVersion}, newer than yours: install the new version to play together.`
    : `${name} has Couch Co-App ${other.appVersion}, older than yours: they need to install the new version to play together.`;
}

function MemberList(props: { room: RoomView; onBan: (member: MemberView) => void }) {
  const { room } = props;
  return (
    <section className="members" aria-label="Members">
      <h2>Members — {room.members.length}</h2>
      <ul>
        {room.members.map((m) => {
          const link = LINKS[m.link];
          const session = room.sessions.find((s) => s.host === m.id);
          const plays = room.sessions.some((s) => playersOf(s).includes(m.id));
          return (
            <li key={m.id} className={`member link-${m.link}`}>
              <span className="presence" title={link.title}>
                <Avatar name={m.name} />
                <span className="dot" aria-hidden="true" />
              </span>
              <span className="member-name" title={m.name}>
                {m.name}
                {m.isOwner && (
                  <span className="owner" title="Owner: the one who lets people in" aria-label="owner">
                    {" "}
                    ♛
                  </span>
                )}
                {plays && (
                  <span className="playing" title="Playing" aria-label="playing">
                    {" "}
                    🎮
                  </span>
                )}
              </span>
              {session && (
                <SessionBadge
                  session={session}
                  title={isLive(session) ? `${m.name} is streaming` : `${m.name} has a session; nothing is streamed now`}
                />
              )}
              {m.otherVersion ? (
                <span className="link-label version" title={otherVersionText(m.name, m.otherVersion)}>
                  {m.otherVersion.newer ? "Update yours" : "Needs update"}
                </span>
              ) : (
                <span className="link-label" title={link.title}>
                  {link.label}
                </span>
              )}
              {room.isOwner && m.link !== "me" && (
                <button className="ban" title={`Ban ${m.name}`} aria-label={`Ban ${m.name}`} onClick={() => props.onBan(m)}>
                  Ban
                </button>
              )}
            </li>
          );
        })}
      </ul>
      {room.members.length === 1 && (
        <p className="hint">
          {room.isOwner ? "Nobody else yet. Invite people to get started." : "Waiting for the others."}
        </p>
      )}
    </section>
  );
}

function UserPanel(props: {
  profile: ProfileView | null;
  info: EngineInfo | null;
  onEdit: () => void;
  onSettings: () => void;
}) {
  const name = props.profile?.displayName ?? "…";
  const profile = props.info?.profile;
  return (
    <footer className="user-panel">
      <Avatar name={name} />
      <div className="user-text">
        <span className="user-name" title={name}>
          {name}
        </span>
        {profile && profile !== "default" && <span className="user-profile">profile {profile}</span>}
      </div>
      <button className="icon" title="Settings: shortcuts and network" aria-label="Settings" onClick={props.onSettings}>
        ⚙
      </button>
      <button className="icon" title="Change your name" aria-label="Change your name" onClick={props.onEdit}>
        ✎
      </button>
    </footer>
  );
}

export function Welcome(props: { onCreate: () => void; onJoin: () => void }) {
  return (
    <main className="stage">
      <div className="welcome">
        <h2>Play together, from your own couches</h2>
        <p className="hint">
          A room is a group of friends. One of you streams a game, and hands the controller around.
        </p>
        <div className="welcome-actions">
          <button className="big primary" onClick={props.onCreate}>
            <strong>Create a room</strong>
            <span>You'll invite friends with a link and a password.</span>
          </button>
          <button className="big" onClick={props.onJoin}>
            <strong>Join with an invite</strong>
            <span>Paste the link a friend sent you.</span>
          </button>
        </div>
      </div>
    </main>
  );
}
