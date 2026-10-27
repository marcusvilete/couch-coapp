// Choosing things: segmented buttons, a select, a switch, a slider, tabs.
import * as RSelect from "@radix-ui/react-select";
import * as RSlider from "@radix-ui/react-slider";
import * as RSwitch from "@radix-ui/react-switch";
import * as RTabs from "@radix-ui/react-tabs";
import * as RToggle from "@radix-ui/react-toggle-group";
import { Check, ChevronDown } from "lucide-react";
import type { ReactNode } from "react";
import "./Controls.css";

/** One of a few, side by side. */
export function Segmented<T extends string | number>(props: {
  value: T;
  options: { value: T; label: ReactNode; disabled?: boolean; title?: string }[];
  onChange: (value: T) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <RToggle.Root
      type="single"
      className="segmented"
      aria-label={props.label}
      value={String(props.value)}
      disabled={props.disabled}
      onValueChange={(v) => {
        const found = props.options.find((o) => String(o.value) === v);
        if (found) props.onChange(found.value);
      }}
    >
      {props.options.map((o) => (
        <RToggle.Item
          key={String(o.value)}
          value={String(o.value)}
          className="segment"
          disabled={o.disabled}
          title={o.title}
        >
          {o.label}
        </RToggle.Item>
      ))}
    </RToggle.Root>
  );
}

/** A drop-down choice. Its options can carry a second line. */
export function Select<T extends string | number>(props: {
  id?: string;
  value: T;
  options: { value: T; label: string; detail?: string; disabled?: boolean }[];
  onChange: (value: T) => void;
  label?: string;
  disabled?: boolean;
}) {
  const current = props.options.find((o) => o.value === props.value);
  return (
    <RSelect.Root
      value={String(props.value)}
      disabled={props.disabled}
      onValueChange={(v) => {
        const found = props.options.find((o) => String(o.value) === v);
        if (found) props.onChange(found.value);
      }}
    >
      <RSelect.Trigger className="select" id={props.id} aria-label={props.label}>
        <RSelect.Value>{current?.label}</RSelect.Value>
        <RSelect.Icon className="select-chevron">
          <ChevronDown />
        </RSelect.Icon>
      </RSelect.Trigger>
      <RSelect.Portal>
        <RSelect.Content className="select-menu" position="popper" sideOffset={4} collisionPadding={8}>
          <RSelect.Viewport>
            {props.options.map((o) => (
              <RSelect.Item key={String(o.value)} value={String(o.value)} className="select-item" disabled={o.disabled}>
                <span className="select-item-text">
                  <RSelect.ItemText>{o.label}</RSelect.ItemText>
                  {o.detail && <span className="select-detail">{o.detail}</span>}
                </span>
                <RSelect.ItemIndicator className="select-check">
                  <Check />
                </RSelect.ItemIndicator>
              </RSelect.Item>
            ))}
          </RSelect.Viewport>
        </RSelect.Content>
      </RSelect.Portal>
    </RSelect.Root>
  );
}

export function Switch(props: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label: ReactNode;
  detail?: ReactNode;
  disabled?: boolean;
}) {
  return (
    <label className={props.disabled ? "switch-row disabled" : "switch-row"}>
      <span className="switch-text">
        <span>{props.label}</span>
        {props.detail && <span className="switch-detail">{props.detail}</span>}
      </span>
      <RSwitch.Root
        className="switch"
        checked={props.checked}
        disabled={props.disabled}
        onCheckedChange={props.onChange}
      >
        <RSwitch.Thumb className="switch-thumb" />
      </RSwitch.Root>
    </label>
  );
}

export function Slider(props: {
  id?: string;
  value: number;
  min: number;
  max: number;
  step: number;
  onChange: (value: number) => void;
  label: string;
  valueText?: string;
  disabled?: boolean;
}) {
  return (
    <RSlider.Root
      id={props.id}
      className="slider"
      value={[props.value]}
      min={props.min}
      max={props.max}
      step={props.step}
      disabled={props.disabled}
      onValueChange={([v]) => props.onChange(v)}
      aria-label={props.label}
    >
      <RSlider.Track className="slider-track">
        <RSlider.Range className="slider-range" />
      </RSlider.Track>
      <RSlider.Thumb className="slider-thumb" aria-label={props.label} aria-valuetext={props.valueText} />
    </RSlider.Root>
  );
}

/** Tabs: only the shown panel is mounted (a hidden one keeps nothing,
 * secrets included); the panels share a fixed-size frame, so switching
 * never jumps. */
export function Tabs<T extends string>(props: {
  value: T;
  onChange: (value: T) => void;
  tabs: { value: T; label: ReactNode; disabled?: boolean }[];
  label: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <RTabs.Root
      className={["tabs", props.className ?? ""].join(" ")}
      value={props.value}
      onValueChange={(v) => props.onChange(v as T)}
    >
      <RTabs.List className="tab-list" aria-label={props.label}>
        {props.tabs.map((t) => (
          <RTabs.Trigger key={t.value} value={t.value} className="tab" disabled={t.disabled}>
            {t.label}
          </RTabs.Trigger>
        ))}
      </RTabs.List>
      {props.children}
    </RTabs.Root>
  );
}

export const TabPanel = (props: { value: string; children: ReactNode; className?: string }) => (
  <RTabs.Content value={props.value} className={["tab-panel", props.className ?? ""].join(" ")}>
    {props.children}
  </RTabs.Content>
);
