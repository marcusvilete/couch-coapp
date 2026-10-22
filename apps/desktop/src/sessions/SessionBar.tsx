// While we host, in every view (another room, Settings): our session at a
// glance, with what waits for us. Requests stay answerable from anywhere:
// the bar takes us to the console. The panic key is shown, or that it is
// missing.
import { Hand, MonitorPlay, ShieldAlert } from "lucide-react";
import type { RoomView } from "../bindings/RoomView";
import type { SessionView } from "../bindings/SessionView";
import { isLive } from "../model/sessions";
import { Button } from "../ui/Button";
import { Badge, Kbd } from "../ui/Feedback";
import { useRun, type Notify } from "./useRun";
import "./SessionBar.css";

export function SessionBar(props: {
  room: RoomView;
  session: SessionView;
  /** The console is on screen: no need to offer it. */
  atConsole: boolean;
  onConsole: () => void;
  onRoom: (room: RoomView) => void;
  notify: Notify;
}) {
  const { room, session: s } = props;
  const { busy, run } = useRun(props.notify, props.onRoom);
  const n = s.requests.length;
  return (
    <div
      className={`session-bar${isLive(s) ? " live" : ""}${n > 0 ? " asking" : ""}`}
      role="region"
      aria-label="Your session"
    >
      <MonitorPlay className="session-bar-icon" aria-hidden="true" />
      {isLive(s) ? <Badge tone="live">Live</Badge> : <Badge>Not streaming</Badge>}
      <span className="session-bar-title">
        {s.title}
        {!props.atConsole && <span className="session-bar-room"> · {room.name}</span>}
      </span>
      {n > 0 && (
        <span className="session-bar-requests">
          <Hand aria-hidden="true" /> {n === 1 ? "1 asks to play" : `${n} ask to play`}
        </span>
      )}
      {s.controlPaused && (
        <Button size="sm" variant="primary" disabled={busy != null} onClick={() => void run("allow", { type: "allowControl" })}>
          Allow control
        </Button>
      )}
      <span className="session-bar-panic">
        {s.panicKey ? (
          <>
            Panic key <Kbd keys={s.panicKey} />
          </>
        ) : (
          <span className="session-bar-nopanic">
            <ShieldAlert aria-hidden="true" /> No panic key: use Take back
          </span>
        )}
      </span>
      {!props.atConsole && (
        <Button size="sm" variant={n > 0 ? "primary" : "secondary"} onClick={props.onConsole}>
          {n > 0 ? "Answer" : "Open console"}
        </Button>
      )}
    </div>
  );
}
