// Settings' sections but Shortcuts: the look, the overlay, the network, the
// drivers, a backup of the profile (M13), and About.
import { Download, Monitor, Moon, RotateCw, Sun, Upload } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import type { DriverName } from "../bindings/DriverName";
import type { DriverState } from "../bindings/DriverState";
import type { DriverView } from "../bindings/DriverView";
import type { EngineInfo } from "../bindings/EngineInfo";
import type { NetworkView } from "../bindings/NetworkView";
import type { RelaySetting } from "../bindings/RelaySetting";
import { errorText, request } from "../engine";
import type { Busy } from "../model/sessions";
import { platform } from "../platform";
import { hostOf } from "../shell/Sidebar";
import { overlaySettings, setOverlaySettings, type OverlaySettings } from "../state/overlaySettings";
import type { ThemeChoice } from "../state/theme";
import { Button } from "../ui/Button";
import { Callout } from "../ui/Callout";
import { Segmented, Switch } from "../ui/Controls";
import { Badge, Skeleton } from "../ui/Feedback";
import { Field, PasswordInput, TextArea, TextInput } from "../ui/Field";

// ---- Appearance ----

export function AppearanceSection(props: { theme: ThemeChoice; onTheme: (theme: ThemeChoice) => void }) {
  return (
    <div className="settings-section">
      <Field label="Theme" hint="The title bar follows it too.">
        {() => (
          <Segmented
            label="Theme"
            value={props.theme}
            onChange={props.onTheme}
            options={[
              { value: "system", label: <span className="seg-icon"><Monitor /> Like the system</span> },
              { value: "dark", label: <span className="seg-icon"><Moon /> Dark</span> },
              { value: "light", label: <span className="seg-icon"><Sun /> Light</span> },
            ]}
          />
        )}
      </Field>
      <p className="settings-note">
        Windows' contrast themes (Settings → Accessibility → Contrast themes) take over the colours, whatever you choose
        here.
      </p>
    </div>
  );
}

// ---- Overlay ----

export function OverlaySection() {
  const [settings, setSettings] = useState<OverlaySettings>(overlaySettings);
  const change = (next: OverlaySettings) => {
    setSettings(next);
    setOverlaySettings(next);
  };
  return (
    <div className="settings-section">
      <p className="settings-lead">
        While you stream, requests for a seat and what happens in the session show in a corner over your game. You
        answer the oldest request with its keys (Shortcuts), without leaving the game. The overlay never takes the
        focus, clicks go through it, and it never shows in your stream.
      </p>
      <div className="settings-card">
        <Switch
          checked={settings.show}
          onChange={(show) => change({ ...settings, show })}
          label="Show requests and events over the game"
        />
        <Switch
          checked={settings.sound}
          onChange={(sound) => change({ ...settings, sound })}
          label="Play a sound when someone asks for a seat"
        />
      </div>
      <p className="settings-note">
        Games in true exclusive full screen draw over everything, so the overlay can't show there; the sound and the
        keys still work. While a game running as administrator is in front, the keys don't reach Couch Co-App: answer
        in the app.
      </p>
    </div>
  );
}

// ---- Stream window ----

export function StreamWindowSection() {
  const [bar, setBar] = useState<boolean | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    request({ type: "getStreamWindow" }, "streamWindow")
      .then((v) => setBar(v.bar))
      .catch((e) => setError(errorText(e)));
  }, []);
  const change = (next: boolean) => {
    const was = bar;
    setBar(next);
    setError(null);
    request({ type: "setStreamWindow", bar: next }, "done").catch((e) => {
      setBar(was);
      setError(errorText(e));
    });
  };
  return (
    <div className="settings-section">
      <p className="settings-lead">
        When you watch, the stream window shows the game with a bar over the picture: who plays, asking for a seat,
        the volume. Watching with the stream window on another monitor, you can turn the bar off and use the controls
        under the session here instead: the volume, full screen, Show window, the bar and the stats.
      </p>
      <div className="settings-card">
        <Switch
          checked={bar ?? true}
          disabled={bar == null}
          onChange={change}
          label="Show the session bar over the picture"
          detail="A stream window open now follows at once."
        />
      </div>
      {error && (
        <p className="field-error selectable" role="alert">
          {error}
        </p>
      )}
      <p className="settings-note">
        Without the bar, the stream window still says why it's black (nothing streamed yet, the game closed), and the
        stats show if you turn them on. Its "Show or hide the overlay" shortcut (Shortcuts) hides all of it, the bar
        included.
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

const STATES: Record<DriverState, { text: string; tone: "ok" | "warn" | "neutral" }> = {
  ready: { text: "Installed", tone: "ok" },
  needsRestart: { text: "Installed: restart Windows to finish", tone: "warn" },
  busy: { text: "Installed (its configuration window is open)", tone: "warn" },
  missing: { text: "Not installed", tone: "neutral" },
};

export function DriversSection() {
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
    <div className="settings-section">
      <p className="settings-lead">
        Two drivers make the most of Couch Co-App. Both are optional, free and open source; each installs with its own
        setup and Windows asks for administrator rights.
      </p>
      {!drivers && !error && <Skeleton lines={4} height={16} />}
      {drivers?.map((d) => (
        <div key={d.driver} className="settings-card driver">
          <div className="driver-text">
            <strong>{DRIVERS[d.driver].title}</strong>
            <span>{DRIVERS[d.driver].why}</span>
          </div>
          <div className="driver-state">
            <Badge tone={STATES[d.state].tone}>{STATES[d.state].text}</Badge>
            {d.state === "missing" &&
              (d.installer ? (
                <Button
                  variant="primary"
                  size="sm"
                  disabled={installing != null}
                  labels={["Install", "Installing…"]}
                  onClick={() => void install(d.driver)}
                >
                  {installing === d.driver ? "Installing…" : "Install"}
                </Button>
              ) : (
                <span className="settings-note">Its installer isn't in this build (cargo xtask stage-dlls).</span>
              ))}
          </div>
        </div>
      ))}
      {installing && (
        <Callout tone="info" compact>
          Its setup is open: follow it, and accept Windows' administrator prompt.
        </Callout>
      )}
      {error && (
        <p className="field-error selectable" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

// ---- Network ----

function same(a: RelaySetting | null, b: RelaySetting | null): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

export function NetworkSection(props: { busy: Busy; onRestarting: (restarting: boolean) => void }) {
  const [view, setView] = useState<NetworkView | null>(null);
  /** Saving or restarting failed; stays until the next try. */
  const [error, setError] = useState<string | null>(null);
  /** The last poll failed; the next good one clears it. */
  const [loadError, setLoadError] = useState<string | null>(null);
  const [mode, setMode] = useState<"builtin" | "public" | "custom">("builtin");
  const [url, setUrl] = useState("");
  const [saving, setSaving] = useState(false);
  /** Restarting would end a stream: asked once more first. */
  const [confirmRestart, setConfirmRestart] = useState(false);
  const canRestart = platform().canRestartItself;

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
    void load().then((v) => {
      if (!v) return;
      if (v.saved === "builtin" || v.saved === "public") {
        setMode(v.saved);
      } else {
        setMode("custom");
        setUrl(v.saved.custom);
      }
    });
    // The home relay shows up a moment after the app starts: polled while
    // this page is shown.
    const timer = window.setInterval(load, 2000);
    return () => window.clearInterval(timer);
  }, []);

  const wanted: RelaySetting = mode === "custom" ? { custom: url.trim() } : mode;
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
    platform()
      .restartApp()
      .catch((e) => {
        props.onRestarting(false);
        setError(errorText(e));
      });
  };

  const choice = (value: typeof mode, label: string, detail?: string) => (
    <label className={mode === value ? "radio-card selected" : "radio-card"}>
      <input type="radio" name="relay" checked={mode === value} onChange={() => setMode(value)} />
      <span className="radio-text">
        <span>{label}</span>
        {detail && <span className="radio-detail">{detail}</span>}
      </span>
    </label>
  );

  return (
    <form className="settings-section" onSubmit={save}>
      <p className="settings-lead">
        Couch Co-App always tries a direct connection first. A relay server starts each connection and carries it when
        a direct path can't be found.
      </p>
      <div className="radio-cards" role="radiogroup" aria-label="Relay">
        {choice(
          "builtin",
          view?.builtIn ? `Couch Co-App relay (${hostOf(view.builtIn)})` : "This build's default: iroh's public relays",
          view?.builtIn ? undefined : "This build has no relay of its own.",
        )}
        {choice("public", "iroh's public relays", "For testing")}
        {choice("custom", "My relay")}
      </div>
      {mode === "custom" && (
        <TextInput
          type="url"
          placeholder={view?.testBuild ? "https://relay.example.com, or http://192.168.x.x:3340 on your LAN" : "https://relay.example.com"}
          value={url}
          autoFocus
          onChange={(e) => setUrl(e.target.value)}
          aria-label="Relay address"
        />
      )}
      <p className="settings-note">
        Everyone in a room should use the same choice here: between members on different relays, a connection that
        can't go direct may not get through.
      </p>
      <p className="settings-note">
        {view == null ? (
          <Skeleton width={260} height={12} />
        ) : (
          <>
            {view.homeRelay ? `Connected through ${hostOf(view.homeRelay)}.` : "Not connected to a relay yet."}
            {view.inUse === "builtin" && " In use: the Couch Co-App relay."}
            {view.inUse && typeof view.inUse === "object" && ` In use: your relay, ${hostOf(view.inUse.custom)}.`}
          </>
        )}
      </p>
      {(error ?? loadError) && (
        <p className="field-error selectable" role="alert">
          {error ?? loadError}
        </p>
      )}
      <div className="settings-row">
        <Button type="submit" disabled={!changed || saving || (mode === "custom" && url.trim() === "")}>
          Save
        </Button>
        {pending && !canRestart && (
          // `npm run tauri dev` stops with the app, and the page server with
          // it, so a development build can't start itself again.
          <span className="settings-warn">Applies when the app restarts: close it and start it again.</span>
        )}
        {pending && canRestart && (
          <>
            <span className="settings-warn">
              {confirmRestart ? "Restarting ends what you're streaming or watching." : "Applies when the app restarts."}
            </span>
            <Button variant={confirmRestart ? "danger" : "primary"} icon={<RotateCw />} onClick={() => void restart()}>
              {confirmRestart ? "Restart anyway" : "Restart now"}
            </Button>
          </>
        )}
      </div>
    </form>
  );
}

// ---- Backup (M13) ----

/** The engine's minimum (`backup::MIN_PASSPHRASE_CHARS`), counted after
 * trimming and NFKC, as it counts. */
const MIN_PASSPHRASE = 12;

/** A backup of this profile, sealed with a passphrase, and restoring one
 * into a profile with no rooms (which then starts again). */
export function BackupSection(props: {
  busy: Busy;
  hasRooms: boolean;
  onRestarting: (restarting: boolean) => void;
  /** Restored in a development build, which can't start itself again. */
  restoredDev: boolean;
  onRestoredDev: () => void;
}) {
  const [passphrase, setPassphrase] = useState("");
  const [again, setAgain] = useState("");
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [restorePass, setRestorePass] = useState("");
  const restoredDev = props.restoredDev;

  const tooShort = [...passphrase.trim().normalize("NFKC")].length < MIN_PASSPHRASE;
  const mismatch = passphrase !== again;

  const backUp = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    setSaved(null);
    if (tooShort) {
      setError(`Use at least ${MIN_PASSPHRASE} characters: a few words make a good passphrase.`);
      return;
    }
    if (mismatch) {
      setError("The passphrases don't match.");
      return;
    }
    setWorking(true);
    try {
      // The app asks where, in its own dialog.
      const { path } = await request({ type: "exportProfile", passphrase }, "backup");
      if (path == null) return;
      setSaved(path);
      setPassphrase("");
      setAgain("");
    } catch (err) {
      setError(errorText(err));
    } finally {
      setWorking(false);
    }
  };

  const restore = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    setWorking(true);
    try {
      // The app asks which file, in its own dialog.
      const { path } = await request({ type: "restoreProfile", passphrase: restorePass }, "backup");
      if (path == null) return;
      setRestorePass("");
      if (!platform().canRestartItself) {
        props.onRestoredDev();
        return;
      }
      // The restored identity takes over from the next start.
      props.onRestarting(true);
      await new Promise((painted) => requestAnimationFrame(() => requestAnimationFrame(painted)));
      await platform().restartApp();
    } catch (err) {
      props.onRestarting(false);
      setError(errorText(err));
    } finally {
      setWorking(false);
    }
  };

  return (
    <div className="settings-section">
      <p className="settings-lead">
        A backup takes you to another PC, or back after reinstalling your system: your identity, your rooms and your
        settings, in one file sealed with a passphrase. Rooms you own come back without their password and invites: you
        set a new password there before inviting anyone.
      </p>
      <form className="settings-card backup" onSubmit={backUp}>
        <h3 className="settings-subtitle">Back up</h3>
        <div className="form-row">
          <Field label="Passphrase">
            {(id) => (
              <PasswordInput id={id} autoComplete="new-password" value={passphrase} onChange={setPassphrase} disabled={working} />
            )}
          </Field>
          <Field label="Passphrase again">
            {(id) => <PasswordInput id={id} autoComplete="new-password" value={again} onChange={setAgain} disabled={working} />}
          </Field>
        </div>
        <p className="settings-note">
          The file and its passphrase together are you, in every room: keep them apart, and the passphrase long. Nobody
          can open the file without it, us included.
        </p>
        <div className="settings-row">
          <Button type="submit" variant="primary" icon={<Download />} disabled={working || passphrase === ""}>
            {working ? "Working…" : "Back up to a file…"}
          </Button>
          {saved && <span className="settings-note selectable">Saved to {saved}.</span>}
        </div>
      </form>

      <form className="settings-card backup" onSubmit={restore}>
        <h3 className="settings-subtitle">Restore a backup</h3>
        {props.hasRooms ? (
          <p className="settings-note">
            This profile already has rooms, so a backup can't be restored into it. Restore on a new PC, or into a new
            profile.
          </p>
        ) : (
          <>
            <Field label="The backup's passphrase">
              {(id) => (
                <PasswordInput id={id} autoComplete="current-password" value={restorePass} onChange={setRestorePass} disabled={working} />
              )}
            </Field>
            <Callout tone="warn" compact>
              Use one identity on one PC at a time: once the backup runs here, stop using Couch Co-App with it on the old
              PC. Your rooms catch up with their members before you can invite or ban.
            </Callout>
            {restoredDev ? (
              <p className="settings-warn">Restored. Close the app and start it again to use it.</p>
            ) : (
              <div className="settings-row">
                <Button
                  type="submit"
                  icon={<Upload />}
                  disabled={working || restorePass.trim() === "" || props.busy.hosting}
                  reason={props.busy.hosting ? "End your session first" : null}
                >
                  {working ? "Working…" : "Choose the backup and restore…"}
                </Button>
              </div>
            )}
          </>
        )}
      </form>

      {error && (
        <p className="field-error selectable" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

// ---- About ----

/** The version, the licence, where the source is, and every bundled
 * component's licence (THIRD-PARTY-NOTICES.txt, installed next to the app). */
export function AboutSection() {
  const [info, setInfo] = useState<EngineInfo | null>(null);
  const [notices, setNotices] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    request({ type: "getInfo" }, "info")
      .then(setInfo)
      .catch((e) => setError(errorText(e)));
    request({ type: "getNotices" }, "notices")
      .then((r) => setNotices(r.text))
      .catch((e) => setError(errorText(e)));
  }, []);

  const source = info?.repository
    ? info.commit && !info.commit.endsWith("-dirty")
      ? `${info.repository}/tree/${info.commit}`
      : info.repository
    : null;

  return (
    <div className="settings-section">
      <div className="settings-card about">
        {info ? (
          <p>
            <strong>Couch Co-App {info.appVersion}</strong>{" "}
            <span className="settings-note">{info.commit ? `(commit ${info.commit})` : "(a development build)"}</span>
          </p>
        ) : (
          <Skeleton width={220} height={16} />
        )}
        <p className="settings-note">
          Free software under the GNU General Public License, version 3: you may use, study, share and change it. If you
          share a changed version, it must stay under the same licence, with its source. It comes with no warranty. The
          licence is the LICENSE file next to the app.
        </p>
        <p className="settings-note">
          Its source code: {source ? <span className="mono selectable">{source}</span> : "ask whoever gave you the app."}{" "}
          FFmpeg's source came with the installer, in the files beside it.
        </p>
      </div>
      {error && (
        <p className="field-error selectable" role="alert">
          {error}
        </p>
      )}
      <Field label="Third-party licences">
        {(id) => <TextArea id={id} className="mono notices" readOnly value={notices ?? "Loading…"} rows={14} />}
      </Field>
    </div>
  );
}
