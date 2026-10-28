// Starting a session (M19): the seats come first, and our controllers are
// hidden from games, before any game starts; streams come later, from the
// console. One step: title and how many can watch, then the seats.
import { Eye, Gamepad2, Keyboard, Minus, Play, Plus, ShieldAlert } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import type { InputSetup } from "../bindings/InputSetup";
import type { KeyboardMouseSeat } from "../bindings/KeyboardMouseSeat";
import type { RoomView } from "../bindings/RoomView";
import type { SourceList } from "../bindings/SourceList";
import { prepareChime } from "../chime";
import { errorText, request } from "../engine";
import { viaRemote, type RemoteTarget } from "../model/remote";
import { nameOf, numbered, padKeys, padName, padsAdded, togglePad } from "../model/sessions";
import { seatsFor } from "../model/seats";
import { KEYS, readStored, storedPads, storedSetup, writeStored } from "../state/storage";
import { Button, IconButton } from "../ui/Button";
import { Callout } from "../ui/Callout";
import { Segmented, Select } from "../ui/Controls";
import { Dialog } from "../ui/Dialog";
import { Skeleton } from "../ui/Feedback";
import { Check, Field, TextInput } from "../ui/Field";
import { MAX_NAME_BYTES, truncateBytes } from "../ui/text";
import { Why } from "../ui/Why";
import "./StartSessionDialog.css";

export function StartSessionDialog(props: {
  room: RoomView;
  onClose: () => void;
  onStarted: (room: RoomView) => void;
  /** On one of our own PCs, run from here (M25): its windows, its virtual
   * controllers, and none of its own controllers (nobody is there). */
  remote?: RemoteTarget;
}) {
  const remote = props.remote;
  const pcName = remote ? nameOf(props.room, remote.member) : null;
  const [sources, setSources] = useState<SourceList | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  // The last title, or one ready to go: the placeholder alone looked filled in.
  const [title, setTitle] = useState(() => truncateBytes(readStored(KEYS.sessionTitle) || "Game night"));
  const [viewers, setViewers] = useState(4);
  const [controllers, setControllers] = useState(() => storedSetup().controllers);
  // From afar the keyboard seat is also desktop control's (M25b): on by
  // default there.
  const [kbm, setKbm] = useState<KeyboardMouseSeat>(() => (remote ? "ownSeat" : storedSetup().keyboardMouse));
  // Our controllers to play through seats, as `padKeys` (M23), in the
  // order listed: each takes the next seat with a pad. None plays outside.
  const [myPads, setMyPads] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    request(viaRemote(remote, { type: "listSources" }), "sources")
      .then((list) => {
        setSources(list);
        // The last choice comes back, for the controllers here; with none
        // ever made, the first one, if it can be hidden.
        const last = storedPads();
        const keys = padKeys(list.hostControllers);
        const ready = list.hiding === "ready";
        setMyPads(!ready ? [] : last == null ? keys.slice(0, 1) : keys.filter((k) => last.includes(k)));
      })
      .catch((e) => setLoadError(errorText(e)));
  }, []);

  const input: InputSetup = { controllers, keyboardMouse: kbm };
  // No seats at all: a session to watch together, nobody plays (M24).
  const watchOnly = controllers === 0 && kbm === "none";
  const padSeat = controllers > 0;
  const hostControllers = sources?.hostControllers ?? [];
  const keys = padKeys(hostControllers);
  const names = numbered(hostControllers.map(padName));
  const chosenKeys = padSeat ? keys.filter((k) => myPads.includes(k)).slice(0, controllers) : [];
  const chosen = hostControllers.filter((_, i) => chosenKeys.includes(keys[i]));
  // Playing through a remapper's pad (M22): the controller behind it stays
  // in games' sight unless the remapper hides it.
  const remapping = chosen.some((c) => c.kind === "otherVirtual");
  const besides = names.filter((_, i) => hostControllers[i].kind === "physical" && !chosenKeys.includes(keys[i]));
  // Another remapper pad left unticked: games see it too, so the general
  // warning stays beside the remapper's.
  const otherRemappers = hostControllers.some((c, i) => c.kind === "otherVirtual" && !chosenKeys.includes(keys[i]));
  // A remapper's pad and the controller it reads both ticked: one press
  // then drives two seats (seen in M22's run).
  const alsoBehind = remapping && chosen.some((c) => c.kind === "physical");
  const others = props.room.members.length - 1;
  const seats = seatsFor(input);

  async function submit(e: FormEvent) {
    e.preventDefault();
    // A click: the webview lets the request chime play from now on.
    prepareChime();
    const problem =
      (title.trim() === "" ? "Give the session a title." : null) ??
      (new TextEncoder().encode(title.trim()).length > MAX_NAME_BYTES ? "That title is too long." : null);
    if (problem) {
      setError(problem);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      let room = await request(
        viaRemote(remote, {
          type: "startSession",
          room: props.room.id,
          title: title.trim(),
          max_viewers: viewers,
          input,
          controllers: chosen,
        }),
        "room",
      );
      writeStored(KEYS.sessionSetup, JSON.stringify(input));
      writeStored(KEYS.sessionTitle, title.trim());
      if (remote) {
        // Run from afar, it is watched from here: the stream window opens
        // once something is streamed.
        room = await request({ type: "watch", room: remote.room, host: remote.member }, "room").catch(() => room);
      } else if (padSeat && hostControllers.length > 0 && sources?.hiding === "ready") {
        // Only a real choice is remembered: with HidHide missing or busy,
        // or no controller seat, none could be made. It keeps the
        // controllers remembered but not plugged in now: they come back
        // when they are.
        const absent = (storedPads() ?? []).filter((k) => !keys.includes(k));
        writeStored(KEYS.myPads, JSON.stringify([...chosenKeys, ...absent]));
      }
      props.onStarted(room);
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  }

  return (
    <Dialog
      title={pcName ? `Start a session on ${pcName}` : `Start a session in ${props.room.name}`}
      description={
        pcName
          ? `Run from here: ${pcName} plugs in the seats' virtual controllers, then you pick one of its windows or screens to stream, and watch it here.`
          : watchOnly
            ? "Guests watch what you stream; nobody else plays. You stream a window or a screen next, and stop or switch whenever you like."
            : "The seats are set up first, and your controllers hidden from games. Then you stream a window or a screen, and stop or switch whenever you like."
      }
      onClose={props.onClose}
      busy={busy}
      size="l"
      fixed
      pinned={
        error && (
          <p className="field-error" role="alert">
            {error}
          </p>
        )
      }
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy}>
            Cancel
          </Button>
          <Button
            type="submit"
            form="start-session"
            variant="primary"
            icon={<Play />}
            disabled={busy}
            labels={["Start the session", "Starting…"]}
          >
            {busy ? "Starting…" : "Start the session"}
          </Button>
        </>
      }
    >
      <form id="start-session" onSubmit={submit} className="start">
        <div className="start-row">
          <Field label="Title" hint="What the room sees it called.">
            {(id) => (
              <TextInput id={id} value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Game night" />
            )}
          </Field>
          <Field
            label="How many can watch"
            hint={
              others === 0
                ? "Nobody else is in the room now."
                : others === 1
                  ? "1 other member is in the room now."
                  : `${others} other members are in the room now.`
            }
          >
            {(id) => (
              <Select
                id={id}
                value={viewers}
                onChange={setViewers}
                options={[1, 2, 3, 4, 5, 6, 7].map((n) => ({
                  value: n,
                  label: String(n),
                  detail: n === 4 ? "recommended" : undefined,
                }))}
              />
            )}
          </Field>
        </div>

        <section className="start-seats" aria-label="Seats">
          <header className="start-section-head">
            <h3>Seats: how the game takes input</h3>
            <Why>
              Each controller seat is its own virtual pad in the game. The seats stay for the whole session: to change
              them, end it and start another. Guests ask for a seat and you decide.
            </Why>
          </header>
          <div className="seat-builder">
            <div className="seat-counter">
              <span className="seat-counter-label">Controllers</span>
              <IconButton
                label="One controller less"
                variant="secondary"
                disabled={controllers === 0}
                onClick={() => setControllers(controllers - 1)}
              >
                <Minus />
              </IconButton>
              <span className="seat-counter-value tabular" aria-live="polite">
                {controllers}
              </span>
              <IconButton
                label="One controller more"
                variant="secondary"
                disabled={controllers === 4}
                onClick={() => setControllers(controllers + 1)}
              >
                <Plus />
              </IconButton>
            </div>
            <Field label="Keyboard and mouse">
              {() => (
                <Segmented
                  label="Keyboard and mouse"
                  value={kbm}
                  onChange={setKbm}
                  options={[
                    { value: "none", label: "Not for guests" },
                    { value: "ownSeat", label: "A seat of their own", title: "One player can hold it beside a controller" },
                  ]}
                />
              )}
            </Field>
          </div>
          <ul className="seat-preview" aria-label="The seats">
            {watchOnly ? (
              <li className="seat-preview-none">
                <Eye aria-hidden="true" /> Just watching: guests see and hear, nobody plays.
              </li>
            ) : (
              seats.map((seat, i) => {
                const mine = seat.pad ? chosenKeys[seats.slice(0, i).filter((x) => x.pad).length] : undefined;
                const mineName = mine != null ? names[keys.indexOf(mine)] : null;
                return (
                  <li key={i} className={`seat-preview-tile${mineName ? " yours" : ""}`}>
                    <span className="seat-preview-icon" aria-hidden="true">
                      {seat.pad ? <Gamepad2 /> : <Keyboard />}
                    </span>
                    <span className="seat-preview-label">{seat.label}</span>
                    <span className="seat-preview-who">{mineName ? `You · ${mineName}` : "A guest"}</span>
                  </li>
                );
              })
            )}
          </ul>
          {kbm !== "none" && remote ? (
            <Callout tone="info" icon={<Keyboard />} title="The keyboard seat is how you control the desktop">
              Stream {pcName}'s screen and turn on desktop control in its console: your keyboard and mouse then reach
              everything on {pcName}, to start a game or switch windows. You can still give the seat to a guest for a
              game's window; they never get the desktop.
            </Callout>
          ) : kbm !== "none" ? (
            <Callout tone="danger" icon={<ShieldAlert />} title="Keyboard and mouse are control of your PC">
              Whoever holds that seat types only into the game, and only while its window is in front, so you'll stream
              a window, not a whole screen; the Windows key, Alt+Tab, Alt+F4, Ctrl+Esc and Ctrl+Shift+Esc never go
              through. Give that seat only to people you trust.
            </Callout>
          ) : watchOnly ? (
            <p className="start-note">
              No virtual controller is plugged in and nothing of yours is hidden: you play as you would alone.
            </p>
          ) : (
            <p className="start-note">Guests play with their gamepad only.</p>
          )}
        </section>

        {remote && padSeat && sources && !sources.virtualPads && (
          <Callout tone="warn" compact>
            ViGEmBus, which makes the virtual controllers, isn't installed on {pcName}: guests' controllers won't reach
            the game. It can be installed there only, in Settings → Drivers.
          </Callout>
        )}
        {padSeat && !remote && (
          <section className="start-pads" aria-label="Your controllers">
            <header className="start-section-head">
              <h3>Your controllers: one seat each</h3>
              {sources?.hiding === "ready" && chosen.length > 0 && (
                <Why>
                  {chosen.length === 1 ? "It is" : "They are"} hidden from games for the whole session and{" "}
                  {chosen.length === 1 ? "plays its seat" : "each play their own seat"} through Couch Co-App, so every
                  seat, yours included, can be handed over. Start the game after the session: a game already running
                  keeps a controller until you reconnect it once
                  {remapping ? " (for a remapper's pad, reconnect the controller behind it)" : ""}. For a Steam game,
                  turn Steam Input off for it (the game's Properties → Controller): otherwise Steam may keep a player for
                  a hidden controller, and one seat gets none.
                </Why>
              )}
            </header>
            {loadError && <p className="field-error selectable">{loadError}</p>}
            {sources == null && !loadError && <Skeleton lines={2} height={14} />}
            {sources && hostControllers.length === 0 && (
              <p className="start-note">No controller of yours is plugged in: you play on your keyboard and mouse.</p>
            )}
            {sources && hostControllers.length > 0 && (
              <div className="pad-list">
                {hostControllers.map((_, i) => {
                  const on = chosenKeys.includes(keys[i]);
                  const place = chosenKeys.indexOf(keys[i]);
                  const full = !on && chosenKeys.length + padsAdded(keys, chosenKeys, keys[i]) > controllers;
                  return (
                    <Check
                      key={keys[i]}
                      checked={on}
                      disabled={full || sources.hiding !== "ready"}
                      title={full ? "Every controller seat has one of yours: add a seat first" : undefined}
                      onChange={(checked) => setMyPads(togglePad(keys, myPads, keys[i], checked))}
                    >
                      {names[i]}
                      {on && <span className="pad-place"> · you start on Controller {place + 1}</span>}
                    </Check>
                  );
                })}
              </div>
            )}
            {sources?.hiding === "notInstalled" && hostControllers.length > 0 && (
              <p className="start-note">
                To play on a seat with your controller, install HidHide: Settings → Drivers (it needs administrator
                rights, then a restart).
              </p>
            )}
            {sources?.hiding === "busy" && (
              <p className="start-note">
                HidHide is busy right now. If its configuration window is open, close it, then start the session again.
              </p>
            )}
            {sources?.hiding === "ready" && hostControllers.length > 0 && chosen.length === 0 && (
              <p className="start-note">You play outside the seats, as the game sees your controllers.</p>
            )}
            {alsoBehind && (
              <p className="start-note">
                If your remapper reads one of the other controllers you ticked, each press on it drives two seats: its
                own, and the remapper's pad's. Tick only the remapper's pad for that controller.
              </p>
            )}
            {remapping && besides.length > 0 && (
              <Callout tone="warn" compact>
                Games still see {besides.join(" and ")}, unless your remapper hides {besides.length === 1 ? "it" : "them"}:
                then one player too many. Turn on the remapper's own hiding (in DS4Windows, "Hide DS4 controller"), or
                tick {besides.length === 1 ? "it" : "them"} to play a seat too.
              </Callout>
            )}
            {(!(remapping && besides.length > 0) || otherRemappers) && (sources?.controllers ?? 0) > chosen.length && (
              <Callout tone="warn" compact>
                A controller is plugged into this PC. Games may give it a player's place, and then a guest's controller
                may not work. Play on a seat with it (above), or unplug it.
              </Callout>
            )}
            {sources && !sources.virtualPads && (
              <Callout tone="warn" compact>
                Guests' controllers won't reach the game: ViGEmBus, which makes the virtual controllers, isn't
                installed. Install it in Settings → Drivers. Watching, and a keyboard-and-mouse seat, still work.
              </Callout>
            )}
          </section>
        )}
        <p className="start-note">Windows' volume mixer sets how loud the game is, for your guests as for you.</p>
      </form>
    </Dialog>
  );
}
