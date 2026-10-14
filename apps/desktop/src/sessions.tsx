// Sessions on screen: the stage with a card per live session, the host's
// panel for its own stream, the Go live dialog and the shortcuts list.
// The engine decides everything; these only show its state and send commands.
import { useEffect, useState, type FormEvent } from "react";
import type { Command } from "./bindings/Command";
import type { HostController } from "./bindings/HostController";
import type { InputSetup } from "./bindings/InputSetup";
import type { RoomView } from "./bindings/RoomView";
import type { SeatChoice } from "./bindings/SeatChoice";
import type { SessionView } from "./bindings/SessionView";
import type { SourceList } from "./bindings/SourceList";
import { prepareChime } from "./chime";
import { engineCommand, errorText, request } from "./engine";
import { Avatar, MAX_NAME_BYTES, Modal, truncateBytes } from "./ui";

/** What this profile is doing in any room: one stream at a time, and not
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
  onGoLive: () => void;
  onInvite: () => void;
  onRoom: (room: RoomView) => void;
  notify: (text: string, tone?: "info" | "warn") => void;
};

export function Stage(props: StageProps) {
  const { room, busy } = props;
  const mine = room.sessions.find((s) => s.role === "host");
  const others = room.sessions.filter((s) => s.role !== "host");
  const goLiveBlocked = busy.hosting
    ? "You're already streaming, in another room."
    : busy.watching
      ? "Stop watching first."
      : null;

  if (mine == null && others.length === 0) {
    const alone = room.members.length === 1;
    return (
      <main className="stage">
        <div className="stage-empty">
          <h2>Nobody is live yet</h2>
          <p className="hint">
            Go live to stream a game window or a screen to the room. The others watch, and you
            hand them the controller.
          </p>
          <div className="stage-empty-actions">
            <button className="primary" onClick={props.onGoLive} disabled={goLiveBlocked != null} title={goLiveBlocked ?? undefined}>
              Go live
            </button>
            {room.isOwner && alone && <button onClick={props.onInvite}>Invite people</button>}
          </div>
          {goLiveBlocked && <p className="hint">{goLiveBlocked}</p>}
        </div>
      </main>
    );
  }

  return (
    <main className="stage live">
      <header className="stage-header">
        <h2>Live in {room.name}</h2>
        {mine == null && (
          <button className="primary" onClick={props.onGoLive} disabled={goLiveBlocked != null} title={goLiveBlocked ?? undefined}>
            Go live
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
    props.busy.hosting ? "Stop streaming first." : props.busy.watching ? "You're watching another stream." : full ? "Full: the host chose how many can watch." : null;

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

  return (
    <article className={`stage-card${s.role !== "none" ? " mine" : ""}`}>
      <div className="card-head">
        <Avatar name={host} size="large" />
        <div className="card-title">
          <span className="live-badge">LIVE</span>
          <h3 title={s.title}>{s.title}</h3>
          <span className="hint">{host}'s stream</span>
        </div>
      </div>
      <p>
        <span className="playing" aria-hidden="true">
          🎮
        </span>{" "}
        {whoPlays(room, s)} · {s.viewers.length} of {s.maxViewers} watching
        {allowsKbm(s) && " · keyboard and mouse allowed"}
      </p>
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
    <section className="host-panel" aria-label="Your stream">
      <div className="host-head">
        <span className="live-badge">LIVE</span>
        <h3 title={s.title}>{s.title}</h3>
        <span className="hint">
          {s.viewers.length} of {s.maxViewers} watching
          {allowsKbm(s) && " · keyboard and mouse allowed"}
        </span>
        <button className="danger" disabled={busy != null} onClick={() => run("stop", { type: "stopLive" })}>
          {busy === "stop" ? "Stopping…" : "Stop streaming"}
        </button>
      </div>

      {pad && (
        <p className="banner" role="status">
          Your {pad} is hidden from games while you're live. If the game still reacts to it directly, reconnect it once
          (turn it off and on, or unplug it and plug it back into the same port) so games and Steam let go of it. If a
          seat gets no player in a Steam game, turn Steam Input off for that game.
        </p>
      )}
      {s.hostControllerProblem && (
        <p className="banner warn" role="status">
          Your controller can't play through a seat: {s.hostControllerProblem}. You play on it directly, beside the seats.
        </p>
      )}

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
          <p className="hint">Nobody yet. The others in the room see your stream and a Watch button.</p>
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

// ---- Go live ----

const QUALITIES: { mbps: number; label: string }[] = [
  { mbps: 5, label: "5 Mbit/s: slow connections" },
  { mbps: 10, label: "10 Mbit/s" },
  { mbps: 20, label: "20 Mbit/s: sharp at 1080p (recommended)" },
  { mbps: 30, label: "30 Mbit/s" },
  { mbps: 50, label: "50 Mbit/s: fast networks only" },
];

type Picked = { kind: "window" | "screen"; id: string; title: string };

/** Where Go live remembers our controller choice (this profile's own web
 * storage): a `padKey`, or "" for none. */
const MY_PAD_KEY = "couch.goLive.controller";

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

export function GoLiveDialog(props: { room: RoomView; onClose: () => void; onLive: (room: RoomView) => void }) {
  const [sources, setSources] = useState<SourceList | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [picked, setPicked] = useState<Picked | null>(null);
  const [title, setTitle] = useState("");
  const [titleEdited, setTitleEdited] = useState(false);
  const [fps, setFps] = useState(60);
  const [quality, setQuality] = useState(20);
  const [limitUpload, setLimitUpload] = useState(false);
  const [upload, setUpload] = useState("50");
  const [viewers, setViewers] = useState(4);
  const [mode, setMode] = useState<"single" | "multi">("single");
  const [seatChoice, setSeatChoice] = useState<SeatChoice>("controller");
  const [controllers, setControllers] = useState(2);
  const [kbmSeat, setKbmSeat] = useState(false);
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

  const pick = (p: Picked, remembered?: InputSetup | null) => {
    setPicked(p);
    if (!titleEdited) setTitle(truncateBytes(p.title.trim()));
    // The setup last used with this program comes back; otherwise the
    // default (one controller), never the last program's choice, which may
    // have handed out keyboard and mouse.
    const setup: InputSetup = remembered ?? { mode: "single", seat: "controller" };
    if (setup.mode === "single") {
      setMode("single");
      setSeatChoice(setup.seat);
    } else {
      setMode("multi");
      setControllers(setup.controllers);
      setKbmSeat(setup.keyboardMouse);
    }
    // Keyboard and mouse need a window.
    if (p.kind === "screen") {
      setSeatChoice("controller");
      setKbmSeat(false);
    }
  };

  const input: InputSetup =
    mode === "single" ? { mode: "single", seat: seatChoice } : { mode: "multi", controllers, keyboardMouse: kbmSeat };
  const kbm = mode === "single" ? seatChoice !== "controller" : kbmSeat;
  const isWindow = picked?.kind !== "screen";
  // Our controller needs a seat with a pad.
  const padSeat = mode === "multi" || seatChoice !== "keyboardMouse";
  const hostControllers = sources?.hostControllers ?? [];
  const controller = padSeat ? (hostControllers.find((c) => padKey(c) === myPad) ?? null) : null;

  // Whole Mbit/s only: "50abc" or "1e9" isn't a limit.
  const uploadMbps = limitUpload ? (/^\s*\d{1,5}\s*$/.test(upload) ? Number(upload) : Number.NaN) : null;
  const others = props.room.members.length - 1;

  async function submit(e: FormEvent) {
    e.preventDefault();
    // A click: the webview lets the request chime play from now on.
    prepareChime();
    const problem =
      (picked == null ? "Pick a window or a screen." : null) ??
      (title.trim() === "" ? "Give the stream a title." : null) ??
      (new TextEncoder().encode(title.trim()).length > MAX_NAME_BYTES ? "That title is too long." : null) ??
      (uploadMbps != null && !(uploadMbps >= 2 && uploadMbps <= 10000)
        ? "The upload limit is a whole number of Mbit/s, from 2 to 10000."
        : null);
    if (problem || picked == null) {
      setError(problem);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const room = await request(
        {
          type: "goLive",
          room: props.room.id,
          source: picked.kind === "window" ? { kind: "window", id: picked.id } : { kind: "screen", id: picked.id },
          title: title.trim(),
          fps,
          max_mbps: quality,
          upload_mbps: uploadMbps,
          max_viewers: viewers,
          input,
          controller,
        },
        "room",
      );
      // Only a real choice is remembered: with HidHide missing or busy the
      // select was forced to "none".
      if (hostControllers.length > 0 && sources?.hiding === "ready") writeStored(MY_PAD_KEY, myPad);
      props.onLive(room);
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  }

  const isPicked = (kind: Picked["kind"], id: string) => picked?.kind === kind && picked.id === id;
  const pickedElevated = sources?.windows.some((w) => w.elevated && isPicked("window", w.id)) ?? false;
  // The screen's refresh rate caps the frames a capture gets. Windows
  // reports whole hertz, 59 for 59.94, hence the 1 Hz of room.
  const pickedRefresh =
    (picked?.kind === "window"
      ? sources?.windows.find((w) => isPicked("window", w.id))?.refreshHz
      : sources?.screens.find((s) => isPicked("screen", s.id))?.refreshHz) ?? null;
  const refreshTooLow = pickedRefresh != null && fps > pickedRefresh + 1;

  return (
    <Modal
      title={`Go live in ${props.room.name}`}
      onClose={props.onClose}
      busy={busy}
      footer={
        <>
          <button type="button" onClick={props.onClose} disabled={busy}>
            Cancel
          </button>
          <button type="submit" form="go-live" className="primary" disabled={busy || picked == null}>
            {busy ? "Going live…" : "Go live"}
          </button>
        </>
      }
    >
      <form id="go-live" onSubmit={submit} className="form">
        <fieldset className="sources">
          <legend>
            What to stream
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
                    onChange={() => pick({ kind: "window", id: w.id, title: w.title }, w.inputSetup)}
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
              {sources.screens.map((s, i) => (
                <label key={`s${s.id}`} className={`source${isPicked("screen", s.id) ? " picked" : ""}`}>
                  <input
                    type="radio"
                    name="source"
                    checked={isPicked("screen", s.id)}
                    onChange={() => pick({ kind: "screen", id: s.id, title: `Screen ${i + 1}` })}
                  />
                  <span className="source-title">
                    Whole screen {i + 1}
                    {s.primary && " (main)"}
                  </span>
                  <span className="hint">
                    {s.width}×{s.height}
                    {s.refreshHz != null && `, ${s.refreshHz} Hz`}
                  </span>
                </label>
              ))}
              {sources.windows.length === 0 && sources.screens.length === 0 && <p className="hint">No windows found. Start the game, then refresh.</p>}
            </div>
          )}
          <p className="hint">A window is best: only the game is shown, and only its sound is sent.</p>
          {pickedElevated && (
            <p className="banner warn" role="status">
              This program runs as administrator. Guests' keyboard and mouse can't reach it, and the panic key doesn't
              work while it is in front: switch to Couch Co-App and use Take back instead. Their gamepad still works.
            </p>
          )}
        </fieldset>

        <label>
          Title
          <input
            value={title}
            onChange={(e) => {
              setTitle(e.target.value);
              setTitleEdited(true);
            }}
            placeholder="What the room sees it called"
          />
        </label>

        <div className="form-row">
          <label>
            Frame rate
            <select value={fps} onChange={(e) => setFps(Number(e.target.value))}>
              <option value={120}>120 fps</option>
              <option value={60}>60 fps</option>
              <option value={30}>30 fps</option>
            </select>
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
        </div>
        {refreshTooLow && (
          <p className="banner warn" role="status">
            Your screen runs at {pickedRefresh} Hz, so the stream can't go above {pickedRefresh} fps. Pick a lower frame rate, or
            raise the screen's refresh rate in Windows' display settings.
          </p>
        )}
        {fps === 120 && !refreshTooLow && (
          <p className="hint">
            120 fps doubles each viewer's encoding on your graphics card, and the same bitrate is spread over twice the frames.
          </p>
        )}

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
        <p className="hint">
          Each viewer gets an encoder of their own on your graphics card and up to {quality} Mbit/s of your upload: up to{" "}
          {quality * viewers} Mbit/s with {viewers} watching. A viewer on a slow connection gets less, without slowing the others.
          {viewers > others && ` (${others === 0 ? "Nobody else is" : others === 1 ? "1 other member is" : `${others} other members are`} in the room now.)`}
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

        <fieldset className="input-setup">
          <legend>How the game takes input</legend>
          <div className="segmented" role="radiogroup">
            <label className={mode === "single" ? "picked" : ""}>
              <input type="radio" name="mode" checked={mode === "single"} onChange={() => setMode("single")} />
              Single player
            </label>
            <label className={mode === "multi" ? "picked" : ""}>
              <input type="radio" name="mode" checked={mode === "multi"} onChange={() => setMode("multi")} />
              Multiplayer
            </label>
          </div>
          {mode === "single" ? (
            <label>
              One player at a time, with
              <select value={seatChoice} onChange={(e) => setSeatChoice(e.target.value as SeatChoice)}>
                <option value="controller">A controller</option>
                <option value="keyboardMouse" disabled={!isWindow}>
                  Keyboard and mouse
                </option>
                <option value="both" disabled={!isWindow}>
                  A controller, keyboard and mouse
                </option>
              </select>
            </label>
          ) : (
            <>
              <label>
                Controllers: one per player, each its own virtual pad
                <select value={controllers} onChange={(e) => setControllers(Number(e.target.value))}>
                  {[1, 2, 3, 4].map((n) => (
                    <option key={n} value={n}>
                      {n}
                    </option>
                  ))}
                </select>
              </label>
              <label className={`check${isWindow ? "" : " disabled"}`}>
                <input type="checkbox" checked={kbmSeat} disabled={!isWindow} onChange={(e) => setKbmSeat(e.target.checked)} />
                Plus a seat with keyboard and mouse
              </label>
              {(sources?.controllers ?? 0) > 0 && controller == null && (
                <p className="banner warn" role="status">
                  A controller is plugged into this PC. Games may give it a player's place, and then a guest's controller
                  may not work. Play on a seat with it (below), or unplug it.
                </p>
              )}
            </>
          )}
          {sources && hostControllers.length > 0 && padSeat && (
            <>
              <label>
                Your controller
                <select value={myPad} disabled={sources.hiding !== "ready"} onChange={(e) => setMyPad(e.target.value)}>
                  <option value="">Not on a seat: the game sees it directly</option>
                  {hostControllers.map((c) => (
                    <option key={padKey(c)} value={padKey(c)}>
                      {c.name}: you start on {mode === "multi" ? "Controller 1" : "the seat"}
                    </option>
                  ))}
                </select>
              </label>
              <p className="hint">
                {sources.hiding === "notInstalled"
                  ? "To play on a seat with your controller, install HidHide: Settings → Drivers (it needs administrator rights, then a restart)."
                  : sources.hiding === "busy"
                    ? "HidHide is busy right now. If its configuration window is open, close it; then refresh the list above."
                    : controller
                      ? "It is hidden from games while you're live and plays its seat through Couch Co-App, so every seat, yours included, can be handed over. Reconnect it once after going live, so games and Steam let go of it. For a Steam game, turn Steam Input off for it (the game's Properties → Controller): otherwise Steam may keep a player for your hidden controller, and one seat gets none."
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
            Guests ask for a seat and you decide. The setup is remembered for this program
            {picked?.kind === "window" ? "" : " (for a window)"}.
          </p>
        </fieldset>
        {kbm ? (
          <p className="banner warn">
            Keyboard and mouse are control of your PC. Whoever holds that seat types only into the game, and only while
            its window is in front; the Windows key, Alt+Tab, Alt+F4, Ctrl+Esc and Ctrl+Shift+Esc never go through. Give
            that seat only to people you trust.
          </p>
        ) : (
          <p className="hint">
            {isWindow ? "" : "Keyboard and mouse need a window, not a whole screen. "}
            Guests play with their gamepad only.
          </p>
        )}
        <p className="hint">
          Guests hear the game{picked?.kind === "screen" ? " (for a screen: everything your PC plays, except Couch Co-App)" : ""}.
          Windows' volume mixer sets how loud it is, for them as for you.
        </p>
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
      </form>
    </Modal>
  );
}
