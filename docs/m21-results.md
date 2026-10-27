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

**With friends** (the 0.17.1 installer, `2dd1f5a`, reported 2026-10-26):
- They were mostly pleased with the new look. Everything else they tried worked, the new UI included.
- **Your own fresh install** offered only HidHide: the installer offers a driver only when its Windows service is missing, and ViGEmBus was already installed. You didn't restart as HidHide asks, nor play on a seat as the host, so that part wasn't tried.
- **A friend hosting on a fresh install had trouble with the virtual controllers.** Their account: the first session took a few seconds to start while Windows found new hardware and installed Xbox controllers; after the session ended, the controllers were still in Device Manager; they removed them by hand, and it took two or three restarts before it worked; in the end they played two controllers on that PC from afar. Their `desktop.log` (2026-10-23) shows what happened:
  - In the first session, all four seats' pads failed to come up: ViGEmBus's `VIGEM_WAIT_DEVICE_READY` answered 0x800701E3, "a fatal device hardware error", so each seat went without a pad ("its gamepad input will be dropped"). It happened again later, after an app start (16:43, probably after removing the devices by hand). Other sessions plugged all four, and a guest played.
  - **Three of those failed pads stayed plugged,** and the host's own controller reader then listed them as "another program's virtual pad", by their serials (48, 10, 3, from the failed plugs). They were still there after the app restarted: the controllers left in Device Manager.
  - So, likely: plugging a pad whose device Windows is still setting up (a new device instance, the first time each serial is used) can fail that check. `vigem.rs` then gives up at once: it drops the pad, whose unplug evidently didn't take, and the pad's marker with it, so the pad, no longer known as ours, reads as a remapper's (M22). Nothing was hidden (the host never chose one as their controller), but it is exactly what M22's rules must never let happen to our own pad.
  - **Fixed in P10** (0.17.3, [results](p10-results.md)): the root cause was vigem-rust choosing a random serial for each pad, each a new device to Windows; pads now take serials from 1, a pad Windows is still setting up is kept and waited for (up to 5 minutes, with Stop waiting), and its serial is in its marker from the moment it is plugged. Device Manager also keeps entries for unplugged devices, greyed under "Show hidden devices": those are harmless.
- **Several controllers on one guest** (M18) worked, a first with friends, but felt confusing: the patch bay shows a guest as one player, whatever their controllers. Wanted: a plug per controller, named (the controller's name, numbered when two are alike), as the host's own controllers are (P11).
- **Their ideas:** a session that streams without offering control, to show something or watch together (M24); and playing your own PC from afar, say a gaming PC at home from a laptop while travelling, which needs some way to let your own device in without anyone at home to accept (M25).

## Part 2: the stream window (0.17.2)

The stream window's HUD (`couch-viewer`, drawn with egui over the video) still had egui's stock grey buttons, three stacked lines of text, buttons wrapping onto two rows and a raw monospace stats dump. You asked for it to match (2026-10-26). No engine, wire or IPC change.

- **Seeing it.** `--hud-preview` has twelve states now (`viewer::Preview::NAMES`: watching, asking, playing two seats, asking for one more, paused, stopped, waiting, the host beside the seats, a single seat, muted, a crowded session with long names, everything at once), keeps the bar up and draws a test picture behind it (a white band where the bar sits: the worst case). `scripts\viewer-shots.ps1` captures each with PrintWindow, plus the crowded one at 480×300 and at 150% and a pointer on a seat chip, and fails on a black capture.
- **Look.** The main window's dark palette and Windows' fonts, read from `%WINDIR%\Fonts` once (Segoe UI Variable and its semibold, Segoe's icons, Cascadia Mono for the numbers); anything missing falls back to egui's, and without an icon font the icon buttons are words.
- **The bar** is four rows of fixed height, so nothing in it moves as its contents change (only a long note and "the game isn't in front" add a row, while they last): LIVE (or "not streaming"), the title, the host and how many watch, then the volume, full screen, the stats and, apart, Leave in red; a chip per seat saying who has it (yours in blue, "You"; one you asked for outlined; a free one "Free"; the host playing beside the seats a chip of their own), a chip you can ask for being the button (its tooltip says whom you ask, or why not); where you stand, a dot in its colour, with the host's note beside it (or below, when long) and the request's or the seats' button; the shortcuts, after the mouse's hint while you hold keyboard and mouse. "The game isn't in front" now shows only to guests playing or asking: it concerns their input. Long titles and names are cut with "…", the full text in a tooltip; in a narrow window the seat icons and the slider go first, and under 330 points the bar narrows with the window so Leave stays on screen.
- **The stats card** is a card of labelled rows, built only while it shows and at most four times a second (they were built every frame, the audio's taking its thread's lock); in a short window it shows the rows that fit, and says how many more.
- **Input safety, kept and written down** (`hud.rs`'s doc): no popup or editable value box, since the bar takes clicks only inside itself; capturing the mouse clears the HUD's pointer, so no hover or tooltip stays over the game; releases still come to the bar once their press did. The bar also stays up while the pointer rests on it, at most 15 s.
- **The review** (a fresh agent, before the PR) found nothing serious, and checked the input rules, the repaints and the fonts' fallbacks hold. Fixed: a seat's "Give back" button pushed the next chips 9 points left (it is placed inside its chip now, and never wider than it, so it stays inside the bar's clickable area); in a window under 330 points the bar's right end, with Leave, went off screen (the bar narrows with the window now; the shots include 320×300); freeing the mouse over a still picture left the old hint drawn, and an expired note too (both repaint now); the muted slider's tooltip; a width that could go negative in a tiny window; `--hud-preview-pointer` as the first argument.

## Known limits

- In the light theme, the window shows its dark background for a moment before the page paints.
- The stream picker shows window titles, not pictures: thumbnails or icons need the engine.
- The title bar is dark or light, not tinted: that needs a Windows call in `couch-platform-win`.
- The stream window is dark only: it sits over a game.
