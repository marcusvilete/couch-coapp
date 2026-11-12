// The window's own title bar (P15, Windows; Linux keeps its desktop's): the
// sidebar's grey to the top, the app's mark and the window's title, and the
// window's three buttons as Windows 11 draws them. The strip itself is the
// system's title bar (`app-region: drag`): it moves and snaps the window,
// its right-click is the system menu, a double click maximizes. The
// maximize button lies under a window of the app's, which answers for it
// (snap layouts) and says how to draw it.
import { useEffect, useState, type MouseEvent } from "react";
import { platform, type WindowState } from "../platform";
import { CouchMark } from "./Sidebar";
import "./TitleBar.css";

/** Windows' own caption glyphs (Segoe Fluent Icons; Segoe MDL2 Assets on
 * Windows 10). */
const GLYPH = {
  minimize: "",
  maximize: "",
  restore: "",
  close: "",
};

const AT_START: WindowState = { maximized: false, focused: true, hover: false, pressed: false };

/** The buttons never take the focus: the system's don't, and a click on
 * one must not pull the focus out of an open dialog. */
const keepFocus = (e: MouseEvent) => e.preventDefault();

export function TitleBar() {
  const bar = platform().titleBar;
  const [state, setState] = useState(AT_START);
  useEffect(() => {
    if (!bar) return;
    let live = true;
    let unlisten: (() => void) | null = null;
    void bar
      .onState((s) => {
        if (live) setState(s);
      })
      .then((u) => {
        if (live) unlisten = u;
        else u();
      });
    return () => {
      live = false;
      unlisten?.();
    };
  }, [bar]);
  if (!bar) return null;

  const maximize = state.maximized ? "Restore" : "Maximize";
  return (
    <header className={`titlebar${state.focused ? "" : " inactive"}`}>
      <span className="titlebar-brand">
        <span className="titlebar-mark" aria-hidden="true">
          <CouchMark />
        </span>
        <span className="titlebar-title">{bar.title}</span>
      </span>
      <div className="titlebar-buttons">
        <button type="button" className="caption" tabIndex={-1} aria-label="Minimize" onMouseDown={keepFocus} onClick={bar.minimize}>
          <span aria-hidden="true">{GLYPH.minimize}</span>
        </button>
        <button
          type="button"
          className={`caption${state.hover ? " hover" : ""}${state.pressed ? " pressed" : ""}`}
          tabIndex={-1}
          aria-label={maximize}
          onMouseDown={keepFocus}
          onClick={bar.toggleMaximize}
        >
          <span aria-hidden="true">{state.maximized ? GLYPH.restore : GLYPH.maximize}</span>
        </button>
        <button
          type="button"
          className="caption caption-close"
          tabIndex={-1}
          aria-label="Close"
          onMouseDown={keepFocus}
          onClick={bar.close}
        >
          <span aria-hidden="true">{GLYPH.close}</span>
        </button>
      </div>
    </header>
  );
}
