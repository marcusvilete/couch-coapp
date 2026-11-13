// Adding a relay (M26a, ADR 0014 "Registering relays"): a relay code pasted
// (what it holds shown first, never its secret), or an address and code
// typed. The engine connects with it before saving: a refused code is said
// plainly, and a relay that doesn't answer can be saved anyway. Used by
// Settings → Network and the wizard.
import { Check, ClipboardPaste, Server } from "lucide-react";
import { useEffect, useState, type FormEvent, type Ref } from "react";
import type { Command } from "../bindings/Command";
import type { NetworkView } from "../bindings/NetworkView";
import type { RelayCheckView } from "../bindings/RelayCheckView";
import type { RelayCodeView } from "../bindings/RelayCodeView";
import { engineCommand, errorText, request } from "../engine";
import { Button } from "../ui/Button";
import { Segmented } from "../ui/Controls";
import { Badge, Spinner } from "../ui/Feedback";
import { PasswordInput, TextInput } from "../ui/Field";
import { Why } from "../ui/Why";

/** A check that failed, and the command to save it anyway. */
type Failed = { check: RelayCheckView; anyway: Command };

export function AddRelayForm(props: {
  /** The relay is in the list now. */
  onAdded: (network: NetworkView, host: string) => void;
  /** Plain http at LAN addresses is allowed (development and test builds). */
  testBuild: boolean;
  codeRef?: Ref<HTMLInputElement>;
  /** Above the field: "Paste the new code for …". */
  hint?: string | null;
  /** Only the code (the wizard). */
  codeOnly?: boolean;
  /** No "Save anyway" (the wizard): a relay that doesn't answer would turn
   * the public relays off at once, just before an invite that needs one. */
  noSaveAnyway?: boolean;
  autoFocus?: boolean;
}) {
  const [mode, setMode] = useState<"code" | "address">("code");
  const [code, setCode] = useState("");
  const [preview, setPreview] = useState<RelayCodeView | null>(null);
  const [readError, setReadError] = useState<string | null>(null);
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");
  const [working, setWorking] = useState(false);
  const [failed, setFailed] = useState<Failed | null>(null);
  const [error, setError] = useState<string | null>(null);

  // What the code holds, as it is pasted or typed.
  useEffect(() => {
    setFailed(null);
    setError(null);
    const text = code.trim();
    if (text === "") {
      setPreview(null);
      setReadError(null);
      return;
    }
    let live = true;
    const timer = window.setTimeout(() => {
      request({ type: "readRelayCode", code: text }, "relayCode")
        .then((v) => {
          if (!live) return;
          setPreview(v);
          setReadError(null);
        })
        .catch((e) => {
          if (!live) return;
          setPreview(null);
          setReadError(errorText(e));
        });
    }, 200);
    return () => {
      live = false;
      window.clearTimeout(timer);
    };
  }, [code]);

  const send = async (command: Command, anyway: Command) => {
    setWorking(true);
    setFailed(null);
    setError(null);
    try {
      const reply = await engineCommand(command);
      if (reply.type === "network") {
        const host = preview?.host ?? hostOfTyped(url);
        setCode("");
        setUrl("");
        setToken("");
        props.onAdded(reply, host);
      } else if (reply.type === "relayCheck") {
        setFailed({ check: reply, anyway });
      }
    } catch (e) {
      setError(errorText(e));
    } finally {
      setWorking(false);
    }
  };

  const addCode = (e?: FormEvent) => {
    e?.preventDefault();
    const text = code.trim();
    if (!preview || preview.sameCode || working) return;
    void send({ type: "addRelayCode", code: text, unchecked: false }, { type: "addRelayCode", code: text, unchecked: true });
  };
  const addAddress = (e?: FormEvent) => {
    e?.preventDefault();
    if (url.trim() === "" || working) return;
    const typed = { url: url.trim(), token: token.trim() === "" ? null : token.trim(), personal: false };
    void send({ type: "addRelay", ...typed, unchecked: false }, { type: "addRelay", ...typed, unchecked: true });
  };

  const codeShown = mode === "code" || !!props.codeOnly;
  /** Checking, or why adding failed: in the form shown only. */
  const sending = (shown: boolean, connecting: string) => {
    if (!shown) return null;
    if (working) {
      return (
        <span className="relay-working">
          <Spinner size={14} /> {connecting}
        </span>
      );
    }
    if (failed) {
      return (
        <FailedCheck
          failed={failed}
          working={working}
          onAnyway={props.noSaveAnyway ? undefined : () => void send(failed.anyway, failed.anyway)}
        />
      );
    }
    if (error) {
      return (
        <span className="field-error selectable" role="alert">
          {error}
        </span>
      );
    }
    return null;
  };

  return (
    <div className="relay-add">
      {!props.codeOnly && (
        <Segmented
          label="How to add it"
          value={mode}
          onChange={(m) => {
            setMode(m);
            setFailed(null);
            setError(null);
          }}
          options={[
            { value: "code", label: "Paste a relay code" },
            { value: "address", label: "Type its address" },
          ]}
        />
      )}
      {/* Both forms laid in one place, the other one hidden: switching
          moves nothing below. */}
      <div className="relay-add-forms">
        <form className={formClass(codeShown)} inert={!codeShown} onSubmit={addCode}>
          <label className="field-label" htmlFor="relay-code">
            {props.hint ?? "Relay code"}
          </label>
          <div className="relay-add-row">
            <TextInput
              id="relay-code"
              ref={props.codeRef}
              className="mono"
              placeholder="couch-relay-1.…"
              value={code}
              autoFocus={props.autoFocus}
              autoComplete="off"
              disabled={working}
              onChange={(e) => setCode(e.target.value)}
            />
            <Button
              type="submit"
              variant="primary"
              disabled={!preview || preview.sameCode || working}
              labels={["Add", "Checking…"]}
            >
              {working ? "Checking…" : "Add"}
            </Button>
          </div>
          {/* Kept even when empty: what the code holds appears here. */}
          <div className="relay-code-preview" aria-live="polite">
            {sending(codeShown, `Connecting to ${preview?.host ?? "the relay"} with this code…`) ??
              (readError ? (
              <span className="field-error selectable">{readError}</span>
            ) : preview ? (
              <CodePreview code={preview} />
            ) : (
              <span className="form-hint">
                Whoever runs the relay gives you its code. It holds a secret: paste it only here.
              </span>
            ))}
          </div>
        </form>
        {!props.codeOnly && (
        <form className={formClass(mode === "address")} inert={mode !== "address"} onSubmit={addAddress}>
          <label className="field-label" htmlFor="relay-url">
            Address
          </label>
          <TextInput
            id="relay-url"
            type="url"
            placeholder={props.testBuild ? "https://relay.example.com, or http://192.168.x.x:3340" : "https://relay.example.com"}
            value={url}
            disabled={working}
            onChange={(e) => {
              setUrl(e.target.value);
              setFailed(null);
              setError(null);
            }}
          />
          <label className="field-label" htmlFor="relay-token">
            Its code (the relay's token), if it asks for one
          </label>
          <div className="relay-add-row">
            <PasswordInput
              id="relay-token"
              value={token}
              disabled={working}
              autoComplete="off"
              onChange={(t) => {
                setToken(t);
                setFailed(null);
                setError(null);
              }}
            />
            <Button type="submit" variant="primary" disabled={url.trim() === "" || working} labels={["Add", "Checking…"]}>
              {working ? "Checking…" : "Add"}
            </Button>
          </div>
          <div className="relay-code-preview" aria-live="polite">
            {sending(!codeShown, "Connecting to the relay…") ?? (
              <span className="form-hint">A relay open to everyone needs no code.</span>
            )}
          </div>
        </form>
        )}
      </div>
    </div>
  );
}

function formClass(shown: boolean): string {
  return shown ? "relay-add-form" : "relay-add-form relay-add-hidden";
}

function hostOfTyped(url: string): string {
  try {
    return new URL(url.trim()).host;
  } catch {
    return url.trim();
  }
}

/** What a pasted code holds: the relay, whether it has a secret, whether
 * it's personal, and whether it's in the list already. */
function CodePreview(props: { code: RelayCodeView }) {
  const c = props.code;
  return (
    <span className="relay-preview">
      <Server aria-hidden="true" />
      <span className="relay-preview-host selectable">{c.host}</span>
      {c.hasCode ? <Badge>with a code</Badge> : <Badge>open to everyone</Badge>}
      {c.personal && (
        <Badge tone="info" title="Given to you alone: never passed on">
          personal
        </Badge>
      )}
      <span className="relay-preview-note">
        {c.sameCode ? (
          <>
            <Check aria-hidden="true" /> Already in your list, with this code.
          </>
        ) : c.held ? (
          <>
            <ClipboardPaste aria-hidden="true" /> In your list: adding gives it this new code.
          </>
        ) : null}
      </span>
    </span>
  );
}

/** The check before saving failed: refused (ask for a new code), or no
 * answer (save anyway). */
function FailedCheck(props: { failed: Failed; working: boolean; onAnyway?: () => void }) {
  const { check } = props.failed;
  if (check.outcome.type === "refused") {
    return (
      <span className="relay-failed">
        <span className="field-error" role="alert">
          {check.host} refused this code: ask its owner for a new one. Nothing was saved.
        </span>
        {check.outcome.reason && (
          <Why label="Details">
            <span className="selectable">{check.outcome.reason}</span>
          </Why>
        )}
      </span>
    );
  }
  const details = check.outcome.details;
  return (
    <span className="relay-failed">
      <span className="settings-warn" role="alert">
        {check.host} didn't answer: it may be down for now. Nothing was saved.
      </span>
      {details && (
        <Why label="Details">
          <span className="selectable">{details}</span>
        </Why>
      )}
      {props.onAnyway ? (
        <Button size="sm" disabled={props.working} onClick={props.onAnyway}>
          Save anyway
        </Button>
      ) : (
        <span className="form-hint">Try again when it answers.</span>
      )}
    </span>
  );
}
