// Sessions on screen: the stage with a card per live session, the host's
// panel for its own stream, the Go live dialog and the shortcuts list.
// The engine decides everything; these only show its state and send commands.
import { useEffect, useState, type FormEvent } from "react";
import type { Command } from "./bindings/Command";
import type { RoomView } from "./bindings/RoomView";
import type { SessionView } from "./bindings/SessionView";
import type { ShortcutView } from "./bindings/ShortcutView";
import type { SourceList } from "./bindings/SourceList";
import { engineCommand, errorText, request } from "./engine";
import { Avatar, MAX_NAME_BYTES, Modal, truncateBytes } from "./ui";

/** What this profile is doing in any room: one stream at a time, and not
 * while watching one. */
export type Busy = { hosting: boolean; watching: boolean };

const WATCHING_ROLES: SessionView["role"][] = ["joining", "watching", "asking", "controlling"];

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

/** Who plays: the controller, or the host while nobody holds it. */
export function playerOf(session: SessionView): string {
  return session.controller ?? session.host;
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
  const player = nameOf(room, playerOf(s));
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
    default: {
      const control: Record<string, [string, Command]> = {
        watching: ["Ask for the controller", { type: "requestControl" }],
        asking: ["Withdraw the request", { type: "cancelControl" }],
        controlling: ["Give it back", { type: "cancelControl" }],
      };
      const [label, command] = control[s.role];
      const paused = s.role === "watching" && s.controlPaused;
      state =
        s.role === "controlling"
          ? "You're playing."
          : s.role === "asking"
            ? `You asked ${host} for the controller.`
            : paused
              ? `You're watching. ${host} paused control for now.`
              : "You're watching.";
      actions = (
        <>
          <button disabled={busy != null || paused} onClick={() => run("control", command)}>
            {label}
          </button>
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
        {player} is playing · {s.viewers.length} of {s.maxViewers} watching
        {s.keyboardMouse && " · keyboard and mouse allowed"}
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
  const playing = s.controller ? nameOf(room, s.controller) : null;
  return (
    <section className="host-panel" aria-label="Your stream">
      <div className="host-head">
        <span className="live-badge">LIVE</span>
        <h3 title={s.title}>{s.title}</h3>
        <span className="hint">
          {s.viewers.length} of {s.maxViewers} watching
          {s.keyboardMouse && " · keyboard and mouse allowed"}
        </span>
        <button className="danger" disabled={busy != null} onClick={() => run("stop", { type: "stopLive" })}>
          {busy === "stop" ? "Stopping…" : "Stop streaming"}
        </button>
      </div>

      {s.controlPaused && (
        <div className="banner warn offer" role="status">
          <span>You took the controller back with the panic key. Nobody can play until you allow it.</span>
          <button disabled={busy != null} onClick={() => run("allow", { type: "allowControl" })}>
            Allow control
          </button>
        </div>
      )}

      {s.requests.length > 0 && (
        <div className="host-list requests">
          <h4>Asking for the controller</h4>
          <ul>
            {s.requests.map((id) => (
              <li key={id}>
                <Avatar name={nameOf(room, id)} />
                <span className="member-name">{nameOf(room, id)}</span>
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

      <div className="host-list">
        <h4>Watching — {s.viewers.length}</h4>
        {s.viewers.length === 0 ? (
          <p className="hint">Nobody yet. The others in the room see your stream and a Watch button.</p>
        ) : (
          <ul>
            {s.viewers.map((id) => {
              const plays = s.controller === id;
              return (
                <li key={id}>
                  <Avatar name={nameOf(room, id)} />
                  <span className="member-name">
                    {nameOf(room, id)}
                    {plays && (
                      <span className="playing" title="Playing" aria-label="playing">
                        {" "}
                        🎮
                      </span>
                    )}
                  </span>
                  {plays ? (
                    <button disabled={busy != null} onClick={() => run("take", { type: "takeBackControl" })}>
                      Take back
                    </button>
                  ) : (
                    <button
                      disabled={busy != null || s.controlPaused}
                      title={s.controlPaused ? "Allow control first" : undefined}
                      onClick={() => run(`pass-${id}`, { type: "passControl", member: id })}
                    >
                      Pass control
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>

      <p className="hint">
        {playing ? `${playing} is playing.` : "You're playing."}{" "}
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
  const [kbm, setKbm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = () => {
    setLoadError(null);
    request({ type: "listSources" }, "sources")
      .then(setSources)
      .catch((e) => setLoadError(errorText(e)));
  };
  useEffect(load, []);

  const pick = (p: Picked) => {
    setPicked(p);
    if (!titleEdited) setTitle(truncateBytes(p.title.trim()));
    if (p.kind === "screen") setKbm(false);
  };

  // Whole Mbit/s only: "50abc" or "1e9" isn't a limit.
  const uploadMbps = limitUpload ? (/^\s*\d{1,5}\s*$/.test(upload) ? Number(upload) : Number.NaN) : null;
  const others = props.room.members.length - 1;

  async function submit(e: FormEvent) {
    e.preventDefault();
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
          keyboard_mouse: kbm,
        },
        "room",
      );
      props.onLive(room);
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  }

  const isPicked = (kind: Picked["kind"], id: string) => picked?.kind === kind && picked.id === id;

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
                  <input type="radio" name="source" checked={isPicked("window", w.id)} onChange={() => pick({ kind: "window", id: w.id, title: w.title })} />
                  <span className="source-title" title={w.title}>
                    {w.title}
                  </span>
                  {w.app && <span className="hint">{w.app}</span>}
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
                  </span>
                </label>
              ))}
              {sources.windows.length === 0 && sources.screens.length === 0 && <p className="hint">No windows found. Start the game, then refresh.</p>}
            </div>
          )}
          <p className="hint">A window is best: only the game is shown, and only its sound is sent.</p>
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

        <label className={`check${picked?.kind === "screen" ? " disabled" : ""}`}>
          <input type="checkbox" checked={kbm} disabled={picked?.kind === "screen"} onChange={(e) => setKbm(e.target.checked)} />
          Let whoever plays use keyboard and mouse too
        </label>
        {kbm ? (
          <p className="banner warn">
            That is control of your PC. Their keys and clicks reach only the game, and only while its window is in front;
            the Windows key, Alt+Tab, Ctrl+Esc and Ctrl+Shift+Esc never go through. Allow it only for people you trust.
          </p>
        ) : (
          <p className="hint">
            {picked?.kind === "screen" ? "Keyboard and mouse need a window, not a whole screen. " : ""}
            Otherwise guests play with their gamepad only.
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

// ---- Shortcuts ----

const SCOPES: Record<ShortcutView["scope"], string> = {
  viewer: "In the stream window",
  host: "Anywhere, while you stream",
};

export function ShortcutsDialog(props: { onClose: () => void }) {
  const [shortcuts, setShortcuts] = useState<ShortcutView[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    request({ type: "getShortcuts" }, "shortcuts")
      .then((r) => setShortcuts(r.shortcuts))
      .catch((e) => setError(errorText(e)));
  }, []);
  return (
    <Modal title="Keyboard shortcuts" onClose={props.onClose} footer={<button className="primary" onClick={props.onClose}>Close</button>}>
      {error && <p className="error">{error}</p>}
      {shortcuts && (
        <table className="shortcuts">
          <tbody>
            {shortcuts.map((s) => (
              <tr key={s.keys}>
                <td>
                  <kbd>{s.keys}</kbd>
                </td>
                <td>
                  {s.action}
                  <span className="hint">
                    {SCOPES[s.scope]}
                    {s.condition && `, ${s.condition}`}
                  </span>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <p className="hint">Shortcuts are never sent to the game. Changing them is coming in a later version.</p>
    </Modal>
  );
}
