// Where popups (menus, selects, tooltips, popovers) may go: clear of the
// window's edges, and of its own title bar (P15, Windows), whose strip is the
// system's title bar there: a menu item over it would move the window.

/** Radix's `collisionPadding`, `edge` pixels from each side, below the bar. */
export function popupPadding(edge = 8) {
  const bar = parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--titlebar-h")) || 0;
  return { top: edge + bar, right: edge, bottom: edge, left: edge };
}
