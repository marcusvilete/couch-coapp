// Settings → Shortcuts (P3): each shortcut can move to another key; the
// modifiers stay. The engine checks every key (`couch_input::shortcuts`).
import { RotateCcw } from "lucide-react";
import { useEffect, useState } from "react";
import type { ShortcutView } from "../bindings/ShortcutView";
import { errorText, request } from "../engine";
import { Button } from "../ui/Button";
import { Kbd, Skeleton } from "../ui/Feedback";

const SCOPES: Record<ShortcutView["scope"], string> = {
  viewer: "In the stream window",
  host: "Anywhere, while you stream",
};

/** Keys that only modify: pressing one while waiting keeps waiting. */
const MODIFIERS = new Set([
  "ShiftLeft",
  "ShiftRight",
  "ControlLeft",
  "ControlRight",
  "AltLeft",
  "AltRight",
  "MetaLeft",
  "MetaRight",
]);

export function ShortcutsSection(props: {
  onCapturing: (capturing: boolean) => void;
  /** Where this system can't host (M16b), the host's keys don't apply. */
  canHost: boolean;
}) {
  const [shortcuts, setShortcuts] = useState<ShortcutView[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** The row waiting for its new key. */
  const [waiting, setWaiting] = useState<string | null>(null);
  /** The last refusal, by row. */
  const [refused, setRefused] = useState<{ id: string; why: string } | null>(null);
  const { onCapturing } = props;

  useEffect(() => {
    request({ type: "getShortcuts" }, "shortcuts")
      .then((r) => setShortcuts(r.shortcuts))
      .catch((e) => setError(errorText(e)));
  }, []);

  // A capture never outlives the page.
  useEffect(() => () => onCapturing(false), [onCapturing]);

  useEffect(() => {
    onCapturing(waiting != null);
    if (waiting == null) return;
    // Ahead of everything else (the page's Escape among them), and the
    // webview never acts on the key (Alt would open the window menu).
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (e.type !== "keydown" || e.repeat || MODIFIERS.has(e.code)) return;
      if (e.code === "Escape") {
        setWaiting(null);
        return;
      }
      const id = waiting;
      setWaiting(null);
      request({ type: "setShortcut", id, code: e.code }, "shortcuts")
        .then((r) => {
          setShortcuts(r.shortcuts);
          setRefused(null);
        })
        .catch((err) => setRefused({ id, why: errorText(err) }));
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("keyup", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("keyup", onKey, true);
    };
  }, [waiting, onCapturing]);

  const reset = (id: string | null) => {
    setRefused(null);
    request({ type: "resetShortcuts", id }, "shortcuts")
      .then((r) => setShortcuts(r.shortcuts))
      .catch((e) => setError(errorText(e)));
  };

  const groups: ShortcutView["scope"][] = props.canHost ? ["host", "viewer"] : ["viewer"];
  return (
    <div className="settings-section">
      <p className="settings-lead">
        Shortcuts are never sent to the game. A new panic key works from your next session; the stream window's keys
        from the next one you open.
      </p>
      {error && <p className="field-error selectable">{error}</p>}
      {!shortcuts && !error && <Skeleton lines={6} height={18} />}
      {shortcuts &&
        groups.map((scope) => (
          <section key={scope} className="shortcut-group">
            <h3 className="settings-subtitle">{SCOPES[scope]}</h3>
            <ul className="shortcuts">
              {shortcuts
                .filter((s) => s.scope === scope)
                .map((s) => (
                  <li key={s.id} className={waiting === s.id ? "shortcut waiting" : "shortcut"}>
                    <span className="shortcut-text">
                      <span>{s.action}</span>
                      {s.condition && <span className="shortcut-condition">{s.condition}</span>}
                      {waiting === s.id && (
                        <span className="shortcut-prompt" role="status">
                          {s.chord
                            ? "Press the key to use with Ctrl+Alt+Shift (you needn't hold them). Esc cancels."
                            : "Press an F-key. Esc cancels."}
                        </span>
                      )}
                      {refused?.id === s.id && (
                        <span className="shortcut-refused" role="alert">
                          {refused.why}
                        </span>
                      )}
                    </span>
                    <span className="shortcut-keys">
                      {waiting === s.id ? (
                        <span className="kbd-waiting">{s.chord ? "Ctrl+Alt+Shift+ …" : "F-key …"}</span>
                      ) : (
                        <Kbd keys={s.keys} />
                      )}
                    </span>
                    <span className="shortcut-actions">
                      <Button
                        size="sm"
                        disabled={waiting != null}
                        onClick={() => {
                          setRefused(null);
                          setWaiting(s.id);
                        }}
                      >
                        Change
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={waiting != null || s.isDefault}
                        title="Back to its default key"
                        onClick={() => reset(s.id)}
                        icon={<RotateCcw />}
                        aria-label={`${s.action}: back to its default key`}
                      />
                    </span>
                  </li>
                ))}
            </ul>
          </section>
        ))}
      <div className="settings-row">
        <Button onClick={() => reset(null)} disabled={waiting != null || (shortcuts?.every((s) => s.isDefault) ?? true)}>
          Reset all
        </Button>
      </div>
    </div>
  );
}
