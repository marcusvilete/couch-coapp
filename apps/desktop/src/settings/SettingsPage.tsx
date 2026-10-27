// Settings, as a page over the room (Esc goes back): your name, the look,
// the keyboard shortcuts (each can move to another key), the overlay, the
// stream window (P11), the network (which relay, applied when the app
// restarts), the drivers, a backup of the profile (M13), and About. Each
// section is mounted only while shown: a passphrase leaves with its page, and
// nothing polls behind it.
import {
  AppWindow,
  Archive,
  ArrowLeft,
  Cpu,
  Info,
  Keyboard,
  Layers,
  Network,
  Palette,
  UserRound,
} from "lucide-react";
import type { ReactNode } from "react";
import type { EngineInfo } from "../bindings/EngineInfo";
import type { ProfileView } from "../bindings/ProfileView";
import type { Busy } from "../model/sessions";
import { NameForm } from "../rooms/dialogs";
import type { ThemeChoice } from "../state/theme";
import { IconButton } from "../ui/Button";
import {
  AboutSection,
  AppearanceSection,
  BackupSection,
  DriversSection,
  NetworkSection,
  OverlaySection,
  StreamWindowSection,
} from "./sections";
import { ShortcutsSection } from "./ShortcutsSection";
import "./SettingsPage.css";

export type SettingsSection =
  | "profile"
  | "appearance"
  | "shortcuts"
  | "overlay"
  | "streamWindow"
  | "network"
  | "drivers"
  | "backup"
  | "about";

const SECTIONS: { id: SettingsSection; label: string; icon: ReactNode }[] = [
  { id: "profile", label: "Profile", icon: <UserRound /> },
  { id: "appearance", label: "Appearance", icon: <Palette /> },
  { id: "shortcuts", label: "Shortcuts", icon: <Keyboard /> },
  { id: "overlay", label: "Overlay and sounds", icon: <Layers /> },
  { id: "streamWindow", label: "Stream window", icon: <AppWindow /> },
  { id: "network", label: "Network", icon: <Network /> },
  { id: "drivers", label: "Drivers", icon: <Cpu /> },
  { id: "backup", label: "Backup", icon: <Archive /> },
  { id: "about", label: "About", icon: <Info /> },
];

export function SettingsPage(props: {
  section: SettingsSection;
  onSection: (section: SettingsSection) => void;
  onClose: () => void;
  busy: Busy;
  hasRooms: boolean;
  profile: ProfileView | null;
  info: EngineInfo | null;
  onProfile: (profile: ProfileView) => void;
  theme: ThemeChoice;
  onTheme: (theme: ThemeChoice) => void;
  /** A shortcut waits for its key: nothing else takes keys meanwhile. */
  onCapturing: (capturing: boolean) => void;
  capturing: boolean;
  onRestarting: (restarting: boolean) => void;
  /** Restored in a development build: said app-wide until it restarts, and
   * Backup offers no second restore. */
  restoredDev: boolean;
  onRestoredDev: () => void;
}) {
  const section = SECTIONS.find((s) => s.id === props.section) ?? SECTIONS[0];
  return (
    <div className="settings">
      <header className="settings-header">
        <IconButton label="Back (Esc)" onClick={props.onClose} disabled={props.capturing}>
          <ArrowLeft />
        </IconButton>
        <h1>Settings</h1>
      </header>
      <div className="settings-body">
        <nav className="settings-nav" aria-label="Settings">
          {SECTIONS.map((s) => (
            <button
              key={s.id}
              className={s.id === section.id ? "settings-tab selected" : "settings-tab"}
              aria-current={s.id === section.id ? "page" : undefined}
              aria-label={s.label}
              title={s.label}
              disabled={props.capturing}
              onClick={() => props.onSection(s.id)}
            >
              {s.icon}
              <span>{s.label}</span>
            </button>
          ))}
        </nav>
        <main className="settings-content" aria-label={section.label}>
          <div className="settings-inner">
            <h2 className="settings-title">{section.label}</h2>
            {section.id === "profile" && props.profile && (
              <div className="settings-section">
                <p className="settings-lead">Everyone in your rooms sees this name.</p>
                <NameForm profile={props.profile} onSaved={props.onProfile} />
                {props.info && (
                  <dl className="settings-facts">
                    <dt>Profile</dt>
                    <dd>{props.info.profile}</dd>
                    <dt>Its data</dt>
                    <dd className="selectable mono">{props.info.dataDir}</dd>
                  </dl>
                )}
              </div>
            )}
            {section.id === "appearance" && <AppearanceSection theme={props.theme} onTheme={props.onTheme} />}
            {section.id === "shortcuts" && <ShortcutsSection onCapturing={props.onCapturing} />}
            {section.id === "overlay" && <OverlaySection />}
            {section.id === "streamWindow" && <StreamWindowSection />}
            {section.id === "network" && <NetworkSection busy={props.busy} onRestarting={props.onRestarting} />}
            {section.id === "drivers" && <DriversSection />}
            {section.id === "backup" && (
              <BackupSection
                busy={props.busy}
                hasRooms={props.hasRooms}
                onRestarting={props.onRestarting}
                restoredDev={props.restoredDev}
                onRestoredDev={props.onRestoredDev}
              />
            )}
            {section.id === "about" && <AboutSection />}
          </div>
        </main>
      </div>
    </div>
  );
}
