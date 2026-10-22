// Small pieces of state: badges, keys, placeholders while loading, empty
// states, a spinner.
import type { ReactNode } from "react";
import "./Feedback.css";

export function Badge(props: {
  tone?: "live" | "neutral" | "accent" | "warn" | "ok" | "info";
  children: ReactNode;
  title?: string;
}) {
  return (
    <span className={`badge badge-${props.tone ?? "neutral"}`} title={props.title}>
      {props.tone === "live" && <span className="badge-dot" aria-hidden="true" />}
      {props.children}
    </span>
  );
}

/** Keys to press: "Ctrl+Alt+Shift+Y" as separate caps. */
export function Kbd(props: { keys: string }) {
  return (
    <span className="kbd-group">
      {props.keys.split("+").map((k, i) => (
        <kbd key={i} className="kbd">
          {k === "" ? "+" : k}
        </kbd>
      ))}
    </span>
  );
}

/** Space held for text still loading, the size of what comes. */
export function Skeleton(props: { width?: number | string; height?: number; lines?: number }) {
  const lines = props.lines ?? 1;
  return (
    <span className="skeleton-group" aria-hidden="true">
      {Array.from({ length: lines }, (_, i) => (
        <span
          key={i}
          className="skeleton"
          style={{ width: i === lines - 1 && lines > 1 ? "60%" : (props.width ?? "100%"), height: props.height ?? 12 }}
        />
      ))}
    </span>
  );
}

export function EmptyState(props: { icon: ReactNode; title: string; children?: ReactNode; actions?: ReactNode }) {
  return (
    <div className="empty">
      <span className="empty-icon" aria-hidden="true">
        {props.icon}
      </span>
      <h2 className="empty-title">{props.title}</h2>
      {props.children && <p className="empty-text">{props.children}</p>}
      {props.actions && <div className="empty-actions">{props.actions}</div>}
    </div>
  );
}

export function Spinner(props: { size?: number }) {
  return <span className="spinner" style={{ width: props.size ?? 16, height: props.size ?? 16 }} aria-hidden="true" />;
}
