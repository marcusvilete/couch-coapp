// Settings: the keyboard shortcuts (each can move to another key) and the
// network (which relay to use, applied when the app restarts).
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState, type FormEvent } from "react";
import type { DriverName } from "./bindings/DriverName";
import type { DriverState } from "./bindings/DriverState";
import type { DriverView } from "./bindings/DriverView";
import type { NetworkView } from "./bindings/NetworkView";
import type { RelaySetting } from "./bindings/RelaySetting";
import type { ShortcutView } from "./bindings/ShortcutView";
import { errorText, request } from "./engine";
import { overlaySettings, setOverlaySettings, type OverlaySettings } from "./overlaySettings";
import type { Busy } from "./sessions";
import { Modal } from "./ui";

export type SettingsSection = "shortcuts" | "overlay" | "network" | "drivers";

export function SettingsDialog(props: { section: SettingsSection; busy: Busy; onClose: () => void }) {
  const [section, setSection] = useState<SettingsSection>(props.section);
  // While a shortcut waits for its key, Escape cancels that, not the dialog.
  const [capturing, setCapturing] = useState(false);
  // The app is closing to start again: nothing closes this meanwhile.
  const [restarting, setRestarting] = useState(false);
  return (
    <Modal
      title="Settings"
      onClose={props.onClose}
      busy={capturing || restarting}
      wide
      footer={
        <button className="primary" onClick={props.onClose} disabled={capturing}>
          Close
        </button>
      }
    >
      <div className="tabs" role="tablist">
        {(
          [
            ["shortcuts", "Shortcuts"],
            ["overlay", "Overlay"],
            ["network", "Network"],
            ["drivers", "Drivers"],
          ] as const
        ).map(([id, label]) => (
          <button
            key={id}
            role="tab"
            aria-selected={section === id}
            className={section === id ? "selected" : undefined}
            disabled={capturing}
            onClick={() => setSection(id)}
          >
            {label}
          </button>
        ))}
      </div>
      {section === "shortcuts" && <ShortcutsSection onCapturing={setCapturing} />}
      {section === "overlay" && <OverlaySection />}
      {section === "network" && <NetworkSection busy={props.busy} onRestarting={setRestarting} />}
      {section === "drivers" && <DriversSection />}
      {restarting && <Restarting />}
    </Modal>
  );
}

/** Covers the app while it closes to start again: telling the rooms we go
 * takes a moment, then the new window opens with the splash. */
function Restarting() {
  return (
    <div className="splash restarting" role="status" aria-live="assertive">
      <div className="restarting-box">
        <span className="restarting-spinner" aria-hidden="true" />
        <strong>Restarting Couch Co-App…</strong>
        <span className="hint">Telling your rooms you're stepping out. The app opens again in a few seconds.</span>
      </div>
    </div>
  );
}

// ---- Shortcuts ----

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

function ShortcutsSection(props: { onCapturing: (capturing: boolean) => void }) {
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

  useEffect(() => {
    onCapturing(waiting != null);
    if (waiting == null) return;
    // Ahead of everything else (the dialog's Escape among them), and the
    // webview never acts on the key (Alt would open the window menu).
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
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

  return (
    <>
      {error && <p className="error">{error}</p>}
      {shortcuts && (
        <table className="shortcuts">
          <tbody>
            {shortcuts.map((s) => (
              <tr key={s.id}>
                <td>
                  {waiting === s.id ? (
                    <kbd className="waiting">{s.chord ? "Ctrl+Alt+Shift+ …" : "F-key …"}</kbd>
                  ) : (
                    <kbd>{s.keys}</kbd>
                  )}
                </td>
                <td>
                  {s.action}
                  <span className="hint">
                    {SCOPES[s.scope]}
                    {s.condition && `, ${s.condition}`}
                  </span>
                  {waiting === s.id && (
                    <span className="hint warn-text">
                      {s.chord
                        ? "Press the key to use with Ctrl+Alt+Shift (you needn't hold them). Esc cancels."
                        : "Press an F-key. Esc cancels."}
                    </span>
                  )}
                  {refused?.id === s.id && (
                    <span className="hint error-text" role="alert">
                      {refused.why}
                    </span>
                  )}
                </td>
                <td className="shortcut-actions">
                  <button
                    onClick={() => {
                      setRefused(null);
                      setWaiting(s.id);
                    }}
                    disabled={waiting != null}
                  >
                    Change
                  </button>
                  {!s.isDefault && (
                    <button onClick={() => reset(s.id)} disabled={waiting != null} title="Back to its default key">
                      Default
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <div className="settings-row">
        <p className="hint">
          Shortcuts are never sent to the game. A new panic key works from your next Go live; the stream window's keys
          from the next one you open.
        </p>
        <button onClick={() => reset(null)} disabled={waiting != null || shortcuts?.every((s) => s.isDefault)}>
          Reset all
        </button>
      </div>
    </>
  );
}

// ---- Overlay ----

function OverlaySection() {
  const [settings, setSettings] = useState<OverlaySettings>(overlaySettings);
  const change = (next: OverlaySettings) => {
    setSettings(next);
    setOverlaySettings(next);
  };
  return (
    <div className="form">
      <p className="hint">
        While you stream, requests for a seat and what happens in the session show in a corner over your game. You
        answer the oldest request with its keys (Shortcuts), without leaving the game. The overlay never takes the
        focus, clicks go through it, and it never shows in your stream.
      </p>
      <label className="check">
        <input type="checkbox" checked={settings.show} onChange={(e) => change({ ...settings, show: e.target.checked })} />
        Show requests and events over the game
      </label>
      <label className="check">
        <input
          type="checkbox"
          checked={settings.sound}
          onChange={(e) => change({ ...settings, sound: e.target.checked })}
        />
        Play a sound when someone asks for a seat
      </label>
      <p className="hint">
        Games in true exclusive full screen draw over everything, so the overlay can't show there; the sound and the
        keys still work. While a game running as administrator is in front, the keys don't reach Couch Co-App: answer in
        the app.
      </p>
    </div>
  );
}

// ---- Drivers ----

const DRIVERS: Record<DriverName, { title: string; why: string }> = {
  vigembus: {
    title: "ViGEmBus: virtual controllers",
    why: "Guests' controllers reach the games you host through it. Without it, guests can watch, and play only with keyboard and mouse.",
  },
  hidhide: {
    title: "HidHide: hides your controller",
    why: "Hides your own controller from games while you stream, so it plays through a seat and never takes a guest's place in co-op games. Without it you play outside the seats.",
  },
};

const STATES: Record<DriverState, string> = {
  ready: "Installed",
  needsRestart: "Installed: restart Windows to finish",
  busy: "Installed (its configuration window is open)",
  missing: "Not installed",
};

function DriversSection() {
  const [drivers, setDrivers] = useState<DriverView[] | null>(null);
  const [installing, setInstalling] = useState<DriverName | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    request({ type: "getDrivers" }, "drivers")
      .then((r) => setDrivers(r.drivers))
      .catch((e) => setError(errorText(e)));
  }, []);

  const install = async (driver: DriverName) => {
    setInstalling(driver);
    setError(null);
    try {
      setDrivers((await request({ type: "installDriver", driver }, "drivers")).drivers);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setInstalling(null);
    }
  };

  return (
    <div className="form">
      <p className="hint">
        Two drivers make the most of Couch Co-App. Both are optional, free and open source; each installs with its own
        setup and Windows asks for administrator rights.
      </p>
      {drivers?.map((d) => (
        <div key={d.driver} className="driver">
          <div>
            <strong>{DRIVERS[d.driver].title}</strong>
            <span className={`hint driver-state ${d.state}`}>{STATES[d.state]}</span>
            <span className="hint">{DRIVERS[d.driver].why}</span>
          </div>
          {d.state === "missing" &&
            (d.installer ? (
              <button className="primary" disabled={installing != null} onClick={() => void install(d.driver)}>
                {installing === d.driver ? "Installing…" : "Install"}
              </button>
            ) : (
              <span className="hint">Its installer isn't in this build (cargo xtask stage-dlls).</span>
            ))}
        </div>
      ))}
      {installing && (
        <p className="hint warn-text" role="status">
          Its setup is open: follow it, and accept Windows' administrator prompt.
        </p>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

// ---- Network ----

/** Served by Vite's dev server (`npm run tauri dev`, and the profiles that
 * `profiles.ps1` starts from the same build). */
const DEV_BUILD = import.meta.env.DEV;

function same(a: RelaySetting | null, b: RelaySetting | null): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/** "use1-1.relay.n0.iroh.link" from its URL. */
function hostOf(url: string): string {
  try {
    return new URL(url).host.replace(/\.$/, "");
  } catch {
    return url;
  }
}

function NetworkSection(props: { busy: Busy; onRestarting: (restarting: boolean) => void }) {
  const [view, setView] = useState<NetworkView | null>(null);
  /** Saving or restarting failed; stays until the next try. */
  const [error, setError] = useState<string | null>(null);
  /** The last poll failed; the next good one clears it. */
  const [loadError, setLoadError] = useState<string | null>(null);
  const [mode, setMode] = useState<"public" | "custom">("public");
  const [url, setUrl] = useState("");
  const [saving, setSaving] = useState(false);
  /** Restarting would end a stream: asked once more first. */
  const [confirmRestart, setConfirmRestart] = useState(false);

  const load = () =>
    request({ type: "getNetwork" }, "network")
      .then((v) => {
        setView(v);
        setLoadError(null);
        return v;
      })
      .catch((e) => {
        setLoadError(errorText(e));
        return null;
      });

  useEffect(() => {
    load().then((v) => {
      if (v && v.saved !== "public") {
        setMode("custom");
        setUrl(v.saved.custom);
      }
    });
    // The home relay shows up a moment after the app starts.
    const timer = window.setInterval(load, 2000);
    return () => window.clearInterval(timer);
  }, []);

  const wanted: RelaySetting = mode === "public" ? "public" : { custom: url.trim() };
  const changed = view != null && !same(wanted, view.saved);
  // The engine compares them as iroh reads URLs, not as typed.
  const pending = view?.restartNeeded ?? false;

  const save = async (e: FormEvent) => {
    e.preventDefault();
    setSaving(true);
    setError(null);
    try {
      await request({ type: "setNetwork", relay: wanted }, "done");
      await load();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setSaving(false);
    }
  };

  const restart = async () => {
    if ((props.busy.hosting || props.busy.watching) && !confirmRestart) {
      setConfirmRestart(true);
      return;
    }
    // Saying goodbye to the rooms takes a moment, and the window freezes
    // while it happens: show that something is going on first, and let it
    // paint before asking.
    props.onRestarting(true);
    await new Promise((painted) => requestAnimationFrame(() => requestAnimationFrame(painted)));
    invoke("restart_app").catch((e) => {
      props.onRestarting(false);
      setError(errorText(e));
    });
  };

  return (
    <form className="form" onSubmit={save}>
      <p className="hint">
        Couch Co-App always tries a direct connection first. A relay server starts each connection and carries it when
        a direct path can't be found.
      </p>
      <fieldset className="input-setup">
        <legend>Relay</legend>
        <label className="check">
          <input type="radio" name="relay" checked={mode === "public"} onChange={() => setMode("public")} />
          iroh's public relays (for testing)
        </label>
        <label className="check">
          <input type="radio" name="relay" checked={mode === "custom"} onChange={() => setMode("custom")} />
          My relay
        </label>
        {mode === "custom" && (
          <input
            type="url"
            placeholder="https://relay.example.com"
            spellCheck={false}
            value={url}
            autoFocus
            onChange={(e) => setUrl(e.target.value)}
            aria-label="Relay address"
          />
        )}
      </fieldset>
      {view && (
        <p className="hint">
          {view.homeRelay ? `Connected through ${hostOf(view.homeRelay)}.` : "Not connected to a relay yet."}
          {view.inUse && view.inUse !== "public" && ` In use: your relay, ${hostOf(view.inUse.custom)}.`}
        </p>
      )}
      {(error ?? loadError) && (
        <p className="error" role="alert">
          {error ?? loadError}
        </p>
      )}
      <div className="settings-row">
        <button type="submit" disabled={!changed || saving || (mode === "custom" && url.trim() === "")}>
          Save
        </button>
        {pending && DEV_BUILD && (
          // `npm run tauri dev` stops with the app, and the page server with
          // it, so a development build can't start itself again.
          <span className="hint warn-text">Applies when the app restarts: close it and start it again.</span>
        )}
        {pending && !DEV_BUILD && (
          <>
            <span className="hint warn-text">
              {confirmRestart ? "Restarting ends what you're streaming or watching." : "Applies when the app restarts."}
            </span>
            <button type="button" className={confirmRestart ? "primary danger" : "primary"} onClick={restart}>
              {confirmRestart ? "Restart anyway" : "Restart now"}
            </button>
          </>
        )}
      </div>
    </form>
  );
}
