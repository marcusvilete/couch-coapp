// The wizard asking for a relay of the user's own (M26a, ADR 0014): it comes
// on creating a room or an invite while only the public relays are in use,
// never at start. Paste a relay code, read how to run one, put it off for a
// week (or until the next version), or stay on the public relays for good.
// Whatever is chosen, what the user was doing goes on.
import { useState } from "react";
import type { NetworkView } from "../bindings/NetworkView";
import { errorText, request } from "../engine";
import { RELAY_LINE } from "../model/network";
import { Button } from "../ui/Button";
import { Dialog } from "../ui/Dialog";
import { AddRelayForm } from "./AddRelayForm";

/** Where the guide to running a relay lives, in the app's repository. */
export function relayGuide(repository: string | null): string | null {
  return repository ? `${repository}/blob/main/docs/self-hosting.md` : null;
}

export function RelayWizard(props: {
  testBuild: boolean;
  repository: string | null;
  onNetwork: (network: NetworkView) => void;
  /** Answered: what the user was doing goes on. */
  onDone: () => void;
  /** Closed without an answer: nothing goes on. */
  onClose: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const guide = relayGuide(props.repository);

  const defer = async (never: boolean) => {
    setBusy(true);
    setError(null);
    try {
      props.onNetwork(await request({ type: "deferRelay", never }, "network"));
      props.onDone();
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  };

  return (
    <Dialog
      title="A relay of your own?"
      description={RELAY_LINE}
      onClose={props.onClose}
      busy={busy}
      size="m"
      fixed
      footer={
        <>
          <Button disabled={busy} onClick={() => void defer(true)}>
            Stay on the public relays
          </Button>
          <Button variant="primary" disabled={busy} onClick={() => void defer(false)}>
            Remind me later
          </Button>
        </>
      }
    >
      <div className="form">
        <p className="form-hint">
          Your connections now go through the public relays, shared by everyone, with limits nobody publishes. A relay
          a friend runs, or your own, is steadier for playing together. Friends in your rooms need its code too: share
          it from Settings → Network.
        </p>
        <AddRelayForm
          codeOnly
          autoFocus
          testBuild={props.testBuild}
          onAdded={(network) => {
            props.onNetwork(network);
            props.onDone();
          }}
        />
        {guide && (
          <p className="form-hint">
            Running your own takes a small server; the guide is at <span className="selectable mono">{guide}</span>
          </p>
        )}
        <p className="form-hint">
          Remind me later asks again in a week, or with the next version. Settings → Network always lets you add one.
        </p>
        <p className="field-error selectable" role={error ? "alert" : undefined}>
          {error ?? " "}
        </p>
      </div>
    </Dialog>
  );
}
