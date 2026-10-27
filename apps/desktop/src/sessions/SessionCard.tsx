// Someone else's session, as a card of a fixed height: the host, what's
// streamed, a tile per seat with who plays it, and what we can do: watch,
// ask for a seat (the tiles), give seats back, stop watching.
import {
  Activity,
  AppWindow,
  Eye,
  Gamepad2,
  Keyboard,
  LogOut,
  Maximize,
  Minimize,
  PanelTop,
  Undo2,
} from "lucide-react";
import type { RoomView } from "../bindings/RoomView";
import type { SeatView } from "../bindings/SeatView";
import type { SessionView } from "../bindings/SessionView";
import { allowsKbm, idleText, isLive, listed, nameOf, numbered, seatPhrase, seatsOf } from "../model/sessions";
import { Avatar } from "../ui/Avatar";
import { Button } from "../ui/Button";
import { Badge, Spinner } from "../ui/Feedback";
import { ContextMenu, DropdownMenu, type MenuEntry } from "../ui/Menu";
import { Tooltip } from "../ui/Tooltip";
import type { Busy } from "../model/sessions";
import { windowCommand } from "./StreamWindowPanel";
import { useRun, type Notify } from "./useRun";
import "./SessionCard.css";

export function SeatIcon(props: { seat: SeatView }) {
  return <span className="seat-icon" aria-hidden="true">{props.seat.pad ? <Gamepad2 /> : <Keyboard />}</span>;
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
  const win = s.streamWindow;
  // No seats: a session to watch together (M24), nothing to ask for.
  const together = s.seats.length === 0;

  let state: string;
  switch (s.role) {
    case "none":
      state = idle ?? (together ? "Watch along with the others." : "Watch, then ask for a seat to play.");
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

  // Our controllers (P13): when one of two or more must leave its seat for
  // a seat with a pad (each plays one already), asking says which.
  const ourNames = numbered(s.ourControllers.map((c) => c.name));
  const choose = (i: number): MenuEntry[] =>
    s.seats[i]?.pad && s.ourControllers.length >= 2 && s.ourControllers.every((c) => c.seat != null)
      ? s.ourControllers.map((c, k) => ({
          label: ourNames[k],
          icon: <Gamepad2 />,
          hint: c.seat != null ? `leaves ${s.seats[c.seat]?.label ?? "its seat"}` : undefined,
          onSelect: () => void run(`ask-${i}`, { type: "requestControl", seat: i, controller: c.slot }),
        }))
      : [];
  const canAskFor = (i: number) => ready && !mine.includes(i) && !s.controlPaused && asked !== i;
  const askEntries: MenuEntry[] = s.seats.flatMap((seat, i): MenuEntry[] => {
    if (!canAskFor(i)) return [];
    const pads = choose(i);
    return [
      pads.length > 0
        ? { label: `Ask for ${seatPhrase(seat.label)} with`, icon: <Gamepad2 />, items: pads }
        : {
            label: `Ask for ${seatPhrase(seat.label)}`,
            icon: seat.pad ? <Gamepad2 /> : <Keyboard />,
            onSelect: () => void run(`ask-${i}`, { type: "requestControl", seat: i }),
          },
    ];
  });

  const menu: MenuEntry[] = watching
    ? [
        ...askEntries,
        ...mine.map((i) => ({
          label: `Give back ${s.seats[i].label}`,
          icon: <Undo2 />,
          onSelect: () => void run(`back-${i}`, { type: "giveBackSeat", seat: i }),
        })),
        ...(s.role === "asking"
          ? [{ label: "Withdraw the request", icon: <Undo2 />, onSelect: () => void run("cancel", { type: "cancelControl" }) }]
          : []),
        // The stream window's own (P11), as under the cards.
        ...(win?.open
          ? ([
              "separator",
              {
                label: win.fullscreen ? "Stream window: back to a window" : "Stream window: full screen",
                icon: win.fullscreen ? <Minimize /> : <Maximize />,
                onSelect: () => void run("window", windowCommand({ type: "fullscreen", on: !win.fullscreen })),
              },
              {
                label: "Show the stream window",
                icon: <AppWindow />,
                onSelect: () => void run("window", windowCommand({ type: "raise" })),
              },
              {
                label: win.bar ? "Hide its bar" : "Show its bar",
                icon: <PanelTop />,
                onSelect: () => void run("window", windowCommand({ type: "showBar", on: !win.bar })),
              },
              {
                label: win.stats ? "Hide its stats" : "Show its stats",
                icon: <Activity />,
                onSelect: () => void run("window", windowCommand({ type: "showStats", on: !win.stats })),
              },
              "separator",
            ] satisfies MenuEntry[])
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
    // With two controllers or more, the tile asks which plays it (P13).
    const pads = canAsk ? choose(i) : [];
    const button = (
      <button
        className="seat-button"
        disabled={!canAsk}
        onClick={pads.length > 0 ? undefined : () => void run(`ask-${i}`, { type: "requestControl", seat: i })}
        aria-label={`Ask for ${seatPhrase(seat.label)}`}
      >
        {tile}
      </button>
    );
    return (
      <li key={i}>
        {pads.length > 0 ? (
          <DropdownMenu items={[{ heading: `Which controller plays ${seat.label}?` }, ...pads]}>{button}</DropdownMenu>
        ) : (
          <Tooltip label={why ?? `Ask for ${seatPhrase(seat.label)}`}>
            {/* A disabled button gets no pointer: the label sits on a wrapper. */}
            {canAsk ? button : <span className="seat-button-wrap" tabIndex={0}>{button}</span>}
          </Tooltip>
        )}
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
          {together ? (
            <li className="seat-strip-whole">
              <span className="seat-tile together">
                <span className="seat-icon" aria-hidden="true">
                  <Eye />
                </span>
                <span className="seat-tile-text">
                  <span className="seat-tile-label">No seats</span>
                  <span className="seat-tile-holder">
                    <span>Watching together: nobody plays</span>
                  </span>
                </span>
              </span>
            </li>
          ) : (
            !s.hostOnSeats && (
              <li>
                <span className="seat-tile host-outside">
                  <Avatar id={s.host} name={host} size={20} />
                  <span className="seat-tile-text">
                    <span className="seat-tile-label">{host}</span>
                    <span className="seat-tile-holder">own controller</span>
                  </span>
                </span>
              </li>
            )
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
