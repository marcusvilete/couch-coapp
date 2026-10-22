// The design system on one page (`preview.html?sheet`): every component in
// its states, for looking at the looks side by side.
import { Gamepad2, Keyboard, MoreHorizontal, Plus, Settings, UserPlus } from "lucide-react";
import { useState } from "react";
import { Avatar } from "../ui/Avatar";
import { Button, IconButton } from "../ui/Button";
import { Callout } from "../ui/Callout";
import { Segmented, Select, Slider, Switch, TabPanel, Tabs } from "../ui/Controls";
import { Dialog } from "../ui/Dialog";
import { Badge, EmptyState, Kbd, Skeleton, Spinner } from "../ui/Feedback";
import { Check, Field, PasswordInput, TextInput } from "../ui/Field";
import { ContextMenu, DropdownMenu, type MenuEntry } from "../ui/Menu";
import { Toasts } from "../ui/Toasts";
import { TooltipProvider } from "../ui/Tooltip";
import "./sheet.css";

const MENU: MenuEntry[] = [
  { label: "Invite people", icon: <UserPlus /> },
  { label: "Give a seat", icon: <Gamepad2 />, items: [{ label: "Controller 1" }, { label: "Controller 2", hint: "moves them" }] },
  { label: "Ban", disabled: "Not while catching up" },
  "separator",
  { label: "Close room for everyone", danger: true },
];

export function Sheet() {
  const [seg, setSeg] = useState(60);
  const [sel, setSel] = useState(20);
  const [on, setOn] = useState(true);
  const [vol, setVol] = useState(100);
  const [tab, setTab] = useState("one");
  const [pw, setPw] = useState("hunter22");
  const [dialog, setDialog] = useState(false);
  return (
    <TooltipProvider>
      <div className="sheet">
        <section>
          <h2>Buttons</h2>
          <div className="row">
            <Button variant="primary" icon={<Plus />}>
              Start a session
            </Button>
            <Button>Secondary</Button>
            <Button variant="ghost">Ghost</Button>
            <Button variant="danger">End session</Button>
            <Button variant="danger-ghost">Ban</Button>
            <Button variant="primary" disabled reason="Stop watching first.">
              Disabled
            </Button>
            <Button size="sm">Small</Button>
            <Button variant="primary" labels={["Start streaming", "Starting…"]}>
              Starting…
            </Button>
            <IconButton label="Settings">
              <Settings />
            </IconButton>
            <DropdownMenu items={MENU}>
              <IconButton label="Room options">
                <MoreHorizontal />
              </IconButton>
            </DropdownMenu>
          </div>
        </section>
        <section>
          <h2>Badges, keys, avatars</h2>
          <div className="row">
            <Badge tone="live">Live</Badge>
            <Badge>Not streaming</Badge>
            <Badge tone="accent">Owner</Badge>
            <Badge tone="warn">Needs update</Badge>
            <Badge tone="ok">Direct</Badge>
            <Badge tone="info">Relayed</Badge>
            <Kbd keys="Ctrl+Alt+Shift+Backspace" />
            <Avatar id="b1a" name="Bia" presence="direct" />
            <Avatar id="ca10" name="Caio Prado" presence="relay" />
            <Avatar id="dan1" name="Dani" presence="online" />
            <Avatar id="ed0" name="Edu" presence="offline" dimmed />
            <Avatar id="50fa" name="Sofá" square size={40} />
            <Spinner />
          </div>
        </section>
        <section>
          <h2>Callouts</h2>
          <div className="stack">
            <Callout tone="info" why="Members away may hold newer changes.">
              Catching up with the room until Bia and Edu have been online.
            </Callout>
            <Callout tone="warn" actions={<Button size="sm">Set a password</Button>}>
              This room has no password yet: set one to invite people.
            </Callout>
            <Callout tone="danger" title="Keyboard and mouse are control of your PC">
              Whoever holds that seat types into the game while its window is in front.
            </Callout>
            <Callout tone="accent" actions={<><Button size="sm" variant="primary">Accept…</Button><Button size="sm">Decline</Button></>}>
              Rafa offers you the room.
            </Callout>
          </div>
        </section>
        <section>
          <h2>Fields and controls</h2>
          <div className="grid">
            <Field label="Room name" hint="Friends see this name.">
              {(id) => <TextInput id={id} placeholder="Friday games" />}
            </Field>
            <Field label="Password" error="The passwords don't match." reserveError>
              {(id) => <PasswordInput id={id} value={pw} onChange={setPw} />}
            </Field>
            <Field label="Frame rate">
              {() => (
                <Segmented
                  label="Frame rate"
                  value={seg}
                  onChange={setSeg}
                  options={[
                    { value: 30, label: "30" },
                    { value: 60, label: "60" },
                    { value: 120, label: "120" },
                  ]}
                />
              )}
            </Field>
            <Field label="Quality for each viewer">
              {(id) => (
                <Select
                  id={id}
                  value={sel}
                  onChange={setSel}
                  options={[
                    { value: 5, label: "5 Mbit/s", detail: "slow connections" },
                    { value: 20, label: "20 Mbit/s", detail: "sharp at 1080p (recommended)" },
                    { value: 50, label: "50 Mbit/s", detail: "fast networks only" },
                  ]}
                />
              )}
            </Field>
            <Switch checked={on} onChange={setOn} label="Show requests over the game" detail="Never takes the focus." />
            <Field label={`Stream volume: ${vol}%`}>
              {(id) => <Slider id={id} value={vol} min={0} max={400} step={5} onChange={setVol} label="Stream volume" />}
            </Field>
            <Check checked={on} onChange={setOn}>
              Limit my total upload
            </Check>
            <Skeleton lines={2} />
          </div>
        </section>
        <section>
          <h2>Tabs, menus, empty state</h2>
          <Tabs
            label="Sources"
            value={tab}
            onChange={setTab}
            tabs={[
              { value: "one", label: "Windows" },
              { value: "two", label: "Screens" },
            ]}
          >
            <TabPanel value="one">
              <p className="pad">The Witcher 3 · Overcooked! 2 · Discord</p>
            </TabPanel>
            <TabPanel value="two">
              <p className="pad">Screen 1 · Screen 2</p>
            </TabPanel>
          </Tabs>
          <ContextMenu items={MENU}>
            <div className="target">Right-click me</div>
          </ContextMenu>
          <EmptyState
            icon={<Keyboard />}
            title="Nobody has a session yet"
            actions={<Button variant="primary">Start a session</Button>}
          >
            Start one to play with the room.
          </EmptyState>
          <Button onClick={() => setDialog(true)}>Open a dialog</Button>
        </section>
        <Toasts
          notices={[
            { id: 1, text: "Invites to “Sofá” revoked.", tone: "info" },
            { id: 2, text: "Lost the connection to Bia's stream, and it didn't come back.", tone: "warn" },
          ]}
          onDismiss={() => {}}
          onHold={() => {}}
        />
        {dialog && (
          <Dialog
            title="Ban Caio?"
            onClose={() => setDialog(false)}
            size="s"
            footer={
              <>
                <Button onClick={() => setDialog(false)}>Cancel</Button>
                <Button variant="danger">Ban</Button>
              </>
            }
          >
            <p>
              Caio is removed from “Sofá” and can't come back, even with an invite. Every invite is revoked too, in case
              they hold one.
            </p>
          </Dialog>
        )}
      </div>
    </TooltipProvider>
  );
}
