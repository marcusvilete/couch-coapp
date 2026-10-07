# P3 results: shortcuts, network settings and the splash screen

Three items from the polish backlog that need only one PC:
- **The shortcut editor** (asked for on 2026-10-04). Every shortcut can move to another key in ⚙ Settings → Shortcuts. The panic key included.
- **Settings → Network.** iroh's public relays (for testing, the default) or your own relay by URL, applied when the app restarts, with a Restart now button.
- **A splash screen,** "Player 2 joins": two controllers slide in from the edges with their P1 and P2 lights blinking, land on a couch, and the name types out.

Your decisions:
- the editor changes the key only; Ctrl+Alt+Shift stays on every chord, and the one shortcut that works alone (F3, the overlay) can become another F-key;
- the splash is "Player 2 joins";
- Network offers public or your own URL. Either way the app tries a direct connection first; the relay starts the connection and carries it when no direct path is found.

**Wire changes:** none between members. Viewer IPC v7: a new `SHORTCUTS` frame (`0x09`), sent right after Hello, carries the keys you moved.

## How it works

**Shortcuts** (`couch_input::shortcuts`)
- `SHORTCUTS` stays the one table of defaults. Each row has a stable id (`panic`, `toggleControl`, `freeMouse`, `fullscreen`, `toggleHud`, `toggleHudAlone`).
- `Shortcuts` is the table in use: the defaults with your keys laid over them. The viewer, the panic-key hook, the HUD's hint and the host panel's reminder all read it.
- **Allowed keys** (`KEYS`): letters, top-row digits, F1 to F12 except F4, Backspace, and punctuation. Each has a plain scancode, which the panic-key hook needs (tested in `couch_platform_win::panic_key`).
  - The arrows, Insert, Home, End, Page Up and Down were allowed at first and dropped after the review: depending on NumLock, keyboards wrap those gray keys in "fake Shift" up and down events, which the panic-key hook can't tell from the real Shift, so a panic key there might never fire.
  - Refused: Esc (Ctrl+Shift+Esc is Task Manager), Tab, Delete (Ctrl+Alt+Del), F4 (Alt+F4 closes the stream window), Enter, Space, the modifiers, the Windows keys, Pause, Print Screen and the numpad.
- **No two shortcuts share a key,** the panic key included: on one PC its hook would swallow a viewer shortcut. The panic key can be moved but not removed.
- **Saved** per profile in `shortcuts.json` (only the ones you moved). A file that breaks a rule loses only the offending entries, which keep their defaults; the rest apply.
- **When a change applies.** The panic key is read at Go live, so a change made while live applies at the next Go live. The stream window's keys reach each new window right after Hello.
- **The app sends the browser's key code** (`KeyboardEvent.code`, a physical key like our HID usages), and the engine maps and checks it, so the rules live in one place.

**Network**
- `network.json` per profile: `"public"` or `{"custom": "https://…"}`. A URL must be https (http only for `localhost` and `127.0.0.1`, for testing); a typo is refused when you save.
- The desktop app starts with `EngineOptions::desktop`, which reads it. A custom URL runs iroh's minimal preset (no third-party address lookup), the shape M7 planned for production.
- **Restart now** goes through the app's normal exit (the rooms hear you go, the single-instance lock is let go), then starts the same profile. Release builds only: in your run, pressing it in the default profile under `npm run tauri dev` ended `tauri dev` too (it stops with its app, the page server with it), and nothing was left running. Development builds now say to close the app and start it again instead. The button itself is still to be tried in a release build.
- **Restart now felt unresponsive** in your release-build try: nothing changed on screen while the app told its rooms it was going. Now a "Restarting Couch Co-App…" screen with a spinner covers the window at once (painted before the restart is asked for), until the window closes; the new one opens with the splash. You tried it on the guest PC's release build: it works.
- **"Applies when the app restarts" stayed after the restart** in your run: the screen compared the URL as typed (`https://use1-1.relay.n0.iroh.link`) with the one in use, which iroh writes with a trailing slash. The engine now makes that comparison itself, as iroh reads both (`NetworkView.restart_needed`, test `a_relay_url_in_use_matches_it_as_typed`).
- **Members on different relays still meet:** invites and member records carry each member's own relay, and iroh dials relays outside its own list.
  - I planned a test of our own for this, with two local relays. iroh's test `endpoint_relay_map_change` already does exactly that: a client connects through a relay missing from its own list. So I relied on it rather than adding iroh's test relay server as a dependency.
  - Your internet run with friends checks it for real.

**The splash** (`apps/desktop/src/splash.tsx`)
- ASCII art and CSS only: about 1.75 s of animation, the finished picture for a second, then a half-second fade (lengthened after your first look, when it vanished too fast). It closes once it has played and the lobby has loaded, at the latest after 5 s, or at once on a click or a key.
- It animates even with Windows' animation effects off. It first followed that setting (`prefers-reduced-motion`) and showed only the last frame; this PC has animations off, so you saw a still picture, and chose to always animate it.
- The engine starts in about 0.1 s (alice's log), before the window shows, so the splash delays nothing.

**Where it is in the app:** the ⌨ button in the user panel is now ⚙ Settings, with two tabs, Shortcuts and Network.

## Checks

| Check | What | Result |
|---|---|---|
| Shortcuts (`couch-input`) | The defaults are valid and ids unique; every allowed key round-trips through its browser code; risky keys are refused; moving and resetting; no two rows on one key (the panic key onto a viewer chord refused, F3 alone and Ctrl+Alt+Shift+F3 are different presses); only F-keys for the row that works alone; saved keys laid over the defaults, including a swap that is valid only as a whole, with bad entries dropped and reported | pass |
| IPC (`couch-proto`) | `ShortcutKeys` round-trips | pass |
| Engine unit tests | `shortcuts.json` and `network.json` round-trip, damaged files fall back; `SetShortcut` refuses a taken key, Esc, an unknown code or row, a letter for the F-key row; reset one and all; relay URLs checked (no scheme, http to a remote host, ftp, no host refused; https and http://localhost saved); the next start binds with the saved relay; a hand-edited URL that no longer parses falls back to public | pass |
| Session scenarios | 29 (1 new: the panic key moved to P installs the hook on P and the host panel names it; a stream window gets Hello, then the viewer's keys, before anything else; another viewer's window gets the defaults; a change while live applies at the next Go live). 3 runs in a row, then 3 more | pass |
| Room scenarios | 20, twice | pass |
| Workspace | All tests (one of three runs exited 101 with its output filtered away, the other two were clean; the scenarios' port helper could hand one port to two tests, fixed in both suites); clippy (all targets), fmt, `cargo deny`, `tsc` and the Vite build clean | pass |
| `input-sink` | 118 keys, 5 buttons, 4 moves, 3 wheel turns through the seat router; 6 blocked combinations held back | pass |
| `release-check` | 273 / 60 / 164 / 380 / 0 ms (limits unchanged) | pass |
| `lifecycle-check` | swap 28 ms; exit 22 ms; source lost 5.2 s; stream window restarted 41 ms; host lost 4.7 s; a key left held released at the next start | pass |
| Your run | Below: the splash (after two changes: longer, and always animated), the panic key moved to P, alice's moved keys, and your own relay URL (after the restart-message fix) all work; Restart now works in the guest package's release build, with its restarting screen | pass |

## Review

A review subagent read the change (no high-severity findings). Each finding was checked against the code:
1. **The gray navigation keys could leave the panic key dead** (medium): keyboards wrap arrows, Insert, Home, End, Page Up and Down in fake Shift events depending on NumLock, and the hook's chord takes them for the real Shift. Fixed by dropping those keys from `KEYS`; `risky_keys_are_refused` now lists them.
2. **Nothing checked that every allowed key has a scancode the hook can use** (low): a new test in `panic_key.rs` checks each key in `KEYS` maps to a plain, non-extended scancode.
3. **One bad entry in `shortcuts.json` could undo a valid swap** (low): `with_keys` now drops unknown and not-allowed entries first, then tries the rest all at once. Test added.
4. **Restart now replays the launch arguments** (low): an app first opened from a `couchco://` link shows that invite's join screen again after restarting. Nothing joins by itself; left for the polish backlog.
5. **Keys are checked within one profile** (low): on one PC, a viewer profile's chord set to the host profile's panic key presses the panic key. Noted in the `app-manual-run` skill.
6. **Small ones**: a duplicated doc line on the tests' `free_port` removed; a relay URL with a user name or password is refused (the setting is logged); a failed Network poll's message now clears on the next good one, while a save error stays.

## Your run (one PC)

The default profile hosts, alice watches.
1. **The splash** plays, animated, at start (also with Windows' animation effects off); a click skips it.
2. **The panic key moved:** Settings → Shortcuts, move it to Ctrl+Alt+Shift+P, Go live. Ctrl+Alt+Shift+P takes control back while Ctrl+Alt+Shift+Backspace does nothing (Backspace reaches the game), and the host panel's reminder says P.
3. **The viewer's shortcuts:** in alice, move "Show or hide the overlay" (the F3 row) to F2 and "Full screen" to J, open the stream window, use both; its bar's hint names the new keys. Try giving two shortcuts one key: refused, with the reason.
4. **Your own relay URL:** in alice, Settings → Network → My relay, with iroh's own relay as shown in the status line (e.g. `https://use1-1.relay.n0.iroh.link`), then Restart now. alice comes back online on the minimal preset, which is the production shape, still meets the default profile, and can watch. Set it back to public afterwards.
