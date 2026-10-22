// Menus: one list of actions per object, shown both by its "⋯" button and
// on right-click, so the two never differ (nothing is only in a context
// menu). An item the engine would refuse is shown disabled with its reason.
import * as CM from "@radix-ui/react-context-menu";
import * as DM from "@radix-ui/react-dropdown-menu";
import { ChevronRight } from "lucide-react";
import type { ReactNode } from "react";
import "./Menu.css";

export type MenuEntry =
  | {
      label: string;
      icon?: ReactNode;
      onSelect?: () => void;
      danger?: boolean;
      /** Disabled, with why. */
      disabled?: string | boolean;
      /** A hint after the label, such as "moves them". */
      hint?: string;
      items?: MenuEntry[];
    }
  | "separator"
  | { heading: string };

type Parts = typeof DM | typeof CM;

function entries(parts: Parts, items: MenuEntry[]): ReactNode {
  return items.map((item, i) => {
    if (item === "separator") return <parts.Separator key={`s${i}`} className="menu-separator" />;
    if ("heading" in item) {
      return (
        <parts.Label key={`h${i}`} className="menu-heading">
          {item.heading}
        </parts.Label>
      );
    }
    const why = typeof item.disabled === "string" ? item.disabled : null;
    const body = (
      <>
        <span className="menu-icon">{item.icon}</span>
        <span className="menu-text">
          <span>{item.label}</span>
          {(item.hint || why) && <span className="menu-hint">{why ?? item.hint}</span>}
        </span>
      </>
    );
    if (item.items) {
      return (
        <parts.Sub key={item.label}>
          <parts.SubTrigger className="menu-item" disabled={!!item.disabled}>
            {body}
            <ChevronRight className="menu-chevron" aria-hidden="true" />
          </parts.SubTrigger>
          <parts.Portal>
            <parts.SubContent className="menu" sideOffset={4} collisionPadding={8}>
              {entries(parts, item.items)}
            </parts.SubContent>
          </parts.Portal>
        </parts.Sub>
      );
    }
    return (
      <parts.Item
        key={item.label}
        className={item.danger ? "menu-item danger" : "menu-item"}
        disabled={!!item.disabled}
        onSelect={item.onSelect}
      >
        {body}
      </parts.Item>
    );
  });
}

/** A "⋯" (or any) button opening `items`. */
export function DropdownMenu(props: {
  items: MenuEntry[];
  children: ReactNode;
  align?: "start" | "end";
  onOpenChange?: (open: boolean) => void;
}) {
  return (
    <DM.Root modal={false} onOpenChange={props.onOpenChange}>
      <DM.Trigger asChild>{props.children}</DM.Trigger>
      <DM.Portal>
        <DM.Content className="menu" align={props.align ?? "end"} sideOffset={4} collisionPadding={8}>
          {entries(DM, props.items)}
        </DM.Content>
      </DM.Portal>
    </DM.Root>
  );
}

/** Right-click on `children` opens `items`; with no items, the right-click
 * does nothing (the page's own guard keeps the browser's menu away). */
export function ContextMenu(props: { items: MenuEntry[]; children: ReactNode; onOpenChange?: (open: boolean) => void }) {
  if (props.items.length === 0) return <>{props.children}</>;
  return (
    <CM.Root modal={false} onOpenChange={props.onOpenChange}>
      <CM.Trigger asChild>{props.children}</CM.Trigger>
      <CM.Portal>
        <CM.Content className="menu" collisionPadding={8}>
          {entries(CM, props.items)}
        </CM.Content>
      </CM.Portal>
    </CM.Root>
  );
}
