// The shell's strip (M26a, ADR 0014): only for what silently breaks every
// room (no internet, a sign-in page, a relay refusing our code, none of our
// relays answering), once it has lasted (the engine waits), gone as soon as
// it clears. One line and the button that fixes it, at the bottom of the
// window: nothing above it moves. Read out by screen readers as it appears
// (a polite live region, always there).
import { WifiOff } from "lucide-react";
import type { NetworkTrouble } from "../bindings/NetworkTrouble";
import type { NetworkView } from "../bindings/NetworkView";
import { troubleStrip, type TroubleFix } from "../model/network";
import { Button } from "../ui/Button";
import "./TroubleStrip.css";

export function TroubleStrip(props: {
  network: NetworkView | null;
  working: boolean;
  onFix: (fix: TroubleFix, trouble: NetworkTrouble) => void;
}) {
  const trouble = props.network?.trouble ?? null;
  const said = trouble ? troubleStrip(trouble, props.network?.fallingBack ?? false) : null;
  return (
    <div className="trouble-region" role="status" aria-live="polite">
      {trouble && said && (
        <div className="trouble-strip">
          <WifiOff aria-hidden="true" />
          <span className="trouble-text">{said.text}</span>
          <Button size="sm" disabled={props.working} onClick={() => props.onFix(said.fix, trouble)}>
            {said.label}
          </Button>
        </div>
      )}
    </div>
  );
}
