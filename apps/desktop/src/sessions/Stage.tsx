// The room's stage: our console when we host here (others' sessions under
// it, smaller), or the sessions of the others as cards, or an invitation to
// start one.
import { Gamepad2, Play, UserPlus } from "lucide-react";
import type { RoomView } from "../bindings/RoomView";
import type { Busy } from "../model/sessions";
import type { Confirm } from "../rooms/dialogs";
import { Button } from "../ui/Button";
import { EmptyState } from "../ui/Feedback";
import { HostConsole } from "./HostConsole";
import { SessionCard } from "./SessionCard";
import { StreamWindowPanel } from "./StreamWindowPanel";
import type { Notify } from "./useRun";
import "./Stage.css";

export function Stage(props: {
  room: RoomView;
  busy: Busy;
  onStartSession: () => void;
  onStream: () => void;
  onInvite: () => void;
  onRoom: (room: RoomView) => void;
  notify: Notify;
  confirm: (c: Confirm) => void;
}) {
  const { room, busy } = props;
  const mine = room.sessions.find((s) => s.role === "host");
  const others = room.sessions.filter((s) => s.role !== "host");
  // The session we watch here, with its stream window's controls (P11).
  const watched = others.find((s) => s.streamWindow != null);
  const startBlocked = busy.hosting
    ? "You have a session already, in another room."
    : busy.watching
      ? "Stop watching first."
      : null;

  if (mine == null && others.length === 0) {
    const alone = room.members.length === 1;
    return (
      <EmptyState
        icon={<Gamepad2 />}
        title="Nobody has a session yet"
        actions={
          <>
            <Button
              variant="primary"
              size="lg"
              icon={<Play />}
              onClick={props.onStartSession}
              disabled={startBlocked != null}
              reason={startBlocked}
            >
              Start a session
            </Button>
            {room.isOwner && alone && !room.needsPassword && !room.catchingUp && (
              <Button size="lg" icon={<UserPlus />} onClick={props.onInvite}>
                Invite people
              </Button>
            )}
          </>
        }
      >
        Set up the controller seats first, then stream a game window or a screen, as often as you like. The others
        watch, and you hand them the controller.
        {startBlocked && <span className="stage-blocked">{startBlocked}</span>}
      </EmptyState>
    );
  }

  return (
    <div className="stage">
      {mine && (
        <HostConsole
          room={room}
          session={mine}
          onRoom={props.onRoom}
          notify={props.notify}
          onStream={props.onStream}
          confirm={props.confirm}
        />
      )}
      {(others.length > 0 || !mine) && (
        <section className="stage-sessions" aria-label="Sessions">
          <header className="stage-head">
            <h2>{mine ? "Other sessions here" : "Sessions"}</h2>
            {!mine && (
              <Button
                variant="primary"
                icon={<Play />}
                onClick={props.onStartSession}
                disabled={startBlocked != null}
                reason={startBlocked}
              >
                Start a session
              </Button>
            )}
          </header>
          <div className="stage-cards">
            {others.map((s) => (
              <SessionCard
                key={s.host}
                room={room}
                session={s}
                busy={busy}
                compact={mine != null}
                onRoom={props.onRoom}
                notify={props.notify}
              />
            ))}
          </div>
          {watched && <StreamWindowPanel room={room} session={watched} notify={props.notify} />}
        </section>
      )}
    </div>
  );
}
