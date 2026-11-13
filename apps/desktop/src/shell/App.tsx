// The app's window (M21): the rooms on the left, the room (or Settings) in
// the middle, its members on the right; the session bar while we host;
// dialogs, notices, the splash. The engine decides everything: this keeps
// what is shown, and sends commands.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { NetworkTrouble } from "../bindings/NetworkTrouble";
import { errorText, request } from "../engine";
import type { TroubleFix } from "../model/network";
import { hosted } from "../model/overlay";
import { moved, ordered, owner } from "../model/rooms";
import { busyIn } from "../model/sessions";
import { useBoardOverlay } from "../board/useBoardOverlay";
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
import { operatedSession, remoteKeyboardSeat } from "../model/remote";
import { UnlockRemoteDialog } from "../sessions/RemotePanel";
import { StartSessionDialog } from "../sessions/StartSessionDialog";
import { StreamPicker } from "../sessions/StreamPicker";
import { useRun } from "../sessions/useRun";
import type { PasteFor } from "../settings/NetworkSection";
import { PasteCodeDialog } from "../settings/PasteCodeDialog";
import { RelayWizard } from "../settings/RelayWizard";
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
import { TitleBar } from "./TitleBar";
import { TroubleStrip } from "./TroubleStrip";
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
  /** The strip asks Settings → Network for a relay's new code. */
  const [pasteFor, setPasteFor] = useState<PasteFor | null>(null);
  const [checking, setChecking] = useState(false);
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
  // A Linux host (M16c): the desktop shares the picture through its dialog,
  // and no window of ours goes over the game: desktop notifications instead.
  const linux = lobby.info?.platform === "linux";
  // While we stream: requests and session notes over the game.
  useOverlay(rooms, linux ? "notices" : "overlay");
  // And what's drawn on the stream, over the game (M27b, Windows).
  useBoardOverlay(rooms);

  useEffect(() => {
    if (room) writeStored(KEYS.selectedRoom, room.id);
  }, [room?.id]);
  useEffect(() => setDrawer(false), [room?.id, drawerMode]);

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

  // Creating a room or an invite while only the public relays are in use
  // asks for a relay of the user's own first (M26a, the wizard), then goes
  // on.
  const askRelay = lobby.network?.askForRelay ?? false;
  // No relay of the user's own: Create a room and Invite say so (M26a-2).
  const publicOnly = lobby.network != null && lobby.network.relays.length === 0 && lobby.network.publicInUse;
  const open = useCallback(
    (next: AppDialog) =>
      setDialog(askRelay && (next.kind === "create" || next.kind === "invite") ? { kind: "relayWizard", then: next } : next),
    [askRelay],
  );

  // The strip's button: check the network again, or Settings → Network.
  const fixTrouble = (fix: TroubleFix, trouble: NetworkTrouble) => {
    if (fix === "check") {
      setChecking(true);
      request({ type: "checkNetwork" }, "network")
        .then(lobby.setNetwork)
        .catch((e) => lobby.notify(errorText(e), "warn"))
        .finally(() => window.setTimeout(() => setChecking(false), 1500));
      return;
    }
    setSection("network");
    setView("settings");
    if (fix === "pasteCode" && trouble.type === "relayRefused") {
      setPasteFor((was) => ({ host: trouble.host, seq: (was?.seq ?? 0) + 1 }));
    }
  };

  const ctx: Context = {
    open,
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
  // One of our PCs' session we run from here (M25), for its stream picker.
  const remoteSession =
    dialog?.kind === "streamPicker" && dialog.remote && dialogRoom
      ? operatedSession(dialogRoom, dialog.remote)
      : undefined;
  const remotePc =
    dialog?.kind === "unlockRemote" && dialogRoom ? dialogRoom.members.find((m) => m.id === dialog.member) : undefined;
  const orphan =
    dialog != null &&
    (("room" in dialog && !dialogRoom) ||
      (dialog.kind === "streamPicker" && (dialog.remote ? remoteSession == null : host == null)) ||
      (dialog.kind === "unlockRemote" && remotePc?.remote == null) ||
      (dialog.kind === "startSession" &&
        dialog.remote != null &&
        !dialogRoom?.members.some((m) => m.id === dialog.remote && m.remote?.unlocked)) ||
      (dialog.kind === "name" && lobby.profile == null));
  useEffect(() => {
    if (orphan) setDialog(null);
  }, [orphan]);

  // The window's own title bar above it all (P15, Windows): this screen
  // asks to close the window.
  if (lobby.failure)
    return (
      <div className="window">
        <TitleBar />
        <Fatal error={lobby.failure} />
      </div>
    );

  const actions = room ? roomActions(room, ctx) : null;
  const myHere = room?.sessions.find((s) => s.role === "host");
  const index = room ? rooms.indexOf(room) : -1;

  return (
    <TooltipProvider>
      <div className="window">
        <TitleBar />
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
            network={lobby.network}
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
                network={lobby.network}
                onNetwork={lobby.setNetwork}
                pasteFor={pasteFor}
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
                        canHost={lobby.info?.capabilities.sessions ?? true}
                        onStartSession={() => setDialog({ kind: "startSession", room: room.id })}
                        onStream={() => setDialog({ kind: "streamPicker", room: room.id })}
                        onInvite={actions.invite}
                        onRoom={lobby.putRoom}
                        notify={lobby.notify}
                        confirm={(c) => setDialog({ kind: "confirm", ...c })}
                        onUnlockRemote={(pc) => setDialog({ kind: "unlockRemote", room: room.id, member: pc })}
                        onStartRemote={(pc) => setDialog({ kind: "startSession", room: room.id, remote: pc })}
                        onStreamRemote={(pc) => setDialog({ kind: "streamPicker", room: room.id, remote: pc })}
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
                onCreate={() => open({ kind: "create" })}
                onJoin={() => setDialog({ kind: "join", invite: "" })}
                onEditName={() => setDialog({ kind: "name" })}
              />
            ) : null}
            <TroubleStrip network={lobby.network} working={checking} onFix={fixTrouble} />
          </div>
        </div>
      </div>

      <Toasts notices={lobby.notices} onDismiss={lobby.dismiss} onHold={lobby.hold} />

      {dialog?.kind === "add" && (
        <AddRoomDialog
          onClose={close}
          onCreate={() => open({ kind: "create" })}
          onJoin={() => setDialog({ kind: "join", invite: "" })}
        />
      )}
      {dialog?.kind === "relayWizard" && (
        <RelayWizard
          testBuild={lobby.network?.testBuild ?? false}
          repository={lobby.info?.repository ?? null}
          onNetwork={lobby.setNetwork}
          onDone={() => setDialog(dialog.then)}
          onClose={close}
        />
      )}
      {dialog?.kind === "pasteCode" && (
        <PasteCodeDialog
          host={dialog.host}
          why={dialog.why}
          testBuild={lobby.network?.testBuild ?? false}
          onAdded={(network, host) => {
            lobby.setNetwork(network);
            lobby.notify(
              host === dialog.host
                ? `${host} added: your rooms reach it now.`
                : `${host} added, but that code isn't ${dialog.host}'s, which still needs its own.`,
              host === dialog.host ? "info" : "warn",
            );
            setDialog(null);
          }}
          onClose={close}
        />
      )}
      {dialog?.kind === "create" && (
        <CreateRoomDialog
          publicRelays={publicOnly}
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
          testBuild={lobby.network?.testBuild ?? false}
          onNetwork={lobby.setNetwork}
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
      {dialog?.kind === "invite" && dialogRoom && <InviteDialog room={dialogRoom} publicRelays={publicOnly} onClose={close} />}
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
          remote={dialog.remote ? { room: dialogRoom.id, member: dialog.remote } : undefined}
          keyboardSeat={
            dialog.remote
              ? remoteKeyboardSeat(dialogRoom.members.find((m) => m.id === dialog.remote)?.remote?.capabilities)
              : (lobby.info?.capabilities.keyboardSeat ?? true)
          }
          guestsKeyboard={
            dialog.remote
              ? (dialogRoom.members.find((m) => m.id === dialog.remote)?.remote?.capabilities?.keyboardSeat ?? true)
              : (lobby.info?.capabilities.keyboardSeat ?? true)
          }
          onClose={close}
          onStarted={(started) => {
            lobby.putRoom(started);
            setDialog(null);
          }}
        />
      )}
      {dialog?.kind === "streamPicker" && dialog.remote && dialogRoom && remoteSession && (
        <StreamPicker
          session={remoteSession}
          remote={{ room: dialogRoom.id, member: dialog.remote }}
          onClose={close}
          onRoom={lobby.putRoom}
          notify={lobby.notify}
        />
      )}
      {dialog?.kind === "streamPicker" && !dialog.remote && host && (
        <StreamPicker
          session={host.session}
          shared={linux}
          onClose={close}
          onRoom={lobby.putRoom}
          notify={lobby.notify}
        />
      )}
      {dialog?.kind === "unlockRemote" && dialogRoom && remotePc && (
        <UnlockRemoteDialog
          room={dialogRoom}
          pc={remotePc}
          onClose={close}
          onUnlocked={(unlocked) => {
            lobby.putRoom(unlocked);
            setDialog(null);
          }}
        />
      )}
      {dialog?.kind === "confirm" && <ConfirmDialog {...dialog} onClose={close} />}
      {splash && <Splash ready={lobby.loaded || lobby.failure != null} onDone={endSplash} />}
      {restarting && <RestartCover />}
    </TooltipProvider>
  );
}
