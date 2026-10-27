// Our session (we host): what's streamed and its controls; the requests to
// play, in a strip that is always there so a new one never moves anything
// under the pointer; who plays what, as a patch bay of cables (drag to hand
// a seat over); the sound guests hear. Every action is also on a menu (⋯ or
// right-click). Answering a request is never bound to a key here: only the
// low-level hook answers with keys (CLAUDE.md), and only a request pending
// for 1 s.
import {
  Eye,
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
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { RoomView } from "../bindings/RoomView";
import type { SessionView } from "../bindings/SessionView";
import { hostIdleText, isLive, listed, nameOf, seatPhrase } from "../model/sessions";
import type { Confirm } from "../rooms/dialogs";
import { Avatar } from "../ui/Avatar";
import { Button, IconButton } from "../ui/Button";
import { Callout } from "../ui/Callout";
import { Badge, Kbd, Spinner } from "../ui/Feedback";
import { DropdownMenu } from "../ui/Menu";
import { Why } from "../ui/Why";
import { ourPads, PAUSED_REASON, requestDetail } from "./hostMenus";
import { PatchBay } from "./PatchBay";
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
                <Why>
                  {s.seats.length === 0
                    ? "Viewers stay, and see the next stream when you start it."
                    : "Viewers stay, and keep their seats; their input waits for the stream, since they can't see."}
                </Why>
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

      {s.seats.length === 0 ? (
        // A session to watch together (M24): no seats, so no requests and
        // no patch bay; who watches, in their place.
        <section className="panel" aria-label="Watching together">
          <header className="panel-head">
            <h3>Watching together</h3>
            <span className="panel-lead">Guests see and hear what you stream; nobody plays but you.</span>
            <span className="panel-why">
              <Why>
                This session has no seats: no virtual controller is plugged in, nothing of yours is hidden from games,
                and guests can't ask to play. To let them play, end it and start one with seats.
              </Why>
            </span>
          </header>
          <ul className="together-viewers" aria-label="Watching">
            {s.viewers.length === 0 ? (
              <li className="request-none">
                <Eye aria-hidden="true" /> Nobody is watching yet.
              </li>
            ) : (
              s.viewers.map((v) => (
                <li key={v} className="together-viewer">
                  <Avatar id={v} name={nameOf(room, v)} size={24} />
                  <span>{nameOf(room, v)}</span>
                </li>
              ))
            )}
          </ul>
          <p className="panel-foot">
            {s.viewers.length} of {s.maxViewers} watching.
          </p>
        </section>
      ) : (
        <>
          <RequestStrip room={room} session={s} busy={busy} run={run} />
          <section className="panel" aria-label="Players and seats">
            <header className="panel-head">
              <h3>Who plays what</h3>
              <span className="panel-lead">
                Drag a player onto a seat to hand it over; pull a plug off a seat to take it back.
              </span>
              <span className="panel-why">
                <Why>
                  {pads.length > 0
                    ? `Every player in the game is one of these seats, yours included, so any of them can be handed over. A seat you take back or leave stays empty; dragging ${pads.length === 1 ? "your controller" : "one of your controllers"} onto a seat plays it. The game numbers its players itself, usually in the order controllers join. Each seat's ⋯ and right-click do the same as dragging.`
                    : "The game numbers its players itself, usually in the order controllers join, and your own controller counts: it often takes player 1. A seat whose controller the game gave no player does nothing; if a guest's seat doesn't respond, move them to another one. Each seat's ⋯ and right-click do the same as dragging."}
                </Why>
              </span>
            </header>
            <PatchBay room={room} session={s} busy={busy} run={run} />
            <p className="panel-foot">
              {whatYouPlay(room, s, pads)} {s.viewers.length} of {s.maxViewers} watching.
            </p>
          </section>
        </>
      )}
      {s.soundLevel != null && (
        <section className="panel" aria-label="Sound">
          <header className="panel-head">
            <h3>Stream volume</h3>
          </header>
          <SoundLevel roomId={room.id} level={s.soundLevel} onRoom={props.onRoom} notify={props.notify} />
        </section>
      )}
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
      {s.padsSettingUp.length > 0 && (
        <Callout
          tone="info"
          icon={<Spinner size={16} />}
          actions={
            <Button size="sm" disabled={props.busy != null} onClick={() => void props.run("stopWaiting", { type: "stopWaitingForPads" })}>
              Stop waiting
            </Button>
          }
          why="Each virtual controller is a new device to Windows the first time it's plugged in on a PC, and setting it up can take a few minutes. It happens once: the next sessions start at once. Stop waiting unplugs the ones not ready yet, and those seats go without a controller this session."
        >
          Windows is setting up the virtual controller for {seatLabels(s, s.padsSettingUp)}, the first time on this PC.
          Guests can take {s.padsSettingUp.length === 1 ? "that seat" : "those seats"}: their gamepad works once it's ready.{" "}
          <Elapsed secs={s.padsSettingUpSecs} />
        </Callout>
      )}
      {s.padsMissing.length > 0 && (
        <Callout tone="warn" icon={<Gamepad2 />}>
          {seatLabels(s, s.padsMissing)} {s.padsMissing.length === 1 ? "has" : "have"} no virtual controller this
          session, so a gamepad there does nothing. End the session and start it again to try again.
        </Callout>
      )}
      {s.panicKey == null && s.seats.length > 0 && (
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
      {streamsElevated(s.streaming?.name) && isLive(s) && s.seats.length > 0 && (
        <Callout tone="warn" icon={<TriangleAlert />}>
          You're streaming a program that runs as administrator: guests' keyboard and mouse can't reach it, and the
          panic key doesn't work while it is in front. Switch to Couch Co-App and use Take back instead. Their gamepad
          still works.
        </Callout>
      )}
      {pads.length > 0 && s.padsSettingUp.length === 0 && (
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

/** The seats `seats` by their labels: "Controller 1 and Controller 2". */
function seatLabels(s: SessionView, seats: number[]): string {
  return listed(seats.map((i) => s.seats[i]?.label ?? `Seat ${i + 1}`));
}

/** How long something has taken, "0:42", counting on from `secs`: the seconds
 * when the engine made the view. */
function Elapsed(props: { secs: number }) {
  const [base, setBase] = useState(() => ({ secs: props.secs, at: Date.now() }));
  const [, tick] = useState(0);
  useEffect(() => setBase({ secs: props.secs, at: Date.now() }), [props.secs]);
  useEffect(() => {
    const timer = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(timer);
  }, []);
  const total = base.secs + Math.floor((Date.now() - base.at) / 1000);
  return (
    <span className="tabular">
      {Math.floor(total / 60)}:{String(total % 60).padStart(2, "0")}
    </span>
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
          s.requests.map(({ member, seat, controller }, i) => {
            const label = s.seats[seat]?.label ?? "a seat";
            // Which of their controllers asks, and the seat it leaves (P13).
            const detail = requestDetail(s, member, seat, controller);
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
                  ) : detail ? (
                    // Which of their controllers moves, and from where (P13);
                    // the keys above still answer it.
                    <span className="request-oldest">{detail.charAt(0).toUpperCase() + detail.slice(1)}</span>
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

