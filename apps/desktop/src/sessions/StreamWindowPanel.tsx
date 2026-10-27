// The stream window's controls in the app (P11), for watching with the
// stream window on another monitor: its volume, full screen, bringing it to
// the front, its bar and stats; and what its bar says that the card doesn't
// (the game out of front on the host's PC, the latest news). Under the cards,
// only while we watch: it never moves a card.
import { AppWindow, Maximize, Minimize, Volume2, VolumeX } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { Command } from "../bindings/Command";
import type { RoomView } from "../bindings/RoomView";
import type { SessionView } from "../bindings/SessionView";
import type { StreamWindowRequest } from "../bindings/StreamWindowRequest";
import type { StreamWindowView } from "../bindings/StreamWindowView";
import { engineCommand, errorText } from "../engine";
import { nameOf } from "../model/sessions";
import { Button, IconButton } from "../ui/Button";
import { Slider, Switch } from "../ui/Controls";
import type { Notify } from "./useRun";
import "./StreamWindowPanel.css";

/** How long a note stays, as on the stream window's bar. */
const NOTE_FOR_MS = 5000;

/** `note` while it is new: shown for a while after it changes, then gone
 * (the engine keeps the last one for good). */
function useFresh(note: string | null): string | null {
  const [fresh, setFresh] = useState<string | null>(null);
  useEffect(() => {
    setFresh(note);
    if (note == null) return;
    const timer = window.setTimeout(() => setFresh(null), NOTE_FOR_MS);
    return () => window.clearTimeout(timer);
  }, [note]);
  return fresh;
}

/** The card's and panel's window actions, as menu entries share them. */
export function windowCommand(request: StreamWindowRequest): Command {
  return { type: "streamWindow", request };
}

export function StreamWindowPanel(props: { room: RoomView; session: SessionView; notify: Notify }) {
  const { room, session: s } = props;
  const w: StreamWindowView = s.streamWindow ?? {
    volume: 100,
    muted: false,
    open: false,
    fullscreen: false,
    bar: true,
    stats: false,
  };
  const host = nameOf(room, s.host);
  const [percent, setPercent] = useState(w.volume);
  // What we last asked for, ahead of the engine's answer: a second quick
  // click on Mute must flip it back, and a late save must not undo it.
  const muted = useRef(w.muted);
  const [mutedShown, setMutedShown] = useState(w.muted);
  // The engine's latest (the window's slider moves it too), for when ours
  // are all answered.
  const engine = useRef({ volume: w.volume, muted: w.muted });
  engine.current = { volume: w.volume, muted: w.muted };
  const sendTimer = useRef<number | null>(null);
  const waiting = useRef<number | null>(null);
  // Volume changes not yet answered: until all are, the engine's volume may
  // be an older one than ours.
  const inFlight = useRef(0);
  const editing = () => sendTimer.current != null || inFlight.current > 0;
  const follow = () => {
    setPercent(engine.current.volume);
    muted.current = engine.current.muted;
    setMutedShown(engine.current.muted);
  };

  // The engine's volume, unless ours has changes still on their way; once
  // they are all answered, whatever came meanwhile is caught up with.
  useEffect(() => {
    if (!editing()) follow();
  }, [w.volume, w.muted]);

  const volume = (value: number, mute: boolean) => {
    inFlight.current++;
    engineCommand({ type: "setStreamVolume", percent: value, muted: mute })
      .catch((e) => props.notify(`The volume didn't change: ${errorText(e)}`, "warn"))
      .finally(() => {
        inFlight.current--;
        if (!editing()) follow();
      });
  };
  const toggleMute = () => {
    muted.current = !muted.current;
    setMutedShown(muted.current);
    volume(percent, muted.current);
  };

  // Saved at most every 150 ms while dragging, and the last change always.
  const change = (value: number) => {
    setPercent(value);
    waiting.current = value;
    if (sendTimer.current != null) window.clearTimeout(sendTimer.current);
    sendTimer.current = window.setTimeout(() => {
      sendTimer.current = null;
      waiting.current = null;
      volume(value, muted.current);
    }, 150);
  };
  useEffect(
    () => () => {
      if (sendTimer.current != null && waiting.current != null) {
        window.clearTimeout(sendTimer.current);
        void engineCommand({ type: "setStreamVolume", percent: waiting.current, muted: muted.current }).catch(() => {});
      }
    },
    [],
  );

  // The window's state comes back from the window itself: nothing to undo.
  const ask = (request: StreamWindowRequest) => {
    engineCommand(windowCommand(request)).catch((e) =>
      props.notify(`The stream window didn't answer: ${errorText(e)}`, "warn"),
    );
  };
  const playing = s.role === "controlling" || s.role === "asking";
  const away = playing && s.gameInFront === false;
  const note = useFresh(s.note);
  const status = !w.open
    ? "Opening the stream window…"
    : away
      ? `The game isn't in front on ${host}'s PC: your input may not reach it`
      : (note ?? "");

  return (
    <section className="window-panel" aria-label="Stream window">
      <header className="window-panel-head">
        <h3>Stream window</h3>
        <span className={`window-status${away && w.open ? " away" : ""}`} role="status" title={status || undefined}>
          {status}
        </span>
      </header>
      <div className="window-panel-row">
        <div className="window-volume">
          <IconButton label={mutedShown ? "Unmute" : "Mute"} disabled={!w.open} onClick={toggleMute}>
            {mutedShown ? <VolumeX /> : <Volume2 />}
          </IconButton>
          <Slider
            value={percent}
            min={0}
            max={100}
            step={5}
            disabled={!w.open}
            onChange={change}
            label="Stream window volume"
            valueText={mutedShown ? `${percent}%, muted` : `${percent}%`}
          />
          <span className={`window-volume-value tabular${mutedShown ? " muted" : ""}`}>{percent}%</span>
        </div>
        <div className="window-actions">
          <Button
            size="sm"
            icon={w.fullscreen ? <Minimize /> : <Maximize />}
            labels={["Full screen", "Back to a window"]}
            disabled={!w.open}
            onClick={() => ask({ type: "fullscreen", on: !w.fullscreen })}
          >
            {w.fullscreen ? "Back to a window" : "Full screen"}
          </Button>
          <Button size="sm" icon={<AppWindow />} disabled={!w.open} onClick={() => ask({ type: "raise" })}>
            Show window
          </Button>
        </div>
        <div className="window-switches">
          <Switch
            checked={w.bar}
            disabled={!w.open}
            onChange={(on) => ask({ type: "showBar", on })}
            label="Bar"
          />
          <Switch
            checked={w.stats}
            disabled={!w.open}
            onChange={(on) => ask({ type: "showStats", on })}
            label="Stats"
          />
        </div>
      </div>
    </section>
  );
}
