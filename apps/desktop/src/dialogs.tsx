// The lobby's dialogs. Passwords and invite links live only in these
// components' state, and go when the dialog closes.
import { useEffect, useState, type FormEvent } from "react";
import type { InvitePreview } from "./bindings/InvitePreview";
import type { ProfileView } from "./bindings/ProfileView";
import type { RoomView } from "./bindings/RoomView";
import { errorText, request } from "./engine";
import { Modal, PasswordInput, duration, nameProblem } from "./ui";

export function CreateRoomDialog(props: { onClose: () => void; onCreated: (room: RoomView) => void }) {
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [again, setAgain] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(e: FormEvent) {
    e.preventDefault();
    const problem =
      nameProblem(name) ??
      (password.trim() === "" ? "Choose a password." : null) ??
      (password !== again ? "The passwords don't match." : null);
    if (problem) {
      setError(problem);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const room = await request({ type: "createRoom", name: name.trim(), password }, "room");
      props.onCreated(room);
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  }

  return (
    <Modal
      title="Create a room"
      onClose={props.onClose}
      busy={busy}
      footer={
        <>
          <button type="button" onClick={props.onClose} disabled={busy}>
            Cancel
          </button>
          <button type="submit" form="create-room" className="primary" disabled={busy}>
            {busy ? "Creating…" : "Create room"}
          </button>
        </>
      }
    >
      <form id="create-room" onSubmit={submit} className="form">
        <label>
          Room name
          <input value={name} onChange={(e) => setName(e.target.value)} placeholder="Friday games" />
        </label>
        <label>
          Password
          <PasswordInput autoComplete="new-password" value={password} onChange={setPassword} />
        </label>
        <label>
          Password again
          <PasswordInput autoComplete="new-password" value={again} onChange={setAgain} />
        </label>
        <p className="hint">
          Friends need an invite link from you and this password. Only you can let people in, so
          they can join while your Couch Co-App is running. The password itself isn't saved
          anywhere, so note it down. You can change it later from the room's menu.
        </p>
        {error && <p className="error" role="alert">{error}</p>}
      </form>
    </Modal>
  );
}

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
    if (props.offered != null && !busy && password === "") takeOffer();
    // Only when a link arrives, not on every keystroke.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.offered]);

  useEffect(() => {
    // A password belongs to the invite it was typed for.
    setPassword("");
    setError(null);
    setPreview(null);
    setInviteError(null);
    if (invite.trim() === "") return;
    let live = true;
    const timer = window.setTimeout(() => {
      request({ type: "previewInvite", invite }, "invitePreview")
        .then((p) => live && setPreview(p))
        .catch((e) => live && setInviteError(errorText(e)));
    }, 150);
    return () => {
      live = false;
      window.clearTimeout(timer);
    };
  }, [invite]);

  const ready = preview?.status === "ready";

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
      const room = await request({ type: "joinRoom", invite, password }, "room");
      props.onJoined(room);
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  }

  return (
    <Modal
      title="Join a room"
      onClose={props.onClose}
      busy={busy}
      footer={
        <>
          <button type="button" onClick={props.onClose} disabled={busy}>
            Cancel
          </button>
          {preview?.status === "member" ? (
            <button type="button" className="primary" onClick={() => props.onOpenRoom(preview.room)}>
              Open room
            </button>
          ) : (
            <button type="submit" form="join-room" className="primary" disabled={busy || !ready}>
              {busy ? "Asking the owner…" : "Join"}
            </button>
          )}
        </>
      }
    >
      <form id="join-room" onSubmit={submit} className="form">
        {props.offered != null && !busy && (
          <div className="banner warn offer" role="status">
            <span>Another invite link was just opened.</span>
            <button type="button" onClick={takeOffer}>
              Use it instead
            </button>
          </div>
        )}
        <label>
          Invite link
          <textarea
            className="mono"
            rows={3}
            spellCheck={false}
            autoComplete="off"
            value={invite}
            disabled={busy}
            onChange={(e) => setInvite(e.target.value)}
            placeholder="couchco://join#… or https://…/j/#…"
          />
        </label>
        {inviteError && <p className="error">{inviteError}</p>}
        {preview && (
          <div className="invite-preview">
            <p>
              An invite to <strong>{preview.name}</strong>
            </p>
            {preview.status === "ready" && (
              <p className="hint">Works for about {duration(preview.expiresInSecs)} more, if not revoked.</p>
            )}
            {preview.status === "expired" && <p className="warn-text">This invite has expired. Ask the owner for a new one.</p>}
            {preview.status === "member" && <p className="hint">You're already in this room.</p>}
            {preview.status === "own" && <p className="warn-text">This is an invite to your own room.</p>}
          </div>
        )}
        {ready && (
          <label>
            Password
            <PasswordInput value={password} disabled={busy} onChange={setPassword} autoFocus />
          </label>
        )}
        {ready && (
          <p className="hint">The room's owner lets you in, so they need to be online.</p>
        )}
        {error && <p className="error" role="alert">{error}</p>}
      </form>
    </Modal>
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
    <Modal
      title={`Invite people to ${props.room.name}`}
      onClose={props.onClose}
      footer={
        <>
          <button type="button" onClick={props.onClose}>
            Done
          </button>
          <button type="button" className="primary" onClick={copy} disabled={link == null}>
            {copied ? "Copied" : "Copy link"}
          </button>
        </>
      }
    >
      <div className="form">
        {link == null && !error && <p className="hint">Making a link…</p>}
        {link != null && (
          <textarea className="mono" rows={4} readOnly value={link} onFocus={(e) => e.target.select()} />
        )}
        {error && <p className="error" role="alert">{error}</p>}
        <p className="hint">
          Send the link and, separately, the password. Anyone with both can join for a week, until
          five wrong passwords, or until you revoke invites. Treat the link like half of the
          password.
        </p>
      </div>
    </Modal>
  );
}

export function ConfirmDialog(props: {
  title: string;
  body: string;
  action: string;
  danger?: boolean;
  run: () => Promise<unknown>;
  onClose: () => void;
}) {
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
    <Modal
      title={props.title}
      onClose={props.onClose}
      busy={busy}
      footer={
        <>
          <button type="button" onClick={props.onClose} disabled={busy}>
            Cancel
          </button>
          <button type="button" className={props.danger ? "primary danger" : "primary"} onClick={go} disabled={busy}>
            {props.action}
          </button>
        </>
      }
    >
      <p>{props.body}</p>
      {error && <p className="error" role="alert">{error}</p>}
    </Modal>
  );
}

export function NameDialog(props: {
  profile: ProfileView;
  onClose: () => void;
  onSaved: (profile: ProfileView) => void;
}) {
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
    try {
      props.onSaved(await request({ type: "setDisplayName", name: name.trim() }, "profile"));
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  }

  return (
    <Modal
      title="Your name"
      onClose={props.onClose}
      busy={busy}
      footer={
        <>
          <button type="button" onClick={props.onClose} disabled={busy}>
            Cancel
          </button>
          <button type="submit" form="edit-name" className="primary" disabled={busy}>
            Save
          </button>
        </>
      }
    >
      <form id="edit-name" onSubmit={submit} className="form">
        <label>
          Display name
          <input value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <p className="hint">Everyone in your rooms sees this name.</p>
        {error && <p className="error" role="alert">{error}</p>}
      </form>
    </Modal>
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
    const problem =
      (password.trim() === "" ? "Choose a password." : null) ??
      (password !== again ? "The passwords don't match." : null);
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
    <Modal
      title={`Take over ${props.room.name}?`}
      onClose={props.onClose}
      busy={busy}
      footer={
        <>
          <button type="button" onClick={props.onClose} disabled={busy}>
            Cancel
          </button>
          <button type="submit" form="accept-room" className="primary" disabled={busy}>
            Take over the room
          </button>
        </>
      }
    >
      <form id="accept-room" onSubmit={submit} className="form">
        <p className="hint">
          {props.ownerName} offers you the room. As its owner, you let people in with invites, you can ban, and leaving
          closes the room for everyone. {props.ownerName} stays as a member; the invites they sent stop working.
        </p>
        <label>
          Password for newcomers
          <PasswordInput autoComplete="new-password" value={password} onChange={setPassword} autoFocus />
        </label>
        <label>
          Password again
          <PasswordInput autoComplete="new-password" value={again} onChange={setAgain} />
        </label>
        <p className="hint">Members already in aren't affected. The password isn't saved anywhere, so note it down.</p>
        {error && <p className="error" role="alert">{error}</p>}
      </form>
    </Modal>
  );
}

export function ChangePasswordDialog(props: { room: RoomView; onClose: () => void; onChanged: () => void }) {
  const [password, setPassword] = useState("");
  const [again, setAgain] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(e: FormEvent) {
    e.preventDefault();
    const problem =
      (password.trim() === "" ? "Choose a password." : null) ??
      (password !== again ? "The passwords don't match." : null);
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
    <Modal
      title={`Change the password of ${props.room.name}`}
      onClose={props.onClose}
      busy={busy}
      footer={
        <>
          <button type="button" onClick={props.onClose} disabled={busy}>
            Cancel
          </button>
          <button type="submit" form="change-password" className="primary" disabled={busy}>
            Change password
          </button>
        </>
      }
    >
      <form id="change-password" onSubmit={submit} className="form">
        <label>
          New password
          <PasswordInput autoComplete="new-password" value={password} onChange={setPassword} autoFocus />
        </label>
        <label>
          New password again
          <PasswordInput autoComplete="new-password" value={again} onChange={setAgain} />
        </label>
        <p className="hint">
          Members stay in the room. Newcomers need the new password; the invite links you've sent
          keep working with it. The password isn't saved anywhere, so note it down.
        </p>
        {error && <p className="error" role="alert">{error}</p>}
      </form>
    </Modal>
  );
}
