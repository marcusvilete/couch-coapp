// Settings' sections but Shortcuts: the look, the overlay, the
// drivers, a backup of the profile (M13), and About.
import { Download, Monitor, Moon, Sun, Upload } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import type { DriverName } from "../bindings/DriverName";
import type { DriverState } from "../bindings/DriverState";
import type { DriverView } from "../bindings/DriverView";
import type { EngineInfo } from "../bindings/EngineInfo";
import { errorText, request } from "../engine";
import type { Busy } from "../model/sessions";
import { platform } from "../platform";
import { overlaySettings, setOverlaySettings, type OverlaySettings } from "../state/overlaySettings";
import type { ThemeChoice } from "../state/theme";
import { Button } from "../ui/Button";
import { Callout } from "../ui/Callout";
import { Segmented, Switch } from "../ui/Controls";
import { Badge, Skeleton } from "../ui/Feedback";
import { Field, PasswordInput, TextArea } from "../ui/Field";

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

/** `linux`: notifications stand in for the overlay there (M16c). */
export function OverlaySection(props: { linux: boolean }) {
  const [settings, setSettings] = useState<OverlaySettings>(overlaySettings);
  const change = (next: OverlaySettings) => {
    setSettings(next);
    setOverlaySettings(next);
  };
  return (
    <div className="settings-section">
      <p className="settings-lead">
        {props.linux
          ? "While you stream, requests for a seat and what happens in the session show as your desktop's notifications, which never take the focus from your game. You answer the oldest request with its keys (Shortcuts), or in the app."
          : "While you stream, requests for a seat and what happens in the session show in a corner over your game. You answer the oldest request with its keys (Shortcuts), without leaving the game. The overlay never takes the focus, clicks go through it, and it never shows in your stream."}
      </p>
      <div className="settings-card">
        <Switch
          checked={settings.show}
          onChange={(show) => change({ ...settings, show })}
          label={props.linux ? "Show requests and events as notifications" : "Show requests and events over the game"}
        />
        <Switch
          checked={settings.sound}
          onChange={(sound) => change({ ...settings, sound })}
          label="Play a sound when someone asks for a seat"
        />
      </div>
      <p className="settings-note">
        {props.linux
          ? "Guests never hear the sound: Couch Co-App's own sounds are left out of your stream. Your desktop may hide notifications while a game is in full screen, or while Do Not Disturb is on; the sound and the keys still work. When you share a whole screen, guests see the notifications too."
          : "Games in true exclusive full screen draw over everything, so the overlay can't show there; the sound and the keys still work. While a game running as administrator is in front, the keys don't reach Couch Co-App: answer in the app."}
      </p>
    </div>
  );
}

// ---- Drawing on the stream (M27a) ----

export function DrawingSection() {
  const [settings, setSettings] = useState<{ on: boolean; pictures: boolean } | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    request({ type: "getDrawingDefault" }, "drawing")
      .then((v) => setSettings({ on: v.on, pictures: v.pictures }))
      .catch((e) => setError(errorText(e)));
  }, []);
  const change = (next: { on: boolean; pictures: boolean }) => {
    const was = settings;
    setSettings(next);
    setError(null);
    request({ type: "setDrawingDefault", ...next }, "drawing").catch((e) => {
      setSettings(was);
      setError(errorText(e));
    });
  };
  const on = settings?.on ?? null;
  return (
    <div className="settings-section">
      <p className="settings-lead">
        Those watching your stream can draw on the picture in their stream window: point at things with lines,
        arrows, boxes, a laser pointer and pings, and see each other's drawings. You turn it on or off for each
        session in its console, clear the board, and stop one person drawing.
      </p>
      <div className="settings-card">
        <Switch
          checked={on ?? false}
          disabled={on == null}
          onChange={(next) => settings && change({ ...settings, on: next })}
          label="Let those watching draw when a session starts"
          detail="A session running now keeps its own switch."
        />
        <Switch
          checked={settings?.pictures ?? false}
          disabled={settings == null}
          onChange={(next) => settings && change({ ...settings, pictures: next })}
          label="Let them put pictures on the board too"
          detail="Pasted or dropped onto the stream: each made smaller, and what a photo carries, such as where it was taken, left out."
        />
      </div>
      {error && (
        <p className="field-error selectable" role="alert">
          {error}
        </p>
      )}
      <p className="settings-note">
        Whoever has the keyboard and mouse can't draw: those are your game's. Drawings are never kept: they go when
        the stream ends.
      </p>
    </div>
  );
}

// ---- Streaming (M29) ----

export function StreamingSection() {
  const [hdr, setHdr] = useState<boolean | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    request({ type: "getStreaming" }, "streaming")
      .then((v) => setHdr(v.hdr))
      .catch((e) => setError(errorText(e)));
  }, []);
  const change = (next: boolean) => {
    const was = hdr;
    setHdr(next);
    setError(null);
    request({ type: "setStreaming", hdr: next }, "streaming").catch((e) => {
      setHdr(was);
      setError(errorText(e));
    });
  };
  return (
    <div className="settings-section">
      <p className="settings-lead">
        On a monitor with HDR turned on, Windows' own capture makes games in HDR look washed out to those watching:
        light greys, whites and everything brighter all turn the same white. Couch Co-App can capture the HDR picture
        itself and turn it into the colours their screens show.
      </p>
      <div className="settings-card">
        <Switch
          checked={hdr ?? true}
          disabled={hdr == null}
          onChange={change}
          label="Convert the picture of a monitor in HDR"
          detail="From the next stream you start. Monitors without HDR are captured as before."
        />
      </div>
      {error && (
        <p className="field-error selectable" role="alert">
          {error}
        </p>
      )}
      <p className="settings-note">
        Streams stay SDR: what is brighter than your monitor's SDR content brightness shows as white. Turn this off
        only if colours look wrong with it on.
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
  uinput: {
    title: "Virtual controllers",
    why: "Guests' controllers reach the games you host as virtual Xbox 360 controllers, and a device of yours run from afar controls the desktop through a virtual keyboard and mouse. Setting it up lets the programs you run make input devices, as Steam's controller support does. Without it, guests can watch only.",
  },
  gnomeextension: {
    title: "Keyboard and mouse for guests (GNOME)",
    why: "A small GNOME extension of ours tells Couch Co-App which window is in front, so guests' keyboard and mouse reach your game only while it is. It only reports: it never moves or changes a window. GNOME reads extensions when you log in, so after installing it, log out and back in once. KDE Plasma needs nothing.",
  },
};

/** The Linux setup by hand, as Set up does it (`couch_platform_linux::pads`,
 * whose test checks this is its `set_up_by_hand`). */
const UINPUT_BY_HAND =
  "sudo modprobe uinput; echo 'KERNEL==\"uinput\", SUBSYSTEM==\"misc\", OPTIONS+=\"static_node=uinput\", TAG+=\"uaccess\"' | sudo tee /etc/udev/rules.d/60-couch-coapp-uinput.rules && sudo udevadm control --reload-rules && sudo udevadm trigger --settle --name-match=uinput";

const STATES: Record<DriverState, { text: string; tone: "ok" | "warn" | "neutral" }> = {
  ready: { text: "Installed", tone: "ok" },
  needsRestart: { text: "Installed: restart Windows to finish", tone: "warn" },
  busy: { text: "Installed (its configuration window is open)", tone: "warn" },
  needsLogout: { text: "Installed: log out and back in to finish", tone: "warn" },
  off: { text: "Installed, turned off", tone: "neutral" },
  extensionsOff: { text: "GNOME's extensions are turned off (its Extensions app)", tone: "warn" },
  unsupported: { text: "Installed, but this GNOME can't run it", tone: "warn" },
  outdated: { text: "An older version is installed", tone: "warn" },
  missing: { text: "Not installed", tone: "neutral" },
};

/** A setup button's label, and while it runs. */
function installLabels(d: DriverView): [string, string] {
  if (d.driver === "uinput") return ["Set up", "Setting up…"];
  if (d.state === "off") return ["Turn on", "Turning on…"];
  if (d.state === "outdated") return ["Update", "Updating…"];
  return ["Install", "Installing…"];
}

/** `linux`: one setting there, not drivers (M16c): Set up asks for the
 * administrator's password and installs one udev rule. */
export function DriversSection(props: { linux: boolean }) {
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
      // A setup can fail after doing its part (on Linux, the rule installed
      // but not yet applied): the cards show what's there now.
      await request({ type: "getDrivers" }, "drivers")
        .then((r) => setDrivers(r.drivers))
        .catch(() => {});
    } finally {
      setInstalling(null);
    }
  };

  return (
    <div className="settings-section">
      <p className="settings-lead">
        {props.linux
          ? "Guests' controllers reach the games you host once virtual controllers are set up, with your administrator password. On GNOME, an extension lets their keyboard and mouse reach the game too."
          : "Two drivers make the most of Couch Co-App. Both are optional, free and open source; each installs with its own setup and Windows asks for administrator rights."}
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
            {(d.state === "missing" || d.state === "off" || d.state === "outdated") &&
              (d.installer ? (
                <Button
                  variant="primary"
                  size="sm"
                  disabled={installing != null}
                  labels={installLabels(d)}
                  onClick={() => void install(d.driver)}
                >
                  {installLabels(d)[installing === d.driver ? 1 : 0]}
                </Button>
              ) : (
                <span className="settings-note">Its installer isn't in this build (cargo xtask stage-dlls).</span>
              ))}
          </div>
        </div>
      ))}
      {installing && (
        <Callout tone="info" compact>
          {installing === "gnomeextension"
            ? "Installing the extension…"
            : props.linux
              ? "Your system asks for the administrator password."
            : "Its setup is open: follow it, and accept Windows' administrator prompt."}
        </Callout>
      )}
      {props.linux && drivers?.some((d) => d.driver === "uinput" && d.state === "missing") && (
        <p className="settings-note">
          Or in a terminal: <code className="selectable">{UINPUT_BY_HAND}</code>
        </p>
      )}
      {error && (
        <p className="field-error selectable" role="alert">
          {error}
        </p>
      )}
    </div>
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
