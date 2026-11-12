// "Why?": the long explanation behind a short line, opened on click.
import * as RP from "@radix-ui/react-popover";
import type { ReactNode } from "react";
import { popupPadding } from "./popper";
import "./Why.css";

export function Why(props: { children: ReactNode; label?: string }) {
  return (
    <RP.Root>
      <RP.Trigger className="why">{props.label ?? "Why?"}</RP.Trigger>
      <RP.Portal>
        <RP.Content className="why-popover" sideOffset={6} collisionPadding={popupPadding(12)} align="start">
          {props.children}
          <RP.Arrow className="why-arrow" />
        </RP.Content>
      </RP.Portal>
    </RP.Root>
  );
}
