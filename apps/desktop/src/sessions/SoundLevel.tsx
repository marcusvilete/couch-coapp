// The host's stream level (P7): how loud guests hear the game, 0 to 400% of
// it (above 100% boosted, peaks rounded off), and a meter of what they hear
// after it.
import { Volume2, VolumeX } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { RoomView } from "../bindings/RoomView";
import { engineCommand, errorText, onEngineEvent } from "../engine";
import { viaRemote, type RemoteTarget } from "../model/remote";
import { Slider } from "../ui/Controls";
import { Why } from "../ui/Why";
import type { Notify } from "./useRun";
import "./SoundLevel.css";

/** The meter's bottom: quieter than this shows as an empty bar. */
const METER_FLOOR_DB = -60;
/** The engine's silence (`couch_media::audio::SILENCE_DB`). */
const SILENCE_DB = -90;
/** A level report older than this means no sound is being sent. */
const METER_STALE_MS = 1000;
/** The host's stream level's top (`couch_media::audio::MAX_BOOST_PERCENT`). */
const MAX_STREAM_LEVEL = 400;

export function SoundLevel(props: {
  roomId: string;
  level: number;
  onRoom: (room: RoomView) => void;
  notify: Notify;
  /** A home PC run from here (M25): the level is set there. */
  remote?: RemoteTarget;
}) {
  const setLevel = (percent: number) => engineCommand(viaRemote(props.remote, { type: "setStreamLevel", percent }));
  const [percent, setPercent] = useState(props.level);
  const [heard, setHeard] = useState<{ peak: number; average: number; at: number } | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const sendTimer = useRef<number | null>(null);
  // Changes not yet answered: a timer waiting, or requests on their way.
  // Until all are, the engine's level may be an older one than the slider's.
  const inFlight = useRef(0);
  const editing = () => sendTimer.current != null || inFlight.current > 0;

  // The engine's level, unless the slider has changes still on their way.
  useEffect(() => {
    if (!editing()) setPercent(props.level);
  }, [props.level]);

  // A change still waiting when the console goes: sent at once, not lost.
  const waiting = useRef<number | null>(null);
  useEffect(
    () => () => {
      if (sendTimer.current != null && waiting.current != null) {
        window.clearTimeout(sendTimer.current);
        sendTimer.current = null;
        void setLevel(waiting.current).catch(() => {});
      }
    },
    [],
  );

  useEffect(() => {
    const unlisten = onEngineEvent((e) => {
      if (e.type === "streamLevel" && e.room === props.roomId) {
        setHeard({ peak: e.peak_db, average: e.average_db, at: Date.now() });
      }
    });
    // Ticks, so the meter empties when reports stop.
    const tick = window.setInterval(() => setNow(Date.now()), 500);
    return () => {
      void unlisten.then((f) => f());
      window.clearInterval(tick);
    };
  }, [props.roomId]);

  // Applied while dragging, but at most every 100 ms: each change is saved.
  const send = (value: number) => {
    inFlight.current++;
    setLevel(value)
      .then((r) => {
        if (r.type === "room") props.onRoom(r);
      })
      .catch((e) => {
        props.notify(`The stream volume didn't change: ${errorText(e)}`, "warn");
        if (inFlight.current === 1 && sendTimer.current == null) setPercent(props.level);
      })
      .finally(() => {
        inFlight.current--;
      });
  };
  const change = (value: number) => {
    setPercent(value);
    waiting.current = value;
    if (sendTimer.current != null) window.clearTimeout(sendTimer.current);
    sendTimer.current = window.setTimeout(() => {
      sendTimer.current = null;
      waiting.current = null;
      send(value);
    }, 100);
  };

  const live = heard != null && now - heard.at < METER_STALE_MS;
  const width = (db: number) => `${Math.max(0, Math.min(100, ((db - METER_FLOOR_DB) / -METER_FLOOR_DB) * 100))}%`;
  const silent = !live || heard.peak <= SILENCE_DB;
  // A boost's peaks at full scale (whole dB: 0 only once bent near it): the
  // limiter is at work.
  const limiting = live && percent > 100 && heard.peak >= 0;
  return (
    <div className="sound">
      <div className="sound-row">
        <span className="sound-icon" aria-hidden="true">
          {percent === 0 ? <VolumeX /> : <Volume2 />}
        </span>
        <Slider
          value={percent}
          min={0}
          max={MAX_STREAM_LEVEL}
          step={5}
          onChange={change}
          label="Stream volume"
          valueText={`${percent}%`}
        />
        <span className="sound-value tabular">
          {percent}%{percent > 100 && <span className="sound-boost"> ×{(percent / 100).toFixed(percent % 100 === 0 ? 0 : 2)}</span>}
        </span>
      </div>
      <div className="sound-row">
        <span className="sound-label">Guests hear</span>
        <div
          className="level-meter"
          role="meter"
          aria-label="What guests hear"
          aria-valuemin={METER_FLOOR_DB}
          aria-valuemax={0}
          aria-valuenow={live ? heard.average : METER_FLOOR_DB}
          title={silent ? "Guests hear nothing right now" : `Guests hear: peak ${heard.peak} dB, average ${heard.average} dB`}
        >
          {live && <div className="level-average" style={{ width: width(heard.average) }} />}
          {live && !silent && <div className="level-peak" style={{ left: width(heard.peak) }} />}
        </div>
        <span className={`sound-state${limiting ? " limiting" : ""}`}>
          {silent ? "silent" : limiting ? "peaks rounded off" : "sending sound"}
        </span>
      </div>
      <p className="sound-note">
        Only guests hear this; you hear the game as it plays.{" "}
        <Why>
          <p>
            To play the game quietly while the stream (and Discord) stay loud: lower the game's slider in Windows'
            volume mixer, then raise this until the meter is back where it was.
          </p>
          <p>Above 100% the loudest peaks are rounded off rather than distorted. Windows' main volume doesn't change the stream.</p>
        </Why>
      </p>
    </div>
  );
}
