// The preview's own controls, in a corner: pick a scenario, make things
// happen (a request, an opened link, the panic key, a failing command), and
// see the host overlay as it would show over the game.
import { useEffect, useState } from "react";
import { OverlayToasts } from "../overlay/OverlayToasts";
import "../overlay/overlay.css";
import { failNextCommand, openInvite, panicKey, someoneAsks } from "./fakeEngine";
import { watchOverlay } from "./platform";
import { SCENARIOS } from "./scenarios";
import "./preview.css";

export function PreviewPanel(props: { scenario: string }) {
  const [open, setOpen] = useState(true);
  const [overlay, setOverlay] = useState<Parameters<Parameters<typeof watchOverlay>[0]>[0] | null>(null);
  useEffect(() => watchOverlay(setOverlay), []);

  const go = (scenario: string) => {
    const url = new URL(window.location.href);
    url.searchParams.set("scenario", scenario);
    window.location.href = url.toString();
  };

  return (
    <>
      {overlay?.show && (
        <div className="preview-overlay" aria-hidden="true">
          <span className="preview-overlay-label">Overlay over the game</span>
          <OverlayToasts toasts={overlay.toasts} />
        </div>
      )}
      <aside className={open ? "preview-panel" : "preview-panel closed"} aria-label="Preview controls">
        <button className="preview-toggle" onClick={() => setOpen(!open)}>
          {open ? "Preview ▾" : "Preview ▸"}
        </button>
        {open && (
          <div className="preview-body">
            <select value={props.scenario} onChange={(e) => go(e.target.value)} aria-label="Scenario">
              {Object.entries(SCENARIOS).map(([key, s]) => (
                <option key={key} value={key}>
                  {s.title}
                </option>
              ))}
            </select>
            <div className="preview-actions">
              <button onClick={someoneAsks}>Someone asks</button>
              <button onClick={() => openInvite("https://marcusvilete.github.io/couch-coapp/j/#v1.movie-night")}>
                Open a link
              </button>
              <button onClick={panicKey}>Panic key</button>
              <button onClick={() => failNextCommand("the engine has stopped")}>Fail next</button>
              <button onClick={() => window.dispatchEvent(new Event("blur"))}>App behind game</button>
            </div>
          </div>
        )}
      </aside>
    </>
  );
}
