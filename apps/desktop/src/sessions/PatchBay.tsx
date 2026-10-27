// The host's patch bay (M21): who plays which seat, as cables. People on the
// left (you, with a plug per controller of yours; each viewer, with a plug
// per controller of theirs, P12), seats on the right, a cable from each seat
// to the controller driving it (or whoever holds it), and a dashed one for
// each request. Drag a person or a controller onto a seat to hand it over
// (a guest's controller: that one plays it); pick a cable up by its plug at
// the seat and drop it in the open to take the seat back, or on another seat
// to move it. Every move is a session command, the same the seats' and
// viewers' menus send (they stay, for the keyboard): the engine decides, and
// the cables follow its answer.
import { Gamepad2, GripVertical, Keyboard, MoreHorizontal, ShieldAlert } from "lucide-react";
import { Fragment, useCallback, useEffect, useLayoutEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import type { RoomView } from "../bindings/RoomView";
import type { SeatView } from "../bindings/SeatView";
import type { SessionView } from "../bindings/SessionView";
import { isLive, listed, nameOf, seatsOf } from "../model/sessions";
import { Avatar, hueOf } from "../ui/Avatar";
import { IconButton } from "../ui/Button";
import { ContextMenu, DropdownMenu } from "../ui/Menu";
import { Tooltip } from "../ui/Tooltip";
import {
  cableFrom,
  controllersOf,
  frees,
  guestPadHint,
  guestPads,
  ourPads,
  passHint,
  PAUSED_REASON,
  seatEntries,
  viewerEntries,
} from "./hostMenus";
import type { Run } from "./useRun";
import "./PatchBay.css";

type Point = { x: number; y: number };

/** What is being dragged: a viewer, one of our controllers, one of a
 * viewer's controllers (by slot, P12), or a seat's cable picked up by its
 * plug. */
type Drag =
  | { kind: "viewer"; member: string; from: string }
  | { kind: "pad"; controller: number; from: string }
  | { kind: "guestPad"; member: string; controller: number; from: string }
  | { kind: "cable"; seat: number; from: string };

/** Who a cable picked up at `seat` drops as: the controller at its other end
 * (ours or a guest's), else its holder. */
function heldBy(s: SessionView, seat: number, from: string): Drag | null {
  const held = s.seats[seat];
  if (held?.host && held.hostController != null) return { kind: "pad", controller: held.hostController, from };
  if (!held?.holder) return null;
  const plug = cableFrom(s, seat);
  if (plug?.startsWith("gpad:") && held.guestController != null) {
    return { kind: "guestPad", member: held.holder, controller: held.guestController, from };
  }
  return { kind: "viewer", member: held.holder, from };
}

/** A drag starts once the pointer has moved this far: a plain click stays a
 * click. */
const DRAG_AFTER_PX = 5;

/** What dropping `drag` on `seat` would do, or why it can't. */
export function dropOn(
  room: RoomView,
  s: SessionView,
  drag: Drag,
  seat: number,
): { ok: boolean; hint: string } {
  const target = s.seats[seat];
  if (!target) return { ok: false, hint: "" };
  const pads = ourPads(s);
  // A cable picked up drops as what's at its other end would.
  if (drag.kind === "cable") {
    if (drag.seat === seat) return { ok: false, hint: "" };
    const who = heldBy(s, drag.seat, drag.from);
    return who ? dropOn(room, s, who, seat) : { ok: false, hint: "" };
  }
  if (drag.kind === "guestPad") return guestPadHint(room, s, drag.member, drag.controller, seat);
  const loses = !target.host && target.holder ? ` · ${nameOf(room, target.holder)} loses it` : "";
  const kbm = target.keyboardMouse ? " · control of your PC" : "";
  if (drag.kind === "pad") {
    if (!target.pad) return { ok: false, hint: "No controller on this seat" };
    if (target.host && target.hostController === drag.controller) return { ok: false, hint: "Already here" };
    const other = s.hostHomes.findIndex((home, j) => j !== drag.controller && home === seat);
    const own = s.hostHomes[drag.controller];
    const what =
      own === seat ? "Back on its seat" : other >= 0 ? `Swaps with your ${pads[other]}` : "Plays here";
    return { ok: true, hint: what + loses };
  }
  if (s.controlPaused) return { ok: false, hint: PAUSED_REASON };
  const held = seatsOf(s, drag.member);
  if (held.includes(seat)) return { ok: false, hint: "Theirs already" };
  // One of theirs must leave its seat and nothing says which (P13).
  if (frees(s, drag.member, seat).ambiguous) {
    return { ok: false, hint: `Drag one of ${nameOf(room, drag.member)}'s controllers` };
  }
  const how = passHint(s, drag.member, seat);
  const what = how == null ? "Hands it over" : how.charAt(0).toUpperCase() + how.slice(1);
  return { ok: true, hint: what + (target.host ? " · yours" : loses) + kbm };
}

/** A plug's name: the model gives way to the width, the number `numbered`
 * added to tell two alike apart stays. */
function PlugName(props: { model: string; name: string }) {
  const n = props.name !== props.model ? props.name.slice(props.model.length).trim() : null;
  return (
    <span className="bay-plug-name" title={props.name}>
      <span className="bay-plug-model">{props.model}</span>
      {n && <span className="bay-plug-n">{n}</span>}
    </span>
  );
}

/** A cable's colour: its player's. */
function hueFor(id: string): string {
  return `${hueOf(id)}`;
}

/** A soft S-curve from `a` (a plug on the left) to `b` (a socket on the
 * right). */
function curve(a: Point, b: Point): string {
  const pull = Math.max(40, Math.abs(b.x - a.x) * 0.5);
  return `M ${a.x} ${a.y} C ${a.x + pull} ${a.y}, ${b.x - pull} ${b.y}, ${b.x} ${b.y}`;
}

export function PatchBay(props: {
  room: RoomView;
  session: SessionView;
  busy: string | null;
  run: Run;
  /** The name of one of our PCs, whose session we run from here (M25). */
  remote?: string;
}) {
  const { room, session: s, run } = props;
  const pads = ourPads(s);
  const live = isLive(s);
  const bay = useRef<HTMLDivElement>(null);
  const anchors = useRef(new Map<string, HTMLElement>());
  const [geo, setGeo] = useState<Record<string, Point>>({});
  const [drag, setDrag] = useState<Drag | null>(null);
  const [pointer, setPointer] = useState<Point | null>(null);
  const [over, setOver] = useState<number | null>(null);
  /** The seat that just took a cable: it pulses once. */
  const [snapped, setSnapped] = useState<number | null>(null);

  const anchor = useCallback(
    (key: string) => (el: HTMLElement | null) => {
      if (el) anchors.current.set(key, el);
      else anchors.current.delete(key);
    },
    [],
  );

  // Where each plug and socket is, in the bay's own coordinates.
  const measure = useCallback(() => {
    const box = bay.current?.getBoundingClientRect();
    if (!box) return;
    const next: Record<string, Point> = {};
    for (const [key, el] of anchors.current) {
      const r = el.getBoundingClientRect();
      next[key] = { x: r.left + r.width / 2 - box.left, y: r.top + r.height / 2 - box.top };
    }
    setGeo((old) => (JSON.stringify(old) === JSON.stringify(next) ? old : next));
  }, []);
  useLayoutEffect(measure);
  useEffect(() => {
    const observer = new ResizeObserver(measure);
    if (bay.current) observer.observe(bay.current);
    return () => observer.disconnect();
  }, [measure]);

  const toBay = (e: { clientX: number; clientY: number }): Point => {
    const box = bay.current?.getBoundingClientRect();
    return { x: e.clientX - (box?.left ?? 0), y: e.clientY - (box?.top ?? 0) };
  };
  const seatAt = (e: { clientX: number; clientY: number }): number | null => {
    for (const el of document.elementsFromPoint(e.clientX, e.clientY)) {
      const seat = (el as HTMLElement).closest?.<HTMLElement>("[data-seat]");
      if (seat && bay.current?.contains(seat)) return Number(seat.dataset.seat);
    }
    return null;
  };

  // Pressing on a plug arms a drag; moving a little starts it; letting go
  // drops it. Escape cancels.
  const press = (start: Drag) => (e: ReactPointerEvent<HTMLElement>) => {
    if (e.button !== 0 || props.busy != null) return;
    const origin = { x: e.clientX, y: e.clientY };
    let dragging = false;
    const move = (ev: PointerEvent) => {
      if (!dragging && Math.hypot(ev.clientX - origin.x, ev.clientY - origin.y) < DRAG_AFTER_PX) return;
      if (!dragging) {
        dragging = true;
        setDrag(start);
        document.body.classList.add("patching");
      }
      setPointer(toBay(ev));
      setOver(seatAt(ev));
    };
    const end = (ev: PointerEvent | null) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("keydown", key, true);
      document.body.classList.remove("patching");
      setDrag(null);
      setPointer(null);
      setOver(null);
      if (!dragging || ev == null) return;
      const seat = seatAt(ev);
      if (seat == null) {
        // A cable dropped in the open: the seat is taken back (left, ours).
        if (start.kind === "cable") void run(`take-${start.seat}`, { type: "takeBackControl", seat: start.seat });
        return;
      }
      if (!dropOn(room, s, start, seat).ok) return;
      setSnapped(seat);
      window.setTimeout(() => setSnapped((n) => (n === seat ? null : n)), 600);
      const who = start.kind === "cable" ? heldBy(s, start.seat, start.from) : start;
      if (who?.kind === "pad") {
        void run(`play-${seat}`, { type: "playSeat", seat, controller: who.controller });
      } else if (who?.kind === "viewer") {
        void run(`pass-${who.member}`, { type: "passControl", member: who.member, seat });
      } else if (who?.kind === "guestPad") {
        void run(`pass-${who.member}`, { type: "passControl", member: who.member, seat, controller: who.controller });
      }
    };
    const up = (ev: PointerEvent) => end(ev);
    const key = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") {
        ev.preventDefault();
        ev.stopPropagation();
        dragging = false;
        end(null);
      }
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("keydown", key, true);
  };

  // The cables: each held seat from its holder, each request dashed.
  const cables: { key: string; from: string; to: string; hue: string; request?: boolean; lifted?: boolean }[] = [];
  s.seats.forEach((seat, i) => {
    const from = cableFrom(s, i);
    if (!seat.holder || !from) return;
    const lifted = drag?.kind === "cable" && drag.seat === i;
    cables.push({ key: `seat-${i}`, from, to: `seat:${i}`, hue: hueFor(seat.holder), lifted });
  });
  // A keyboard seat no guest holds is the host's own keyboard and mouse:
  // they always reach the game, and then they're the only ones.
  const kbmSeats = s.seats.flatMap((seat, i) => (seat.keyboardMouse ? [i] : []));
  for (const i of kbmSeats) {
    if (!s.seats[i].holder) cables.push({ key: `kbm-${i}`, from: "kbm", to: `seat:${i}`, hue: hueFor(s.host) });
  }
  for (const r of s.requests) {
    cables.push({ key: `ask-${r.member}`, from: `viewer:${r.member}`, to: `seat:${r.seat}`, hue: hueFor(r.member), request: true });
  }
  // A cable picked up at its seat stays plugged into its player: its loose
  // end is the one in your hand.
  const liftedFrom = drag?.kind === "cable" ? cableFrom(s, drag.seat) : undefined;
  const dragFrom = !drag ? undefined : liftedFrom ? geo[liftedFrom] : geo[drag.from];
  const target = drag && over != null ? dropOn(room, s, drag, over) : null;
  const dragHue =
    drag?.kind === "viewer" || drag?.kind === "guestPad"
      ? hueFor(drag.member)
      : drag?.kind === "cable"
        ? hueFor(s.seats[drag.seat]?.holder ?? s.host)
        : hueFor(s.host);
  const releaseToTake = drag?.kind === "cable" && over == null;

  return (
    <div className={`bay${drag ? " dragging" : ""}${live ? " live" : ""}`} ref={bay}>
      <svg className="bay-cables" aria-hidden="true">
        {cables.map((c) => {
          const a = geo[c.from];
          const b = geo[c.to];
          if (!a || !b || c.lifted) return null;
          const d = curve(a, b);
          return (
            <g key={c.key} className={c.request ? "cable request" : "cable"} style={{ "--hue": c.hue } as React.CSSProperties}>
              <path className="cable-glow" d={d} />
              <path className="cable-line" d={d} />
              {!c.request && <path className="cable-flow" d={d} />}
            </g>
          );
        })}
        {drag && dragFrom && pointer && (
          <g
            className={`cable live-drag${target && !target.ok ? " refused" : ""}${releaseToTake ? " releasing" : ""}`}
            style={{ "--hue": dragHue } as React.CSSProperties}
          >
            <path className="cable-glow" d={curve(dragFrom, pointer)} />
            <path className="cable-line" d={curve(dragFrom, pointer)} />
            <circle className="cable-tip" cx={pointer.x} cy={pointer.y} r={7} />
          </g>
        )}
      </svg>

      <div className="bay-people" aria-label="Players">
        <h4 className="bay-head">Players</h4>
        <div className="bay-person host">
          <Avatar id={s.host} name={nameOf(room, s.host)} size={32} />
          <span className="bay-person-text">
            <strong>{props.remote ?? "You"}</strong>
            <span>
              {props.remote
                ? "your PC, run from here"
                : pads.length > 0
                  ? pads.length === 1
                    ? "your controller"
                    : "your controllers"
                  : kbmSeats.length > 0
                    ? "your keyboard and mouse"
                    : "beside the seats"}
            </span>
          </span>
        </div>
        {(pads.length > 0 || kbmSeats.length > 0) && (
          <div className="bay-pads">
            {pads.map((name, k) => (
              <Tooltip key={k} label={`Drag onto a seat to play it with your ${name}`} side="left">
                <button
                  className="bay-plug pad"
                  onPointerDown={press({ kind: "pad", controller: k, from: `pad:${k}` })}
                  aria-label={`Your ${name}: drag onto a seat`}
                >
                  <Gamepad2 aria-hidden="true" />
                  <PlugName model={s.hostControllers[k] ?? name} name={name} />
                  <span className="bay-jack" ref={anchor(`pad:${k}`)} style={{ "--hue": hueFor(s.host) } as React.CSSProperties} />
                </button>
              </Tooltip>
            ))}
            {kbmSeats.length > 0 && (
              <Tooltip
                label="Your keyboard and mouse always reach the game: they need no seat. While no guest holds the keyboard seat, they're the only ones."
                side="left"
              >
                <span className="bay-plug pad fixed" tabIndex={0}>
                  <Keyboard aria-hidden="true" />
                  <PlugName model="Keyboard and mouse" name="Keyboard and mouse" />
                  <span className="bay-jack" ref={anchor("kbm")} style={{ "--hue": hueFor(s.host) } as React.CSSProperties} />
                </span>
              </Tooltip>
            )}
          </div>
        )}
        {pads.length === 0 && (
          <p className="bay-note">
            {props.remote
              ? "Its own controllers stay out of the seats: nobody plays at it. Hand a seat to yourself to play from here."
              : kbmSeats.length > 0
                ? "Your own controller plays the game directly."
                : "Your own controller, keyboard and mouse play the game directly."}
          </p>
        )}

        {s.viewers.length === 0 ? (
          <p className="bay-note bay-nobody">Nobody watching yet. The others in the room see your session and a Watch button.</p>
        ) : (
          s.viewers.map((id) => {
            const held = seatsOf(s, id);
            const controllers = controllersOf(s, id);
            const asking = s.requests.some((r) => r.member === id);
            // Their controllers, a plug each (P12), when there's a seat with
            // a pad to play.
            const theirs = s.seats.some((seat) => seat.pad) ? guestPads(s, id) : [];
            return (
              <Fragment key={id}>
                <ContextMenu items={viewerEntries(room, s, id, run)}>
                  <div className={`bay-person viewer${asking ? " asking" : ""}`}>
                    <button
                      className="bay-plug viewer"
                      onPointerDown={press({ kind: "viewer", member: id, from: `viewer:${id}` })}
                      aria-label={`${nameOf(room, id)}: drag onto a seat to hand it over`}
                      title={s.controlPaused ? PAUSED_REASON : "Drag onto a seat"}
                    >
                      <GripVertical className="bay-grip" aria-hidden="true" />
                      <Avatar id={id} name={nameOf(room, id)} size={32} />
                      <span className="bay-person-text">
                        <strong>{nameOf(room, id)}</strong>
                        <span>
                          {held.length > 0 ? `plays ${listed(held.map((i) => s.seats[i].label))}` : asking ? "asking to play" : "watching"}
                          {s.seats.length > 1 && theirs.length === 0 && ` · ${controllers === 1 ? "1 controller" : `${controllers} controllers`}`}
                        </span>
                      </span>
                      <span className="bay-jack" ref={anchor(`viewer:${id}`)} style={{ "--hue": hueFor(id) } as React.CSSProperties} />
                    </button>
                    <DropdownMenu items={viewerEntries(room, s, id, run)}>
                      <IconButton label={`${nameOf(room, id)}: seats`} size="sm" disabled={props.busy != null}>
                        <MoreHorizontal />
                      </IconButton>
                    </DropdownMenu>
                  </div>
                </ContextMenu>
                {theirs.length > 0 && (
                  <div className="bay-pads guest">
                    {theirs.map((p) => (
                      <Tooltip key={p.slot} label={`Drag onto a seat: ${nameOf(room, id)} plays it with their ${p.name}`} side="left">
                        <button
                          className="bay-plug pad"
                          onPointerDown={press({ kind: "guestPad", member: id, controller: p.slot, from: `gpad:${id}:${p.slot}` })}
                          aria-label={`${nameOf(room, id)}'s ${p.name}: drag onto a seat`}
                        >
                          <Gamepad2 aria-hidden="true" />
                          <PlugName model={p.model} name={p.name} />
                          <span className="bay-jack" ref={anchor(`gpad:${id}:${p.slot}`)} style={{ "--hue": hueFor(id) } as React.CSSProperties} />
                        </button>
                      </Tooltip>
                    ))}
                  </div>
                )}
              </Fragment>
            );
          })
        )}
      </div>

      <div className="bay-seats" aria-label="Seats">
        <h4 className="bay-head">Seats</h4>
        {s.seats.map((seat, i) => (
          <SeatSocket
            key={i}
            room={room}
            session={s}
            seat={seat}
            index={i}
            busy={props.busy}
            run={run}
            anchor={anchor(`seat:${i}`)}
            target={over === i ? target : null}
            snapped={snapped === i}
            dragging={drag != null}
            lifted={drag?.kind === "cable" && drag.seat === i}
            onPlug={press({ kind: "cable", seat: i, from: `seat:${i}` })}
          />
        ))}
      </div>

      {/* Over a seat, the seat itself says what letting go does. */}
      {drag && pointer && !target && (
        <div className="bay-hint" style={{ left: pointer.x + 16, top: pointer.y + 14 }} role="status">
          {releaseToTake
            ? s.seats[drag.kind === "cable" ? drag.seat : 0]?.host
              ? "Let go to leave this seat empty"
              : "Let go to take it back"
            : "Drop on a seat"}
        </div>
      )}
    </div>
  );
}

function SeatSocket(props: {
  room: RoomView;
  session: SessionView;
  seat: SeatView;
  index: number;
  busy: string | null;
  run: Run;
  anchor: (el: HTMLElement | null) => void;
  target: { ok: boolean; hint: string } | null;
  snapped: boolean;
  dragging: boolean;
  /** Its cable is in the host's hand: the socket shows empty meanwhile. */
  lifted: boolean;
  onPlug: (e: ReactPointerEvent<HTMLElement>) => void;
}) {
  const { room, session: s, seat, index: i } = props;
  const entries = seatEntries(room, s, i, props.run);
  const pads = ourPads(s);
  // A keyboard seat no guest holds is played by the host's own keyboard.
  const yourKeys = seat.keyboardMouse && !seat.holder;
  const who = seat.host || yourKeys ? "You" : seat.holder ? nameOf(room, seat.holder) : "Free";
  const pad = (seat.hostController != null ? pads[seat.hostController] : undefined) ?? "your controller";
  // A guest's: the controller of theirs driving it, once they've said (P12).
  const theirs = seat.holder && !seat.host ? guestPads(s, seat.holder).find((p) => p.slot === seat.guestController)?.name : undefined;
  const detail = seat.host
    ? pad
    : seat.holder
      ? (theirs ?? "guest")
      : yourKeys
        ? "your keyboard and mouse"
        : "drop someone here";
  return (
    <ContextMenu items={entries}>
      <div
        className={[
          "bay-seat",
          seat.holder || yourKeys ? "held" : "free",
          seat.host ? "yours" : "",
          props.target ? (props.target.ok ? "can-drop" : "no-drop") : "",
          props.snapped ? "snapped" : "",
          props.dragging ? "awaiting" : "",
        ].join(" ")}
        data-seat={i}
      >
        {seat.holder ? (
          <Tooltip label="Drag the plug away to take the seat back, or onto another seat to move it">
            <button
              className={props.lifted ? "bay-socket plugged lifted" : "bay-socket plugged"}
              ref={props.anchor}
              onPointerDown={props.onPlug}
              aria-label={`${seat.label}: drag the plug to move or take back`}
              style={{ "--hue": hueFor(seat.holder) } as React.CSSProperties}
            />
          </Tooltip>
        ) : yourKeys ? (
          <span
            className="bay-socket plugged fixed"
            ref={props.anchor}
            style={{ "--hue": hueFor(s.host) } as React.CSSProperties}
          />
        ) : (
          <span className="bay-socket" ref={props.anchor} />
        )}
        <span className="bay-seat-icon" aria-hidden="true">
          {seat.pad ? <Gamepad2 /> : <Keyboard />}
        </span>
        <span className="bay-seat-text">
          <span className="bay-seat-label">{seat.label}</span>
          <span className="bay-seat-who">
            {(seat.holder ?? (yourKeys ? s.host : null)) && (
              <Avatar id={seat.holder ?? s.host} name={nameOf(room, seat.holder ?? s.host)} size={20} />
            )}
            <strong>{who}</strong>
            <span className="bay-seat-detail">{detail}</span>
          </span>
          {seat.keyboardMouse && (
            <Tooltip label="Whoever holds this seat types only into the game, and only while its window is in front; the Windows key, Alt+Tab, Alt+F4, Ctrl+Esc and Ctrl+Shift+Esc never go through.">
              <span className="bay-seat-warn">
                <ShieldAlert aria-hidden="true" /> Control of your PC
              </span>
            </Tooltip>
          )}
        </span>
        {props.target && <span className={`bay-drop-hint${props.target.ok ? "" : " no"}`}>{props.target.hint}</span>}
        {entries.length > 0 && (
          <DropdownMenu items={entries}>
            <IconButton label={`${seat.label}: actions`} size="sm" disabled={props.busy != null}>
              <MoreHorizontal />
            </IconButton>
          </DropdownMenu>
        )}
      </div>
    </ContextMenu>
  );
}
