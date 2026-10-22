// Buttons. A button whose label changes ("Start streaming" → "Starting…")
// keeps the width of its longest label (`labels`), so nothing beside it
// moves: the no-jump rule.
import { forwardRef, type ButtonHTMLAttributes, type ReactNode } from "react";
import { Tooltip } from "./Tooltip";
import "./Button.css";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger" | "danger-ghost";

type Props = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: ButtonVariant;
  size?: "sm" | "md" | "lg";
  icon?: ReactNode;
  /** Every label this button can show: it keeps the widest one's width. */
  labels?: string[];
  /** Why it is disabled, shown on hover. */
  reason?: string | null;
  wide?: boolean;
};

export const Button = forwardRef<HTMLButtonElement, Props>(function Button(
  { variant = "secondary", size = "md", icon, labels, reason, wide, className, children, ...rest },
  ref,
) {
  const button = (
    <button
      ref={ref}
      type="button"
      className={["btn", `btn-${variant}`, `btn-${size}`, wide ? "btn-wide" : "", className ?? ""].join(" ")}
      {...rest}
    >
      {icon && <span className="btn-icon">{icon}</span>}
      {labels ? (
        <span className="btn-labels">
          {labels.map((l) => (
            <span key={l} aria-hidden="true" className="btn-reserve">
              {l}
            </span>
          ))}
          <span className="btn-label">{children}</span>
        </span>
      ) : (
        children != null && <span className="btn-label">{children}</span>
      )}
    </button>
  );
  if (reason && rest.disabled) {
    // A disabled button gets no pointer events: the tooltip sits on a wrapper.
    return (
      <Tooltip label={reason}>
        <span className={wide ? "btn-reason btn-wide" : "btn-reason"} tabIndex={0}>
          {button}
        </span>
      </Tooltip>
    );
  }
  return button;
});

/** A square button with an icon only; `label` names it for screen readers
 * and shows on hover. */
export const IconButton = forwardRef<
  HTMLButtonElement,
  ButtonHTMLAttributes<HTMLButtonElement> & { label: string; variant?: "ghost" | "secondary"; size?: "sm" | "md" }
>(function IconButton({ label, variant = "ghost", size = "md", className, children, ...rest }, ref) {
  return (
    <Tooltip label={label}>
      <button
        ref={ref}
        type="button"
        aria-label={label}
        className={["icon-btn", `btn-${variant}`, `icon-btn-${size}`, className ?? ""].join(" ")}
        {...rest}
      >
        {children}
      </button>
    </Tooltip>
  );
});
