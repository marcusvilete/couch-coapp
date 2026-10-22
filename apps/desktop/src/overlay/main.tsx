// The host overlay's page (M8): toasts over the game, in a transparent,
// click-through window that never takes the focus (the main window opens and
// closes it and sends what to show; `overlay/useOverlay.ts`). It only draws,
// and loads nothing of the main window's: no theme, no global styles.
import { emit, listen } from "@tauri-apps/api/event";
import { StrictMode, useEffect, useState } from "react";
import ReactDOM from "react-dom/client";
import type { Toast } from "../model/overlay";
import { OVERLAY_READY, OVERLAY_TOASTS } from "./events";
import { OverlayToasts } from "./OverlayToasts";
import "./overlay.css";
import "./page.css";

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
  return <OverlayToasts toasts={toasts} />;
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <StrictMode>
    <Overlay />
  </StrictMode>,
);
