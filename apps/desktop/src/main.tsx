import React from "react";
import ReactDOM from "react-dom/client";
import { installPlatform } from "./platform";
import { tauriPlatform } from "./platform/tauri";
import App from "./shell/App";
import { Boundary } from "./shell/Boundary";
import { installGuards } from "./shell/guards";
import { applyStoredTheme } from "./state/theme";
import "./styles/tokens.css";
import "./styles/base.css";

installPlatform(tauriPlatform);
installGuards(import.meta.env.DEV);
applyStoredTheme();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <Boundary>
      <App />
    </Boundary>
  </React.StrictMode>,
);
