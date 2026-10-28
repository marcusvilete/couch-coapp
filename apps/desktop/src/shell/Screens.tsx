// Whole-window screens: the first run (no rooms yet), the app that couldn't
// start, and the cover while it restarts.
import { OctagonAlert } from "lucide-react";
import { ChoiceCards } from "../rooms/dialogs";
import { Spinner } from "../ui/Feedback";
import { CouchMark } from "./Sidebar";
import "./Screens.css";

export function Welcome(props: { name: string | null; onCreate: () => void; onJoin: () => void; onEditName: () => void }) {
  return (
    <div className="welcome">
      <div className="welcome-inner">
        <span className="welcome-mark" aria-hidden="true">
          <CouchMark weight={64} />
        </span>
        <h1 className="welcome-title">Play together, from your own couches</h1>
        <p className="welcome-lead">
          A room is a group of friends. One of you streams a game, and hands the controller around.
        </p>
        <ChoiceCards onCreate={props.onCreate} onJoin={props.onJoin} />
        {props.name && (
          <p className="welcome-name">
            Your friends will see you as <strong>{props.name}</strong>.{" "}
            <button className="link-button" onClick={props.onEditName}>
              Change
            </button>
          </p>
        )}
      </div>
    </div>
  );
}

export function Fatal(props: { error: string }) {
  return (
    <div className="fatal">
      <div className="fatal-inner">
        <span className="fatal-icon" aria-hidden="true">
          <OctagonAlert />
        </span>
        <h1>Couch Co-App couldn't start</h1>
        <p className="fatal-error selectable">{props.error}</p>
        <p className="fatal-hint">Close this window and start it again. If it says the profile is running, it's open already.</p>
      </div>
    </div>
  );
}

/** Covers the app while it closes to start again: telling the rooms we go
 * takes a moment, then the new window opens with the splash. */
export function RestartCover() {
  return (
    <div className="restart-cover" role="status" aria-live="assertive">
      <Spinner size={32} />
      <strong>Restarting Couch Co-App…</strong>
      <span>Telling your rooms you're stepping out. The app opens again in a few seconds.</span>
    </div>
  );
}
