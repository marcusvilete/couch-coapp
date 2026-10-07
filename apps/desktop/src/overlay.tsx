// The host overlay's page (M8): toasts over the game, in a transparent,
// click-through window that never takes the focus (`useOverlay`, in the main
// window, opens and closes it and sends what to show). It only draws.
import { emit, listen } from "@tauri-apps/api/event";
import { StrictMode, useEffect, useState } from "react";
import ReactDOM from "react-dom/client";
import type { Toast } from "./overlayModel";
import { OVERLAY_READY, OVERLAY_TOASTS } from "./overlayEvents";
import "./overlay.css";

function Overlay() {
  const [toasts, setToasts] = useState<Toast[]>([]);
  useEffect(() => {
    const unlisten = listen<Toast[]>(OVERLAY_TOASTS, (e) => setToasts(e.payload));
    // Then ask: what the main window had before this page loaded.
    void unlisten.then(() => emit(OVERLAY_READY).catch(() => {}));
    return () => {
      void unlisten.then((f) => f());
    };
  }, []);
  return (
    <div className="overlay">
      {toasts.map((t) =>
        t.kind === "request" ? (
          <div key={t.id} className={`toast request${t.first ? " first" : ""}`}>
            <strong>{t.text}</strong>
            {t.first && t.accept && t.decline ? (
              <span className="keys">
                <kbd>{t.accept}</kbd> accept · <kbd>{t.decline}</kbd> decline
              </span>
            ) : (
              <span className="keys">{t.first ? "Answer in the app" : "Waiting"}</span>
            )}
          </div>
        ) : (
          <div key={t.id} className="toast event">
            {t.text}
          </div>
        ),
      )}
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <StrictMode>
    <Overlay />
  </StrictMode>,
);
