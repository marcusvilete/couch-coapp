// Notices from the engine and the app, bottom right: they slide in, go by
// themselves after a while (not while the pointer is on them), and can be
// dismissed. Rendered outside any dialog, so a screen reader still hears
// them while one is open.
import { AlertTriangle, Info, X } from "lucide-react";
import type { Notice } from "../state/lobby";
import "./Toasts.css";

export function Toasts(props: { notices: Notice[]; onDismiss: (id: number) => void; onHold: (held: boolean) => void }) {
  return (
    <div className="toasts" aria-live="polite" onMouseEnter={() => props.onHold(true)} onMouseLeave={() => props.onHold(false)}>
      {props.notices.map((n) => (
        <Toast key={n.id} text={n.text} tone={n.tone} onDismiss={() => props.onDismiss(n.id)} />
      ))}
    </div>
  );
}

function Toast(props: { text: string; tone: Notice["tone"]; onDismiss: () => void }) {
  return (
    <div className={`toast toast-${props.tone}`} role={props.tone === "warn" ? "alert" : "status"}>
      <span className="toast-icon" aria-hidden="true">
        {props.tone === "warn" ? <AlertTriangle /> : <Info />}
      </span>
      <span className="toast-text selectable">{props.text}</span>
      <button className="toast-close" aria-label="Dismiss" onClick={props.onDismiss}>
        <X />
      </button>
    </div>
  );
}
