// The app's window (M21): the rooms on the left, the room (or Settings) in
// the middle, its members on the right; the session bar while we host;
// dialogs, notices, the splash. The engine decides everything: this keeps
// what is shown, and sends commands.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { NetworkView } from "../bindings/NetworkView";
import { request } from "../engine";
import { hosted } from "../model/overlay";
import { moved, ordered, owner } from "../model/rooms";
import { busyIn } from "../model/sessions";
import { useOverlay } from "../overlay/useOverlay";
import { platform } from "../platform";
import { roomActions, type AppDialog, type Context } from "../rooms/actions";
import { RoomCallouts } from "../rooms/Callouts";
import {
  AcceptRoomDialog,
  AddRoomDialog,
  ConfirmDialog,
  CreateRoomDialog,
  InviteDialog,
  JoinRoomDialog,
  NameDialog,
  PasswordDialog,
} from "../rooms/dialogs";
import { MembersPanel } from "../rooms/MembersPanel";
import { roomMenu } from "../rooms/menus";
import { viewerEntries } from "../sessions/hostMenus";
import { SessionBar } from "../sessions/SessionBar";
import { Stage } from "../sessions/Stage";
import { StartSessionDialog } from "../sessions/StartSessionDialog";
import { StreamPicker } from "../sessions/StreamPicker";
import { useRun } from "../sessions/useRun";
import { SettingsPage, type SettingsSection } from "../settings/SettingsPage";
import { useLobby } from "../state/lobby";
import { LINK_WAITS_TEXT, linkOpened, opensJoin, type Surface } from "../state/openedLink";
import { KEYS, storedOrder, writeStored, readStored } from "../state/storage";
import { useTheme } from "../state/theme";
import { Callout } from "../ui/Callout";
import { Toasts } from "../ui/Toasts";
import { TooltipProvider } from "../ui/Tooltip";
import { RoomHeader } from "./RoomHeader";
import { Fatal, RestartCover, Welcome } from "./Screens";
import { Sidebar } from "./Sidebar";
import { Splash } from "./Splash";
import { useNarrow } from "./useNarrow";
import "./App.css";

export default function App() {
  const lobby = useLobby();
  const [theme, setTheme] = useTheme();
  const [selected, setSelected] = useState<string | null>(() => readStored(KEYS.selectedRoom));
  const [order, setOrder] = useState<string[]>(storedOrder);
  const [view, setView] = useState<"room" | "settings">("room");
  const [section, setSection] = useState<SettingsSection>("profile");
  const [dialog, setDialog] = useState<AppDialog | null>(null);
  const [capturing, setCapturing] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [restoredDev, setRestoredDev] = useState(false);
  const [drawer, setDrawer] = useState(false);
  const [network, setNetwork] = useState<NetworkView | null>(null);
  const [splash, setSplash] = useState(() => platform().splash);
  const endSplash = useCallback(() => setSplash(false), []);
  const close = useCallback(() => setDialog(null), []);
  const scroller = useRef<HTMLDivElement>(null);

  const rooms = ordered(lobby.rooms, order);
  const room = rooms.find((r) => r.id === selected) ?? rooms[0] ?? null;
  const busy = busyIn(rooms);
  const host = hosted(rooms);
  const drawerMode = useNarrow(1180);
  const members = useRun(lobby.notify, lobby.putRoom);
  // While we stream: requests and session notes over the game.
  useOverlay(rooms);

  useEffect(() => {
    if (room) writeStored(KEYS.selectedRoom, room.id);
  }, [room?.id]);
  useEffect(() => setDrawer(false), [room?.id, drawerMode]);

  // The relay we're reachable through, for your card: asked until there is
  // one, then again whenever the window comes back.
  useEffect(() => {
    let live = true;
    let timer = 0;
    const ask = () =>
      request({ type: "getNetwork" }, "network")
        .then((v) => {
          if (!live) return;
          setNetwork(v);
          if (v.homeRelay == null) timer = window.setTimeout(ask, 2000);
        })
        .catch(() => {});
    void ask();
    const onFocus = () => void ask();
    window.addEventListener("focus", onFocus);
    return () => {
      live = false;
      window.clearTimeout(timer);
      window.removeEventListener("focus", onFocus);
    };
  }, []);

  const move = useCallback(
    (id: string, to: number) => {
      const next = moved(
        rooms.map((r) => r.id),
        id,
        to,
      );
      setOrder(next);
      writeStored(KEYS.roomOrder, JSON.stringify(next));
    },
    [rooms],
  );

  const ctx: Context = {
    open: setDialog,
    notify: lobby.notify,
    putRoom: lobby.putRoom,
    move: (id, delta) => move(id, rooms.findIndex((r) => r.id === id) + delta),
  };

  const show = (id: string) => {
    setSelected(id);
    setView("room");
  };
  const toConsole = () => {
    if (!host) return;
    show(host.room.id);
    scroller.current?.scrollTo({ top: 0 });
  };

  // ---- Invite links the app was opened with ----
  // Nothing is sent until the password is typed and Join clicked. Any web
  // page can open one, so it never replaces what the user is in the middle
  // of: it waits while anything else is up, and an open join dialog only
  // offers it (`state/openedLink.ts`).
  const surface: Surface =
    restarting || capturing
      ? "other"
      : dialog == null
        ? view === "settings"
          ? "other"
          : "none"
        : dialog.kind === "add"
          ? "add"
          : dialog.kind === "join"
            ? "join"
            : "other";
  const [pendingLink, setPendingLink] = useState<string | null>(null);
  const { opened, clearOpened, notify } = lobby;
  useEffect(() => {
    if (opened == null) return;
    setPendingLink(opened.link);
    clearOpened();
    if (linkOpened(surface).notify) notify(LINK_WAITS_TEXT);
    // Only a newly opened link matters here, not every change of surface.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [opened, clearOpened, notify]);
  useEffect(() => {
    if (pendingLink != null && opensJoin(surface)) {
      setDialog({ kind: "join", invite: pendingLink });
      setPendingLink(null);
    }
  }, [pendingLink, surface]);

  // ---- The app's keys: Ctrl+1…9 opens a room, Ctrl+, Settings, Esc back ----
  // Never in a text field, a dialog, a menu, a shortcut capture or a
  // restart; never a key that answers a request or passes a seat.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || dialog != null || capturing || restarting) return;
      const target = e.target as HTMLElement | null;
      if (target?.closest("input, textarea, select, [contenteditable]")) return;
      if (document.querySelector("[role=menu], [role=listbox]")) return;
      if (e.ctrlKey && !e.altKey && !e.shiftKey && /^Digit[1-9]$/.test(e.code)) {
        const target = rooms[Number(e.code.slice(5)) - 1];
        if (target) {
          e.preventDefault();
          show(target.id);
        }
      } else if (e.ctrlKey && e.code === "Comma") {
        e.preventDefault();
        setView("settings");
      } else if (e.key === "Escape") {
        if (drawer) setDrawer(false);
        else if (view === "settings") setView("room");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const dialogRoom = useMemo(
    () => (dialog && "room" in dialog ? rooms.find((r) => r.id === dialog.room) : undefined),
    [dialog, rooms],
  );
  // A dialog with nothing left to show closes: about a room that went, the
  // stream picker once our session ended, our name before the profile. An
  // invisible one would still hold the keys and any opened link.
  const orphan =
    dialog != null &&
    (("room" in dialog && !dialogRoom) ||
      (dialog.kind === "streamPicker" && host == null) ||
      (dialog.kind === "name" && lobby.profile == null));
  useEffect(() => {
    if (orphan) setDialog(null);
  }, [orphan]);

  if (lobby.failure) return <Fatal error={lobby.failure} />;

  const actions = room ? roomActions(room, ctx) : null;
  const myHere = room?.sessions.find((s) => s.role === "host");
  const index = room ? rooms.indexOf(room) : -1;

  return (
    <TooltipProvider>
      {/* Restarting: nothing behind the cover takes a click or a key. */}
      <div className="shell" inert={restarting}>
        <Sidebar
          rooms={rooms}
          selected={room?.id ?? null}
          inSettings={view === "settings"}
          onSelect={show}
          onMove={move}
          onAdd={() => setDialog({ kind: "add" })}
          menuOf={(r, i) => roomMenu(r, roomActions(r, ctx), { index: i, count: rooms.length })}
          profile={lobby.profile}
          info={lobby.info}
          network={network}
          onEditName={() => setDialog({ kind: "name" })}
          onSettings={() => setView("settings")}
        />
        <div className="main">
          {host && (
            <SessionBar
              room={host.room}
              session={host.session}
              atConsole={view === "room" && room?.id === host.room.id}
              onConsole={toConsole}
              onRoom={lobby.putRoom}
              notify={lobby.notify}
            />
          )}
          {restoredDev && (
            // A development build can't start itself again; until it does,
            // the engine refuses creating, joining and renaming.
            <div className="app-banner">
              <Callout tone="warn">
                A backup was restored into this profile: close the app and start it again to use it. Until then,
                creating, joining and renaming are refused.
              </Callout>
            </div>
          )}
          {view === "settings" ? (
            <SettingsPage
              section={section}
              onSection={setSection}
              onClose={() => setView("room")}
              busy={busy}
              hasRooms={rooms.length > 0}
              profile={lobby.profile}
              info={lobby.info}
              onProfile={lobby.setProfile}
              theme={theme}
              onTheme={setTheme}
              capturing={capturing}
              onCapturing={setCapturing}
              onRestarting={setRestarting}
              restoredDev={restoredDev}
              onRestoredDev={() => setRestoredDev(true)}
            />
          ) : room && actions ? (
            <>
              <RoomHeader
                room={room}
                actions={actions}
                menu={roomMenu(room, actions, { index, count: rooms.length })}
                onMembers={drawerMode ? () => setDrawer(!drawer) : undefined}
              />
              <div className="room-body">
                <div className="room-scroll" ref={scroller}>
                  <div className="room-content">
                    <RoomCallouts room={room} pausedUntil={lobby.pausedUntil[room.id]} actions={actions} />
                    <Stage
                      room={room}
                      busy={busy}
                      onStartSession={() => setDialog({ kind: "startSession", room: room.id })}
                      onStream={() => setDialog({ kind: "streamPicker", room: room.id })}
                      onInvite={actions.invite}
                      onRoom={lobby.putRoom}
                      notify={lobby.notify}
                      confirm={(c) => setDialog({ kind: "confirm", ...c })}
                    />
                  </div>
                </div>
                {(!drawerMode || drawer) && (
                  <MembersPanel
                    room={room}
                    actions={actions}
                    hostEntries={myHere ? (m) => viewerEntries(room, myHere, m.id, members.run) : undefined}
                    onClose={drawerMode ? () => setDrawer(false) : undefined}
                  />
                )}
              </div>
            </>
          ) : lobby.loaded ? (
            <Welcome
              name={lobby.profile?.displayName ?? null}
              onCreate={() => setDialog({ kind: "create" })}
              onJoin={() => setDialog({ kind: "join", invite: "" })}
              onEditName={() => setDialog({ kind: "name" })}
            />
          ) : null}
        </div>
      </div>

      <Toasts notices={lobby.notices} onDismiss={lobby.dismiss} onHold={lobby.hold} />

      {dialog?.kind === "add" && (
        <AddRoomDialog
          onClose={close}
          onCreate={() => setDialog({ kind: "create" })}
          onJoin={() => setDialog({ kind: "join", invite: "" })}
        />
      )}
      {dialog?.kind === "create" && (
        <CreateRoomDialog
          onClose={close}
          onCreated={(created) => {
            lobby.putRoom(created);
            show(created.id);
            setDialog(null);
          }}
        />
      )}
      {dialog?.kind === "join" && (
        <JoinRoomDialog
          initialInvite={dialog.invite}
          offered={pendingLink}
          onTakeOffer={() => setPendingLink(null)}
          onClose={close}
          onJoined={(joined) => {
            lobby.putRoom(joined);
            show(joined.id);
            setDialog(null);
          }}
          onOpenRoom={(id) => {
            show(id);
            setDialog(null);
          }}
        />
      )}
      {dialog?.kind === "invite" && dialogRoom && <InviteDialog room={dialogRoom} onClose={close} />}
      {dialog?.kind === "password" && dialogRoom && (
        <PasswordDialog
          room={dialogRoom}
          onClose={close}
          onChanged={() => {
            lobby.notify(`The password of “${dialogRoom.name}” was changed.`);
            setDialog(null);
          }}
        />
      )}
      {dialog?.kind === "accept" && dialogRoom && (
        <AcceptRoomDialog
          room={dialogRoom}
          ownerName={owner(dialogRoom)?.name ?? "The owner"}
          onClose={close}
          onAccepted={(accepted) => {
            lobby.putRoom(accepted);
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
      {dialog?.kind === "startSession" && dialogRoom && (
        <StartSessionDialog
          room={dialogRoom}
          onClose={close}
          onStarted={(started) => {
            lobby.putRoom(started);
            setDialog(null);
          }}
        />
      )}
      {dialog?.kind === "streamPicker" && host && (
        <StreamPicker session={host.session} onClose={close} onRoom={lobby.putRoom} notify={lobby.notify} />
      )}
      {dialog?.kind === "confirm" && <ConfirmDialog {...dialog} onClose={close} />}
      {splash && <Splash ready={lobby.loaded || lobby.failure != null} onDone={endSplash} />}
      {restarting && <RestartCover />}
    </TooltipProvider>
  );
}
