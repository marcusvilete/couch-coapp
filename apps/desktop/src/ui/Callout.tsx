// A room's or session's state worth attention, in one style: an icon, one
// line, actions, and the detail behind Why?.
import { AlertTriangle, Info, OctagonAlert, Sparkles } from "lucide-react";
import type { ReactNode } from "react";
import { Why } from "./Why";
import "./Callout.css";

export type Tone = "info" | "warn" | "danger" | "accent";

const ICONS: Record<Tone, ReactNode> = {
  info: <Info />,
  warn: <AlertTriangle />,
  danger: <OctagonAlert />,
  accent: <Sparkles />,
};

export function Callout(props: {
  tone?: Tone;
  icon?: ReactNode;
  title?: ReactNode;
  children?: ReactNode;
  why?: ReactNode;
  actions?: ReactNode;
  compact?: boolean;
}) {
  const tone = props.tone ?? "info";
  return (
    <div className={`callout callout-${tone}${props.compact ? " compact" : ""}`}>
      <span className="callout-icon" aria-hidden="true">
        {props.icon ?? ICONS[tone]}
      </span>
      <div className="callout-text">
        {props.title && <strong className="callout-title">{props.title}</strong>}
        {props.children && <span className="callout-body">{props.children}</span>}
        {props.why && <Why>{props.why}</Why>}
      </div>
      {props.actions && <div className="callout-actions">{props.actions}</div>}
    </div>
  );
}
