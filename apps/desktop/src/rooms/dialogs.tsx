// The rooms' dialogs. Passwords and invite links live only in these
// components' state, and go when the dialog closes.
import { Check as CheckIcon, Clock, Copy, DoorOpen, Link2, Plus, TriangleAlert } from "lucide-react";
import { useEffect, useRef, useState, type FormEvent } from "react";
import type { InvitePreview } from "../bindings/InvitePreview";
import type { ProfileView } from "../bindings/ProfileView";
import type { RoomView } from "../bindings/RoomView";
import { errorText, request } from "../engine";
import { takesOfferItself } from "../state/openedLink";
import { Avatar } from "../ui/Avatar";
import { Button } from "../ui/Button";
import { Callout } from "../ui/Callout";
import { Dialog } from "../ui/Dialog";
import { Badge, Skeleton } from "../ui/Feedback";
import { Field, PasswordInput, TextArea, TextInput } from "../ui/Field";
import { duration, nameProblem } from "../ui/text";
import "./dialogs.css";

/** "Add a room": create one, or join with an invite. */
export function AddRoomDialog(props: { onClose: () => void; onCreate: () => void; onJoin: () => void }) {
  return (
    <Dialog title="Add a room" onClose={props.onClose} size="m">
      <ChoiceCards onCreate={props.onCreate} onJoin={props.onJoin} />
    </Dialog>
  );
}

export function ChoiceCards(props: { onCreate: () => void; onJoin: () => void }) {
  return (
    <div className="choice-cards">
      <button className="choice-card primary" onClick={props.onCreate}>
        <span className="choice-icon">
          <Plus />
        </span>
        <strong>Create a room</strong>
        <span>You'll invite friends with a link and a password.</span>
      </button>
      <button className="choice-card" onClick={props.onJoin}>
        <span className="choice-icon">
          <Link2 />
        </span>
        <strong>Join with an invite</strong>
        <span>Paste the link a friend sent you.</span>
      </button>
    </div>
  );
}

function passwordProblem(password: string, again: string): string | null {
  return (password.trim() === "" ? "Choose a password." : null) ?? (password !== again ? "The passwords don't match." : null);
}

export function CreateRoomDialog(props: { onClose: () => void; onCreated: (room: RoomView) => void }) {
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [again, setAgain] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(e: FormEvent) {
    e.preventDefault();
    const problem = nameProblem(name) ?? passwordProblem(password, again);
    if (problem) {
      setError(problem);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      props.onCreated(await request({ type: "createRoom", name: name.trim(), password }, "room"));
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  }

  return (
    <Dialog
      title="Create a room"
      description="A room is a group of friends. One of you streams a game, and hands the controller around."
      onClose={props.onClose}
      busy={busy}
      size="m"
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy}>
            Cancel
          </Button>
          <Button type="submit" form="create-room" variant="primary" disabled={busy} labels={["Create room", "Creating…"]}>
            {busy ? "Creating…" : "Create room"}
          </Button>
        </>
      }
    >
      <form id="create-room" onSubmit={submit} className="form">
        <Field label="Room name">
          {(id) => <TextInput id={id} value={name} onChange={(e) => setName(e.target.value)} placeholder="Friday games" />}
        </Field>
        <div className="form-row">
          <Field label="Password">
            {(id) => <PasswordInput id={id} autoComplete="new-password" value={password} onChange={setPassword} />}
          </Field>
          <Field label="Password again">
            {(id) => <PasswordInput id={id} autoComplete="new-password" value={again} onChange={setAgain} />}
          </Field>
        </div>
        <p className="form-hint">
          Friends need an invite link from you and this password. Only you let people in, so they can join while your
          Couch Co-App is running. The password isn't saved anywhere: note it down. You can change it later from the
          room's menu.
        </p>
        <p className="field-error" role={error ? "alert" : undefined}>
          {error ?? " "}
        </p>
      </form>
    </Dialog>
  );
}

const STATUS: Record<InvitePreview["status"], { tone: "ok" | "warn" | "info"; text: string }> = {
  ready: { tone: "ok", text: "Valid" },
  expired: { tone: "warn", text: "Expired" },
  member: { tone: "info", text: "You're in it" },
  own: { tone: "warn", text: "Your own room" },
};

export function JoinRoomDialog(props: {
  initialInvite: string;
  /** A link opened while this dialog is up (any web page can open one). */
  offered: string | null;
  onTakeOffer: () => void;
  onClose: () => void;
  onJoined: (room: RoomView) => void;
  onOpenRoom: (room: string) => void;
}) {
  const [invite, setInvite] = useState(props.initialInvite);
  const [preview, setPreview] = useState<InvitePreview | null>(null);
  const [checking, setChecking] = useState(false);
  const [inviteError, setInviteError] = useState<string | null>(null);
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const takeOffer = () => {
    if (props.offered == null) return;
    setInvite(props.offered);
    props.onTakeOffer();
  };
  // An offered link is taken only while nothing is typed or under way;
  // otherwise the user is asked, so a link can't slip in under a password
  // typed for another room.
  useEffect(() => {
    if (props.offered != null && takesOfferItself({ busy, password })) takeOffer();
    // Only when a link arrives, not on every keystroke.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.offered]);

  useEffect(() => {
    // A password belongs to the invite it was typed for.
    setPassword("");
    setError(null);
    setPreview(null);
    setInviteError(null);
    if (invite.trim() === "") {
      setChecking(false);
      return;
    }
    setChecking(true);
    let live = true;
    const timer = window.setTimeout(() => {
      request({ type: "previewInvite", invite }, "invitePreview")
        .then((p) => live && setPreview(p))
        .catch((e) => live && setInviteError(errorText(e)))
        .finally(() => live && setChecking(false));
    }, 150);
    return () => {
      live = false;
      window.clearTimeout(timer);
    };
  }, [invite]);

  const ready = preview?.status === "ready";
  // The field is always there (no jump), so it takes the focus itself once
  // the invite checks out.
  const passwordField = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ready) passwordField.current?.focus();
  }, [ready]);

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (!ready) return;
    if (password.trim() === "") {
      setError("Type the room's password.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      props.onJoined(await request({ type: "joinRoom", invite, password }, "room"));
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  }

  return (
    <Dialog
      title="Join a room"
      description="The room's owner lets you in, so they need to be online."
      onClose={props.onClose}
      busy={busy}
      size="m"
      fixed
      pinned={
        props.offered != null &&
        !busy && (
          <Callout tone="warn" compact actions={<Button size="sm" onClick={takeOffer}>Use it instead</Button>}>
            Another invite link was just opened.
          </Callout>
        )
      }
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy}>
            Cancel
          </Button>
          {preview?.status === "member" ? (
            <Button variant="primary" onClick={() => props.onOpenRoom(preview.room)} icon={<DoorOpen />}>
              Open room
            </Button>
          ) : (
            <Button
              type="submit"
              form="join-room"
              variant="primary"
              disabled={busy || !ready}
              labels={["Join", "Asking the owner…"]}
            >
              {busy ? "Asking the owner…" : "Join"}
            </Button>
          )}
        </>
      }
    >
      <form id="join-room" onSubmit={submit} className="form">
        <Field label="Invite link">
          {(id) => (
            <TextArea
              id={id}
              className="mono"
              rows={3}
              autoComplete="off"
              value={invite}
              disabled={busy}
              onChange={(e) => setInvite(e.target.value)}
              placeholder="couchco://join#… or https://…/j/#…"
            />
          )}
        </Field>
        <div className="invite-preview" aria-live="polite">
          {preview ? (
            <>
              <Avatar id={preview.room} name={preview.name} square size={40} />
              <div className="invite-preview-text">
                <span className="invite-preview-name">{preview.name}</span>
                <span className="invite-preview-detail">
                  {preview.status === "ready" && (
                    <>
                      <Clock aria-hidden="true" /> Works for about {duration(preview.expiresInSecs)} more, if not revoked.
                    </>
                  )}
                  {preview.status === "expired" && "This invite has expired. Ask the owner for a new one."}
                  {preview.status === "member" && "You're already in this room."}
                  {preview.status === "own" && "This is an invite to your own room."}
                </span>
              </div>
              <Badge tone={STATUS[preview.status].tone}>{STATUS[preview.status].text}</Badge>
            </>
          ) : checking ? (
            <>
              <span className="invite-preview-blank" />
              <div className="invite-preview-text">
                <Skeleton width={160} height={14} />
                <Skeleton width={240} height={11} />
              </div>
            </>
          ) : inviteError ? (
            <>
              <span className="invite-preview-blank warn">
                <TriangleAlert />
              </span>
              <span className="invite-preview-detail selectable">{inviteError}</span>
            </>
          ) : (
            <>
              <span className="invite-preview-blank">
                <Link2 />
              </span>
              <span className="invite-preview-detail">Paste a link to see where it leads.</span>
            </>
          )}
        </div>
        <Field label="Password" error={error} reserveError>
          {(id) => (
            <PasswordInput
              id={id}
              inputRef={passwordField}
              value={password}
              disabled={busy || !ready}
              onChange={setPassword}
            />
          )}
        </Field>
      </form>
    </Dialog>
  );
}

export function InviteDialog(props: { room: RoomView; onClose: () => void }) {
  const [link, setLink] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    let live = true;
    request({ type: "getInvite", room: props.room.id }, "invite")
      .then((r) => live && setLink(r.link))
      .catch((e) => live && setError(errorText(e)));
    return () => {
      live = false;
    };
  }, [props.room.id]);

  async function copy() {
    if (link == null) return;
    try {
      await navigator.clipboard.writeText(link);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      setError("Couldn't copy. Select the link and press Ctrl+C.");
    }
  }

  return (
    <Dialog
      title={`Invite people to ${props.room.name}`}
      description="Send the link and, separately, the password."
      onClose={props.onClose}
      size="m"
      footer={
        <>
          <Button onClick={props.onClose}>Done</Button>
          <Button
            variant="primary"
            onClick={copy}
            disabled={link == null}
            icon={copied ? <CheckIcon /> : <Copy />}
            labels={["Copy link", "Copied"]}
          >
            {copied ? "Copied" : "Copy link"}
          </Button>
        </>
      }
    >
      <div className="form">
        <div className="invite-link">
          {link != null ? (
            <TextArea className="mono" rows={4} readOnly value={link} onFocus={(e) => e.target.select()} aria-label="Invite link" />
          ) : (
            <div className="invite-link-loading" role="status">
              <span className="form-hint">Making a link…</span>
              <Skeleton lines={2} height={13} />
            </div>
          )}
        </div>
        <p className="form-hint">
          Anyone with both can join for a week, until five wrong passwords, or until you revoke invites. Treat the link
          like half of the password.
        </p>
        <p className="field-error" role={error ? "alert" : undefined}>
          {error ?? " "}
        </p>
      </div>
    </Dialog>
  );
}

/** A dialog asking before doing something that can't be undone. */
export type Confirm = {
  title: string;
  body: string;
  action: string;
  danger?: boolean;
  run: () => Promise<unknown>;
};

export function ConfirmDialog(props: Confirm & { onClose: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function go() {
    setBusy(true);
    setError(null);
    try {
      await props.run();
      props.onClose();
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  }

  return (
    <Dialog
      title={props.title}
      onClose={props.onClose}
      busy={busy}
      size="s"
      footer={
        <>
          {/* Cancel first in focus where it destroys something: Enter never
              bans or closes a room by accident. */}
          <Button onClick={props.onClose} disabled={busy} data-autofocus={props.danger ? true : undefined}>
            Cancel
          </Button>
          <Button variant={props.danger ? "danger" : "primary"} onClick={go} disabled={busy}>
            {props.action}
          </Button>
        </>
      }
    >
      <p className="confirm-body">{props.body}</p>
      {error && (
        <p className="field-error selectable" role="alert">
          {error}
        </p>
      )}
    </Dialog>
  );
}

export function NameDialog(props: { profile: ProfileView; onClose: () => void; onSaved: (profile: ProfileView) => void }) {
  return (
    <Dialog title="Your name" description="Everyone in your rooms sees this name." onClose={props.onClose} size="s">
      <NameForm profile={props.profile} onSaved={props.onSaved} onCancel={props.onClose} />
    </Dialog>
  );
}

/** Your name, in its dialog and in Settings → Profile. */
export function NameForm(props: { profile: ProfileView; onSaved: (profile: ProfileView) => void; onCancel?: () => void }) {
  const [name, setName] = useState(props.profile.displayName);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(e: FormEvent) {
    e.preventDefault();
    const problem = nameProblem(name);
    if (problem) {
      setError(problem);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      props.onSaved(await request({ type: "setDisplayName", name: name.trim() }, "profile"));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form onSubmit={submit} className="form">
      <Field label="Display name" error={error} reserveError>
        {(id) => <TextInput id={id} value={name} onChange={(e) => setName(e.target.value)} />}
      </Field>
      <div className="form-actions">
        {props.onCancel && (
          <Button onClick={props.onCancel} disabled={busy}>
            Cancel
          </Button>
        )}
        <Button type="submit" variant="primary" disabled={busy || name.trim() === props.profile.displayName}>
          Save
        </Button>
      </div>
    </form>
  );
}

/** The owner offers us the room (M13): accept with the password newcomers
 * will need, which we choose. */
export function AcceptRoomDialog(props: {
  room: RoomView;
  ownerName: string;
  onClose: () => void;
  onAccepted: (room: RoomView) => void;
}) {
  const [password, setPassword] = useState("");
  const [again, setAgain] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(e: FormEvent) {
    e.preventDefault();
    const problem = passwordProblem(password, again);
    if (problem) {
      setError(problem);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      props.onAccepted(await request({ type: "answerOffer", room: props.room.id, accept: true, password }, "room"));
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  }

  return (
    <Dialog
      title={`Take over ${props.room.name}?`}
      onClose={props.onClose}
      busy={busy}
      size="m"
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy}>
            Cancel
          </Button>
          <Button type="submit" form="accept-room" variant="primary" disabled={busy}>
            Take over the room
          </Button>
        </>
      }
    >
      <form id="accept-room" onSubmit={submit} className="form">
        <p className="form-lead">
          {props.ownerName} offers you the room. As its owner, you let people in with invites, you can ban, and leaving
          closes the room for everyone. {props.ownerName} stays as a member; the invites they sent stop working.
        </p>
        <div className="form-row">
          <Field label="Password for newcomers">
            {(id) => <PasswordInput id={id} autoComplete="new-password" value={password} onChange={setPassword} autoFocus />}
          </Field>
          <Field label="Password again">
            {(id) => <PasswordInput id={id} autoComplete="new-password" value={again} onChange={setAgain} />}
          </Field>
        </div>
        <p className="form-hint">Members already in aren't affected. The password isn't saved anywhere: note it down.</p>
        <p className="field-error" role={error ? "alert" : undefined}>
          {error ?? " "}
        </p>
      </form>
    </Dialog>
  );
}

/** Changing a room's password, or setting one where it has none (restored
 * from a backup, or handed to us after a restart: M13). */
export function PasswordDialog(props: { room: RoomView; onClose: () => void; onChanged: () => void }) {
  const [password, setPassword] = useState("");
  const [again, setAgain] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const setting = props.room.needsPassword;

  async function submit(e: FormEvent) {
    e.preventDefault();
    const problem = passwordProblem(password, again);
    if (problem) {
      setError(problem);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await request({ type: "changePassword", room: props.room.id, password }, "done");
      props.onChanged();
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  }

  return (
    <Dialog
      title={setting ? `Set a password for ${props.room.name}` : `Change the password of ${props.room.name}`}
      onClose={props.onClose}
      busy={busy}
      size="m"
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy}>
            Cancel
          </Button>
          <Button type="submit" form="change-password" variant="primary" disabled={busy}>
            {setting ? "Set password" : "Change password"}
          </Button>
        </>
      }
    >
      <form id="change-password" onSubmit={submit} className="form">
        <div className="form-row">
          <Field label="New password">
            {(id) => <PasswordInput id={id} autoComplete="new-password" value={password} onChange={setPassword} autoFocus />}
          </Field>
          <Field label="New password again">
            {(id) => <PasswordInput id={id} autoComplete="new-password" value={again} onChange={setAgain} />}
          </Field>
        </div>
        <p className="form-hint">
          {setting
            ? "Members already in aren't affected. Newcomers need this password. It isn't saved anywhere: note it down."
            : "Members stay in the room. Newcomers need the new password; the invite links you've sent keep working with it. The password isn't saved anywhere: note it down."}
        </p>
        <p className="field-error" role={error ? "alert" : undefined}>
          {error ?? " "}
        </p>
      </form>
    </Dialog>
  );
}
