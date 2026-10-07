// Small building blocks shared by the lobby's screens.
import { useEffect, useRef, useState, type ReactNode } from "react";

/** A dialog over the app. Escape and the backdrop close it unless `busy`. */
export function Modal(props: {
  title: string;
  onClose: () => void;
  busy?: boolean;
  /** Room for a table, such as Settings'. */
  wide?: boolean;
  children: ReactNode;
  footer?: ReactNode;
}) {
  const { title, onClose, busy, wide, children, footer } = props;
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !busy) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, onClose]);
  useEffect(() => {
    // Focus the first field, or the dialog itself.
    const first = box.current?.querySelector<HTMLElement>("input, textarea, button.primary");
    (first ?? box.current)?.focus();
  }, []);
  return (
    <div
      className="backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget && !busy) onClose();
      }}
    >
      <div
        className={wide ? "modal wide" : "modal"}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        ref={box}
        tabIndex={-1}
      >
        <h2>{title}</h2>
        <div className="modal-body">{children}</div>
        {footer && <div className="modal-footer">{footer}</div>}
      </div>
    </div>
  );
}

/** A round badge with a name's first letters. */
export function Avatar(props: { name: string; size?: "small" | "large"; title?: string }) {
  return (
    <span className={`avatar ${props.size ?? "small"}`} title={props.title} aria-hidden="true">
      {initials(props.name)}
    </span>
  );
}

export function initials(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean);
  const letters = words.length > 1 ? [words[0], words[1]] : [words[0] ?? "?"];
  return letters
    .map((w) => Array.from(w)[0] ?? "")
    .join("")
    .toUpperCase();
}

/** Names travel as UTF-8, at most this many bytes (`couch_proto::room`). */
export const MAX_NAME_BYTES = 96;

/** `text` cut to at most `max` UTF-8 bytes, between characters, with an
 * ellipsis if anything was cut. */
export function truncateBytes(text: string, max: number = MAX_NAME_BYTES): string {
  const encoder = new TextEncoder();
  if (encoder.encode(text).length <= max) return text;
  const ellipsis = "…";
  let room = max - encoder.encode(ellipsis).length;
  let out = "";
  for (const char of text) {
    const size = encoder.encode(char).length;
    if (size > room) break;
    out += char;
    room -= size;
  }
  return out.trimEnd() + ellipsis;
}

export function nameProblem(name: string): string | null {
  if (name.trim() === "") return "Type a name.";
  if (new TextEncoder().encode(name.trim()).length > MAX_NAME_BYTES) return "That name is too long.";
  return null;
}

/** "3 days", "5 hours", "12 minutes", "40 seconds". */
export function duration(secs: number): string {
  const units: [number, string][] = [
    [86400, "day"],
    [3600, "hour"],
    [60, "minute"],
  ];
  for (const [size, unit] of units) {
    if (secs >= size) {
      const n = Math.round(secs / size);
      return `${n} ${unit}${n === 1 ? "" : "s"}`;
    }
  }
  const n = Math.max(1, Math.round(secs));
  return `${n} second${n === 1 ? "" : "s"}`;
}

/** A password field with its own show/hide button. WebView2's built-in
 * reveal button hides for good once the field loses focus, so it is turned
 * off in the CSS. */
export function PasswordInput(props: {
  value: string;
  onChange: (value: string) => void;
  autoComplete?: string;
  disabled?: boolean;
  autoFocus?: boolean;
}) {
  const [shown, setShown] = useState(false);
  return (
    <span className="password">
      <input
        type={shown ? "text" : "password"}
        autoComplete={props.autoComplete ?? "off"}
        spellCheck={false}
        value={props.value}
        disabled={props.disabled}
        autoFocus={props.autoFocus}
        onChange={(e) => props.onChange(e.target.value)}
      />
      <button
        type="button"
        className="icon reveal"
        aria-label={shown ? "Hide password" : "Show password"}
        aria-pressed={shown}
        title={shown ? "Hide password" : "Show password"}
        onClick={() => setShown(!shown)}
      >
        {shown ? <EyeOffIcon /> : <EyeIcon />}
      </button>
    </span>
  );
}

function EyeIcon() {
  return (
    <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
      <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z" fill="none" stroke="currentColor" strokeWidth="2" />
      <circle cx="12" cy="12" r="3" fill="none" stroke="currentColor" strokeWidth="2" />
    </svg>
  );
}

function EyeOffIcon() {
  return (
    <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
      <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z" fill="none" stroke="currentColor" strokeWidth="2" />
      <circle cx="12" cy="12" r="3" fill="none" stroke="currentColor" strokeWidth="2" />
      <path d="M3 3l18 18" stroke="currentColor" strokeWidth="2" />
    </svg>
  );
}

export function PlusIcon() {
  return (
    <svg viewBox="0 0 24 24" width="22" height="22" aria-hidden="true">
      <path d="M12 4v16M4 12h16" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" />
    </svg>
  );
}
