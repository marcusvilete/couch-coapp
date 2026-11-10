// Settings → Shortcuts (P3): each shortcut can move to another key; the
// modifiers stay. The engine checks every key (`couch_input::shortcuts`).
import { RotateCcw } from "lucide-react";
import { useEffect, useState } from "react";
import type { DrawingKeyView } from "../bindings/DrawingKeyView";
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

const DESKTOP_HOLDS = "Your desktop holds this key: change it in its settings";

export function ShortcutsSection(props: {
  onCapturing: (capturing: boolean) => void;
  /** Where this system can't host (M16b), the host's keys don't apply. */
  canHost: boolean;
  /** Linux (M16c): the desktop binds the host's keys, offered these as
   * Ctrl+Alt+Shift chords, and keeps what it bound: changed here, they
   * wouldn't reach it. */
  linux: boolean;
}) {
  const desktopHolds = (s: ShortcutView) => props.linux && s.scope === "host";
  const [shortcuts, setShortcuts] = useState<ShortcutView[] | null>(null);
  /** The drawing editor's keys (M27a): fixed, listed only. */
  const [drawingKeys, setDrawingKeys] = useState<DrawingKeyView[]>([]);
  const [error, setError] = useState<string | null>(null);
  /** The row waiting for its new key. */
  const [waiting, setWaiting] = useState<string | null>(null);
  /** The last refusal, by row. */
  const [refused, setRefused] = useState<{ id: string; why: string } | null>(null);
  const { onCapturing } = props;

  useEffect(() => {
    request({ type: "getShortcuts" }, "shortcuts")
      .then((r) => {
        setShortcuts(r.shortcuts);
        setDrawingKeys(r.drawingKeys);
      })
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
  // The rows Reset all may change: on Linux not the ones the desktop holds.
  const resettable = (shortcuts ?? []).filter((s) => !desktopHolds(s) && !s.isDefault);
  const resetAll = async () => {
    if (!props.linux) return reset(null);
    setRefused(null);
    try {
      let latest: ShortcutView[] | null = null;
      for (const s of resettable) latest = (await request({ type: "resetShortcuts", id: s.id }, "shortcuts")).shortcuts;
      if (latest) setShortcuts(latest);
    } catch (e) {
      setError(errorText(e));
    }
  };

  const groups: ShortcutView["scope"][] = props.canHost ? ["host", "viewer"] : ["viewer"];
  return (
    <div className="settings-section">
      <p className="settings-lead">
        {props.linux
          ? "Shortcuts are never sent to the game. A new key for the stream window works from the next one you open."
          : "Shortcuts are never sent to the game. A new panic key works from your next session; the stream window's keys from the next one you open."}
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
                        disabled={waiting != null || desktopHolds(s)}
                        reason={desktopHolds(s) ? DESKTOP_HOLDS : null}
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
                        disabled={waiting != null || s.isDefault || desktopHolds(s)}
                        title="Back to its default key"
                        onClick={() => reset(s.id)}
                        icon={<RotateCcw />}
                        aria-label={`${s.action}: back to its default key`}
                      />
                    </span>
                  </li>
                ))}
            </ul>
            {scope === "host" && props.linux && (
              <p className="settings-note">
                Your desktop binds these the first time you go live, offered as shown, and keeps them from then on: change
                them in its own settings (on KDE, System Settings → Keyboard → Shortcuts → Couch Co-App). The host console
                shows the keys it bound.
              </p>
            )}
          </section>
        ))}
      <div className="settings-row">
        <Button onClick={() => void resetAll()} disabled={waiting != null || resettable.length === 0}>
          Reset all
        </Button>
      </div>
      {shortcuts && drawingKeys.length > 0 && (
        <section className="shortcut-group">
          <h3 className="settings-subtitle">While drawing in the stream window</h3>
          <p className="settings-note">
            The board's own keys, as in Excalidraw: like a text box's, they can't be moved.
          </p>
          <ul className="shortcuts">
            {drawingKeys.map((k) => (
              <li key={k.keys} className="shortcut">
                <span className="shortcut-text">
                  <span>{k.action}</span>
                </span>
                <span className="shortcut-keys drawing-keys">
                  {k.keys.split(", ").map((keys) => (
                    <Kbd key={keys} keys={keys} />
                  ))}
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}
