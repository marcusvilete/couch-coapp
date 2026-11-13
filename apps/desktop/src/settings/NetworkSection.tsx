// Settings → Network (M26a, ADR 0014): the relays this profile holds and
// each one's state, this network's, adding a relay (a code, or an address),
// sharing one, removing one with a warning naming who is reached through
// it, the public relays' switches, and help: the diagnostics to copy and the
// log folder. Kept current by the engine's network events (`lobby.network`).
import { Copy, FolderOpen, Globe, MoreHorizontal, RefreshCw, Server, Share2, Trash2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { NetworkView } from "../bindings/NetworkView";
import type { RelayUseView } from "../bindings/RelayUseView";
import type { RelayView } from "../bindings/RelayView";
import { errorText, request } from "../engine";
import { networkLine, RELAY_LINE, relayState, removeWarning, type Tone } from "../model/network";
import { PERSONAL_WHY } from "../model/reasons";
import { Button, IconButton } from "../ui/Button";
import { Switch } from "../ui/Controls";
import { Dialog } from "../ui/Dialog";
import { Badge, Skeleton } from "../ui/Feedback";
import { ContextMenu, DropdownMenu, type MenuEntry } from "../ui/Menu";
import { Why } from "../ui/Why";
import { AddRelayForm } from "./AddRelayForm";
import "./NetworkSection.css";

/** Asks for a relay's new code: from the shell's strip, or a row's menu. */
export type PasteFor = { host: string; seq: number };

export function NetworkSection(props: {
  network: NetworkView | null;
  onNetwork: (network: NetworkView) => void;
  /** Focus the code field, for this relay's new code. */
  pasteFor: PasteFor | null;
  /** This PC can be a home PC: remote play's warning applies. */
  homePc: boolean;
}) {
  const view = props.network;
  const [status, setStatus] = useState<{ text: string; tone: "ok" | "error" } | null>(null);
  const [working, setWorking] = useState(false);
  const [removing, setRemoving] = useState<RelayView | null>(null);
  const [hint, setHint] = useState<string | null>(null);
  const [remotePlay, setRemotePlay] = useState(false);
  const codeField = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!props.homePc) return;
    request({ type: "getRemotePlay" }, "remotePlay")
      .then((v) => setRemotePlay(v.on))
      .catch(() => {});
  }, [props.homePc]);

  const askForCode = (host: string) => {
    setHint(`Paste the new code for ${host}`);
    codeField.current?.scrollIntoView({ block: "center" });
    codeField.current?.focus();
  };
  useEffect(() => {
    if (props.pasteFor) askForCode(props.pasteFor.host);
  }, [props.pasteFor?.seq]);

  const run = async (work: () => Promise<string | null>) => {
    setWorking(true);
    setStatus(null);
    try {
      const said = await work();
      if (said) setStatus({ text: said, tone: "ok" });
    } catch (e) {
      setStatus({ text: errorText(e), tone: "error" });
    } finally {
      setWorking(false);
    }
  };
  const change = (command: Parameters<typeof request>[0]) =>
    run(async () => {
      props.onNetwork(await request(command, "network"));
      return null;
    });

  const share = (relay: RelayView) =>
    run(async () => {
      await request({ type: "shareRelay", url: relay.url }, "done");
      return `${relay.host}'s code is copied, kept out of the clipboard's history. Send it only to people you trust: it lets them use the relay.`;
    });
  const copyDiagnostics = () =>
    run(async () => {
      const { text } = await request({ type: "getDiagnostics" }, "diagnostics");
      await navigator.clipboard.writeText(text);
      return "Diagnostics copied: they name your relays and how each member is connected, never people, addresses or codes.";
    });
  const openLogs = () =>
    run(async () => {
      await request({ type: "openLogFolder" }, "done");
      return null;
    });

  const menu = (relay: RelayView): MenuEntry[] => [
    {
      label: "Share its code",
      icon: <Share2 />,
      onSelect: () => void share(relay),
      disabled: relay.personal ? "Personal: given to you alone" : working,
    },
    { label: "Paste a new code", icon: <Copy />, onSelect: () => askForCode(relay.host) },
    "separator",
    {
      label: "Remove…",
      icon: <Trash2 />,
      danger: true,
      onSelect: () => setRemoving(relay),
      disabled: relay.builtIn ? "This app's own relay stays" : working,
    },
  ];

  const line = view ? networkLine(view) : null;

  return (
    <div className="settings-section">
      <p className="settings-lead">
        {RELAY_LINE} Couch Co-App always tries a direct connection first; your relays carry the rest, and let your
        friends find you.
      </p>

      <h3 className="settings-subtitle">This network</h3>
      <div className="settings-card">
        <div className="net-line">
          {line ? (
            <span className="net-state">
              <Dot tone={line.tone} />
              <span>{line.text}</span>
            </span>
          ) : (
            <Skeleton width={280} height={12} />
          )}
          <Button
            size="sm"
            icon={<RefreshCw />}
            disabled={view == null || working}
            onClick={() => void change({ type: "checkNetwork" })}
          >
            Check again
          </Button>
        </div>
      </div>

      <h3 className="settings-subtitle">Your relays</h3>
      <div className="settings-card">
        {view == null ? (
          <Skeleton lines={2} height={14} />
        ) : !view.changeable ? (
          <p className="settings-note">This app's relays were fixed when it started.</p>
        ) : (
          <>
            {view.relays.length === 0 && (
              <p className="settings-note">
                You have no relay of your own: the public relays carry your connections. They're shared by everyone,
                with limits nobody publishes. Add one a friend runs, or run your own.
              </p>
            )}
            <ul className="relay-list">
              {view.relays.map((r) => (
                <RelayRow
                  key={r.url}
                  relay={r}
                  menu={menu(r)}
                  home={view.homeRelay === r.url}
                  onPaste={() => askForCode(r.host)}
                />
              ))}
              {view.public && <PublicRow view={view} />}
            </ul>
            {view.relays.length > 0 && (
              <>
                <Switch
                  checked={view.keepPublic}
                  disabled={working}
                  onChange={(on) => void change({ type: "setPublicRelays", on })}
                  label="Use the public relays too"
                  detail="Your connections could then go through relays shared by everyone, rather than yours."
                />
                <Switch
                  checked={view.publicFallback}
                  disabled={working || view.keepPublic}
                  onChange={(on) => void change({ type: "setPublicFallback", on })}
                  label="Let the public relays stand in when none of mine works"
                  detail="Friends on your relays alone still reach you while yours are down."
                />
              </>
            )}
          </>
        )}
      </div>

      {view?.changeable && (
        <>
          <h3 className="settings-subtitle">Add a relay</h3>
          <div className="settings-card">
            <AddRelayForm
              testBuild={view.testBuild}
              codeRef={codeField}
              hint={hint}
              onAdded={(network, host) => {
                props.onNetwork(network);
                setHint(null);
                setStatus({ text: `${host} is in your list, and in use.`, tone: "ok" });
              }}
            />
            {remotePlay && (
              <p className="settings-warn">
                Remote play is on: when your relays change, a device of yours may lose its way to this PC until it's
                fixed here.
              </p>
            )}
          </div>
        </>
      )}

      {/* Kept even when empty: what was just done, or why it failed. */}
      <p
        className={status?.tone === "error" ? "field-error selectable" : "settings-note"}
        role={status?.tone === "error" ? "alert" : "status"}
      >
        {status?.text ?? " "}
      </p>

      <h3 className="settings-subtitle">Help</h3>
      <div className="settings-card">
        <div className="settings-row">
          <Button icon={<Copy />} disabled={working} onClick={() => void copyDiagnostics()}>
            Copy diagnostics
          </Button>
          <Button icon={<FolderOpen />} disabled={working} onClick={() => void openLogs()}>
            Open the log folder
          </Button>
        </div>
        <p className="settings-note">
          For asking someone for help: the diagnostics say which versions run, each relay's state and how each member
          is connected, numbered. They never name anyone, nor hold addresses, codes or passwords.
        </p>
      </div>

      {removing && <RemoveRelayDialog relay={removing} onClose={() => setRemoving(null)} onNetwork={props.onNetwork} />}
    </div>
  );
}

function Dot(props: { tone: Tone }) {
  return <span className={`net-dot net-dot-${props.tone}`} aria-hidden="true" />;
}

function RelayRow(props: { relay: RelayView; menu: MenuEntry[]; home: boolean; onPaste: () => void }) {
  const r = props.relay;
  const state = relayState(r.state);
  return (
    <ContextMenu items={props.menu}>
      <li className="relay-row">
        <span className="relay-icon" aria-hidden="true">
          <Server />
        </span>
        <span className="relay-text">
          <span className="relay-name">
            <span className="relay-host selectable">{r.host}</span>
            {r.builtIn && <Badge tone="accent">this app's</Badge>}
            {!r.hasCode && <Badge>open to everyone</Badge>}
            {r.personal && (
              <>
                <Badge tone="info" title="Given to you alone: never passed on">
                  personal
                </Badge>
                <Why label="Details">{PERSONAL_WHY}</Why>
              </>
            )}
          </span>
          <span className={`relay-state relay-state-${state.tone}`}>
            <Dot tone={state.tone} />
            {state.text}
            {state.details && (
              <Why label="Details">
                <span className="selectable">{state.details}</span>
              </Why>
            )}
          </span>
        </span>
        {r.state.type === "refused" && (
          <Button size="sm" variant="primary" onClick={props.onPaste}>
            Paste a new code
          </Button>
        )}
        <DropdownMenu items={props.menu}>
          <IconButton label={`${r.host}: options`} size="sm">
            <MoreHorizontal />
          </IconButton>
        </DropdownMenu>
      </li>
    </ContextMenu>
  );
}

/** The public relays, as one row, while they're in use. */
function PublicRow(props: { view: NetworkView }) {
  const state = relayState(props.view.public ?? { type: "checking" });
  return (
    <li className="relay-row">
      <span className="relay-icon" aria-hidden="true">
        <Globe />
      </span>
      <span className="relay-text">
        <span className="relay-name">
          <span className="relay-host">Public relays</span>
          {props.view.fallingBack && <Badge tone="warn">standing in</Badge>}
        </span>
        <span className={`relay-state relay-state-${state.tone}`}>
          <Dot tone={state.tone} />
          {state.text}
          {state.details && (
            <Why label="Details">
              <span className="selectable">{state.details}</span>
            </Why>
          )}
        </span>
      </span>
    </li>
  );
}

/** Before removing a relay: the rooms and members reached through it, and
 * remote play. */
function RemoveRelayDialog(props: { relay: RelayView; onClose: () => void; onNetwork: (n: NetworkView) => void }) {
  const [use, setUse] = useState<RelayUseView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    request({ type: "relayUse", url: props.relay.url }, "relayUse")
      .then((v) => live && setUse(v))
      .catch((e) => live && setError(errorText(e)));
    return () => {
      live = false;
    };
  }, [props.relay.url]);

  const remove = async () => {
    setBusy(true);
    setError(null);
    try {
      props.onNetwork(await request({ type: "removeRelay", url: props.relay.url }, "network"));
      props.onClose();
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  };
  const lines = use ? removeWarning(use) : null;

  return (
    <Dialog
      title={`Remove ${props.relay.host}?`}
      onClose={props.onClose}
      busy={busy}
      size="s"
      fixed
      footer={
        <>
          <Button data-autofocus onClick={props.onClose} disabled={busy}>
            Keep it
          </Button>
          <Button variant="danger" disabled={busy || (use == null && error == null)} onClick={() => void remove()}>
            Remove
          </Button>
        </>
      }
    >
      <div className="form">
        {lines == null && error == null ? (
          <Skeleton lines={2} height={13} />
        ) : lines != null && lines.length > 0 ? (
          lines.map((l) => (
            <p key={l} className="settings-warn">
              {l}
            </p>
          ))
        ) : (
          <p className="form-hint">Nobody in your rooms reaches you through it now.</p>
        )}
        <p className="form-hint">
          To use it again later, you'll need its code. {props.relay.personal ? "" : "Share its code first to keep it."}
        </p>
        <p className="field-error selectable" role={error ? "alert" : undefined}>
          {error ?? " "}
        </p>
      </div>
    </Dialog>
  );
}
