// Paste a relay's code where it's needed (M26a-2): a member on a relay that
// wants a code we lack, a join whose owner's relay wants one, a room whose
// members share that cause. The same form as Settings → Network's, code
// only; once the relay lets us in it is ours, and what needed it goes on.
import type { NetworkView } from "../bindings/NetworkView";
import { Button } from "../ui/Button";
import { Dialog } from "../ui/Dialog";
import { AddRelayForm } from "./AddRelayForm";

export function PasteCodeDialog(props: {
  /** The relay wanting a code, by its server's name. */
  host: string;
  /** Why it's asked, one sentence. */
  why: string;
  testBuild: boolean;
  /** The relay added, by its server's name: the code pasted may be
   * another's. */
  onAdded: (network: NetworkView, host: string) => void;
  onClose: () => void;
}) {
  return (
    <Dialog
      title={`A code for ${props.host}`}
      description={props.why}
      onClose={props.onClose}
      size="m"
      fixed
      footer={<Button onClick={props.onClose}>Cancel</Button>}
    >
      <div className="form">
        <AddRelayForm
          codeOnly
          autoFocus
          hint={`Paste the code for ${props.host}`}
          testBuild={props.testBuild}
          onAdded={(network, host) => props.onAdded(network, host)}
        />
        <p className="form-hint">
          Whoever runs the relay, or anyone who has it, can share its code from Settings → Network.
        </p>
      </div>
    </Dialog>
  );
}
