// The webview is a browser underneath: its own right-click menu offers
// Refresh and Print, and F5, Ctrl+R and friends reload or print the app.
// Neither belongs in an app. Text fields and selectable text keep the menu
// (pasting an invite link with a right-click must work); the developer tools
// stay in development builds.

/** Keys the webview acts on by itself, by `KeyboardEvent.code`, with Ctrl. */
const CTRL_KEYS = new Set(["KeyR", "KeyP", "KeyF", "KeyU", "KeyS", "KeyG", "KeyJ", "KeyH", "KeyO", "KeyN"]);
/** And without. */
const PLAIN_KEYS = new Set(["F5", "F3", "F7", "BrowserBack", "BrowserForward", "BrowserRefresh"]);
/** The developer tools: kept in development builds. */
const DEV_TOOLS = new Set(["F12"]);

/** Where the native menu stays: editable fields and text meant to be copied. */
function keepsMenu(target: EventTarget | null): boolean {
  return (
    target instanceof Element &&
    target.closest("input, textarea, [contenteditable=''], [contenteditable='true'], .selectable") != null
  );
}

export function installGuards(dev: boolean) {
  window.addEventListener("contextmenu", (e) => {
    // Our own context menus (Radix) cancel the event themselves first.
    if (!e.defaultPrevented && !keepsMenu(e.target)) e.preventDefault();
  });
  window.addEventListener(
    "keydown",
    (e) => {
      const ctrl = e.ctrlKey || e.metaKey;
      const devTools = DEV_TOOLS.has(e.code) || (ctrl && e.shiftKey && (e.code === "KeyI" || e.code === "KeyJ"));
      if (devTools) {
        if (!dev) e.preventDefault();
        return;
      }
      if (PLAIN_KEYS.has(e.code) || (ctrl && CTRL_KEYS.has(e.code))) e.preventDefault();
    },
    // Ahead of the page's own handlers; it only stops the browser's action.
    true,
  );
  // The mouse's back and forward buttons navigate a browser.
  window.addEventListener("mouseup", (e) => {
    if (e.button === 3 || e.button === 4) e.preventDefault();
  });
}
