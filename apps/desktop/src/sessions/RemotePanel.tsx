// Remote play on a device (M25, ADR 0010): for each member of this room that
// offered it, one of our own PCs or one asking to be, what can be done here.
// First accepting it as ours, once the codes on both screens match: until
// then no PIN ever goes to it. Then unlocking it with its PIN, starting a
// session on it, and locking it again. Its session, once there, shows on the
// stage as a host console run from here.
import { Lock, LockOpen, MonitorX, Play, Router } from "lucide-react";
import { useState, type FormEvent } from "react";
import type { MemberView } from "../bindings/MemberView";
import type { RoomView } from "../bindings/RoomView";
import type { SessionView } from "../bindings/SessionView";
import { errorText, request } from "../engine";
import { lockedText, waitText } from "../model/remote";
import { Button } from "../ui/Button";
import { Callout } from "../ui/Callout";
import { Dialog } from "../ui/Dialog";
import { Field, PasswordInput } from "../ui/Field";
import { useRun, type Notify } from "./useRun";

export function RemotePanel(props: {
  room: RoomView;
  pc: MemberView;
  /** Its session in this room, run from here. */
  session: SessionView | undefined;
  onUnlock: () => void;
  onStart: () => void;
  onRoom: (room: RoomView) => void;
  notify: Notify;
}) {
  const { room, pc } = props;
  const remote = pc.remote;
  const { busy, run } = useRun(props.notify, props.onRoom);
  if (!remote) return null;
  const target = { room: room.id, member: pc.id };

  if (!remote.accepted) {
    return (
      <Callout
        tone="accent"
        icon={<Router />}
        title={`${pc.name} offers to let this device run it from afar`}
        why="A PC you add to its Settings → Remote play offers itself to that device. Only your own PC should: once accepted, this device sends it its PIN when you unlock it. A member who isn't your PC never gets a PIN from here."
        actions={
          <Button
            size="sm"
            variant="primary"
            disabled={busy != null}
            onClick={() => void run("accept", { type: "acceptRemoteHost", member: pc.id })}
          >
            It's my PC
          </Button>
        }
      >
        Only if it's your own PC: its Settings → Remote play shows the code <strong className="tabular">{remote.code}</strong>
        . Check it matches there before you accept.
      </Callout>
    );
  }

  if (!remote.unlocked) {
    return (
      <Callout
        tone="info"
        icon={<Lock />}
        title={`${pc.name}, your PC`}
        actions={
          <>
            <Button size="sm" variant="primary" icon={<LockOpen />} onClick={props.onUnlock}>
              Unlock…
            </Button>
            <Button
              size="sm"
              disabled={busy != null}
              onClick={() => void run("forget", { type: "forgetRemoteHost", member: pc.id })}
            >
              Forget
            </Button>
          </>
        }
      >
        {remote.waitSecs != null
          ? `Too many wrong PINs: the next try can come in ${waitText(remote.waitSecs)}.`
          : "Unlock it with its PIN to run its sessions from here."}
      </Callout>
    );
  }

  return (
    <Callout
      // Windows locked there (M25c): said in the same place, so nothing
      // moves.
      tone={remote.locked ? "warn" : "info"}
      icon={remote.locked ? <MonitorX /> : <Router />}
      title={remote.locked ? `${pc.name} is locked` : `You run ${pc.name} from here`}
      why="Its sessions start, stream and hand out seats as if you were at it. Someone there can stop it with the panic key, which locks this device out until you give the PIN again."
      actions={
        <>
          {!props.session && (
            <Button size="sm" variant="primary" icon={<Play />} onClick={props.onStart} disabled={busy != null}>
              Start a session on it…
            </Button>
          )}
          <Button
            size="sm"
            icon={<Lock />}
            disabled={busy != null}
            onClick={() => void run("lock", { type: "lockRemote", ...target })}
          >
            Lock
          </Button>
        </>
      }
    >
      {remote.locked
        ? lockedText()
        : props.session
          ? "Its session is below."
          : "No session there now."}
    </Callout>
  );
}

/** The PIN of one of our PCs, to run it from here. */
export function UnlockRemoteDialog(props: {
  room: RoomView;
  pc: MemberView;
  onClose: () => void;
  onUnlocked: (room: RoomView) => void;
}) {
  const [pin, setPin] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const wait = props.pc.remote?.waitSecs ?? null;

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (pin.trim() === "") {
      setError("Type the PIN set on that PC.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const room = await request(
        { type: "unlockRemote", room: props.room.id, member: props.pc.id, pin },
        "room",
      );
      props.onUnlocked(room);
    } catch (err) {
      setError(errorText(err));
      setPin("");
      setBusy(false);
    }
  }

  return (
    <Dialog
      title={`Unlock ${props.pc.name}`}
      description="The PIN set in its Settings → Remote play. This device remembers it until the app closes, so a dropped connection unlocks again by itself."
      onClose={props.onClose}
      busy={busy}
      size="s"
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy}>
            Cancel
          </Button>
          <Button type="submit" form="unlock-remote" variant="primary" disabled={busy} labels={["Unlock", "Unlocking…"]}>
            {busy ? "Unlocking…" : "Unlock"}
          </Button>
        </>
      }
    >
      <form id="unlock-remote" onSubmit={submit}>
        <Field
          label="PIN"
          error={error ?? (wait != null ? `Too many wrong PINs: try again in ${waitText(wait)}.` : null)}
          reserveError
        >
          {(id) => <PasswordInput id={id} value={pin} onChange={setPin} autoComplete="off" autoFocus />}
        </Field>
      </form>
    </Dialog>
  );
}
