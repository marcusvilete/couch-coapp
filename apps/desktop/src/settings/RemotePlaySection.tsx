// Settings → Remote play (M25, ADR 0010). On the home PC: turning it on,
// the PIN, and the devices of ours it trusts (added only here, never from
// afar). On a device: its own code, to compare when a PC adds it, and the
// PCs it accepted as its own.
import { Laptop, Router, ShieldAlert, Trash2 } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import type { RemoteDeviceView } from "../bindings/RemoteDeviceView";
import type { RemotePlayView } from "../bindings/RemotePlayView";
import { errorText, request } from "../engine";
import { waitText } from "../model/remote";
import { Button, IconButton } from "../ui/Button";
import { Callout } from "../ui/Callout";
import { Select, Switch } from "../ui/Controls";
import { Badge, Skeleton } from "../ui/Feedback";
import { Check, Field, PasswordInput } from "../ui/Field";
import "./RemotePlaySection.css";

/** As the engine counts it (`couch_engine::remote::MIN_PIN_CHARS`). */
const MIN_PIN = 6;

export function RemotePlaySection() {
  const [view, setView] = useState<RemotePlayView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [working, setWorking] = useState(false);

  useEffect(() => {
    request({ type: "getRemotePlay" }, "remotePlay")
      .then(setView)
      .catch((e) => setError(errorText(e)));
  }, []);

  const send = async (command: Parameters<typeof request>[0]) => {
    setWorking(true);
    setError(null);
    try {
      setView(await request(command, "remotePlay"));
      return true;
    } catch (e) {
      setError(errorText(e));
      return false;
    } finally {
      setWorking(false);
    }
  };

  if (view == null) {
    return (
      <div className="settings-section">
        {error ? <p className="field-error selectable">{error}</p> : <Skeleton lines={4} height={14} />}
      </div>
    );
  }

  return (
    <div className="settings-section">
      <p className="settings-lead">
        Play this PC from a laptop of yours when you're away: a device you add here can start sessions on this PC,
        pick what it streams and hand out its seats, once it gives the PIN. Leave Couch Co-App running, and this PC
        signed in and unlocked: Windows lets nothing in on its lock screen.
      </p>
      <Callout tone="warn" icon={<ShieldAlert />} title="Only your own devices">
        With the PIN, a device you add runs this PC's sessions as you would at it. Add only your own, and choose a
        PIN nobody sees you type. Whoever is at this PC can stop it with the panic key.
      </Callout>

      <h3 className="settings-subtitle">This PC</h3>
      <div className="settings-card">
        <Switch
          checked={view.on}
          disabled={working || !view.pinSet}
          onChange={(on) => void send({ type: "setRemotePlay", on })}
          label="Let my devices run this PC from afar"
          detail={view.pinSet ? "Off locks every device out at once." : "Set a PIN first."}
        />
        <PinForm
          pinSet={view.pinSet}
          working={working || view.desktop != null}
          onSet={(pin) => send({ type: "setRemotePin", pin })}
        />
        {view.desktop != null && (
          // M25b: a device clicking through these settings must not add
          // another or lock you out with a PIN of its own.
          <p className="settings-warn">
            {view.desktop} controls this PC's desktop: adding a device and changing the PIN wait until that ends. The
            panic key ends it.
          </p>
        )}
        {view.waitSecs != null && (
          <p className="settings-warn">
            Wrong PINs were tried: the next try can come in {waitText(view.waitSecs)}.
          </p>
        )}
      </div>

      <h3 className="settings-subtitle">This PC while you're away</h3>
      <div className="settings-card">
        <Switch
          checked={view.startAtSignIn === true}
          disabled={working || view.startAtSignIn == null}
          onChange={(on) => void send({ type: "setStartAtSignIn", on })}
          label="Start Couch Co-App when I sign in to Windows"
          detail={
            view.startAtSignIn == null
              ? "Only an installed Couch Co-App can, for its main profile."
              : "It starts in the notification area, without opening its window."
          }
        />
        <p className="settings-note">
          {view.on
            ? "While remote play is on, closing the window keeps Couch Co-App running in the notification area (Quit there ends it), and this PC doesn't fall asleep on its own (choosing Sleep, or closing a laptop's lid, still works). While a device of yours is unlocked, its screen stays on too."
            : "With remote play on, closing the window keeps Couch Co-App running in the notification area, and this PC doesn't fall asleep on its own."}
          {view.on && view.startAtSignIn === false && " After a restart, start Couch Co-App yourself, or turn on the switch above."}
        </p>
      </div>

      <h3 className="settings-subtitle">My devices</h3>
      <div className="settings-card">
        {view.devices.length === 0 ? (
          <p className="settings-note">No device of yours yet.</p>
        ) : (
          <ul className="remote-devices">
            {view.devices.map((d) => (
              <DeviceRow
                key={d.id}
                device={d}
                action={
                  <IconButton
                    label={`Remove ${d.name}`}
                    size="sm"
                    disabled={working}
                    onClick={() => void send({ type: "removeRemoteDevice", member: d.id })}
                  >
                    <Trash2 />
                  </IconButton>
                }
              />
            ))}
          </ul>
        )}
        <AddDevice
          candidates={view.candidates}
          disabled={working || !view.pinSet || view.desktop != null}
          onAdd={(member) => send({ type: "addRemoteDevice", member })}
        />
      </div>

      <h3 className="settings-subtitle">This device</h3>
      <div className="settings-card">
        <dl className="settings-facts">
          <dt>Its code</dt>
          <dd className="selectable mono">{view.myCode}</dd>
        </dl>
        <p className="settings-note">
          Adding this device on your PC, check its code there reads the same. When your PC then offers itself here,
          accept it in the room once its code matches the one in its own Remote play settings.
        </p>
        {view.hosts.length > 0 && (
          <ul className="remote-devices">
            {view.hosts.map((h) => (
              <DeviceRow
                key={h.id}
                device={h}
                pc
                action={
                  <Button
                    size="sm"
                    disabled={working}
                    onClick={() => void send({ type: "forgetRemoteHost", member: h.id })}
                  >
                    Forget
                  </Button>
                }
              />
            ))}
          </ul>
        )}
      </div>

      {error && (
        <p className="field-error selectable" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

function DeviceRow(props: { device: RemoteDeviceView; pc?: boolean; action: React.ReactNode }) {
  const d = props.device;
  return (
    <li className="remote-device">
      <span className="remote-device-icon" aria-hidden="true">
        {props.pc ? <Router /> : <Laptop />}
      </span>
      <span className="remote-device-text">
        <span className="remote-device-name">{d.name}</span>
        <span className="remote-device-code mono">{d.code}</span>
      </span>
      {d.online ? <Badge tone="ok">Online</Badge> : <Badge>Offline</Badge>}
      {props.action}
    </li>
  );
}

function PinForm(props: { pinSet: boolean; working: boolean; onSet: (pin: string) => Promise<boolean> }) {
  const [open, setOpen] = useState(!props.pinSet);
  const [pin, setPin] = useState("");
  const [again, setAgain] = useState("");
  const [problem, setProblem] = useState<string | null>(null);

  if (!open) {
    return (
      <div className="settings-row">
        <span className="settings-note">A PIN is set.</span>
        <Button size="sm" onClick={() => setOpen(true)} disabled={props.working}>
          Change the PIN…
        </Button>
      </div>
    );
  }

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if ([...pin.trim().normalize("NFKC")].length < MIN_PIN) {
      setProblem(`Use at least ${MIN_PIN} characters.`);
      return;
    }
    if (pin !== again) {
      setProblem("The PINs don't match.");
      return;
    }
    setProblem(null);
    if (await props.onSet(pin)) {
      setPin("");
      setAgain("");
      setOpen(false);
    }
  };

  return (
    <form className="remote-pin" onSubmit={submit}>
      <Field label={props.pinSet ? "New PIN" : "PIN"} hint="Your devices unlock this PC with it. A new one locks them out until they give it.">
        {(id) => <PasswordInput id={id} value={pin} onChange={setPin} autoComplete="new-password" />}
      </Field>
      <Field label="Again" error={problem} reserveError>
        {(id) => <PasswordInput id={id} value={again} onChange={setAgain} autoComplete="new-password" />}
      </Field>
      <div className="settings-row">
        <Button type="submit" variant="primary" size="sm" disabled={props.working}>
          {props.pinSet ? "Change the PIN" : "Set the PIN"}
        </Button>
        {props.pinSet && (
          <Button size="sm" onClick={() => setOpen(false)} disabled={props.working}>
            Cancel
          </Button>
        )}
      </div>
    </form>
  );
}

function AddDevice(props: {
  candidates: RemoteDeviceView[];
  disabled: boolean;
  onAdd: (member: string) => Promise<boolean>;
}) {
  // The one being added is normally online: listed first.
  const [picked, setPicked] = useState<string>(() => props.candidates[0]?.id ?? "");
  const [matches, setMatches] = useState(false);
  const chosen = props.candidates.find((c) => c.id === picked) ?? props.candidates[0];
  if (props.candidates.length === 0) {
    return <p className="settings-note">Add a device by joining one of your rooms with it first.</p>;
  }
  return (
    <div className="remote-add">
      <Field label="Add a device" hint="A member of one of your rooms: your own laptop, signed in as another profile.">
        {(id) => (
          <Select
            id={id}
            value={chosen?.id ?? ""}
            disabled={props.disabled}
            onChange={(v) => {
              setPicked(v);
              setMatches(false);
            }}
            options={props.candidates.map((c) => ({
              value: c.id,
              label: c.name,
              detail: c.online ? "online" : "offline",
            }))}
          />
        )}
      </Field>
      {chosen && (
        <>
          <p className="remote-code">
            Its code: <strong className="mono">{chosen.code}</strong>
          </p>
          <Check checked={matches} onChange={setMatches} disabled={props.disabled}>
            Settings → Remote play on that device shows the same code
          </Check>
        </>
      )}
      <div className="settings-row">
        <Button
          variant="primary"
          size="sm"
          disabled={props.disabled || !chosen || !matches}
          onClick={async () => {
            if (chosen && (await props.onAdd(chosen.id))) setMatches(false);
          }}
        >
          Add it
        </Button>
      </div>
    </div>
  );
}
