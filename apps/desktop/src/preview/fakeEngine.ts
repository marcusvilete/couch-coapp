// A fake engine for the preview (development only): it answers the UI's
// commands from a world kept in memory, as the real engine would, refusals
// and their texts included, and announces changes as events. Replies and
// events arrive after a little latency, in either order, so the UI's
// newest-revision rule is exercised as in the app. Its state lives at module
// level: React's double effects in development don't reset it.
import type { Command } from "../bindings/Command";
import type { DriverView } from "../bindings/DriverView";
import type { EngineInfo } from "../bindings/EngineInfo";
import type { Event as EngineEvent } from "../bindings/Event";
import type { HostController } from "../bindings/HostController";
import type { InputSetup } from "../bindings/InputSetup";
import type { InvitePreview } from "../bindings/InvitePreview";
import type { MemberView } from "../bindings/MemberView";
import type { NetworkView } from "../bindings/NetworkView";
import type { ProfileView } from "../bindings/ProfileView";
import type { RemoteAccessView } from "../bindings/RemoteAccessView";
import type { RemoteAction } from "../bindings/RemoteAction";
import type { RemotePlayView } from "../bindings/RemotePlayView";
import type { Response } from "../bindings/Response";
import type { RoomView } from "../bindings/RoomView";
import type { SeatView } from "../bindings/SeatView";
import type { SessionView } from "../bindings/SessionView";
import type { ShortcutView } from "../bindings/ShortcutView";
import type { SourceList } from "../bindings/SourceList";
import { seatsFor } from "../model/seats";
import { keeps, numbered, seatsOf } from "../model/sessions";
import { frees } from "../sessions/hostMenus";

export { seatsFor };

export type FakeRoom = Omit<RoomView, "revision">;

export type World = {
  info: EngineInfo;
  profile: ProfileView;
  rooms: FakeRoom[];
  shortcuts: ShortcutView[];
  network: NetworkView;
  drivers: DriverView[];
  sources: SourceList;
  notices: string;
  /** An invite link the app was "opened with", taken once. */
  openedInvite: string | null;
  /** What known invite links lead to; any other link looks like a fresh
   * invite to "Movie night". */
  invites: Record<string, InvitePreview>;
  /** How many controllers our stream window has, for seats we're passed. */
  myControllers: number;
  /** How the hosts we watch answer our requests. */
  hostAnswers: "accept" | "decline" | "wait";
  /** The backup dialogs: a path chosen, or `null` (cancelled). */
  backupPath: string | null;
  /** `getInfo` fails with this: the app's fatal screen. */
  startFailure: string | null;
  /** Sound guests hear, for the host's meter. */
  sound: "game" | "silent";
  /** Settings → Stream window (P11): new stream windows show their bar. */
  streamWindowBar: boolean;
  /** Remote play (M25): Settings → Remote play. Members offering it carry
   * their `remote` in the rooms. A PC unlocks with "246810". */
  remotePlay: RemotePlayView;
};

let world: World | null = null;
let revision = 0;
const revisions = new Map<string, number>();
const listeners = new Set<(event: EngineEvent) => void>();
let latency = true;
/** The next command fails with this text, once. */
let failNext: string | null = null;
let level: number | null = null;

function w(): World {
  if (world == null) throw new Error("the engine has stopped");
  return world;
}

/** Puts a scenario's world in place. */
export function loadWorld(next: World, options: { latency: boolean }) {
  world = structuredClone(next);
  latency = options.latency;
  for (const room of world.rooms) revisions.set(room.id, ++revision);
  if (level == null) level = window.setInterval(streamLevels, 100);
}

export function failNextCommand(text: string) {
  failNext = text;
}

// ---- Delivery ----

function later(ms: number, run: () => void) {
  window.setTimeout(run, latency ? ms : 0);
}

function jitter(min: number, max: number): number {
  return min + Math.random() * (max - min);
}

export function emit(event: EngineEvent) {
  later(jitter(10, 250), () => listeners.forEach((l) => l(event)));
}

export function onEvent(handler: (event: EngineEvent) => void): Promise<() => void> {
  listeners.add(handler);
  return Promise.resolve(() => listeners.delete(handler));
}

export function command(cmd: Command): Promise<Response> {
  return new Promise((resolve, reject) => {
    later(jitter(50, 300), () => {
      if (failNext != null) {
        const text = failNext;
        failNext = null;
        reject(text);
        return;
      }
      try {
        const reply = handle(cmd);
        if (reply instanceof Promise) reply.then(resolve, (e) => reject(String(e)));
        else resolve(reply);
      } catch (e) {
        reject(e instanceof Error ? e.message : String(e));
      }
    });
  });
}

// ---- Rooms ----

function me(): string {
  return w().profile.memberId;
}

function find(id: string): FakeRoom {
  const room = w().rooms.find((r) => r.id === id);
  if (!room) throw "no such room";
  return room;
}

function view(room: FakeRoom): RoomView {
  return structuredClone({ ...room, revision: revisions.get(room.id) ?? 0 });
}

/** A room changed: a newer revision, announced. */
function changed(room: FakeRoom): RoomView {
  revisions.set(room.id, ++revision);
  const v = view(room);
  emit({ type: "roomChanged", room: v });
  return v;
}

function roomReply(room: FakeRoom): Response {
  return { type: "room", ...changed(room) };
}

function owned(id: string): FakeRoom {
  const room = find(id);
  if (!room.isOwner) throw "only the room's owner can do that";
  return room;
}

function notCatchingUp(room: FakeRoom) {
  if (room.catchingUp) {
    throw "this room was restored from a backup and is catching up with its members: try again once they have been online, or stop waiting";
  }
}

function newId(): string {
  return Array.from({ length: 16 }, () => Math.floor(Math.random() * 16).toString(16)).join("");
}

function nameProblem(name: string): string | null {
  const bytes = new TextEncoder().encode(name.trim()).length;
  return bytes === 0 || bytes > 96 ? "names must be 1 to 96 bytes, with nothing invisible in them" : null;
}

function self(): MemberView {
  return { id: me(), name: w().profile.displayName, isOwner: true, link: "me", otherVersion: null, remote: null };
}

function linkFor(room: FakeRoom): string {
  return `https://marcusvilete.github.io/couch-coapp/j/#v1.${room.id}.${encodeURIComponent(room.name)}.Zm9vYmFyYmF6cXV4`;
}

function preview(invite: string): InvitePreview {
  const link = invite.trim();
  const known = w().invites[link];
  if (known) return known;
  if (!/^(couchco:\/\/|https?:\/\/)/.test(link)) throw "that isn't an invite";
  const own = w().rooms.find((r) => link.includes(r.id));
  if (own) return { room: own.id, name: own.name, status: own.isOwner ? "own" : "member", expiresInSecs: 0 };
  return { room: "f00dfacecafe0001", name: "Movie night", status: "ready", expiresInSecs: 6 * 86400 + 3600 };
}

// ---- Sessions ----

/** Where we play a part, in any room. */
function ours(): { room: FakeRoom; session: SessionView } | null {
  for (const room of w().rooms) {
    const session = room.sessions.find((s) => s.role !== "none");
    if (session) return { room, session };
  }
  return null;
}

function hosting(): { room: FakeRoom; session: SessionView } {
  for (const room of w().rooms) {
    const session = room.sessions.find((s) => s.role === "host");
    if (session) return { room, session };
  }
  throw "you aren't streaming";
}

function watching(): { room: FakeRoom; session: SessionView } {
  const found = ours();
  if (!found || found.session.role === "host") throw "you aren't watching a stream";
  return found;
}

export function hostSession(
  host: string,
  title: string,
  maxViewers: number,
  input: InputSetup,
  controllers: string[],
): SessionView {
  const seats = seatsFor(input);
  const homes: number[] = [];
  let next = 0;
  controllers.forEach((_, c) => {
    while (next < seats.length && !seats[next].pad) next++;
    if (next >= seats.length) return;
    Object.assign(seats[next], { holder: host, host: true, hostController: c });
    homes.push(next);
    next++;
  });
  return {
    host,
    title,
    maxViewers,
    viewers: [],
    viewerControllers: [],
    viewerPads: [],
    ourControllers: [],
    seats,
    hostOnSeats: homes.length > 0,
    controlPaused: false,
    role: "host",
    requests: [],
    askedSeat: null,
    // Watching together (M24): no seats, so no hook for the host's keys.
    panicKey: seats.length > 0 ? "Ctrl+Alt+Shift+Backspace" : null,
    answerKeys: seats.length > 0 ? { accept: "Ctrl+Alt+Shift+Y", decline: "Ctrl+Alt+Shift+N" } : null,
    overlayArea: null,
    hostControllers: numbered(controllers).slice(0, homes.length),
    hostHomes: homes,
    hostControllerProblem: null,
    stream: { state: "waiting" },
    streaming: null,
    soundLevel: 100,
    padsSettingUp: [],
    padsSettingUpSecs: 0,
    padsMissing: [],
    streamWindow: null,
    gameInFront: null,
    note: null,
    operator: false,
    operatedBy: [],
    desktop: null,
  };
}

function free(seat: SeatView) {
  Object.assign(seat, { holder: null, host: false, hostController: null, guestController: null });
}

/** The host passes `seat` to `member`, as `HostSession` does: added to the
 * seats they hold while they have a controller to spare, otherwise they move
 * (their other pad seats go first). With `controller`, that one of theirs
 * drives it, as their side decides (`couch_input::forward`, P12): a seat it
 * drove swaps; to a seat they hold, only that changes. */
function pass(s: SessionView, member: string, seat: number, controller?: number, answering = false) {
  const target = s.seats[seat];
  if (!target) throw "there is no such seat";
  const i = s.viewers.indexOf(member);
  if (i < 0) throw "that member isn't watching";
  const pads = (s.viewerPads[i] ?? []).map((p) => p.slot);
  const chosen = controller != null && pads.includes(controller) ? controller : undefined;
  const held = seatsOf(s, member);
  if (held.includes(seat)) {
    if (chosen == null || !target.pad) return;
    const other = held.find((j) => j !== seat && s.seats[j].guestController === chosen);
    if (other != null) s.seats[other].guestController = target.guestController;
    target.guestController = chosen;
    return;
  }
  if (s.controlPaused) throw "control is paused; allow it again first";
  // Beyond their controllers, only the moving one's seat goes (P13); an
  // answer guesses rather than refuse, as `HostSession::answer` does.
  const freed = frees(s, member, seat, chosen);
  if (freed.ambiguous && !answering) throw "say which of their controllers moves to it: each of them plays a seat already";
  for (const h of freed.seats) free(s.seats[h]);
  free(target);
  target.holder = member;
  if (target.pad) {
    const theirs = seatsOf(s, member).filter((j) => j !== seat && s.seats[j].pad);
    const used = theirs.map((j) => s.seats[j].guestController);
    const spare = pads.find((c) => !used.includes(c)) ?? null;
    target.guestController = chosen ?? spare;
    const other = theirs.find((j) => chosen != null && s.seats[j].guestController === chosen);
    if (other != null) s.seats[other].guestController = spare;
  }
  s.requests = s.requests.filter((r) => r.member !== member);
}

/** Our controller `c` plays `seat` (M23): onto another controller's own
 * seat, the two swap. */
function play(s: SessionView, seat: number, c: number) {
  if (!s.hostOnSeats) throw "your controller doesn't play through a seat in this session";
  const target = s.seats[seat];
  if (!target) throw "there is no such seat";
  if (!target.pad) throw "that seat has no controller";
  if (c >= s.hostControllers.length) throw "none of your controllers in this session is that one";
  const current = s.seats.findIndex((x) => x.host && x.hostController === c);
  const other = target.host ? target.hostController : null;
  const otherHome = s.hostHomes.findIndex((home, j) => j !== c && home === seat);
  free(target);
  Object.assign(target, { holder: s.host, host: true, hostController: c });
  if (current >= 0 && current !== seat) {
    free(s.seats[current]);
    if (other != null) Object.assign(s.seats[current], { holder: s.host, host: true, hostController: other });
  }
  if (otherHome >= 0) {
    [s.hostHomes[c], s.hostHomes[otherHome]] = [s.hostHomes[otherHome], s.hostHomes[c]];
  } else {
    s.hostHomes[c] = seat;
  }
}

function streamLevels() {
  if (world == null || listeners.size === 0) return;
  for (const room of world.rooms) {
    const s = room.sessions.find((x) => x.role === "host");
    if (!s || s.stream.state !== "live" || s.soundLevel == null) continue;
    if (world.sound === "silent" || s.soundLevel === 0) {
      listeners.forEach((l) => l({ type: "streamLevel", room: room.id, peak_db: -90, average_db: -90 }));
      continue;
    }
    const gain = 20 * Math.log10(s.soundLevel / 100);
    const average = Math.round(Math.min(-3, -24 + gain + jitter(-4, 4)));
    const peak = Math.round(Math.min(0, average + jitter(6, 14)));
    listeners.forEach((l) => l({ type: "streamLevel", room: room.id, peak_db: peak, average_db: average }));
  }
}

// ---- Remote play (M25) ----

/** The PIN every PC in the preview unlocks with. */
export const REMOTE_PIN = "246810";

type RemoteCommand = Extract<
  Command,
  {
    type:
      | "getRemotePlay"
      | "setRemotePlay"
      | "setStartAtSignIn"
      | "setRemotePin"
      | "addRemoteDevice"
      | "removeRemoteDevice"
      | "acceptRemoteHost"
      | "forgetRemoteHost"
      | "unlockRemote"
      | "lockRemote"
      | "remote";
  }
>;

/** `pc`, offering us remote play in `roomId`. */
function offering(roomId: string, pc: string): { room: FakeRoom; member: MemberView; remote: RemoteAccessView } {
  const room = find(roomId);
  const member = room.members.find((m) => m.id === pc);
  if (!member?.remote) throw "that PC isn't offering remote play to this device now";
  return { room, member, remote: member.remote };
}

function remoteCommand(cmd: RemoteCommand): Response {
  const world = w();
  const rp = world.remotePlay;
  const reply = (): Response => ({ type: "remotePlay", ...structuredClone(rp) });
  // Each room where `pc` offers remote play.
  const everywhere = (pc: string, change: (remote: RemoteAccessView) => void) => {
    for (const room of world.rooms) {
      const m = room.members.find((x) => x.id === pc);
      if (m?.remote) {
        change(m.remote);
        changed(room);
      }
    }
  };
  switch (cmd.type) {
    case "getRemotePlay":
      return reply();
    case "setRemotePlay":
      if (cmd.on && !rp.pinSet) throw "set a PIN first";
      rp.on = cmd.on;
      return reply();
    case "setStartAtSignIn":
      if (rp.startAtSignIn == null) throw "only an installed Couch Co-App can start with Windows";
      rp.startAtSignIn = cmd.on;
      return reply();
    case "setRemotePin":
      if ([...cmd.pin.trim()].length < 6) throw "the PIN must be at least 6 characters";
      rp.pinSet = true;
      return reply();
    case "addRemoteDevice": {
      if (!rp.pinSet) throw "set a PIN first";
      const device = rp.candidates.find((c) => c.id === cmd.member);
      if (!device) throw "no such member";
      rp.candidates = rp.candidates.filter((c) => c !== device);
      rp.devices.push(device);
      return reply();
    }
    case "removeRemoteDevice": {
      const device = rp.devices.find((d) => d.id === cmd.member);
      rp.devices = rp.devices.filter((d) => d !== device);
      if (device) rp.candidates.push(device);
      return reply();
    }
    case "acceptRemoteHost": {
      const room = world.rooms.find((r) => r.members.some((m) => m.id === cmd.member && m.remote));
      const pc = room?.members.find((m) => m.id === cmd.member);
      if (!pc?.remote) throw "that member hasn't offered remote play";
      if (!rp.hosts.some((h) => h.id === pc.id)) {
        rp.hosts.push({ id: pc.id, name: pc.name, code: pc.remote.code, online: true });
      }
      everywhere(pc.id, (r) => (r.accepted = true));
      return reply();
    }
    case "forgetRemoteHost":
      rp.hosts = rp.hosts.filter((h) => h.id !== cmd.member);
      everywhere(cmd.member, (r) => Object.assign(r, { accepted: false, unlocked: false }));
      return reply();
    case "unlockRemote": {
      const { room, remote } = offering(cmd.room, cmd.member);
      if (!remote.accepted) throw "accept it as your PC first";
      if (cmd.pin !== REMOTE_PIN) throw "wrong PIN";
      remote.unlocked = true;
      for (const s of room.sessions) if (s.host === cmd.member) s.operator = true;
      return roomReply(room);
    }
    case "lockRemote": {
      const { room, remote } = offering(cmd.room, cmd.member);
      remote.unlocked = false;
      for (const s of room.sessions) if (s.host === cmd.member) s.operator = false;
      changed(room);
      return reply();
    }
    case "remote": {
      const { room, member, remote } = offering(cmd.room, cmd.member);
      if (!remote.unlocked) throw "unlock your PC first";
      return remoteAction(room, member, cmd.action);
    }
  }
}

/** One of the host's commands, run on the PC `pc` as from afar: the preview
 * starts, streams and ends sessions; seats and answers stay as they are. */
function remoteAction(room: FakeRoom, pc: MemberView, action: RemoteAction): Response {
  const s = room.sessions.find((x) => x.host === pc.id);
  switch (action.type) {
    case "listSources":
      return { type: "sources", ...structuredClone(w().sources), controllers: 0, hostControllers: [], hiding: "notInstalled" };
    case "startSession": {
      if (s) throw "you have a session already";
      const started = hostSession(pc.id, action.title, action.max_viewers, action.input, []);
      Object.assign(started, { role: "none", operator: true, panicKey: null, answerKeys: null });
      room.sessions.push(started);
      return roomReply(room);
    }
    case "startStream": {
      if (!s) throw "there's no session";
      s.stream = { state: "live" };
      const screen = action.source.kind === "screen";
      s.streaming = {
        name: action.name,
        fps: action.fps,
        maxMbps: action.max_mbps,
        uploadMbps: action.upload_mbps,
        screen,
      };
      s.gameInFront = screen ? null : true;
      return roomReply(room);
    }
    case "stopStream":
      if (!s) throw "there's no session";
      s.stream = { state: "stopped", reason: "hostStopped" };
      s.streaming = null;
      s.desktop = null;
      return roomReply(room);
    // Desktop control (M25b): the keyboard seat comes to us with it.
    case "desktop": {
      if (!s) throw "there's no session";
      const seat = s.seats.find((x) => x.keyboardMouse);
      if (!seat) throw "this session has no keyboard-and-mouse seat";
      s.desktop = action.on ? me() : null;
      Object.assign(seat, { holder: action.on ? me() : null, host: false });
      changed(room);
      return { type: "done" };
    }
    case "bringToFront":
      if (s?.streaming == null || s.streaming.screen) throw "only a window being streamed can be brought to the front";
      s.gameInFront = true;
      changed(room);
      return { type: "done" };
    case "endSession":
      room.sessions = room.sessions.filter((x) => x !== s);
      return roomReply(room);
    default:
      return roomReply(room);
  }
}

// ---- Commands ----

function handle(cmd: Command): Response | Promise<Response> {
  const world = w();
  switch (cmd.type) {
    case "getInfo":
      if (world.startFailure) throw world.startFailure;
      return { type: "info", ...world.info };
    case "ping":
      emit({ type: "pong", nonce: cmd.nonce });
      return { type: "pong", nonce: cmd.nonce };
    case "getProfile":
      return { type: "profile", ...world.profile };
    case "setDisplayName": {
      const problem = nameProblem(cmd.name);
      if (problem) throw problem;
      world.profile.displayName = cmd.name.trim();
      for (const room of world.rooms) {
        room.members[0].name = world.profile.displayName;
        changed(room);
      }
      return { type: "profile", ...world.profile };
    }
    case "listRooms":
      return { type: "rooms", rooms: world.rooms.map(view) };
    case "createRoom": {
      const problem = nameProblem(cmd.name);
      if (problem) throw problem;
      if (cmd.password.trim() === "") throw "passwords can't be empty";
      const room: FakeRoom = {
        id: newId(),
        name: cmd.name.trim(),
        isOwner: true,
        members: [self()],
        joinsPausedForSecs: null,
        sessions: [],
        catchingUp: false,
        waitingFor: [],
        needsPassword: false,
        offeringTo: null,
        offered: null,
        canOffer: true,
      };
      world.rooms.push(room);
      return roomReply(room);
    }
    case "previewInvite":
      return { type: "invitePreview", ...preview(cmd.invite) };
    case "takeOpenedInvite": {
      const link = world.openedInvite;
      world.openedInvite = null;
      return { type: "openedInvite", link };
    }
    case "joinRoom": {
      const p = preview(cmd.invite);
      if (p.status === "own") throw "this is your own room's invite";
      if (p.status === "expired") throw "the owner has closed this room";
      if (cmd.password.trim() === "") throw "passwords can't be empty";
      if (cmd.password === "wrong") throw "wrong password";
      if (cmd.password === "offline") throw "the room's owner isn't online; invites work while the owner's Couch Co-App is running";
      const room: FakeRoom = {
        id: p.room,
        name: p.name,
        isOwner: false,
        members: [
          { ...self(), isOwner: false },
          { id: "0wner", name: "Rafa", isOwner: true, link: "direct", otherVersion: null, remote: null },
          { id: "m1", name: "Lia", isOwner: false, link: "relay", otherVersion: null, remote: null },
        ],
        joinsPausedForSecs: null,
        sessions: [],
        catchingUp: false,
        waitingFor: [],
        needsPassword: false,
        offeringTo: null,
        offered: null,
        canOffer: false,
      };
      world.rooms.push(room);
      return roomReply(room);
    }
    case "getInvite": {
      const room = owned(cmd.room);
      notCatchingUp(room);
      if (room.needsPassword) throw "this room came back from a backup without its password: set a new one first";
      return { type: "invite", link: linkFor(room) };
    }
    case "changePassword": {
      const room = owned(cmd.room);
      if (cmd.password.trim() === "") throw "passwords can't be empty";
      room.needsPassword = false;
      changed(room);
      return { type: "done" };
    }
    case "revokeInvites":
      owned(cmd.room);
      return { type: "done" };
    case "leaveRoom": {
      const room = find(cmd.room);
      if (room.offered === true) {
        throw "you accepted the owner's offer of this room: wait until they hand it over or withdraw it";
      }
      world.rooms = world.rooms.filter((r) => r !== room);
      emit({ type: "roomRemoved", room: room.id, reason: "left" });
      return { type: "done" };
    }
    case "banMember": {
      const room = owned(cmd.room);
      notCatchingUp(room);
      if (cmd.member === me()) throw "the owner can't ban themselves";
      if (!room.members.some((m) => m.id === cmd.member)) throw "no such member";
      room.members = room.members.filter((m) => m.id !== cmd.member);
      for (const s of room.sessions) {
        s.viewers = s.viewers.filter((v) => v !== cmd.member);
        for (const seat of s.seats) if (seat.holder === cmd.member) free(seat);
      }
      room.sessions = room.sessions.filter((s) => s.host !== cmd.member);
      return roomReply(room);
    }
    case "stopCatchingUp": {
      const room = owned(cmd.room);
      room.catchingUp = false;
      room.waitingFor = [];
      room.canOffer = false;
      return roomReply(room);
    }
    case "offerRoom": {
      const room = owned(cmd.room);
      const member = room.members.find((m) => m.id === cmd.member);
      if (!member) throw "no such member";
      if (member.link !== "direct" && member.link !== "relay") throw "they must be online to be offered the room";
      room.offeringTo = member.id;
      room.canOffer = false;
      return roomReply(room);
    }
    case "withdrawOffer": {
      const room = owned(cmd.room);
      room.offeringTo = null;
      room.canOffer = true;
      return roomReply(room);
    }
    case "answerOffer": {
      const room = find(cmd.room);
      if (room.offered == null) throw "there's no offer to answer";
      if (!cmd.accept) {
        if (room.offered) {
          throw "you accepted the owner's offer of this room: wait until they hand it over or withdraw it";
        }
        room.offered = null;
        return roomReply(room);
      }
      if ((cmd.password ?? "").trim() === "") throw "passwords can't be empty";
      room.offered = true;
      // The owner signs the handover a moment later.
      later(2500, () => {
        for (const m of room.members) m.isOwner = m.id === me();
        Object.assign(room, { isOwner: true, offered: null, needsPassword: false, canOffer: true });
        changed(room);
        emit({ type: "ownerChanged", room: room.id, owner: me() });
      });
      return roomReply(room);
    }
    case "getShortcuts":
      return { type: "shortcuts", shortcuts: world.shortcuts };
    case "setShortcut": {
      const row = world.shortcuts.find((s) => s.id === cmd.id);
      if (!row) throw `there is no shortcut "${cmd.id}"`;
      const key = keyName(cmd.code);
      if (key == null) throw "that key can't be used for a shortcut";
      if (!row.chord && !/^F([1-9]|1[0-2])$/.test(key)) throw "only F1 to F12 (but F4) work without Ctrl+Alt+Shift";
      const keys = row.chord ? `Ctrl+Alt+Shift+${key}` : key;
      const taken = world.shortcuts.find((s) => s.id !== row.id && s.keys === keys);
      if (taken) throw `${keys} already does this: ${taken.action}`;
      row.keys = keys;
      row.isDefault = keys === DEFAULT_KEYS[row.id];
      return { type: "shortcuts", shortcuts: world.shortcuts };
    }
    case "resetShortcuts":
      for (const row of world.shortcuts) {
        if (cmd.id == null || row.id === cmd.id) {
          row.keys = DEFAULT_KEYS[row.id];
          row.isDefault = true;
        }
      }
      return { type: "shortcuts", shortcuts: world.shortcuts };
    case "getNetwork":
      return { type: "network", ...world.network };
    case "getStreamWindow":
      return { type: "streamWindow", bar: world.streamWindowBar };
    case "getRemotePlay":
    case "setRemotePlay":
    case "setStartAtSignIn":
    case "setRemotePin":
    case "addRemoteDevice":
    case "removeRemoteDevice":
    case "acceptRemoteHost":
    case "forgetRemoteHost":
    case "unlockRemote":
    case "lockRemote":
    case "remote":
      return remoteCommand(cmd);
    case "setStreamWindow": {
      world.streamWindowBar = cmd.bar;
      const found = ours();
      if (found?.session.streamWindow) {
        found.session.streamWindow.bar = cmd.bar;
        changed(found.room);
      }
      return { type: "done" };
    }
    case "setStreamVolume": {
      const { room, session: s } = watching();
      if (s.streamWindow) {
        s.streamWindow.volume = Math.max(0, Math.min(100, cmd.percent));
        s.streamWindow.muted = cmd.muted;
      }
      changed(room);
      return { type: "done" };
    }
    case "streamWindow": {
      const { room, session: s } = watching();
      const win = s.streamWindow;
      if (!win) throw "you aren't watching a stream";
      // The window answers with its new state a moment later, as the real one.
      later(120, () => {
        const r = cmd.request;
        if (r.type === "fullscreen") win.fullscreen = r.on;
        if (r.type === "showBar") win.bar = r.on;
        if (r.type === "showStats") win.stats = r.on;
        changed(room);
      });
      return { type: "done" };
    }
    case "setNetwork": {
      if (typeof cmd.relay === "object" && !/^https?:\/\/[^/\s]+/.test(cmd.relay.custom)) {
        throw "that relay address won't work: relative URL without a base";
      }
      world.network.saved = cmd.relay;
      world.network.restartNeeded = JSON.stringify(world.network.saved) !== JSON.stringify(world.network.inUse);
      return { type: "done" };
    }
    case "getDrivers":
      return { type: "drivers", drivers: world.drivers };
    case "installDriver":
      return new Promise((resolve) =>
        later(1500, () => {
          const d = world.drivers.find((x) => x.driver === cmd.driver);
          if (d) d.state = cmd.driver === "hidhide" ? "needsRestart" : "ready";
          resolve({ type: "drivers", drivers: world.drivers });
        }),
      );
    case "getNotices":
      return { type: "notices", text: world.notices };
    case "exportProfile":
      if ([...cmd.passphrase.trim().normalize("NFKC")].length < 12) throw "the passphrase must be at least 12 characters";
      return { type: "backup", path: world.backupPath };
    case "restoreProfile":
      if (world.rooms.length > 0) {
        throw "this profile already has rooms: a backup is restored only into a new profile (or on a new PC)";
      }
      if (cmd.passphrase === "wrong") throw "wrong passphrase, or the file is damaged";
      return { type: "backup", path: world.backupPath };
    case "listSources":
      return { type: "sources", ...world.sources };
    case "startSession": {
      const room = find(cmd.room);
      const found = ours();
      if (found?.session.role === "host") throw "you have a session already";
      if (found) throw "stop watching first";
      if (nameProblem(cmd.title)) throw "titles must be 1 to 96 bytes, with nothing invisible in them";
      if (cmd.input.controllers > 4) throw "a session has at most 4 controllers and one keyboard-and-mouse seat";
      const hiding = world.sources.hiding;
      const chosen = hiding === "ready" ? cmd.controllers.map((c: HostController) => c.name) : [];
      const s = hostSession(me(), cmd.title.trim(), cmd.max_viewers, cmd.input, chosen);
      if (hiding !== "ready" && cmd.controllers.length > 0) {
        s.hostControllerProblem = hiding === "busy" ? "HidHide is busy" : "HidHide isn't installed";
      }
      room.sessions.unshift(s);
      return roomReply(room);
    }
    case "startStream": {
      const { room, session: s } = hosting();
      if (s.stream.state === "starting") throw "a stream is starting already";
      s.stream = { state: "starting" };
      s.streaming = null;
      changed(room);
      later(1200, () => {
        if (s.stream.state !== "starting") return;
        s.stream = { state: "live" };
        const screen = cmd.source.kind === "screen";
        s.streaming = { name: cmd.name, fps: cmd.fps, maxMbps: cmd.max_mbps, uploadMbps: cmd.upload_mbps, screen };
        s.gameInFront = screen ? null : true;
        changed(room);
      });
      return roomReply(room);
    }
    case "stopStream": {
      const { room, session: s } = hosting();
      if (s.stream.state !== "live" && s.stream.state !== "starting") throw "nothing is being streamed";
      s.stream = { state: "stopped", reason: "hostStopped" };
      s.streaming = null;
      changed(room);
      return { type: "done" };
    }
    case "setStreamLevel": {
      const { room, session: s } = hosting();
      s.soundLevel = Math.max(0, Math.min(400, cmd.percent));
      return roomReply(room);
    }
    case "endSession": {
      const { room, session: s } = hosting();
      room.sessions = room.sessions.filter((x) => x !== s);
      changed(room);
      return { type: "done" };
    }
    case "watch": {
      const room = find(cmd.room);
      const s = room.sessions.find((x) => x.host === cmd.host);
      if (!s) throw "that stream has ended";
      if (s.host === me()) throw "that is your own stream";
      const found = ours();
      if (found?.session.role === "host") throw "stop streaming first";
      if (found) throw "stop watching first";
      s.role = "joining";
      // The stream window opens at once, and says how it is a moment after.
      s.streamWindow = { volume: 80, muted: false, open: false, fullscreen: false, bar: world.streamWindowBar, stats: false };
      s.gameInFront = true;
      later(700, () => {
        if (s.role !== "joining") return;
        s.role = "watching";
        s.viewers.push(me());
        if (s.streamWindow) s.streamWindow.open = true;
        changed(room);
      });
      return roomReply(room);
    }
    case "stopWatching": {
      const { room, session: s } = watching();
      s.role = "none";
      s.streamWindow = null;
      s.gameInFront = null;
      s.note = null;
      s.askedSeat = null;
      s.viewers = s.viewers.filter((v) => v !== me());
      for (const seat of s.seats) if (seat.holder === me()) free(seat);
      changed(room);
      return { type: "done" };
    }
    case "requestControl": {
      const { room, session: s } = watching();
      if (s.role === "joining") throw "not watching yet";
      if (s.role === "reconnecting") throw "reconnecting to the host";
      if (!s.seats[cmd.seat]) throw "there is no such seat";
      if (s.controlPaused) throw "control is paused; allow it again first";
      s.askedSeat = cmd.seat;
      s.role = "asking";
      changed(room);
      if (world.hostAnswers !== "wait") {
        later(2500, () => {
          if (s.askedSeat !== cmd.seat) return;
          s.askedSeat = null;
          if (world.hostAnswers === "accept") {
            const held = seatsOf(s, me());
            // Beyond our controllers, the asking one's seat goes (P13), or
            // with one controller its one seat.
            const asking = s.ourControllers.find((c) => c.slot === cmd.controller);
            if (held.length > 0 && !keeps(s, held, world.myControllers, cmd.seat)) {
              const leaves = asking?.seat ?? held.find((h) => s.seats[h].pad);
              if (leaves != null) free(s.seats[leaves]);
            }
            free(s.seats[cmd.seat]);
            s.seats[cmd.seat].holder = me();
            if (asking && s.seats[cmd.seat].pad) asking.seat = cmd.seat;
          }
          s.role = seatsOf(s, me()).length > 0 ? "controlling" : "watching";
          changed(room);
        });
      }
      return { type: "done" };
    }
    case "cancelControl": {
      const { room, session: s } = watching();
      if (s.askedSeat != null) {
        s.askedSeat = null;
      } else {
        for (const seat of s.seats) if (seat.holder === me()) free(seat);
      }
      s.role = seatsOf(s, me()).length > 0 ? "controlling" : "watching";
      changed(room);
      return { type: "done" };
    }
    case "giveBackSeat": {
      const { room, session: s } = watching();
      if (s.seats[cmd.seat]?.holder !== me()) throw "you don't have that seat";
      free(s.seats[cmd.seat]);
      s.role = seatsOf(s, me()).length > 0 ? (s.askedSeat != null ? "asking" : "controlling") : "watching";
      changed(room);
      return { type: "done" };
    }
    case "answerControl": {
      const { room, session: s } = hosting();
      const request = s.requests.find((r) => r.member === cmd.member);
      if (!request) throw "that member hasn't asked for control";
      if (cmd.accept) pass(s, cmd.member, request.seat, request.controller ?? undefined, true);
      s.requests = s.requests.filter((r) => r.member !== cmd.member);
      return roomReply(room);
    }
    case "passControl": {
      const { room, session: s } = hosting();
      pass(s, cmd.member, cmd.seat, cmd.controller);
      return roomReply(room);
    }
    case "takeBackControl": {
      const { room, session: s } = hosting();
      if (cmd.seat == null) {
        for (const seat of s.seats) if (!seat.host) free(seat);
      } else {
        if (!s.seats[cmd.seat]) throw "there is no such seat";
        free(s.seats[cmd.seat]);
      }
      return roomReply(room);
    }
    case "playSeat": {
      const { room, session: s } = hosting();
      play(s, cmd.seat, cmd.controller);
      return roomReply(room);
    }
    case "allowControl": {
      const { room, session: s } = hosting();
      s.controlPaused = false;
      return roomReply(room);
    }
    case "stopWaitingForPads": {
      const { room, session: s } = hosting();
      s.padsMissing = [...s.padsMissing, ...s.padsSettingUp];
      s.padsSettingUp = [];
      changed(room);
      return { type: "done" };
    }
    case "bringToFront": {
      const { room, session: s } = hosting();
      if (s.streaming == null || s.streaming.screen) throw "only a window being streamed can be brought to the front";
      s.gameInFront = true;
      changed(room);
      return { type: "done" };
    }
  }
}

// ---- Shortcuts ----

const DEFAULT_KEYS: Record<string, string> = {
  panic: "Ctrl+Alt+Shift+Backspace",
  acceptRequest: "Ctrl+Alt+Shift+Y",
  declineRequest: "Ctrl+Alt+Shift+N",
  toggleControl: "Ctrl+Alt+Shift+C",
  freeMouse: "Ctrl+Alt+Shift+M",
  fullscreen: "Ctrl+Alt+Shift+F",
  volumeDown: "Ctrl+Alt+Shift+-",
  volumeUp: "Ctrl+Alt+Shift+=",
  mute: "Ctrl+Alt+Shift+0",
  toggleHud: "Ctrl+Alt+Shift+H",
  toggleHudAlone: "F3",
};

/** The engine's name for a key the app accepts, or `null`. */
function keyName(code: string): string | null {
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter) return letter[1];
  const digit = /^Digit(\d)$/.exec(code);
  if (digit) return digit[1];
  const f = /^F(\d{1,2})$/.exec(code);
  if (f && Number(f[1]) >= 1 && Number(f[1]) <= 12 && f[1] !== "4") return `F${f[1]}`;
  return { Minus: "-", Equal: "=", Backspace: "Backspace", BracketLeft: "[", BracketRight: "]" }[code] ?? null;
}

export function defaultShortcuts(): ShortcutView[] {
  const rows: [string, boolean, string, ShortcutView["scope"], string][] = [
    ["panic", true, "Take the controller back from everyone and pause control", "host", "while you stream; the game never sees it"],
    ["acceptRequest", true, "Accept the oldest request for a seat", "host", "while you stream and someone asks; the game never sees it"],
    ["declineRequest", true, "Decline the oldest request for a seat", "host", "while you stream and someone asks; the game never sees it"],
    ["toggleControl", true, "Ask for the controller, withdraw the request, or give it back", "viewer", ""],
    ["freeMouse", true, "Free the mouse, or capture it again", "viewer", "while you may use the mouse"],
    ["fullscreen", true, "Full screen, or back to a window", "viewer", ""],
    ["volumeDown", true, "Turn the stream down", "viewer", ""],
    ["volumeUp", true, "Turn the stream up, to as loud as the host sends it", "viewer", ""],
    ["mute", true, "Mute the stream, or unmute it", "viewer", ""],
    ["toggleHud", true, "Show or hide the overlay", "viewer", ""],
    ["toggleHudAlone", false, "Show or hide the overlay", "viewer", "while you aren't sending keys to the game"],
  ];
  return rows.map(([id, chord, action, scope, condition]) => ({
    id,
    keys: DEFAULT_KEYS[id],
    chord,
    isDefault: true,
    action,
    scope,
    condition,
  }));
}

// ---- Scripted happenings, for the preview's picker ----

/** A viewer of our session asks for a seat. */
export function someoneAsks() {
  const found = world?.rooms.flatMap((room) => room.sessions.filter((s) => s.role === "host").map((s) => ({ room, s })))[0];
  if (!found) return;
  const { room, s } = found;
  const asker = s.viewers.find((v) => !s.requests.some((r) => r.member === v));
  if (!asker) return;
  const seat = Math.max(0, s.seats.findIndex((x) => x.holder !== asker));
  s.requests.push({ member: asker, seat, controller: null });
  changed(room);
  emit({ type: "controlRequested", room: room.id, member: asker, seat: s.seats[seat].label });
}

/** An invite link arrives, as if a web page opened one. */
export function openInvite(link: string) {
  if (world) world.openedInvite = link;
  emit({ type: "inviteOpened" });
}

/** The host pressed the panic key. */
export function panicKey() {
  const found = world?.rooms.flatMap((room) => room.sessions.filter((s) => s.role === "host").map((s) => ({ room, s })))[0];
  if (!found) return;
  for (const seat of found.s.seats) if (!seat.host) free(seat);
  found.s.controlPaused = true;
  changed(found.room);
}
