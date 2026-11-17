// The shell's strip (M26a, ADR 0014): first for what silently breaks every
// room (no internet, a sign-in page, a relay refusing our code, none of our
// relays answering), once it has lasted (the engine waits), gone as soon as
// it clears; then, with nothing wrong, a newer version (M26c-2) until
// dismissed for that version. One line and its buttons, at the bottom of the
// window: nothing above it moves. Read out by screen readers as it appears
// (a polite live region, always there).
import { ArrowUpCircle, WifiOff, X } from "lucide-react";
import type { NetworkTrouble } from "../bindings/NetworkTrouble";
import type { NetworkView } from "../bindings/NetworkView";
import type { UpdatesView } from "../bindings/UpdatesView";
import { troubleStrip, type TroubleFix } from "../model/network";
import { newVersionText } from "../model/updates";
import { Button, IconButton } from "../ui/Button";
import "./TroubleStrip.css";

export function TroubleStrip(props: {
  network: NetworkView | null;
  working: boolean;
  onFix: (fix: TroubleFix, trouble: NetworkTrouble) => void;
  updates: UpdatesView | null;
  /** Opens the new version's release page. */
  onGetUpdate: () => void;
  onDismissUpdate: (version: string) => void;
}) {
  const trouble = props.network?.trouble ?? null;
  const said = trouble ? troubleStrip(trouble) : null;
  const newVersion = props.updates?.newVersion ?? null;
  return (
    <div className="trouble-region" role="status" aria-live="polite">
      {trouble && said ? (
        <div className="trouble-strip">
          <WifiOff aria-hidden="true" />
          <span className="trouble-text">{said.text}</span>
          <Button size="sm" disabled={props.working} onClick={() => props.onFix(said.fix, trouble)}>
            {said.label}
          </Button>
        </div>
      ) : (
        newVersion && (
          <div className="trouble-strip update-strip">
            <ArrowUpCircle aria-hidden="true" />
            <span className="trouble-text">{newVersionText(newVersion)}</span>
            <Button size="sm" onClick={props.onGetUpdate}>
              See the release
            </Button>
            <IconButton size="sm" label="Dismiss until the next version" onClick={() => props.onDismissUpdate(newVersion.version)}>
              <X />
            </IconButton>
          </div>
        )
      )}
    </div>
  );
}
