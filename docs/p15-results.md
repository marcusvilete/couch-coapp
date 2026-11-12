# P15 results: the app's own title bar

## Why

Your choice (2026-11-10, option 1 of two): a title bar like Discord's, in the app's style, instead of Windows' white or black one above it. On 2026-11-12 you chose Windows only: Linux keeps its desktop's bar (KDE's or GNOME's), with the window's menu, its theme and GNOME's shadow, and no Linux work or VM run for P15.

No wire change: **version 0.32.2**, which still talks to 0.32.0 and 0.32.1.

## What changed

- **The main window has no frame of the system's on Windows** (`decorations: false`, set in `src-tauri/src/title_bar.rs`), and the page draws the bar (`shell/TitleBar.tsx`): the sidebar's grey to the top, the app's mark, the window's title (with the profile's name beside the default's, as before), and minimize, maximize or restore, and close, drawn as Windows 11 draws them (its own glyphs and close red). The title and the glyphs fade while the window is in the background. In High Contrast it has a border and the system's colours.
- **The app tells the page whether there is a bar before any of the page's scripts run** (an initialization script: `platform().titleBar`), so it is there from the first frame and nothing moves. The bar's height and buttons' width come with it, for the page and for the snap button alike.
- **What the system's title bar did, it still does.** The strip is WebView2's own title bar (`app-region: drag`, which wry turns on): moving the window, dragging it to a screen's edge to snap it, double-clicking it to maximize, right-clicking it for the system menu. The resize edges and the shadow are Tauri's.
- **Snap layouts on the maximize button.** Windows offers them only to a window that answers `HTMAXBUTTON`, and the webview answers for itself. So an invisible window of ours (`couch_platform_win::snap_button`, about 300 lines, instead of a plugin) lies over the page's button and answers for it. It takes the button's clicks, maximizing or restoring the window with the system's own command, and tells the page how to draw the button. It follows the window's size, and the page's own scale, which the page reports: WebView2 draws at the screen's scale times Windows' text size (Settings → Accessibility), which the window's scale leaves out. While the window isn't maximized, the top few pixels above the button still resize the window, as over Windows' own buttons. A double click's second press does nothing, as on Windows' own buttons.
- **Close is the system's close:** with the notification area icon on, the window hides there; without it, the app quits.
- **The sidebar's header says "Rooms"** where the bar carries the mark (Windows); under the desktop's bar (Linux) it keeps the mark and name.
- **Layers over the whole window leave the bar free:** dialogs, the splash and the restart cover start below it, so the window's buttons always work, and a click in the bar (moving, minimizing) leaves a dialog open. The screen shown when the app can't start, which says to close the window, has the bar too, and so does the one shown if the page itself fails to draw (a new error boundary): without the system's frame, a blank window would have had no way to move or close it but Alt+F4.
- **Menus, selects, tooltips and popovers stay below the bar** (`ui/popper.ts`), and none of them is ever the window's drag strip (`app-region: no-drag`): over the strip, a click on a menu item would have moved the window.
- The main window may now minimize and toggle maximize (two permissions, the main window's only).
- **After updating, the window opens once a few pixels off and about 30 pixels shorter:** its place was remembered with the system's frame around it. It is remembered as it is from then on.

## Checked

- **On this PC, by script** (a development build, profile `p15`):
  - over the maximize button, `WindowFromPoint` finds our window, which answers `HTMAXBUTTON`; over the strip WebView2 answers `HTCAPTION`; over minimize and close, the page;
  - a press and release sent to our window maximize the window and restore it; the page showed the pressed look, then Restore, then Maximize again;
  - maximized, the window fills the work area exactly (nothing past the screen's edge), and the snap button follows;
  - the page's Close closes the app as the system's would;
  - the page's drawing shows through our window;
  - after the review's fixes: the window's top edge over the button answers `HTTOP` (resizing), the button below it `HTMAXBUTTON`; a page scale of 1.25 sent as the page sends it moved the button to exactly the page's 125% button (x 1085 to 1142 of 1200), and 1 moved it back; the bar starts active with the window in front.
- `npx tsc --noEmit`, `npx vitest run` (103), `npm run build`, `npm run shots`: every scenario in three looks and sizes, no problems, nothing jumps. The bar was looked at in dark, light and High Contrast, at 1200×760 and 760×520 (the rail), with a dialog open.
- `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` on Windows; Linux through WSL (nothing changes there but `title_bar.rs`'s script, which says there is no bar).

## The review

A fresh review agent went over the change. Nothing high. What it found, and what was done:

- **Medium: with Windows' text size above 100%, the button lay in the wrong place**, and the left part of Close maximized. WebView2's scale includes the text size (its documentation says so); the window's doesn't. Fixed: the page reports its scale and the button follows it.
- **Medium: a popup reaching into the bar was the window's drag strip.** Fixed: popups stay below the bar, and are never a drag strip.
- Low: the bar could start in its background look (the window's focus says "no" while its webview has the keys). Fixed: the page's own focus.
- Low: a double click on the maximize button maximized and restored. Fixed, as above.
- Low: the button's hover look could stay after its capture was taken away (Alt+Tab while held). Fixed: hover is read from the pointer then, and leaving is watched on every move.
- Low: a page that failed left a frameless window. Fixed: the error boundary.
- Low: no resizing from the top edge above the maximize button. Fixed.
- Low: a panic caught in the button's callback still runs the process's panic hook, which lets go of a host's held keys. Only an event sent while the app's window is being destroyed could panic there, and the app is ending then: noted in the code.
- Low: the window's place after updating; noted above. Docs: the row's plan text, this page's line count and the capability's description, fixed.

It checked and found sound: the button's state and its freeing, z-order against WebView2's windows (nothing raises them above ours), threads, the capture's edge cases, minimizing and the notification area, the overlay and board windows (no script, no permissions; neither the frame nor the script is a WebView2 environment option, so the shared data folder's rule holds), the host-keys hook (untouched), the capabilities, Linux, and the front end's layers.

## Tried by hand

What a script can't do on this PC is move the real pointer. **You tried these on 2026-11-12, and everything worked as described**, the 125% text size included. In `npm run tauri dev`:

1. Hover the maximize button: the snap layouts appear after a moment; choose one.
2. Drag the bar: the window moves; to a screen's top or side, it snaps. Double-click the bar to maximize and again to restore. Right-click it for the system menu.
3. Resize from every edge and corner; look at the shadow and the corners.
4. Minimize, maximize, restore and close with the bar's buttons; with Settings → Remote play → "Keep Couch Co-App in the notification area" on, close hides it there.
5. Open a dialog (Create a room), then move and minimize the window by the bar: the dialog stays.
6. With a second monitor at another scale, move the window there: the maximize button still offers snap layouts.
7. Optional: Settings → Accessibility → Text size at 125%, then start the app: hovering the left edge of Close still lights Close, not maximize.
