// Sessions on screen: the stage with a card per session, the host's panel
// for its own session and the stream in it, the dialog that starts a session.
// The engine decides everything; these only show its state and send commands.
import { useEffect, useState, type FormEvent } from "react";
import type { Command } from "./bindings/Command";
import type { HostController } from "./bindings/HostController";
import type { InputSetup } from "./bindings/InputSetup";
import type { KeyboardMouseSeat } from "./bindings/KeyboardMouseSeat";
import type { RoomView } from "./bindings/RoomView";
import type { SessionView } from "./bindings/SessionView";
import type { SourceList } from "./bindings/SourceList";
import type { StreamStateView } from "./bindings/StreamStateView";
import { prepareChime } from "./chime";
import { engineCommand, errorText, request } from "./engine";
import { Avatar, MAX_NAME_BYTES, Modal, truncateBytes } from "./ui";

/** What this profile is doing in any room: one session at a time, and not
 * while watching one. */
export type Busy = { hosting: boolean; watching: boolean };

const WATCHING_ROLES: SessionView["role"][] = ["joining", "watching", "asking", "controlling", "reconnecting"];

export function busyIn(rooms: RoomView[]): Busy {
  const all = rooms.flatMap((r) => r.sessions);
  return {
    hosting: all.some((s) => s.role === "host"),
    watching: all.some((s) => WATCHING_ROLES.includes(s.role)),
  };
}

export function nameOf(room: RoomView, member: string | null | undefined): string {
  return room.members.find((m) => m.id === member)?.name ?? "someone";
}

/** Who plays: each seat's holder (the host's own controller included), and
 * the host while a seat is free if they play outside the seats. */
export function playersOf(session: SessionView): string[] {
  const holders = session.seats.flatMap((s) => (s.holder ? [s.holder] : []));
  const outside = !session.hostOnSeats && holders.length < session.seats.length;
  return outside ? [...holders, session.host] : holders;
}

/** A seat's label in a sentence: "the controller", "Controller 2". */
export function seatPhrase(label: string): string {
  return /\d$/.test(label) ? label : `the ${label.toLowerCase()}`;
}

/** Whether a session streams something now. */
export function isLive(session: SessionView): boolean {
  return session.stream.state === "live";
}

/** "Bia is playing" (the host while nobody holds the one seat and they play
 * outside it), or per seat: "Controller 1: Ana · Controller 2: free". A host
 * playing outside the seats, on whatever the game sees directly, is listed
 * beside them: "Ana: own controller". */
function whoPlays(room: RoomView, s: SessionView): string {
  if (s.seats.length === 1) {
    const holder = s.seats[0].holder ?? (s.hostOnSeats ? null : s.host);
    return holder ? `${nameOf(room, holder)} is playing` : "Nobody is playing";
  }
  const seats = s.seats.map((seat) => `${seat.label}: ${seat.holder ? nameOf(room, seat.holder) : "free"}`);
  const outside = s.hostOnSeats ? [] : [`${nameOf(room, s.host)}: own controller`];
  return [...seats, ...outside].join(" · ");
}

function allowsKbm(s: SessionView): boolean {
  return s.seats.some((seat) => seat.keyboardMouse);
}

/** Why a session streams nothing, for those watching it; `null` while live. */
function idleText(host: string, stream: StreamStateView): string | null {
  switch (stream.state) {
    case "live":
      return null;
    case "starting":
      return `${host} is starting a stream…`;
    case "waiting":
      return `${host} isn't streaming anything yet.`;
    case "stopped":
      switch (stream.reason) {
        case "gameExited":
          return "The game closed; nothing is streamed now.";
        case "sourceLost":
          return "The window or screen being streamed went away; nothing is streamed now.";
        case "error":
          return "The stream stopped after an error; nothing is streamed now.";
        default:
          return `${host} stopped streaming for now.`;
      }
  }
}

/** The LIVE badge while streaming, SESSION otherwise. */
export function SessionBadge(props: { session: SessionView; title?: string }) {
  return isLive(props.session) ? (
    <span className="live-badge" title={props.title}>
      LIVE
    </span>
  ) : (
    <span className="session-badge" title={props.title}>
      SESSION
    </span>
  );
}

/** Runs one command at a time, showing failures as notices. */
function useRun(notify: (text: string, tone?: "info" | "warn") => void, onRoom: (room: RoomView) => void) {
  const [busy, setBusy] = useState<string | null>(null);
  const run = async (key: string, command: Command) => {
    setBusy(key);
    try {
      const reply = await engineCommand(command);
      if (reply.type === "room") onRoom(reply);
    } catch (e) {
      notify(errorText(e), "warn");
    } finally {
      setBusy(null);
    }
  };
  return { busy, run };
}

type StageProps = {
  room: RoomView;
  busy: Busy;
  onStartSession: () => void;
  onInvite: () => void;
  onRoom: (room: RoomView) => void;
  notify: (text: string, tone?: "info" | "warn") => void;
};

export function Stage(props: StageProps) {
  const { room, busy } = props;
  const mine = room.sessions.find((s) => s.role === "host");
  const others = room.sessions.filter((s) => s.role !== "host");
  const startBlocked = busy.hosting
    ? "You have a session already, in another room."
    : busy.watching
      ? "Stop watching first."
      : null;

  if (mine == null && others.length === 0) {
    const alone = room.members.length === 1;
    return (
      <main className="stage">
        <div className="stage-empty">
          <h2>Nobody has a session yet</h2>
          <p className="hint">
            Start a session to play with the room: the controller seats come first, then you stream a game window or a
            screen, as often as you like. The others watch, and you hand them the controller.
          </p>
          <div className="stage-empty-actions">
            <button className="primary" onClick={props.onStartSession} disabled={startBlocked != null} title={startBlocked ?? undefined}>
              Start a session
            </button>
            {room.isOwner && alone && <button onClick={props.onInvite}>Invite people</button>}
          </div>
          {startBlocked && <p className="hint">{startBlocked}</p>}
        </div>
      </main>
    );
  }

  return (
    <main className="stage live">
      <header className="stage-header">
        <h2>Sessions in {room.name}</h2>
        {mine == null && (
          <button className="primary" onClick={props.onStartSession} disabled={startBlocked != null} title={startBlocked ?? undefined}>
            Start a session
          </button>
        )}
      </header>
      {mine && <HostPanel room={room} session={mine} onRoom={props.onRoom} notify={props.notify} />}
      {others.length > 0 && (
        <div className="stage-cards">
          {others.map((s) => (
            <StageCard key={s.host} room={room} session={s} busy={busy} onRoom={props.onRoom} notify={props.notify} />
          ))}
        </div>
      )}
    </main>
  );
}

function StageCard(props: {
  room: RoomView;
  session: SessionView;
  busy: Busy;
  onRoom: (room: RoomView) => void;
  notify: (text: string, tone?: "info" | "warn") => void;
}) {
  const { room, session: s } = props;
  const { busy, run } = useRun(props.notify, props.onRoom);
  const host = nameOf(room, s.host);
  // This profile is listed first.
  const me = room.members[0]?.id;
  const full = s.viewers.length >= s.maxViewers;
  const watchBlocked =
    props.busy.hosting ? "End your session first." : props.busy.watching ? "You're watching another session." : full ? "Full: the host chose how many can watch." : null;

  let state: string;
  let actions;
  switch (s.role) {
    case "none":
      state = "";
      actions = (
        <button className="primary" disabled={busy != null || watchBlocked != null} title={watchBlocked ?? undefined} onClick={() => run("watch", { type: "watch", room: room.id, host: s.host })}>
          {busy === "watch" ? "Opening…" : "Watch"}
        </button>
      );
      break;
    case "joining":
      state = `Waiting for ${host}…`;
      actions = <StopWatching run={run} busy={busy} />;
      break;
    case "reconnecting":
      state = `Lost the connection to ${host}. Reconnecting…`;
      actions = <StopWatching run={run} busy={busy} />;
      break;
    default: {
      const mine = s.seats.findIndex((seat) => seat.holder != null && seat.holder === me);
      const asked = s.askedSeat;
      const seatName = (i: number | null) => (i == null || i < 0 ? "a seat" : seatPhrase(s.seats[i].label));
      state =
        s.role === "asking"
          ? mine >= 0
            ? `You're playing on ${s.seats[mine].label}, and asked ${host} for ${seatName(asked)}.`
            : `You asked ${host} for ${seatName(asked)}.`
          : mine >= 0
            ? s.seats.length > 1
              ? `You're playing, on ${s.seats[mine].label}.`
              : "You're playing."
            : s.controlPaused
              ? `You're watching. ${host} paused control for now.`
              : "You're watching.";
      // Any seat but ours: a free one, someone else's (the host decides),
      // or another than ours (a move).
      const asks = s.seats.map((seat, i) =>
        i === mine ? null : (
          <button key={i} disabled={busy != null || s.controlPaused || asked === i} onClick={() => run(`ask-${i}`, { type: "requestControl", seat: i })}>
            {asked === i ? `Asked for ${seatPhrase(seat.label)}` : `Ask for ${seatPhrase(seat.label)}`}
          </button>
        ),
      );
      actions = (
        <>
          {asks}
          {s.role === "asking" && (
            <button disabled={busy != null} onClick={() => run("cancel", { type: "cancelControl" })}>
              Withdraw the request
            </button>
          )}
          {s.role === "controlling" && (
            <button disabled={busy != null} onClick={() => run("cancel", { type: "cancelControl" })}>
              Give it back
            </button>
          )}
          <StopWatching run={run} busy={busy} />
        </>
      );
    }
  }
  const idle = idleText(host, s.stream);

  return (
    <article className={`stage-card${s.role !== "none" ? " mine" : ""}`}>
      <div className="card-head">
        <Avatar name={host} size="large" />
        <div className="card-title">
          <SessionBadge session={s} />
          <h3 title={s.title}>{s.title}</h3>
          <span className="hint">{host}'s session</span>
        </div>
      </div>
      <p>
        <span className="playing" aria-hidden="true">
          🎮
        </span>{" "}
        {whoPlays(room, s)} · {s.viewers.length} of {s.maxViewers} watching
        {allowsKbm(s) && " · keyboard and mouse allowed"}
      </p>
      {idle && <p className="hint">{idle}</p>}
      {state && <p className="card-state">{state}</p>}
      <div className="card-actions">{actions}</div>
    </article>
  );
}

function StopWatching(props: { run: (key: string, c: Command) => Promise<void>; busy: string | null }) {
  return (
    <button disabled={props.busy != null} onClick={() => props.run("stop", { type: "stopWatching" })}>
      Stop watching
    </button>
  );
}

function HostPanel(props: {
  room: RoomView;
  session: SessionView;
  onRoom: (room: RoomView) => void;
  notify: (text: string, tone?: "info" | "warn") => void;
}) {
  const { room, session: s } = props;
  const { busy, run } = useRun(props.notify, props.onRoom);
  const seatOf = (member: string) => s.seats.findIndex((seat) => seat.holder === member);
  const single = s.seats.length === 1;
  const pass = (member: string, seat: number) => run(`pass-${member}`, { type: "passControl", member, seat });
  // Our own controller plays through a seat (hidden from games), or we play
  // outside the seats on whatever the game sees directly.
  const pad = s.hostController;
  const ourSeat = s.seats.findIndex((seat) => seat.host);
  const holderName = (seat: SessionView["seats"][number]) =>
    seat.host ? `you (${pad ?? "your controller"})` : seat.holder ? nameOf(room, seat.holder) : "free";
  return (
    <section className="host-panel" aria-label="Your session">
      <div className="host-head">
        <SessionBadge session={s} />
        <h3 title={s.title}>{s.title}</h3>
        <span className="hint">
          {s.viewers.length} of {s.maxViewers} watching
          {allowsKbm(s) && " · keyboard and mouse allowed"}
        </span>
        <button className="danger" disabled={busy != null} onClick={() => run("end", { type: "endSession" })}>
          {busy === "end" ? "Ending…" : "End session"}
        </button>
      </div>

      {pad && (
        <p className="banner" role="status">
          Your {pad} is hidden from games for the whole session, so start the game now: it never sees the controller,
          only the seats. A game that was already running keeps it until you reconnect it once (turn it off and on, or
          unplug it and plug it back into the same port). If a seat gets no player in a Steam game, turn Steam Input off
          for that game.
        </p>
      )}
      {s.hostControllerProblem && (
        <p className="banner warn" role="status">
          Your controller can't play through a seat: {s.hostControllerProblem}. You play on it directly, beside the seats.
        </p>
      )}

      <StreamSection session={s} onRoom={props.onRoom} notify={props.notify} />

      {s.controlPaused && (
        <div className="banner warn offer" role="status">
          <span>
            You took the controller back with the panic key. Nobody else can play until you allow it
            {pad ? "; you still can" : ""}.
          </span>
          <button disabled={busy != null} onClick={() => run("allow", { type: "allowControl" })}>
            Allow control
          </button>
        </div>
      )}

      {s.requests.length > 0 && (
        <div className="host-list requests">
          <h4>Asking to play</h4>
          <ul>
            {s.requests.map(({ member: id, seat }) => (
              <li key={id}>
                <Avatar name={nameOf(room, id)} />
                <span className="member-name">
                  {nameOf(room, id)}
                  <span className="hint"> asks for {seatPhrase(s.seats[seat]?.label ?? "a seat")}</span>
                </span>
                <button className="primary" disabled={busy != null} onClick={() => run(`accept-${id}`, { type: "answerControl", member: id, accept: true })}>
                  Accept
                </button>
                <button disabled={busy != null} onClick={() => run(`decline-${id}`, { type: "answerControl", member: id, accept: false })}>
                  Decline
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}

      {(!single || pad) && (
        <div className="host-list">
          <h4>Seats — {s.seats.length}</h4>
          <ul>
            <li className="own-devices">
              <span className="seat-kind" aria-hidden="true">
                {pad ? "⌨" : "🎮"}
              </span>
              <span className="member-name">
                {pad ? "Your keyboard and mouse" : "Your own controller, keyboard and mouse"}
                <span className="hint"> · you, beside the seats</span>
              </span>
            </li>
            {s.seats.map((seat, i) => (
              <li key={i}>
                <span className="seat-kind" aria-hidden="true">
                  {seat.pad ? "🎮" : "⌨"}
                </span>
                <span className="member-name">
                  {seat.label}
                  <span className="hint"> · {holderName(seat)}</span>
                </span>
                {seat.holder && (
                  <button
                    disabled={busy != null}
                    title={seat.host ? `Your ${pad ?? "controller"} stops playing it; it stays empty` : "It stays empty"}
                    onClick={() => run(`take-${i}`, { type: "takeBackControl", seat: i })}
                  >
                    {seat.host ? "Leave" : "Take back"}
                  </button>
                )}
                {pad && seat.pad && !seat.host && (
                  <button
                    disabled={busy != null}
                    title={seat.holder ? `${nameOf(room, seat.holder)} loses it` : undefined}
                    onClick={() => run(`play-${i}`, { type: "playSeat", seat: i })}
                  >
                    Play here
                  </button>
                )}
              </li>
            ))}
          </ul>
          <p className="hint">
            {pad
              ? "Every player in the game is one of these seats, yours included, so any of them can be handed over. A seat you take back or leave stays empty; Play here puts your controller on one. The game numbers its players itself, usually in the order controllers join."
              : "The game numbers its players itself, usually in the order controllers join, and your own controller counts: it often takes player 1. A seat whose controller the game gave no player does nothing; if a guest's seat doesn't respond, move them to another one."}
          </p>
        </div>
      )}

      <div className="host-list">
        <h4>Watching — {s.viewers.length}</h4>
        {s.viewers.length === 0 ? (
          <p className="hint">Nobody yet. The others in the room see your session and a Watch button.</p>
        ) : (
          <ul>
            {s.viewers.map((id) => {
              const seat = seatOf(id);
              const blocked = busy != null || s.controlPaused;
              const why = s.controlPaused ? "Allow control first" : undefined;
              return (
                <li key={id}>
                  <Avatar name={nameOf(room, id)} />
                  <span className="member-name">
                    {nameOf(room, id)}
                    {seat >= 0 && (
                      <span className="playing" title={`Playing: ${s.seats[seat].label}`} aria-label="playing">
                        {" "}
                        🎮{!single && <span className="hint"> {s.seats[seat].label}</span>}
                      </span>
                    )}
                  </span>
                  {single ? (
                    seat >= 0 ? (
                      <button disabled={busy != null} onClick={() => run("take", { type: "takeBackControl", seat: 0 })}>
                        Take back
                      </button>
                    ) : (
                      <button disabled={blocked} title={why} onClick={() => pass(id, 0)}>
                        Pass control
                      </button>
                    )
                  ) : (
                    <select
                      className="pass-seat"
                      aria-label={`Give ${nameOf(room, id)} a seat`}
                      value=""
                      disabled={blocked}
                      title={why}
                      onChange={(e) => pass(id, Number(e.target.value))}
                    >
                      <option value="" disabled>
                        {seat >= 0 ? "Move to…" : "Pass a seat…"}
                      </option>
                      {s.seats.map((other, i) =>
                        i === seat ? null : (
                          <option key={i} value={i}>
                            {other.label}
                            {other.host ? " (yours)" : other.holder ? ` (${nameOf(room, other.holder)}'s)` : ""}
                          </option>
                        ),
                      )}
                    </select>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>

      <p className="hint">
        {pad
          ? ourSeat >= 0
            ? `You play ${single ? "the seat" : s.seats[ourSeat].label} with your ${pad}; your keyboard and mouse work beside the seats.`
            : `Your ${pad} plays no seat now: use Play here to take one. Your keyboard and mouse still work.`
          : single
            ? s.seats[0].holder
              ? `${nameOf(room, s.seats[0].holder)} is playing; your own controller, keyboard and mouse still work too.`
              : "You're playing."
            : "You play on your own controller, keyboard and mouse, beside the seats."}{" "}
        {s.panicKey ? (
          <>
            <kbd>{s.panicKey}</kbd> takes the controller back from anyone at once, even from inside the game.
          </>
        ) : (
          <span className="warn-text">The panic key couldn't be set up this time: use Take back.</span>
        )}
      </p>
    </section>
  );
}

// ---- The stream in our session ----

const QUALITIES: { mbps: number; label: string }[] = [
  { mbps: 5, label: "5 Mbit/s: slow connections" },
  { mbps: 10, label: "10 Mbit/s" },
  { mbps: 20, label: "20 Mbit/s: sharp at 1080p (recommended)" },
  { mbps: 30, label: "30 Mbit/s" },
  { mbps: 50, label: "50 Mbit/s: fast networks only" },
];
const FRAME_RATES = [120, 60, 30];

type Picked = { kind: "window" | "screen"; id: string; title: string };

/** Where the app remembers choices (this profile's own web storage). The
 * controller's key keeps its name from before M19, so the choice stays. */
const MY_PAD_KEY = "couch.goLive.controller";
const SETUP_KEY = "couch.session.setup";
const TITLE_KEY = "couch.session.title";
const FPS_KEY = "couch.stream.fps";
const QUALITY_KEY = "couch.stream.quality";

function padKey(c: HostController): string {
  return `${c.vendor}:${c.product}`;
}

function readStored(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeStored(key: string, value: string) {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    // A convenience only.
  }
}

/** A remembered number, if it is one of `allowed`. */
function storedNumber(key: string, allowed: number[], fallback: number): number {
  const n = Number(readStored(key));
  return allowed.includes(n) ? n : fallback;
}

/** The setup last used, or one controller. */
function storedSetup(): InputSetup {
  const fallback: InputSetup = { controllers: 1, keyboardMouse: "none" };
  try {
    const s = JSON.parse(readStored(SETUP_KEY) ?? "null");
    const kbm: KeyboardMouseSeat[] = ["none", "ownSeat", "withController1"];
    if (Number.isInteger(s?.controllers) && s.controllers >= 0 && s.controllers <= 4 && kbm.includes(s.keyboardMouse)) {
      return { controllers: s.controllers, keyboardMouse: s.keyboardMouse };
    }
  } catch {
    // Not one: the default.
  }
  return fallback;
}

/** What the host panel says while nothing is streamed. */
function hostIdleText(stream: StreamStateView): string {
  switch (stream.state) {
    case "stopped":
      switch (stream.reason) {
        case "gameExited":
          return "The game closed, so the stream stopped.";
        case "sourceLost":
          return "The window or screen you were streaming went away, so the stream stopped.";
        case "error":
          return "The stream stopped after an error.";
        default:
          return "Nothing is streamed now.";
      }
    default:
      return "Nothing is streamed yet. Start the game, then pick its window below.";
  }
}

/** The host panel's Stream section (M19): what to stream and how; start,
 * stop, and start again with other choices, the session going on. */
function StreamSection(props: {
  session: SessionView;
  onRoom: (room: RoomView) => void;
  notify: (text: string, tone?: "info" | "warn") => void;
}) {
  const s = props.session;
  const { busy, run } = useRun(props.notify, props.onRoom);
  const [sources, setSources] = useState<SourceList | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [picked, setPicked] = useState<Picked | null>(null);
  const [fps, setFps] = useState(() => storedNumber(FPS_KEY, FRAME_RATES, 60));
  const [quality, setQuality] = useState(() =>
    storedNumber(
      QUALITY_KEY,
      QUALITIES.map((q) => q.mbps),
      20,
    ),
  );
  const [limitUpload, setLimitUpload] = useState(false);
  const [upload, setUpload] = useState("50");
  const [error, setError] = useState<string | null>(null);

  const load = () => {
    setLoadError(null);
    request({ type: "listSources" }, "sources")
      .then(setSources)
      .catch((e) => setLoadError(errorText(e)));
  };
  useEffect(load, []);

  const state = s.stream.state;
  const streaming = s.streaming;
  const kbmSeats = allowsKbm(s);
  // Whole Mbit/s only: "50abc" or "1e9" isn't a limit.
  const uploadMbps = limitUpload ? (/^\s*\d{1,5}\s*$/.test(upload) ? Number(upload) : Number.NaN) : null;
  const isPicked = (kind: Picked["kind"], id: string) => picked?.kind === kind && picked.id === id;
  const pickedElevated = sources?.windows.some((w) => w.elevated && isPicked("window", w.id)) ?? false;
  // The screen's refresh rate caps the frames a capture gets. Windows
  // reports whole hertz, 59 for 59.94, hence the 1 Hz of room.
  const pickedRefresh =
    (picked?.kind === "window"
      ? sources?.windows.find((w) => isPicked("window", w.id))?.refreshHz
      : sources?.screens.find((sc) => isPicked("screen", sc.id))?.refreshHz) ?? null;
  const refreshTooLow = pickedRefresh != null && fps > pickedRefresh + 1;
  const screenWithKbm = kbmSeats && picked?.kind === "screen";

  async function start(e: FormEvent) {
    e.preventDefault();
    const problem =
      (picked == null ? "Pick a window or a screen." : null) ??
      (screenWithKbm ? "This session has a keyboard-and-mouse seat: stream a window, not a whole screen." : null) ??
      (uploadMbps != null && !(uploadMbps >= 2 && uploadMbps <= 10000)
        ? "The upload limit is a whole number of Mbit/s, from 2 to 10000."
        : null);
    if (problem || picked == null) {
      setError(problem);
      return;
    }
    setError(null);
    writeStored(FPS_KEY, String(fps));
    writeStored(QUALITY_KEY, String(quality));
    await run("stream", {
      type: "startStream",
      source: picked.kind === "window" ? { kind: "window", id: picked.id } : { kind: "screen", id: picked.id },
      name: picked.title,
      fps,
      max_mbps: quality,
      upload_mbps: uploadMbps,
    });
  }

  return (
    <form className="form stream-section" onSubmit={start} aria-label="Stream">
      <h4>Stream</h4>
      {state === "live" && streaming ? (
        <div className="banner offer" role="status">
          <span>
            Streaming <strong>{streaming.name}</strong> at {streaming.fps} fps, up to {streaming.maxMbps} Mbit/s for each viewer.
          </span>
          <button type="button" className="danger" disabled={busy != null} onClick={() => run("stop-stream", { type: "stopStream" })}>
            {busy === "stop-stream" ? "Stopping…" : "Stop streaming"}
          </button>
        </div>
      ) : state === "starting" ? (
        <p className="hint" role="status">
          Starting the stream…
        </p>
      ) : (
        <p className="hint" role="status">
          {hostIdleText(s.stream)} Viewers stay, and keep their seats; their input waits for the stream, since they can't
          see.
        </p>
      )}

      <fieldset className="sources">
        <legend>
          {state === "live" ? "Stream something else" : "What to stream"}
          <button type="button" className="icon" title="Refresh the list" aria-label="Refresh the list" onClick={load}>
            ⟳
          </button>
        </legend>
        {loadError && <p className="error">{loadError}</p>}
        {sources == null && !loadError && <p className="hint">Looking for windows…</p>}
        {sources && (
          <div className="source-list" role="radiogroup">
            {sources.windows.map((w) => (
              <label key={`w${w.id}`} className={`source${isPicked("window", w.id) ? " picked" : ""}`}>
                <input
                  type="radio"
                  name="source"
                  checked={isPicked("window", w.id)}
                  onChange={() => setPicked({ kind: "window", id: w.id, title: w.title })}
                />
                <span className="source-title" title={w.title}>
                  {w.title}
                </span>
                {w.app && <span className="hint">{w.app}</span>}
                {w.elevated && (
                  <span className="hint warn-text" title="Runs as administrator">
                    admin
                  </span>
                )}
              </label>
            ))}
            {sources.screens.map((sc, i) => (
              <label key={`s${sc.id}`} className={`source${isPicked("screen", sc.id) ? " picked" : ""}`}>
                <input
                  type="radio"
                  name="source"
                  checked={isPicked("screen", sc.id)}
                  onChange={() => setPicked({ kind: "screen", id: sc.id, title: `Screen ${i + 1}` })}
                />
                <span className="source-title">
                  Whole screen {i + 1}
                  {sc.primary && " (main)"}
                </span>
                <span className="hint">
                  {sc.width}×{sc.height}
                  {sc.refreshHz != null && `, ${sc.refreshHz} Hz`}
                </span>
              </label>
            ))}
            {sources.windows.length === 0 && sources.screens.length === 0 && <p className="hint">No windows found. Start the game, then refresh.</p>}
          </div>
        )}
        <p className="hint">
          A window is best: only the game is shown, and only its sound is sent
          {kbmSeats ? "; keyboard and mouse need one" : ""}.
        </p>
        {pickedElevated && (
          <p className="banner warn" role="status">
            This program runs as administrator. Guests' keyboard and mouse can't reach it, and the panic key doesn't work
            while it is in front: switch to Couch Co-App and use Take back instead. Their gamepad still works.
          </p>
        )}
        {screenWithKbm && (
          <p className="banner warn" role="status">
            This session has a keyboard-and-mouse seat, which needs a window: pick the game's window, not a whole screen.
          </p>
        )}
      </fieldset>

      <div className="form-row">
        <label>
          Frame rate
          <select value={fps} onChange={(e) => setFps(Number(e.target.value))}>
            {FRAME_RATES.map((f) => (
              <option key={f} value={f}>
                {f} fps
              </option>
            ))}
          </select>
        </label>
        <label>
          Quality for each viewer
          <select value={quality} onChange={(e) => setQuality(Number(e.target.value))}>
            {QUALITIES.map((q) => (
              <option key={q.mbps} value={q.mbps}>
                {q.label}
              </option>
            ))}
          </select>
        </label>
      </div>
      {refreshTooLow && (
        <p className="banner warn" role="status">
          Your screen runs at {pickedRefresh} Hz, so the stream can't go above {pickedRefresh} fps. Pick a lower frame rate,
          or raise the screen's refresh rate in Windows' display settings.
        </p>
      )}
      <p className="hint">
        {fps === 120 && !refreshTooLow
          ? "120 fps doubles each viewer's encoding on your graphics card, and the same bitrate is spread over twice the frames. "
          : ""}
        Each viewer gets an encoder of their own on your graphics card and up to {quality} Mbit/s of your upload: up to{" "}
        {quality * s.maxViewers} Mbit/s with {s.maxViewers} watching. A viewer on a slow connection gets less, without slowing
        the others. Guests hear the game{picked?.kind === "screen" ? " (for a screen: everything your PC plays, except Couch Co-App)" : ""}.
      </p>
      <label className="check">
        <input type="checkbox" checked={limitUpload} onChange={(e) => setLimitUpload(e.target.checked)} />
        Limit my total upload
      </label>
      {limitUpload && (
        <label>
          Upload limit, Mbit/s (shared between viewers)
          <input inputMode="numeric" value={upload} onChange={(e) => setUpload(e.target.value)} />
        </label>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <div className="card-actions">
        <button type="submit" className="primary" disabled={busy != null || picked == null || state === "starting"}>
          {busy === "stream" ? "Starting…" : state === "live" ? "Stream this instead" : "Start streaming"}
        </button>
      </div>
    </form>
  );
}

// ---- Starting a session ----

export function StartSessionDialog(props: { room: RoomView; onClose: () => void; onStarted: (room: RoomView) => void }) {
  const [sources, setSources] = useState<SourceList | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [title, setTitle] = useState(() => truncateBytes(readStored(TITLE_KEY) ?? ""));
  const [viewers, setViewers] = useState(4);
  const [controllers, setControllers] = useState(() => storedSetup().controllers);
  const [kbm, setKbm] = useState<KeyboardMouseSeat>(() => storedSetup().keyboardMouse);
  // Our controller to play through a seat, as `padKey`; "" plays outside.
  const [myPad, setMyPad] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = () => {
    setLoadError(null);
    request({ type: "listSources" }, "sources")
      .then((list) => {
        setSources(list);
        // The last choice comes back if that controller is here; otherwise
        // the first one, if it can be hidden.
        const last = readStored(MY_PAD_KEY);
        const keys = list.hostControllers.map(padKey);
        const ready = list.hiding === "ready";
        setMyPad(!ready ? "" : last === "" ? "" : last != null && keys.includes(last) ? last : (keys[0] ?? ""));
      })
      .catch((e) => setLoadError(errorText(e)));
  };
  useEffect(load, []);

  // Keyboard and mouse "with Controller 1" needs a Controller 1.
  const kbmChoice: KeyboardMouseSeat = controllers === 0 && kbm === "withController1" ? "ownSeat" : kbm;
  const input: InputSetup = { controllers, keyboardMouse: kbmChoice };
  const noSeats = controllers === 0 && kbmChoice === "none";
  // Our controller needs a seat with a pad.
  const padSeat = controllers > 0;
  const hostControllers = sources?.hostControllers ?? [];
  const controller = padSeat ? (hostControllers.find((c) => padKey(c) === myPad) ?? null) : null;
  const others = props.room.members.length - 1;

  async function submit(e: FormEvent) {
    e.preventDefault();
    // A click: the webview lets the request chime play from now on.
    prepareChime();
    const problem =
      (title.trim() === "" ? "Give the session a title." : null) ??
      (new TextEncoder().encode(title.trim()).length > MAX_NAME_BYTES ? "That title is too long." : null) ??
      (noSeats ? "Give the guests at least one seat: a controller, or keyboard and mouse." : null);
    if (problem) {
      setError(problem);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const room = await request(
        {
          type: "startSession",
          room: props.room.id,
          title: title.trim(),
          max_viewers: viewers,
          input,
          controller,
        },
        "room",
      );
      writeStored(SETUP_KEY, JSON.stringify(input));
      writeStored(TITLE_KEY, title.trim());
      // Only a real choice is remembered: with HidHide missing or busy the
      // select was forced to "none".
      if (hostControllers.length > 0 && sources?.hiding === "ready") writeStored(MY_PAD_KEY, myPad);
      props.onStarted(room);
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  }

  return (
    <Modal
      title={`Start a session in ${props.room.name}`}
      onClose={props.onClose}
      busy={busy}
      footer={
        <>
          <button type="button" onClick={props.onClose} disabled={busy}>
            Cancel
          </button>
          <button type="submit" form="start-session" className="primary" disabled={busy}>
            {busy ? "Starting…" : "Start the session"}
          </button>
        </>
      }
    >
      <form id="start-session" onSubmit={submit} className="form">
        <p className="hint">
          The seats are set up first, and your controller hidden from games, before any game starts. Then you stream a
          window or a screen from the session's panel, and stop or switch whenever you like.
        </p>
        <label>
          Title
          <input
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder="What the room sees it called, such as Game night"
          />
        </label>
        <label>
          How many can watch
          <select value={viewers} onChange={(e) => setViewers(Number(e.target.value))}>
            {[1, 2, 3, 4, 5, 6, 7].map((n) => (
              <option key={n} value={n}>
                {n}
                {n === 4 ? " (recommended)" : ""}
              </option>
            ))}
          </select>
        </label>
        {viewers > others && (
          <p className="hint">
            {others === 0 ? "Nobody else is" : others === 1 ? "1 other member is" : `${others} other members are`} in the room now.
          </p>
        )}

        <fieldset className="input-setup">
          <legend>Seats: how the game takes input</legend>
          <label>
            Controllers: one per player, each its own virtual pad
            <select
              value={controllers}
              onChange={(e) => {
                const n = Number(e.target.value);
                setControllers(n);
                if (n === 0 && kbm === "withController1") setKbm("ownSeat");
              }}
            >
              {[0, 1, 2, 3, 4].map((n) => (
                <option key={n} value={n}>
                  {n === 0 ? "None" : n}
                </option>
              ))}
            </select>
          </label>
          <label>
            Keyboard and mouse
            <select value={kbmChoice} onChange={(e) => setKbm(e.target.value as KeyboardMouseSeat)}>
              <option value="none">Not for guests</option>
              <option value="ownSeat">A seat of their own</option>
              <option value="withController1" disabled={controllers === 0}>
                With Controller 1: one player on both
              </option>
            </select>
          </label>
          {noSeats && (
            <p className="error" role="alert">
              Give the guests at least one seat: a controller, or keyboard and mouse.
            </p>
          )}
          {(sources?.controllers ?? 0) > 0 && controller == null && padSeat && (
            <p className="banner warn" role="status">
              A controller is plugged into this PC. Games may give it a player's place, and then a guest's controller may not
              work. Play on a seat with it (below), or unplug it.
            </p>
          )}
          {loadError && <p className="error">{loadError}</p>}
          {sources && hostControllers.length > 0 && padSeat && (
            <>
              <label>
                Your controller
                <select value={myPad} disabled={sources.hiding !== "ready"} onChange={(e) => setMyPad(e.target.value)}>
                  <option value="">Not on a seat: the game sees it directly</option>
                  {hostControllers.map((c) => (
                    <option key={padKey(c)} value={padKey(c)}>
                      {c.name}: you start on Controller 1
                    </option>
                  ))}
                </select>
              </label>
              <p className="hint">
                {sources.hiding === "notInstalled"
                  ? "To play on a seat with your controller, install HidHide: Settings → Drivers (it needs administrator rights, then a restart)."
                  : sources.hiding === "busy"
                    ? "HidHide is busy right now. If its configuration window is open, close it, then start the session again."
                    : controller
                      ? "It is hidden from games for the whole session and plays its seat through Couch Co-App, so every seat, yours included, can be handed over. Start the game after the session: a game already running keeps the controller until you reconnect it once. For a Steam game, turn Steam Input off for it (the game's Properties → Controller): otherwise Steam may keep a player for your hidden controller, and one seat gets none."
                      : "You play outside the seats, as the game sees your controller."}
              </p>
            </>
          )}
          {sources && !sources.virtualPads && padSeat && (
            <p className="banner warn" role="status">
              Guests' controllers won't reach the game: ViGEmBus, which makes the virtual controllers, isn't installed.
              Install it in Settings → Drivers. Watching, and a keyboard-and-mouse seat, still work.
            </p>
          )}
          <p className="hint">
            The seats stay for the whole session: to change them, end it and start another. Guests ask for a seat and you
            decide.
          </p>
        </fieldset>
        {kbmChoice !== "none" ? (
          <p className="banner warn">
            Keyboard and mouse are control of your PC. Whoever holds that seat types only into the game, and only while
            its window is in front, so you'll stream a window, not a whole screen; the Windows key, Alt+Tab, Alt+F4,
            Ctrl+Esc and Ctrl+Shift+Esc never go through. Give that seat only to people you trust.
          </p>
        ) : (
          <p className="hint">Guests play with their gamepad only.</p>
        )}
        <p className="hint">Windows' volume mixer sets how loud the game is, for your guests as for you.</p>
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
      </form>
    </Modal>
  );
}
