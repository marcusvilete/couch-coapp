// The preview (development only, `preview.html`): the app on a fake engine,
// in a scenario picked by `?scenario=`. Options: `nosplash`, `bare` (no
// picker, for screenshots), `release` (a build that restarts itself),
// `latency=0` (replies and events at once), `sheet` (the design system's
// components instead of the app).
import React from "react";
import ReactDOM from "react-dom/client";
import { installPlatform } from "../platform";
import { installGuards } from "../shell/guards";
import "../styles/tokens.css";
import "../styles/base.css";
import { KEYS, writeStored } from "../state/storage";
import { loadWorld } from "./fakeEngine";
import { previewPlatform } from "./platform";
import { DEFAULT_SCENARIO, SCENARIOS } from "./scenarios";

const params = new URLSearchParams(window.location.search);
const name = params.get("scenario") ?? DEFAULT_SCENARIO;
const scenario = SCENARIOS[name] ?? SCENARIOS[DEFAULT_SCENARIO];

loadWorld(scenario.world(), { latency: params.get("latency") !== "0" });
// `theme=system|dark|light` sets Settings → Appearance's choice (the
// screenshots use `system`, so the browser's emulated look decides).
const theme = params.get("theme");
if (theme === "system" || theme === "dark" || theme === "light") writeStored(KEYS.theme, theme);
installPlatform(
  previewPlatform({
    release: scenario.release === true || params.has("release"),
    splash: !params.has("nosplash"),
  }),
);
installGuards(true);
document.title = `Preview: ${scenario.title}`;
// For the screenshot script.
(window as unknown as { __couchScenarios: string[] }).__couchScenarios = Object.keys(SCENARIOS);

const root = ReactDOM.createRoot(document.getElementById("root") as HTMLElement);
if (params.has("sheet")) {
  const { Sheet } = await import("./Sheet");
  root.render(
    <React.StrictMode>
      <Sheet />
    </React.StrictMode>,
  );
} else {
  const [{ default: App }, { PreviewPanel }] = await Promise.all([import("../shell/App"), import("./PreviewPanel")]);
  root.render(
    <React.StrictMode>
      <App />
      {!params.has("bare") && <PreviewPanel scenario={name} />}
    </React.StrictMode>,
  );
}
