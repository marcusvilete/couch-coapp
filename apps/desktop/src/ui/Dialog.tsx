// Dialogs (Radix Dialog), in three fixed widths. A dialog whose contents
// change as you use it (`fixed`) also has a fixed height: its body scrolls
// and the footer stays put, so nothing jumps. While `busy`, nothing closes it.
import * as RD from "@radix-ui/react-dialog";
import { X } from "lucide-react";
import type { ReactNode } from "react";
import { IconButton } from "./Button";
import "./Dialog.css";

export function Dialog(props: {
  title: string;
  /** One line under the title. */
  description?: ReactNode;
  onClose: () => void;
  busy?: boolean;
  size?: "s" | "m" | "l";
  fixed?: boolean;
  footer?: ReactNode;
  /** Pinned above the footer: warnings that must stay in sight. */
  pinned?: ReactNode;
  children: ReactNode;
}) {
  const { busy } = props;
  const hold = (e: Event) => {
    if (busy) e.preventDefault();
  };
  return (
    <RD.Root open onOpenChange={(open) => !open && !busy && props.onClose()}>
      <RD.Portal>
        <RD.Overlay className="dialog-scrim" />
        <RD.Content
          className={`dialog dialog-${props.size ?? "m"}${props.fixed ? " dialog-fixed" : ""}`}
          onEscapeKeyDown={hold}
          onPointerDownOutside={hold}
          onInteractOutside={hold}
          onOpenAutoFocus={(e) => {
            // The first field, or what the dialog marks (`data-autofocus`:
            // Cancel, in a dialog that would destroy something; the first
            // choice), else its main button, else the dialog itself: never
            // the close button.
            e.preventDefault();
            const box = e.currentTarget as HTMLElement;
            const first = box.querySelector<HTMLElement>(
              "[data-autofocus]:not(:disabled), .dialog-body input:not(:disabled), .dialog-body textarea:not([readonly]):not(:disabled), .dialog-footer .btn-primary:not(:disabled)",
            );
            (first ?? box).focus();
          }}
        >
          <header className="dialog-header">
            <div className="dialog-titles">
              <RD.Title className="dialog-title">{props.title}</RD.Title>
              {props.description ? (
                <RD.Description className="dialog-description">{props.description}</RD.Description>
              ) : (
                <RD.Description className="sr-only">{props.title}</RD.Description>
              )}
            </div>
            <RD.Close asChild disabled={busy}>
              <IconButton label="Close" size="sm" disabled={busy}>
                <X />
              </IconButton>
            </RD.Close>
          </header>
          <div className="dialog-body">{props.children}</div>
          {props.pinned && <div className="dialog-pinned">{props.pinned}</div>}
          {props.footer && <footer className="dialog-footer">{props.footer}</footer>}
        </RD.Content>
      </RD.Portal>
    </RD.Root>
  );
}
