// Our session (we host): what's streamed and its controls; the requests to
// play, in a strip that is always there so a new one never moves anything
// under the pointer; a tile per seat with who plays it; who's watching; the
// sound guests hear. Every action is also on the right-click of its tile or
// row. Answering a request is never bound to a key here: only the low-level
// hook answers with keys (CLAUDE.md), and only a request pending for 1 s.
import {
  Gamepad2,
  Hand,
  Keyboard,
  MonitorPlay,
  MoreHorizontal,
  Play,
  Power,
  ShieldAlert,
  Square,
  TriangleAlert,
  Users,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { RoomView } from "../bindings/RoomView";
import type { SessionView } from "../bindings/SessionView";
import { hostIdleText, isLive, listed, nameOf, seatPhrase, seatsOf } from "../model/sessions";
import type { Confirm } from "../rooms/dialogs";
import { Avatar } from "../ui/Avatar";
import { Button, IconButton } from "../ui/Button";
import { Callout } from "../ui/Callout";
import { Badge, Kbd } from "../ui/Feedback";
import { ContextMenu, DropdownMenu } from "../ui/Menu";
import { Tooltip } from "../ui/Tooltip";
import { Why } from "../ui/Why";
import { controllersOf, ourPads, PAUSED_REASON, seatEntries, viewerEntries } from "./hostMenus";
import { SeatIcon } from "./SessionCard";
import { SoundLevel } from "./SoundLevel";
import { streamsElevated } from "./streamMemory";
import { useRun, type Notify } from "./useRun";
import "./HostConsole.css";

/** A request's Accept waits this long after it shows (as the answer keys
 * do, `rooms::sessions::ANSWER_AFTER`): a click meant for something else
 * never accepts one that just arrived. */
const ANSWER_AFTER_MS = 1000;

export function HostConsole(props: {
  room: RoomView;
  session: SessionView;
  onRoom: (room: RoomView) => void;
  notify: Notify;
  onStream: () => void;
  confirm: (c: Confirm) => void;
}) {
  const { room, session: s } = props;
  const { busy, run } = useRun(props.notify, props.onRoom);
  const live = isLive(s);
  const pads = ourPads(s);
  const starting = s.stream.state === "starting";

  const end = () => {
    if (s.viewers.length === 0) {
      void run("end", { type: "endSession" });
      return;
    }
    props.confirm({
      title: "End the session?",
      body: `${listed(s.viewers.map((v) => nameOf(room, v)))} ${s.viewers.length === 1 ? "is" : "are"} watching. Ending stops the stream for everyone and frees every seat.`,
      action: "End session",
      danger: true,
      run: () => run("end", { type: "endSession" }),
    });
  };

  return (
    <section className="console" aria-label="Your session">
      <header className="console-top">
        <span className={`console-screen${live ? " live" : ""}`} aria-hidden="true">
          <MonitorPlay />
        </span>
        <div className="console-titles">
          <div className="console-title-row">
            {live ? <Badge tone="live">Live</Badge> : <Badge>{starting ? "Starting…" : "Not streaming"}</Badge>}
            <h2 className="console-title" title={s.title}>
              {s.title}
            </h2>
          </div>
          <span className="console-stream">
            {live && s.streaming ? (
              <>
                <strong>{s.streaming.name}</strong> · {s.streaming.fps} fps · up to {s.streaming.maxMbps} Mbit/s each
                {s.streaming.uploadMbps != null && ` · ${s.streaming.uploadMbps} Mbit/s in all`}
              </>
            ) : starting ? (
              "Starting the stream…"
            ) : (
              <>
                {hostIdleText(s.stream)}{" "}
                <Why>Viewers stay, and keep their seats; their input waits for the stream, since they can't see.</Why>
              </>
            )}
          </span>
        </div>
        <div className="console-actions">
          {live ? (
            <>
              <Button icon={<MonitorPlay />} onClick={props.onStream} disabled={busy != null}>
                Stream something else…
              </Button>
              <Button
                variant="danger-ghost"
                icon={<Square />}
                disabled={busy != null}
                labels={["Stop streaming", "Stopping…"]}
                onClick={() => void run("stop-stream", { type: "stopStream" })}
              >
                {busy === "stop-stream" ? "Stopping…" : "Stop streaming"}
              </Button>
            </>
          ) : (
            <Button variant="primary" icon={<Play />} onClick={props.onStream} disabled={busy != null || starting}>
              Start streaming…
            </Button>
          )}
          <DropdownMenu
            items={[{ label: "End session…", icon: <Power />, danger: true, onSelect: end }]}
          >
            <IconButton label="Session options" variant="secondary">
              <MoreHorizontal />
            </IconButton>
          </DropdownMenu>
        </div>
      </header>

      <ConsoleBanners room={room} session={s} busy={busy} run={run} pads={pads} />

      <div className="console-grid">
        <div className="console-col">
          <RequestStrip room={room} session={s} busy={busy} run={run} />
          <section className="panel" aria-label="Seats">
            <header className="panel-head">
              <h3>Seats</h3>
              <span className="panel-count tabular">{s.seats.length}</span>
              <span className="panel-why">
                <Why>
                  {pads.length > 0
                    ? `Every player in the game is one of these seats, yours included, so any of them can be handed over. A seat you take back or leave stays empty; Play here puts ${pads.length === 1 ? "your controller" : "one of your controllers"} on one. The game numbers its players itself, usually in the order controllers join.`
                    : "The game numbers its players itself, usually in the order controllers join, and your own controller counts: it often takes player 1. A seat whose controller the game gave no player does nothing; if a guest's seat doesn't respond, move them to another one."}
                </Why>
              </span>
            </header>
            <SeatBoard room={room} session={s} busy={busy} run={run} />
            <p className="panel-foot">{whatYouPlay(room, s, pads)}</p>
          </section>
        </div>
        <div className="console-col">
          <Watching room={room} session={s} busy={busy} run={run} />
          {s.soundLevel != null && (
            <section className="panel" aria-label="Sound">
              <header className="panel-head">
                <h3>Stream volume</h3>
              </header>
              <SoundLevel roomId={room.id} level={s.soundLevel} onRoom={props.onRoom} notify={props.notify} />
            </section>
          )}
        </div>
      </div>
    </section>
  );
}

/** The console's one-line summary of what we play. */
function whatYouPlay(room: RoomView, s: SessionView, pads: string[]): string {
  const single = s.seats.length === 1;
  const ours = s.seats.flatMap((seat, i) => (seat.host ? [i] : []));
  const padOf = (i: number) => {
    const c = s.seats[i].hostController;
    return (c != null ? pads[c] : undefined) ?? "your controller";
  };
  if (pads.length > 0) {
    return ours.length > 0
      ? `You play ${listed(ours.map((i) => `${single ? "the seat" : s.seats[i].label} with your ${padOf(i)}`))}; your keyboard and mouse work beside the seats.`
      : `Your ${listed(pads)} ${pads.length === 1 ? "plays" : "play"} no seat now: use Play here to take one. Your keyboard and mouse still work.`;
  }
  if (single) {
    return s.seats[0].holder
      ? `${nameOf(room, s.seats[0].holder)} is playing; your own controller, keyboard and mouse still work too.`
      : "You're playing.";
  }
  return "You play on your own controller, keyboard and mouse, beside the seats.";
}

function ConsoleBanners(props: {
  room: RoomView;
  session: SessionView;
  busy: string | null;
  run: ReturnType<typeof useRun>["run"];
  pads: string[];
}) {
  const { session: s, pads } = props;
  return (
    <div className="console-banners">
      {s.controlPaused && (
        <Callout
          tone="warn"
          icon={<Hand />}
          actions={
            <Button size="sm" variant="primary" disabled={props.busy != null} onClick={() => void props.run("allow", { type: "allowControl" })}>
              Allow control
            </Button>
          }
        >
          You took the controller back with the panic key. Nobody else can play until you allow it
          {pads.length > 0 ? "; you still can" : ""}.
        </Callout>
      )}
      {s.panicKey == null && (
        <Callout tone="danger" icon={<ShieldAlert />}>
          The panic key couldn't be set up this time: use Take back on a seat to stop someone playing.
        </Callout>
      )}
      {s.hostControllerProblem && (
        <Callout tone="warn">
          Your controllers can't play through seats: {s.hostControllerProblem}. You play on them directly, beside the
          seats.
        </Callout>
      )}
      {streamsElevated(s.streaming?.name) && isLive(s) && (
        <Callout tone="warn" icon={<TriangleAlert />}>
          You're streaming a program that runs as administrator: guests' keyboard and mouse can't reach it, and the
          panic key doesn't work while it is in front. Switch to Couch Co-App and use Take back instead. Their gamepad
          still works.
        </Callout>
      )}
      {pads.length > 0 && (
        <Callout
          tone="info"
          compact
          icon={<Gamepad2 />}
          why={`Games never see your ${listed(pads)}, only the seats. A game that was already running keeps a controller until you reconnect it once (turn it off and on, or unplug it and plug it back into the same port). If a seat gets no player in a Steam game, turn Steam Input off for that game.`}
        >
          Your {listed(pads)} {pads.length === 1 ? "is" : "are"} hidden from games for the whole session: start the game
          now.
        </Callout>
      )}
    </div>
  );
}

function RequestStrip(props: {
  room: RoomView;
  session: SessionView;
  busy: string | null;
  run: ReturnType<typeof useRun>["run"];
}) {
  const { room, session: s } = props;
  // When each request showed at its place: its Accept waits a second, and
  // again when it moves up (the one above was answered), since it then sits
  // where the pointer just clicked.
  const seen = useRef(new Map<string, { at: number; place: number }>());
  const [, tick] = useState(0);
  const now = Date.now();
  s.requests.forEach((r, place) => {
    const key = `${r.member}:${r.seat}`;
    const known = seen.current.get(key);
    if (!known || known.place !== place) seen.current.set(key, { at: now, place });
  });
  for (const key of [...seen.current.keys()]) {
    if (!s.requests.some((r) => `${r.member}:${r.seat}` === key)) seen.current.delete(key);
  }
  const waiting = [...seen.current.values()].some(({ at }) => now - at < ANSWER_AFTER_MS);
  useEffect(() => {
    if (!waiting) return;
    const timer = window.setTimeout(() => tick((n) => n + 1), 250);
    return () => window.clearTimeout(timer);
  });

  return (
    <section className="panel requests" aria-label="Asking to play">
      <header className="panel-head">
        <h3>Asking to play</h3>
        {s.requests.length > 0 && <span className="request-count tabular">{s.requests.length}</span>}
        {s.answerKeys && s.requests.length > 0 && (
          <span className="request-keys">
            Oldest: <Kbd keys={s.answerKeys.accept} /> accept · <Kbd keys={s.answerKeys.decline} /> decline
          </span>
        )}
      </header>
      <div className="request-strip">
        {s.requests.length === 0 ? (
          <p className="request-none">
            <Hand aria-hidden="true" /> No requests. Guests ask from the room or their stream window; you decide.
          </p>
        ) : (
          s.requests.map(({ member, seat }, i) => {
            const label = s.seats[seat]?.label ?? "a seat";
            const kbm = s.seats[seat]?.keyboardMouse ?? false;
            const fresh = now - (seen.current.get(`${member}:${seat}`)?.at ?? 0) < ANSWER_AFTER_MS;
            const why = s.controlPaused ? PAUSED_REASON : fresh ? "Just arrived" : null;
            return (
              <article key={`${member}:${seat}`} className={`request${i === 0 ? " oldest" : ""}${kbm ? " kbm" : ""}`}>
                <Avatar id={member} name={nameOf(room, member)} size={32} />
                <div className="request-text">
                  <span className="request-what">
                    <strong>{nameOf(room, member)}</strong> asks for {seatPhrase(label)}
                  </span>
                  {kbm ? (
                    <span className="request-kbm">
                      <Keyboard aria-hidden="true" /> Control of your PC: only for people you trust
                    </span>
                  ) : (
                    i === 0 && s.answerKeys && <span className="request-oldest">Answered by the keys above</span>
                  )}
                </div>
                <div className="request-actions">
                  <Button
                    size="sm"
                    variant="primary"
                    disabled={props.busy != null || why != null}
                    reason={why}
                    onClick={() => void props.run(`accept-${member}`, { type: "answerControl", member, accept: true })}
                  >
                    Accept
                  </Button>
                  <Button
                    size="sm"
                    disabled={props.busy != null}
                    onClick={() => void props.run(`decline-${member}`, { type: "answerControl", member, accept: false })}
                  >
                    Decline
                  </Button>
                </div>
              </article>
            );
          })
        )}
      </div>
    </section>
  );
}

function SeatBoard(props: {
  room: RoomView;
  session: SessionView;
  busy: string | null;
  run: ReturnType<typeof useRun>["run"];
}) {
  const { room, session: s, run } = props;
  const pads = ourPads(s);
  const single = s.seats.length === 1;
  return (
    <ul className={`seat-board${single ? " single" : ""}`}>
      {s.seats.map((seat, i) => {
        const entries = seatEntries(room, s, i, run);
        const holder = seat.host ? "You" : seat.holder ? nameOf(room, seat.holder) : "Free";
        const detail = seat.host
          ? ((seat.hostController != null ? pads[seat.hostController] : undefined) ?? "your controller")
          : seat.holder
            ? "guest"
            : "nobody plays it";
        return (
          <ContextMenu key={i} items={entries}>
            <li className={`board-seat${seat.holder ? "" : " free"}${seat.host ? " yours" : ""}`}>
              <div className="board-seat-top">
                <SeatIcon seat={seat} />
                <span className="board-seat-label">{seat.label}</span>
                {entries.length > 0 && (
                  <DropdownMenu items={entries}>
                    <IconButton label={`${seat.label}: actions`} size="sm" disabled={props.busy != null}>
                      <MoreHorizontal />
                    </IconButton>
                  </DropdownMenu>
                )}
              </div>
              <div className="board-seat-holder">
                {seat.holder && <Avatar id={seat.holder} name={nameOf(room, seat.holder)} size={24} />}
                <span className="board-seat-who">
                  <span>{holder}</span>
                  <span className="board-seat-detail">{detail}</span>
                </span>
              </div>
              {seat.keyboardMouse && (
                <Tooltip label="Whoever holds this seat types only into the game, and only while its window is in front; the Windows key, Alt+Tab, Alt+F4, Ctrl+Esc and Ctrl+Shift+Esc never go through.">
                  <span className="board-seat-warn">
                    <ShieldAlert aria-hidden="true" /> Control of your PC
                  </span>
                </Tooltip>
              )}
              {seat.holder && (
                <Tooltip label={seat.host ? `Your ${detail} stops playing it; it stays empty` : "It stays empty"}>
                  <Button
                    size="sm"
                    variant="ghost"
                    className="board-seat-take"
                    disabled={props.busy != null}
                    onClick={() => void run(`take-${i}`, { type: "takeBackControl", seat: i })}
                  >
                    {seat.host ? "Leave" : "Take back"}
                  </Button>
                </Tooltip>
              )}
            </li>
          </ContextMenu>
        );
      })}
      {!s.hostOnSeats && (
        <li className="board-seat beside">
          <div className="board-seat-top">
            <span className="seat-icon" aria-hidden="true">
              <Users />
            </span>
            <span className="board-seat-label">Beside the seats</span>
          </div>
          <div className="board-seat-holder">
            <span>{pads.length > 0 ? "Your keyboard and mouse" : "You · own controller, keyboard and mouse"}</span>
          </div>
        </li>
      )}
    </ul>
  );
}

function Watching(props: {
  room: RoomView;
  session: SessionView;
  busy: string | null;
  run: ReturnType<typeof useRun>["run"];
}) {
  const { room, session: s, run } = props;
  const single = s.seats.length === 1;
  return (
    <section className="panel" aria-label="Watching">
      <header className="panel-head">
        <h3>Watching</h3>
        <span className="panel-count tabular">
          {s.viewers.length} of {s.maxViewers}
        </span>
      </header>
      {s.viewers.length === 0 ? (
        <p className="panel-empty">Nobody yet. The others in the room see your session and a Watch button.</p>
      ) : (
        <ul className="watchers">
          {s.viewers.map((id) => {
            const held = seatsOf(s, id);
            const controllers = controllersOf(s, id);
            const entries = viewerEntries(room, s, id, run);
            const blocked = props.busy != null || s.controlPaused;
            return (
              <ContextMenu key={id} items={entries}>
                <li className="watcher">
                  <Avatar id={id} name={nameOf(room, id)} size={32} />
                  <span className="watcher-text">
                    <span className="watcher-name">{nameOf(room, id)}</span>
                    <span className="watcher-detail">
                      {held.length > 0 ? (
                        <>
                          <Gamepad2 aria-hidden="true" /> {listed(held.map((i) => s.seats[i].label))}
                        </>
                      ) : (
                        "watching"
                      )}
                      {!single && ` · ${controllers === 1 ? "1 controller" : `${controllers} controllers`}`}
                    </span>
                  </span>
                  {single ? (
                    held.length > 0 ? (
                      <Button size="sm" disabled={props.busy != null} onClick={() => void run("take", { type: "takeBackControl", seat: 0 })}>
                        Take back
                      </Button>
                    ) : (
                      <Button
                        size="sm"
                        variant="primary"
                        disabled={blocked}
                        reason={s.controlPaused ? PAUSED_REASON : null}
                        onClick={() => void run(`pass-${id}`, { type: "passControl", member: id, seat: 0 })}
                      >
                        Pass control
                      </Button>
                    )
                  ) : (
                    <DropdownMenu items={entries}>
                      <Button size="sm" icon={<Hand />} disabled={props.busy != null}>
                        {held.length > 0 ? "Pass another seat…" : "Pass a seat…"}
                      </Button>
                    </DropdownMenu>
                  )}
                </li>
              </ContextMenu>
            );
          })}
        </ul>
      )}
    </section>
  );
}
