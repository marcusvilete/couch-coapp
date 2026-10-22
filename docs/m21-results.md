# M21 results: a new interface

## Why

Twenty-three milestones each added their controls where they fitted. The screens filled up: long explanations everywhere, a host panel that was one long scroll with its requests below the fold, choices scattered over a rail, a column and a stage. You handed every design choice over (2026-10-21: "re-design completely the UI/UX… the absolutely best experience"), asking only for a desktop app, context menus, any licence that fits, and **no "wiggly" windows**. No engine, wire or IPC change: still wire revision 6, so 0.17.0 talks to 0.16.0.

## Seeing it

Until M21 the UI could only be seen by running sessions. Now:
- **A preview** (`preview.html`, development only) runs the app on a fake engine (`src/preview/fakeEngine.ts`) that answers every command as the engine would, refusals and their texts included, with latency and events arriving in either order. Scenarios (`src/preview/scenarios.ts`) cover every state worth seeing.
- **`npm run shots`** drives the Edge already on Windows, headless, through every scenario and the dialogs, menus and Settings pages, in three looks and three sizes. It fails on text spilling out of its box, a menu that doesn't open, and a region that moves as you go between rooms, Settings pages or a dialog's tabs (the no-jump check).
- **The running app** can be attached to from a script in development builds (a WebView2 debugging port, per launch); release builds drop the `WEBVIEW2_*` variables at start, since the port would let any program drive the app.

Every screen of this milestone was looked at this way before you saw it; the fixes it led to are below.

## The design, and why

- **Look.** A dark theme, a light theme, and Windows' High Contrast, in near-neutral greys after claude.ai's own settings (your choice, after the first version's warm tint and amber accent read as the "piss filter" AI-made screens are teased for): main buttons in the text's colour (black on light, white on dark), blue only for what you interact with (switches, checks, the chosen item, focus, links), clay only for the app's mark and the owner's crown, red only for live or danger, no yellow or brown avatars. Dark until you choose otherwise (your choice); Settings → Appearance offers Light and Like Windows (the title bar follows). Windows' own fonts (Segoe UI Variable, Cascadia Mono): crisp, and nothing to bundle or license.
- **Layout.** Rooms as named rows with a line saying what's happening there (the old round initials said nothing); the room's members beside it, online first, with how each is connected; the room's state (no password, catching up, a handover) as callouts at the top. Below 1180 px the members become a drawer, below 900 px the rooms a rail of icons, as before.
- **Sessions.** Others' sessions are cards with a tile per seat showing who plays it: watching, you ask for a seat by clicking its tile. Your own session is a console: what's streamed and its buttons; a request strip that is **always there**, so a new request never pushes anything under the pointer; **a patch bay** (your idea: drag and drop, "very cool"); the sound.
- **The patch bay.** Players on the left (you, with a plug per controller of yours; each viewer), seats on the right, and a cable from each seat to whoever holds it, in that player's colour, with a faint signal running along it while live; a request is a dashed, breathing cable to the seat asked for. Drag a player, or one of your controllers, onto a seat: a live cable follows the pointer and the seat lights up saying what letting go does ("Hands it over · Bia loses it", "Added", "Moves them", "Swaps with your Xbox", "control of your PC" on the keyboard seat), or why not ("Allow control first"). Pick a cable up by its plug at the seat: drop it in the open to take the seat back, on another seat to move it. Esc cancels. A keyboard seat no guest holds is wired to your own keyboard and mouse (your idea): they always reach the game and need no seat, so that plug is shown, not picked up; handing the seat to a guest moves the cable to them. Each drop sends the same command the seat's and viewer's menus do (they stay, for the keyboard and screen readers); the engine decides and the cables follow its answer. The drop rules are tested (`PatchBay.test.ts`), and `npm run shots` shoots a drag in progress, a drop and a cable pulled off. Choosing what to stream is its own dialog (it was half the old panel). Starting a session is one step, with a preview of the seats it makes.
- **Requests stay answerable from anywhere.** While you host, a bar at the top of every view (another room, Settings) shows the requests waiting, with Answer, and the panic key.
- **Say less.** Each warning keeps its place beside what it concerns (keyboard and mouse, admin windows, the panic key, Stop waiting); the long explanations moved behind **Why?**. The six confirm dialogs keep their words; the destructive ones put the focus on Cancel.
- **Right-click everything.** Rooms, members, sessions, seats and watchers have menus; each holds the same actions as the object's buttons, so nothing hides in a context menu. The webview's own menu (Refresh, Print) and browser keys (F5) are gone, but in text fields.
- **Nothing jumps.** Fixed regions and dialog sizes, buttons that keep their width when their label changes, placeholders the size of what loads; state appears in reserved space or below what you use.
- **Libraries.** Radix UI primitives (MIT) for menus, dialogs, tooltips, tabs and the like: keyboard and focus handled correctly. Lucide icons (ISC). Plain CSS with tokens rather than Tailwind (what I first suggested): the components stay readable and the build doesn't change. One Radix helper ships without its licence file; its repository's text is pinned in `xtask/licences/npm/`, and the notices say so.
- **The window** remembers its size and place per profile (`window.json`), placed before it shows; the overlay is never touched.

## Kept as it was

Everything the old UI did is in `docs/m21-inventory.md`, row by row, and the review checked each row. In particular: room views newest-revision-wins; an opened invite link never joins and waits behind dialogs, Settings, a shortcut capture or a restart (now a tested rule, `state/openedLink.ts`); the overlay's window code, step for step (moved to `platform/tauri.ts`); backup dialogs opened from Rust; restart flows in development and release; every remembered choice under its old storage key. The splash keeps its story and timing, in the new colours.

## The review

A fresh review agent read the new UI against the inventory and the safety rules. Fixed:
- **Right-clicking a room in the sidebar did nothing:** the tooltip around each row swallowed the menu's handlers. The tooltip now passes them on, and the screenshot run fails when a right-click opens no menu (checked: it fails with the old tooltip).
- **The title bar's theme never changed:** the main window lacked the permission (now granted to it alone).
- A development-build restore's "restart to finish" note showed only inside a room, never on the first-run screen; it is app-wide now.
- A dialog left with nothing to show (the stream picker after the session ended) stayed invisibly open, blocking the keys and any opened link; it closes now. The restart cover now blocks the keyboard too.
- High Contrast lost the chosen window, the selected Settings page, your seats and the oldest request: outlines now.
- The join dialog's password took the focus no more; a member's row could change group under an open menu; a request moving up under the pointer had a live Accept (its second's wait restarts now); a few wordings had drifted from the old UI.

## Your run

You found text spilling out of the "A seat of their own" button: segmented buttons now wrap rather than overflow, that choice gets the whole row, and the screenshot run checks every text for it.

Your one-PC run (2026-10-21): "way better already". Your further comments come as follow-up work after this PR.

## Known limits

- In the light theme, the window shows its dark background for a moment before the page paints.
- The stream picker shows window titles, not pictures: thumbnails or icons need the engine.
- The title bar is dark or light, not tinted: that needs a Windows call in `couch-platform-win`.
- The stream window's on-screen display (Rust) wasn't redone.
