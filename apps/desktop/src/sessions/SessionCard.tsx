// Someone else's session, as a card of a fixed height: the host, what's
// streamed, a tile per seat with who plays it, and what we can do: watch,
// ask for a seat (the tiles), give seats back, stop watching.
import { Eye, Gamepad2, Keyboard, LogOut, MousePointer2, Undo2 } from "lucide-react";
import type { RoomView } from "../bindings/RoomView";
import type { SeatView } from "../bindings/SeatView";
import type { SessionView } from "../bindings/SessionView";
import { allowsKbm, idleText, isLive, listed, nameOf, seatPhrase, seatsOf } from "../model/sessions";
import { Avatar } from "../ui/Avatar";
import { Button } from "../ui/Button";
import { Badge, Spinner } from "../ui/Feedback";
import { ContextMenu, type MenuEntry } from "../ui/Menu";
import { Tooltip } from "../ui/Tooltip";
import type { Busy } from "../model/sessions";
import { useRun, type Notify } from "./useRun";
import "./SessionCard.css";

export function SeatIcon(props: { seat: SeatView }) {
  const { seat } = props;
  if (seat.pad && seat.keyboardMouse) {
    return (
      <span className="seat-icon both" aria-hidden="true">
        <Gamepad2 />
        <MousePointer2 />
      </span>
    );
  }
  return <span className="seat-icon" aria-hidden="true">{seat.pad ? <Gamepad2 /> : <Keyboard />}</span>;
}

export function SessionCard(props: {
  room: RoomView;
  session: SessionView;
  busy: Busy;
  compact?: boolean;
  onRoom: (room: RoomView) => void;
  notify: Notify;
}) {
  const { room, session: s } = props;
  const { busy, run } = useRun(props.notify, props.onRoom);
  const host = nameOf(room, s.host);
  // This profile is listed first.
  const me = room.members[0]?.id;
  const mine = seatsOf(s, me);
  const watching = s.role !== "none";
  const ready = s.role === "watching" || s.role === "asking" || s.role === "controlling";
  const full = s.viewers.length >= s.maxViewers;
  const watchBlocked = props.busy.hosting
    ? "End your session first."
    : props.busy.watching
      ? "You're watching another session."
      : full
        ? "Full: the host chose how many can watch."
        : null;
  const asked = s.askedSeat;
  const idle = idleText(host, s.stream);

  let state: string;
  switch (s.role) {
    case "none":
      state = idle ?? "Watch, then ask for a seat to play.";
      break;
    case "joining":
      state = `Waiting for ${host}…`;
      break;
    case "reconnecting":
      state = `Lost the connection to ${host}. Reconnecting…`;
      break;
    default: {
      const seatName = (i: number | null) => (i == null || i < 0 ? "a seat" : seatPhrase(s.seats[i].label));
      const ours = listed(mine.map((i) => s.seats[i].label));
      state =
        s.role === "asking"
          ? mine.length > 0
            ? `You're playing on ${ours}, and asked ${host} for ${seatName(asked)}.`
            : `You asked ${host} for ${seatName(asked)}.`
          : mine.length > 0
            ? s.seats.length > 1
              ? `You're playing, on ${ours}.`
              : "You're playing."
            : s.controlPaused
              ? `You're watching. ${host} paused control for now.`
              : "You're watching.";
    }
  }

  const menu: MenuEntry[] = watching
    ? [
        ...mine.map((i) => ({
          label: `Give back ${s.seats[i].label}`,
          icon: <Undo2 />,
          onSelect: () => void run(`back-${i}`, { type: "giveBackSeat", seat: i }),
        })),
        ...(s.role === "asking"
          ? [{ label: "Withdraw the request", icon: <Undo2 />, onSelect: () => void run("cancel", { type: "cancelControl" }) }]
          : []),
        { label: "Stop watching", icon: <LogOut />, onSelect: () => void run("stop", { type: "stopWatching" }) },
      ]
    : [
        {
          label: "Watch",
          icon: <Eye />,
          disabled: watchBlocked ?? false,
          onSelect: () => void run("watch", { type: "watch", room: room.id, host: s.host }),
        },
      ];

  const seatTile = (seat: SeatView, i: number) => {
    const ours = mine.includes(i);
    const holder = seat.holder;
    const label = holder ? (ours ? "You" : nameOf(room, holder)) : "Free";
    const canAsk = ready && !ours && !s.controlPaused && asked !== i && busy == null;
    const tile = (
      <span className={`seat-tile${ours ? " ours" : ""}${asked === i ? " asked" : ""}${holder ? "" : " free"}`}>
        <SeatIcon seat={seat} />
        <span className="seat-tile-text">
          <span className="seat-tile-label">{seat.label}</span>
          <span className="seat-tile-holder">
            {holder && <Avatar id={holder} name={nameOf(room, holder)} size={20} />}
            <span>{asked === i ? "Asked…" : label}</span>
          </span>
        </span>
      </span>
    );
    if (!ready || ours) {
      return (
        <li key={i}>
          {ours ? (
            <Tooltip label={`Give back ${s.seats[i].label}`}>
              <button
                className="seat-button"
                disabled={busy != null}
                onClick={() => void run(`back-${i}`, { type: "giveBackSeat", seat: i })}
                aria-label={`Give back ${seat.label}`}
              >
                {tile}
              </button>
            </Tooltip>
          ) : (
            tile
          )}
        </li>
      );
    }
    const why = s.controlPaused ? `${host} paused control` : asked === i ? "Asked" : undefined;
    const button = (
      <button
        className="seat-button"
        disabled={!canAsk}
        onClick={() => void run(`ask-${i}`, { type: "requestControl", seat: i })}
        aria-label={`Ask for ${seatPhrase(seat.label)}`}
      >
        {tile}
      </button>
    );
    return (
      <li key={i}>
        <Tooltip label={why ?? `Ask for ${seatPhrase(seat.label)}`}>
          {/* A disabled button gets no pointer: the label sits on a wrapper. */}
          {canAsk ? button : <span className="seat-button-wrap" tabIndex={0}>{button}</span>}
        </Tooltip>
      </li>
    );
  };

  return (
    <ContextMenu items={menu}>
      <article className={`session-card${watching ? " mine" : ""}${props.compact ? " compact" : ""}`}>
        <header className="session-card-head">
          <Avatar id={s.host} name={host} size={40} />
          <div className="session-card-titles">
            <h3 className="session-card-title" title={s.title}>
              {s.title}
            </h3>
            <span className="session-card-host">{host}'s session</span>
          </div>
          {isLive(s) ? <Badge tone="live">Live</Badge> : <Badge>Not streaming</Badge>}
        </header>
        <ul className="seat-strip" aria-label="Seats">
          {s.seats.map(seatTile)}
          {!s.hostOnSeats && (
            <li>
              <span className="seat-tile host-outside">
                <Avatar id={s.host} name={host} size={20} />
                <span className="seat-tile-text">
                  <span className="seat-tile-label">{host}</span>
                  <span className="seat-tile-holder">own controller</span>
                </span>
              </span>
            </li>
          )}
        </ul>
        <p className="session-card-state" role="status">
          {(s.role === "joining" || s.role === "reconnecting") && <Spinner size={13} />}
          <span>{state}</span>
        </p>
        <footer className="session-card-foot">
          <span className="session-card-facts">
            <span className="tabular">
              {s.viewers.length} of {s.maxViewers} watching
            </span>
            {allowsKbm(s) && (
              <Tooltip label="A seat has keyboard and mouse">
                <Keyboard className="fact-icon" aria-label="keyboard and mouse allowed" />
              </Tooltip>
            )}
          </span>
          <span className="session-card-actions">
            {s.role === "asking" && (
              <Button size="sm" disabled={busy != null} onClick={() => void run("cancel", { type: "cancelControl" })}>
                Withdraw
              </Button>
            )}
            {s.role === "controlling" && (
              <Button size="sm" disabled={busy != null} onClick={() => void run("cancel", { type: "cancelControl" })}>
                {mine.length > 1 ? "Give all back" : "Give it back"}
              </Button>
            )}
            {watching ? (
              <Button size="sm" disabled={busy != null} onClick={() => void run("stop", { type: "stopWatching" })}>
                Stop watching
              </Button>
            ) : (
              <Button
                size="sm"
                variant="primary"
                icon={<Eye />}
                disabled={busy != null || watchBlocked != null}
                reason={watchBlocked}
                labels={["Watch", "Opening…"]}
                onClick={() => void run("watch", { type: "watch", room: room.id, host: s.host })}
              >
                {busy === "watch" ? "Opening…" : "Watch"}
              </Button>
            )}
          </span>
        </footer>
      </article>
    </ContextMenu>
  );
}
