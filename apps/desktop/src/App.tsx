import { useCallback, useEffect, useState } from "react";
import type { MemberView } from "./bindings/MemberView";
import type { RoomView } from "./bindings/RoomView";
import { ChangePasswordDialog, ConfirmDialog, CreateRoomDialog, InviteDialog, JoinRoomDialog, NameDialog } from "./dialogs";
import { request } from "./engine";
import { useLobby } from "./lobby";
import { RoomColumn, RoomRail, Welcome, type RoomActions } from "./rooms";
import { Stage, StartSessionDialog, busyIn } from "./sessions";
import { SettingsDialog } from "./settings";
import { Splash } from "./splash";
import { Modal } from "./ui";
import { useOverlay } from "./useOverlay";
import "./App.css";

type Dialog =
  | { kind: "add" }
  | { kind: "create" }
  | { kind: "join"; invite: string }
  | { kind: "invite"; room: RoomView }
  | { kind: "name" }
  | { kind: "startSession"; room: RoomView }
  | { kind: "settings" }
  | { kind: "password"; room: RoomView }
  | { kind: "confirm"; title: string; body: string; action: string; danger?: boolean; run: () => Promise<unknown> };

const SELECTED_KEY = "selectedRoom";
/** Room ids in the order the user dragged them into. */
const ORDER_KEY = "roomOrder";

// What the rail remembers lives in this profile's webview storage: a
// convenience, so a failure only loses the order.
function remembered(): string | null {
  try {
    return localStorage.getItem(SELECTED_KEY);
  } catch {
    return null;
  }
}

function rememberedOrder(): string[] {
  try {
    const order: unknown = JSON.parse(localStorage.getItem(ORDER_KEY) ?? "[]");
    return Array.isArray(order) ? order.filter((id): id is string => typeof id === "string") : [];
  } catch {
    return [];
  }
}

/** `rooms` in the user's order; rooms not placed yet go last, by name. */
function ordered(rooms: RoomView[], order: string[]): RoomView[] {
  const rank = new Map(order.map((id, i) => [id, i]));
  const placed = rooms.filter((r) => rank.has(r.id)).sort((a, b) => rank.get(a.id)! - rank.get(b.id)!);
  return [...placed, ...rooms.filter((r) => !rank.has(r.id))];
}

function App() {
  const lobby = useLobby();
  const [selected, setSelected] = useState<string | null>(remembered);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const close = useCallback(() => setDialog(null), []);
  const [splash, setSplash] = useState(true);
  const endSplash = useCallback(() => setSplash(false), []);

  const [order, setOrder] = useState<string[]>(rememberedOrder);
  const rooms = ordered(lobby.rooms, order);
  const room = rooms.find((r) => r.id === selected) ?? rooms[0] ?? null;
  // While we stream: requests and session notes over the game.
  useOverlay(rooms);

  const move = (id: string, to: number) => {
    const ids = rooms.map((r) => r.id).filter((r) => r !== id);
    ids.splice(to, 0, id);
    setOrder(ids);
    try {
      localStorage.setItem(ORDER_KEY, JSON.stringify(ids));
    } catch {
      // Only a convenience.
    }
  };

  useEffect(() => {
    try {
      if (room) localStorage.setItem(SELECTED_KEY, room.id);
    } catch {
      // Only a convenience.
    }
  }, [room]);

  // An invite link the app was opened with goes to the join screen; nothing
  // is sent until the password is typed and Join clicked. Any web page can
  // open one, so it never replaces what the user is in the middle of: it
  // waits for other dialogs to close, and an open join dialog only offers it.
  const [pendingLink, setPendingLink] = useState<string | null>(null);
  const { opened, clearOpened, notify } = lobby;
  useEffect(() => {
    if (opened == null) return;
    setPendingLink(opened.link);
    clearOpened();
    if (dialog != null && dialog.kind !== "add" && dialog.kind !== "join") {
      notify("An invite link was opened. It will show when you close this window.");
    }
    // Only a newly opened link matters here, not every dialog change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [opened, clearOpened, notify]);
  useEffect(() => {
    if (pendingLink != null && (dialog == null || dialog.kind === "add")) {
      setDialog({ kind: "join", invite: pendingLink });
      setPendingLink(null);
    }
  }, [pendingLink, dialog]);

  const entered = (joined: RoomView) => {
    lobby.putRoom(joined);
    setSelected(joined.id);
    setDialog(null);
  };

  const actions: RoomActions = {
    invite: () => room && setDialog({ kind: "invite", room }),
    changePassword: () => room && setDialog({ kind: "password", room }),
    revoke: () =>
      room &&
      setDialog({
        kind: "confirm",
        title: "Revoke all invites?",
        body: `Every invite link to “${room.name}” you've sent stops working. Members stay in the room.`,
        action: "Revoke invites",
        run: async () => {
          await request({ type: "revokeInvites", room: room.id }, "done");
          lobby.notify(`Invites to “${room.name}” revoked.`);
        },
      }),
    leave: () =>
      room &&
      setDialog(
        room.isOwner
          ? {
              kind: "confirm",
              title: `Close “${room.name}”?`,
              body:
                "The room is gone for good, for everyone. Members who are offline find out when they come back.",
              action: "Close room",
              danger: true,
              run: () => request({ type: "leaveRoom", room: room.id }, "done"),
            }
          : {
              kind: "confirm",
              title: `Leave “${room.name}”?`,
              body: "To come back you'll need a new invite from the owner.",
              action: "Leave room",
              danger: true,
              run: () => request({ type: "leaveRoom", room: room.id }, "done"),
            },
      ),
    ban: (member: MemberView) =>
      room &&
      setDialog({
        kind: "confirm",
        title: `Ban ${member.name}?`,
        body: `${member.name} is removed from “${room.name}” and can't come back, even with an invite. Every invite is revoked too, in case they hold one.`,
        action: "Ban",
        danger: true,
        run: async () => {
          // The engine answers with the room as it is now, without the banned member.
          lobby.putRoom(await request({ type: "banMember", room: room.id, member: member.id }, "room"));
        },
      }),
  };

  if (lobby.failure) {
    return (
      <div className="fatal">
        <h1>Couch Co-App couldn't start</h1>
        <p className="error">{lobby.failure}</p>
      </div>
    );
  }

  return (
    <div className="app">
      <RoomRail
        rooms={rooms}
        selected={room?.id ?? null}
        onSelect={setSelected}
        onMove={move}
        onAdd={() => setDialog({ kind: "add" })}
      />
      <RoomColumn
        room={room}
        pausedUntil={room ? lobby.pausedUntil[room.id] : undefined}
        profile={lobby.profile}
        info={lobby.info}
        actions={actions}
        onEditName={() => setDialog({ kind: "name" })}
        onSettings={() => setDialog({ kind: "settings" })}
      />
      {room ? (
        <Stage
          room={room}
          busy={busyIn(rooms)}
          onStartSession={() => setDialog({ kind: "startSession", room })}
          onInvite={actions.invite}
          onRoom={lobby.putRoom}
          notify={lobby.notify}
        />
      ) : lobby.loaded ? (
        <Welcome onCreate={() => setDialog({ kind: "create" })} onJoin={() => setDialog({ kind: "join", invite: "" })} />
      ) : (
        <main className="stage" />
      )}

      <div className="notices" aria-live="polite">
        {lobby.notices.map((n) => (
          <div key={n.id} className={`notice ${n.tone}`}>
            <span>{n.text}</span>
            <button className="icon" aria-label="Dismiss" onClick={() => lobby.dismiss(n.id)}>
              ×
            </button>
          </div>
        ))}
      </div>

      {dialog?.kind === "add" && (
        <Modal title="Add a room" onClose={close}>
          <div className="welcome-actions">
            <button className="big primary" onClick={() => setDialog({ kind: "create" })}>
              <strong>Create a room</strong>
              <span>You'll invite friends with a link and a password.</span>
            </button>
            <button className="big" onClick={() => setDialog({ kind: "join", invite: "" })}>
              <strong>Join with an invite</strong>
              <span>Paste the link a friend sent you.</span>
            </button>
          </div>
        </Modal>
      )}
      {dialog?.kind === "create" && <CreateRoomDialog onClose={close} onCreated={entered} />}
      {dialog?.kind === "join" && (
        <JoinRoomDialog
          initialInvite={dialog.invite}
          offered={pendingLink}
          onTakeOffer={() => setPendingLink(null)}
          onClose={close}
          onJoined={entered}
          onOpenRoom={(id) => {
            setSelected(id);
            setDialog(null);
          }}
        />
      )}
      {dialog?.kind === "invite" && <InviteDialog room={dialog.room} onClose={close} />}
      {dialog?.kind === "password" && (
        <ChangePasswordDialog
          room={dialog.room}
          onClose={close}
          onChanged={() => {
            lobby.notify(`The password of “${dialog.room.name}” was changed.`);
            setDialog(null);
          }}
        />
      )}
      {dialog?.kind === "name" && lobby.profile && (
        <NameDialog
          profile={lobby.profile}
          onClose={close}
          onSaved={(p) => {
            lobby.setProfile(p);
            setDialog(null);
          }}
        />
      )}
      {dialog?.kind === "startSession" && (
        <StartSessionDialog
          room={dialog.room}
          onClose={close}
          onStarted={(started) => {
            lobby.putRoom(started);
            setDialog(null);
          }}
        />
      )}
      {dialog?.kind === "settings" && <SettingsDialog section="shortcuts" busy={busyIn(rooms)} onClose={close} />}
      {splash && <Splash ready={lobby.loaded || lobby.failure != null} onDone={endSplash} />}
      {dialog?.kind === "confirm" && (
        <ConfirmDialog
          title={dialog.title}
          body={dialog.body}
          action={dialog.action}
          danger={dialog.danger}
          run={dialog.run}
          onClose={close}
        />
      )}
    </div>
  );
}

export default App;
