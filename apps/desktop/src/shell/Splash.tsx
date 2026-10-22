// The splash at start ("Player 2 joins"): two controllers slide in from the
// edges, their P1 and P2 lights blinking, land on a couch, and the name
// types itself out. It only covers the lobby's first load: the engine is
// already running. A click or a key skips it. It animates even with
// Windows' animations off (the user's choice, 2026-10-07): it is short and
// skippable.
import { useEffect, useState } from "react";
import "./Splash.css";

/** How long the animation plays (Splash.css: the name is typed
 * out by 1.75 s), then the finished picture stays a second. */
const PLAY_MS = 2800;
/** The splash never stays longer, loaded or not. */
const MAX_MS = 5000;
/** Splash.css, `.splash` transition. */
const FADE_MS = 500;

const PAD = [" .-=====-. ", "/ +    o  \\", "\\__/---\\__/"].join("\n");

const COUCH = [
  "   ________________________   ",
  "  /                        \\  ",
  " |  ______________________  | ",
  "[| |                      | |]",
  "[|_|______________________|_|]",
  "   '                      '   ",
].join("\n");

const TITLE = "Couch Co-App";

export function Splash(props: { ready: boolean; onDone: () => void }) {
  const [played, setPlayed] = useState(false);
  const [skipped, setSkipped] = useState(false);
  const [leaving, setLeaving] = useState(false);
  const { ready, onDone } = props;

  useEffect(() => {
    const play = window.setTimeout(() => setPlayed(true), PLAY_MS);
    const cap = window.setTimeout(() => setSkipped(true), MAX_MS);
    const skip = () => setSkipped(true);
    window.addEventListener("keydown", skip);
    window.addEventListener("mousedown", skip);
    return () => {
      window.clearTimeout(play);
      window.clearTimeout(cap);
      window.removeEventListener("keydown", skip);
      window.removeEventListener("mousedown", skip);
    };
  }, []);

  // Once true, stays true: each part only ever turns true.
  const leave = skipped || (played && ready);
  useEffect(() => {
    if (!leave) return;
    setLeaving(true);
    const done = window.setTimeout(onDone, FADE_MS);
    return () => window.clearTimeout(done);
  }, [leave, onDone]);

  return (
    <div
      className={leaving ? "splash leaving" : "splash"}
      aria-label={TITLE}
      role="img"
    >
      <div className="splash-stage">
        <pre className="splash-pad p1">
          <span className="splash-light">P1</span>
          {"\n" + PAD}
        </pre>
        <pre className="splash-pad p2">
          <span className="splash-light">P2</span>
          {"\n" + PAD}
        </pre>
        <pre className="splash-couch">{COUCH}</pre>
        <div className="splash-title">
          <span>{TITLE}</span>
        </div>
      </div>
    </div>
  );
}
