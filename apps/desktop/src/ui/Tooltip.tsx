// Hover and focus labels (Radix Tooltip). One provider for the app, so
// moving between buttons shows the next label at once.
import { Slot } from "@radix-ui/react-slot";
import * as RT from "@radix-ui/react-tooltip";
import { forwardRef, type HTMLAttributes, type ReactNode } from "react";
import "./Tooltip.css";

export function TooltipProvider(props: { children: ReactNode }) {
  return (
    <RT.Provider delayDuration={450} skipDelayDuration={250}>
      {props.children}
    </RT.Provider>
  );
}

/** A label on hover and focus. Whatever a wrapping trigger hands it (a
 * context menu's handlers, its ref) goes on to the child, label or not:
 * dropping them once left a room's right-click menu dead. */
export const Tooltip = forwardRef<
  HTMLElement,
  HTMLAttributes<HTMLElement> & { label: ReactNode; side?: "top" | "right" | "bottom" | "left"; children: ReactNode }
>(function Tooltip({ label, side, children, onFocus, ...rest }, ref) {
  if (label == null || label === "") {
    return (
      <Slot ref={ref} onFocus={onFocus} {...rest}>
        {children}
      </Slot>
    );
  }
  return (
    <RT.Root>
      <RT.Trigger
        asChild
        ref={ref as React.Ref<HTMLButtonElement>}
        {...rest}
        // Focus opens the label only when it came from the keyboard: a
        // dialog placing the focus, or focus coming back after a click,
        // shows nothing until hovered.
        onFocus={(e) => {
          onFocus?.(e);
          if (!e.currentTarget.matches(":focus-visible")) e.preventDefault();
        }}
      >
        {children}
      </RT.Trigger>
      <RT.Portal>
        <RT.Content className="tooltip" side={side ?? "top"} sideOffset={6} collisionPadding={8}>
          {label}
        </RT.Content>
      </RT.Portal>
    </RT.Root>
  );
});
