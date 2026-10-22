// The preview's scenarios: worlds for the fake engine, one per state worth
// seeing. Picked by `?scenario=<name>` or the preview's picker.
import type { MemberLink } from "../bindings/MemberLink";
import type { MemberView } from "../bindings/MemberView";
import type { SessionView } from "../bindings/SessionView";
import { defaultShortcuts, hostSession, seatsFor, type FakeRoom, type World } from "./fakeEngine";

const ME = "a11ce000000000000000000000000001";

function member(id: string, name: string, link: MemberLink, extra: Partial<MemberView> = {}): MemberView {
  return { id, name, isOwner: false, link, otherVersion: null, ...extra };
}

const you = (owner: boolean): MemberView => member(ME, "Ana", "me", { isOwner: owner });
const bia = member("b1a", "Bia", "direct");
const caio = member("ca10", "Caio", "relay");
const dani = member("dan1", "Dani", "online");
const edu = member("ed0", "Edu", "offline");
const fer = member("fe2", "Fer", "direct", { otherVersion: { appVersion: "0.12.0", newer: false } });

function room(id: string, name: string, over: Partial<FakeRoom> = {}): FakeRoom {
  return {
    id,
    name,
    isOwner: true,
    members: [you(true)],
    joinsPausedForSecs: null,
    sessions: [],
    catchingUp: false,
    waitingFor: [],
    needsPassword: false,
    offeringTo: null,
    offered: null,
    canOffer: true,
    ...over,
  };
}

/** Someone else's session, as a member who isn't in it sees it. */
function othersSession(host: string, title: string, over: Partial<SessionView> = {}): SessionView {
  const seats = seatsFor({ controllers: 2, keyboardMouse: "none" });
  return {
    host,
    title,
    maxViewers: 4,
    viewers: [],
    viewerControllers: [],
    seats,
    hostOnSeats: false,
    controlPaused: false,
    role: "none",
    requests: [],
    askedSeat: null,
    panicKey: null,
    answerKeys: null,
    overlayArea: null,
    hostControllers: [],
    hostHomes: [],
    hostControllerProblem: null,
    stream: { state: "live" },
    streaming: null,
    soundLevel: null,
    ...over,
  };
}

function base(rooms: FakeRoom[], over: Partial<World> = {}): World {
  return {
    info: {
      appVersion: "0.17.0",
      commit: null,
      repository: "https://github.com/marcusvilete/couch-coapp",
      protocol: "6",
      profile: "default",
      dataDir: "C:\\Users\\ana\\AppData\\Local\\CouchCoApp\\profiles\\default",
    },
    profile: { displayName: "Ana", memberId: ME },
    rooms,
    shortcuts: defaultShortcuts(),
    network: {
      saved: "builtin",
      builtIn: "https://couchrelay.duckdns.org/",
      testBuild: false,
      inUse: "builtin",
      homeRelay: "https://couchrelay.duckdns.org/",
      restartNeeded: false,
    },
    drivers: [
      { driver: "vigembus", state: "ready", installer: true },
      { driver: "hidhide", state: "missing", installer: true },
    ],
    sources: {
      windows: [
        { id: "101", title: "The Witcher 3: Wild Hunt", app: "witcher3.exe", elevated: false, refreshHz: 144 },
        { id: "102", title: "Overcooked! 2", app: "Overcooked2.exe", elevated: false, refreshHz: 144 },
        { id: "103", title: "Discord", app: "Discord.exe", elevated: false, refreshHz: 144 },
        { id: "104", title: "Task Manager", app: "Taskmgr.exe", elevated: true, refreshHz: 144 },
      ],
      screens: [
        { id: "s1", name: "DELL U2723QE", width: 3840, height: 2160, primary: true, refreshHz: 60 },
        { id: "s2", name: "LG 27GL850", width: 2560, height: 1440, primary: false, refreshHz: 144 },
      ],
      controllers: 1,
      hostControllers: [{ name: "DualSense Wireless Controller", vendor: 1356, product: 3302, kind: "physical" }],
      hiding: "ready",
      virtualPads: true,
    },
    notices: "Couch Co-App's third-party notices\n\nThe crates it links (Rust)\n…\n",
    openedInvite: null,
    invites: {
      "https://marcusvilete.github.io/couch-coapp/j/#expired": {
        room: "dead0000",
        name: "Old games",
        status: "expired",
        expiresInSecs: 0,
      },
    },
    myControllers: 1,
    hostAnswers: "accept",
    backupPath: "C:\\Users\\ana\\Documents\\Ana.couchbackup",
    startFailure: null,
    sound: "game",
    ...over,
  };
}

const sofa = (over: Partial<FakeRoom> = {}) =>
  room("50fa0000000000000000000000000001", "Sofá", { members: [you(true), bia, caio, dani, edu, fer], ...over });
const friday = (over: Partial<FakeRoom> = {}) =>
  room("f41d0000000000000000000000000002", "Friday games", {
    isOwner: false,
    canOffer: false,
    members: [you(false), member("ra4", "Rafa", "direct", { isOwner: true }), caio, member("l1a", "Lia", "offline")],
    ...over,
  });
const retro = () =>
  room("2e720000000000000000000000000003", "Retro night", {
    members: [you(true), member("g0", "Gui", "offline")],
  });

/** Our session in Sofá: four pads and keyboard and mouse, Bia, Caio and Dani
 * watching, our two controllers on seats. */
function hosting(over: Partial<SessionView> = {}, live = true): SessionView {
  const s = hostSession(ME, "Game night", 4, { controllers: 4, keyboardMouse: "ownSeat" }, ["DualSense Wireless Controller", "Xbox Wireless Controller"]);
  s.viewers = [bia.id, caio.id, dani.id];
  s.viewerControllers = [2, 1, 1];
  s.seats[2].holder = bia.id;
  s.seats[4].holder = caio.id;
  s.requests = [
    { member: dani.id, seat: 3 },
    { member: bia.id, seat: 4 },
  ];
  if (live) {
    s.stream = { state: "live" };
    s.streaming = { name: "The Witcher 3: Wild Hunt", fps: 60, maxMbps: 20, uploadMbps: null };
  }
  return Object.assign(s, over);
}

export type Scenario = { title: string; world: () => World; release?: boolean };

export const SCENARIOS: Record<string, Scenario> = {
  "first-run": { title: "First run: no rooms", world: () => base([]) },
  fatal: {
    title: "The app couldn't start",
    world: () => base([], { startFailure: 'profile "default" is already running in another Couch Co-App' }),
  },
  lobby: {
    title: "Rooms: Bia streams in Sofá",
    world: () =>
      base([
        sofa({ sessions: [othersSession(bia.id, "Overcooked with the gang", { seats: seatsFor({ controllers: 2, keyboardMouse: "none" }) })] }),
        friday(),
        retro(),
      ]),
  },
  "owner-alone": { title: "An owner alone in a new room", world: () => base([room("a10e", "Movie night")]) },
  "member-room": { title: "A room someone else owns", world: () => base([friday(), sofa()]) },
  "many-members": {
    title: "Twelve members, every link, both version notes",
    world: () =>
      base([
        sofa({
          members: [
            you(true),
            bia,
            caio,
            dani,
            edu,
            fer,
            member("g1", "Gui", "direct", { otherVersion: { appVersion: "0.18.0", newer: true } }),
            member("h1", "Helena Maria dos Santos Albuquerque", "relay"),
            member("i1", "イチロー", "online"),
            member("j1", "Jo 🎮", "offline"),
            member("k1", "Kai", "offline"),
            member("l1", "Lu", "direct"),
          ],
        }),
      ]),
  },
  "joins-paused": { title: "Joining paused after wrong passwords", world: () => base([sofa({ joinsPausedForSecs: 540 })]) },
  "restored-owner": {
    title: "A restored owner: no password, catching up",
    world: () => base([sofa({ catchingUp: true, waitingFor: [bia.id, edu.id], needsPassword: true, canOffer: false })]),
  },
  "member-catching-up": {
    title: "A restored member catching up",
    world: () => base([friday({ catchingUp: true, waitingFor: ["ra4", caio.id] })]),
  },
  "handover-offering": {
    title: "Offering the room to Bia",
    world: () => base([sofa({ offeringTo: bia.id, canOffer: false })]),
  },
  "handover-offered": { title: "Rafa offers us the room", world: () => base([friday({ offered: false })]) },
  "handover-accepted": { title: "We accepted Rafa's offer", world: () => base([friday({ offered: true })]) },
  watching: {
    title: "Watching Bia's session",
    world: () => base([sofa({ sessions: [othersSession(bia.id, "Overcooked with the gang", { role: "watching", viewers: [ME] })] })]),
  },
  asking: {
    title: "Asking for Controller 2",
    world: () =>
      base([sofa({ sessions: [othersSession(bia.id, "Overcooked with the gang", { role: "asking", askedSeat: 1, viewers: [ME] })] })], {
        hostAnswers: "wait",
      }),
  },
  "playing-two": {
    title: "Playing two seats with two controllers",
    world: () => {
      const s = othersSession(bia.id, "Overcooked with the gang", { role: "controlling", viewers: [ME, caio.id] });
      s.seats = seatsFor({ controllers: 3, keyboardMouse: "ownSeat" });
      s.seats[0].holder = ME;
      s.seats[1].holder = ME;
      s.seats[2].holder = caio.id;
      return base([sofa({ sessions: [s] })], { myControllers: 2 });
    },
  },
  "watching-paused": {
    title: "Watching while the host paused control",
    world: () => base([sofa({ sessions: [othersSession(bia.id, "Overcooked", { role: "watching", viewers: [ME], controlPaused: true })] })]),
  },
  reconnecting: {
    title: "Reconnecting to the host",
    world: () => base([sofa({ sessions: [othersSession(bia.id, "Overcooked", { role: "reconnecting", viewers: [ME] })] })]),
  },
  "session-full": {
    title: "A full session, and one not streaming",
    world: () =>
      base([
        sofa({
          sessions: [
            othersSession(bia.id, "Overcooked", { maxViewers: 2, viewers: [caio.id, dani.id] }),
            othersSession(caio.id, "Stardew Valley", { stream: { state: "stopped", reason: "gameExited" } }),
          ],
        }),
      ]),
  },
  "hosting-idle": {
    title: "Hosting: nothing streamed yet, one seat",
    world: () => base([sofa({ sessions: [hostSession(ME, "Game night", 4, { controllers: 1, keyboardMouse: "none" }, [])] })]),
  },
  "hosting-live": { title: "Hosting: live, five seats, two requests", world: () => base([sofa({ sessions: [hosting()] })]) },
  "hosting-paused": {
    title: "Hosting: control paused",
    world: () => base([sofa({ sessions: [hosting({ controlPaused: true, requests: [] })] })]),
  },
  "hosting-trouble": {
    title: "Hosting: no panic key, controllers can't hide",
    world: () => {
      const s = hostSession(ME, "Game night", 4, { controllers: 2, keyboardMouse: "none" }, []);
      Object.assign(s, { panicKey: null, answerKeys: null, hostControllerProblem: "HidHide isn't installed" });
      s.viewers = [bia.id];
      s.viewerControllers = [1];
      return base([sofa({ sessions: [s] })]);
    },
  },
  "start-no-drivers": {
    title: "Start a session without ViGEmBus or HidHide",
    world: () =>
      base([sofa()], {
        sources: { ...base([]).sources, hiding: "notInstalled", virtualPads: false },
      }),
  },
  "start-remapper": {
    title: "Start a session: a remapper's pad and two of one model",
    world: () =>
      base([sofa()], {
        sources: {
          ...base([]).sources,
          controllers: 3,
          hostControllers: [
            { name: "DualSense Wireless Controller", vendor: 1356, product: 3302, kind: "physical" },
            { name: "DualSense Wireless Controller", vendor: 1356, product: 3302, kind: "physical" },
            { name: "Xbox 360 Controller", vendor: 1118, product: 654, kind: "otherVirtual" },
          ],
        },
      }),
  },
  "release-build": {
    title: "A release build (restarts itself)",
    release: true,
    world: () => base([sofa(), friday()], { info: { ...base([]).info, commit: "e0203b1" } }),
  },
  overflow: {
    title: "Twenty rooms with long names",
    world: () =>
      base(
        Array.from({ length: 20 }, (_, i) =>
          room(`0verf10w${String(i).padStart(4, "0")}`, i % 3 === 0 ? `Sala de jogos do fim de semana com os amigos ${i}` : i % 3 === 1 ? `ゲームの夜 ${i}` : `Room ${i} 🎮`),
        ),
      ),
  },
};

export const DEFAULT_SCENARIO = "lobby";
