# M5b results: sessions on screen

M5b puts M5a's sessions in front of people:
- **The app.**
  - A card per live stream on the stage, with Watch, ask for the controller, and Stop watching.
  - A Go live dialog: window or screen, title, frame rate, quality per viewer, upload limit, how many can watch (4 by default, up to 7, with the upload cost), the keyboard-and-mouse switch with its warning, and the volume-mixer note.
  - The host's panel: viewers with Pass control or Take back, requests with Accept or Decline, a "control paused" banner with Allow control, Stop streaming, and the panic key's reminder.
  - A LIVE badge and a 🎮 for whoever plays in the member list.
  - The taskbar flashes on a request, and a notice says why a stream or a watch ended.
- **The stream window's HUD.**
  - A session bar shows the title, the host, who plays, where you stand and a note, with buttons to ask for the controller (or withdraw, or give it back), go full screen, show the stats and leave.
  - It shows after a change or while the pointer moves, and takes clicks only while the mouse is free; a click on it never reaches the game.
  - New shortcuts: Ctrl+Alt+Shift+C asks or gives back, Ctrl+Alt+Shift+F toggles full screen. The window takes the stream's title, and closing it stops watching.
- **The panic key**, Ctrl+Alt+Shift+Backspace: a low-level keyboard hook while you stream. It counts only physical presses, swallows the key so the game never sees it, and releases straight through the input task.
- **Every shortcut in one table** (`couch_input::shortcuts`), which the viewer, the hook and the app's ⌨ list read. Editing them is a follow-up.

No wire change: M5b uses M5a's messages and viewer IPC v3.

**It works**, with one limit of testing on one PC (below). Measured 2026-10-06 on the M1 PC (RTX 3060 Ti).

## Checks

| Check | What | Result |
|---|---|---|
| Shortcuts table | No two rows share keys; every shortcut has a row; only F3 goes without the chord, and only with a condition; the panic key is the host's, never the viewer's | pass |
| Panic-key logic (`Chord`) | The chord presses once and hides the key, its repeats and its release; Backspace alone or with part of the chord is the game's; injected keys never count, nor complete the host's half, nor release a physical modifier; a key-up lost to the secure desktop doesn't hold a modifier forever; the extended twin of a key is another key | pass |
| The real hook | Installs and is removed within 2 s; a key without a scancode is refused | pass |
| Engine | `GetShortcuts` lists the table, panic key first; the session scenarios press the panic key through the backend's hook (a fake one) and check the hook goes with the stream; the host's view says whether the hook is in place | pass |
| Pad markers | A running host's marker counts; a crashed host's and one whose process id was reused are stale and removed | pass |
| Workspace | 355 tests; clippy (all targets) and fmt clean; `cargo deny` ok; `tsc` and the Vite build pass | pass |
| `release-check` | Viewer's main process killed: suspended after 269 ms. Window killed: gone, 93 ms. Closed: gone, 226 ms. Frozen: suspended, 335 ms, active again after thawing. Host stopped: 1 ms. (Your three app instances were running alongside; an earlier run without them: 246, 59, 169, 361 and 9 ms) | pass |
| `input-sink` | 118 keys, 5 mouse buttons, 4 moves and 3 wheel turns, in order; your run after the review fixes passed too (one of mine failed only on a click made during it) | pass |
| HUD by eye | `couch-viewer --hud-preview watching`, `asking` and `controlling`, screenshotted: the bar, the note, the waiting line and the stats draw right | pass |
| **Your run** (alice, bob, carol and the default profile on this PC) | Going live, watching and stopping; asking, a decline and the cooldown, accepting; passing control and taking it back; the panic key (in the game, in other apps, and, after the fix below, in the app's own window) and allowing control again, then again after the review fixes, with the game in front and with the app in front; the game window closing; stopping; closing alice's app while a guest played; a ban mid-stream | pass |
| A guest's gamepad reaches the host | `scripts\xinput-watch.ps1` on the host while carol played with a DualSense: the host's virtual pad (XInput slot 0) showed every button, both sticks and both triggers under the right names | pass |
| A guest's gamepad in games, on one PC | Graveyard Keeper 2: only the D-pad. Heroes of Hammerwatch: nothing. Fling to the Finish (co-op): neither player. See "Things to know" | one-PC limit |

## Found and fixed during your run

- **The stream window crashed at once in the app.** The HUD read egui's texture updates without draining them, which egui checks only in debug builds; the app runs the debug viewer, the CLI the release one. It now drains them.
- **The HUD never appeared on a still picture** (before the first frame, a paused game). egui lays a new panel out unseen on its first frame and asks to paint again; video repainted anyway, a still picture didn't. The HUD now honours egui's repaint requests, and shows "Waiting for …'s picture" before the first frame.
- **The panic key went blind while the app's own window had the focus.** Tauri registers the app for raw keyboard input, and while a process that did is in front, Windows skips that process's own low-level keyboard hooks. Measured with a small test program: 0 of 2 keys with the registration, 2 of 2 without it or after removing it. The app now turns raw input off (`device_event_filter`); we never used it. Recorded in the hook's docs and the `windows-ffi-pitfalls` skill.
- **Games saw the host's virtual pad as an unknown joystick.** Since M2 it reported made-up ids (1209:0001) so a stream window on the same PC could skip it. Games and Steam pick a controller's layout by its ids; with unknown ones only the D-pad works, and Steam announced "XInput Controller #1". The app's pad now reports a real Xbox 360 controller's ids (045E:028E), and Steam announces "Xbox 360 Controller".
  - A stream window on the same PC now skips every Xbox 360 pad while a host here has a pad plugged in (a marker file per pad, `couch_platform_win::virtual_pads`), and checks again every second. Your choice, over finding ViGEm devices in Windows' device tree.
  - Skipping by XInput slot was tried first and doesn't work: the same pad had slot 0 in the process that plugged it and slot 1 in every other process (probably Steam renumbering pads in programs with windows).
From the code review (a subagent read the whole change; each finding was checked against the code before fixing):
- **A guest with keyboard permission could block the panic key.** The hook required Windows' own record of each modifier to agree it was down, but Windows keeps one down bit per key for physical and injected input alike: a guest's injected Shift or Ctrl key-up (sprint, crouch) cleared it while the host still held the key. Now, once an injected event has touched a modifier, the hook trusts only its own physical tracking for it. A guest still can't press or complete the key. New test: `a_guests_modifier_key_ups_cannot_block_it`.
- **A Backspace held before the chord could stick down in the game**: its auto-repeat, once the chord was complete, counted as the panic key, and its release was swallowed. A key that went down without the chord now stays the game's, repeats and release included; auto-repeat is told from a new press by timing, which also stops a lost key-up from costing a press.
- **Logging inside the hook** (the near-miss line added while diagnosing): every key on the PC waits for the hook, and a log write can block (a paused console). Removed; the hook only acts.
- **The pad marker could be lost** to a viewer reading it half-written, or when the host process couldn't be queried. Markers are now written aside and renamed, and removed only when their process is known to be gone or replaced. The host also marks before plugging the pad, so a viewer never sees the pad unmarked.
- **A panic-key press now names its stream**, so one queued as the host stops and goes live again can't pause the new stream.
- **The HUD bar could stay up** after the window lost the focus mid-click.
- **Go live:** a long window title is cut by bytes between characters, not mid-character; the upload limit takes whole numbers from 2 to 10000 only.
- Tests now check the hook goes on every end path (window closed, room left, shutdown), and the marker's doubtful cases.
- Smaller: the stream window's log is appended, not replaced; `profiles.ps1` gives its instances `dev.ps1`'s log level, and checks that `couch-viewer.exe` was built; `npm run tauri dev` builds it first; the panic-key hook logs each press, and a press with only part of the chord held.

## Things to know

- **On one PC, games choose your physical controller.** The guest's DualSense is plugged into the host's PC too, and games read it directly: Graveyard Keeper 2 (Unity) binds the player to it and lets other pads only move through menus, which is why the D-pad worked. Turning Steam's PlayStation and Xbox support off didn't change that. The stream itself is proven correct at the XInput level (above). A real test of games needs a second PC as the guest, or HidHide hiding the DualSense from games.
- **The same can happen for real.** A host with their own controller connected keeps it as controller 1, and a single-player game may ignore the guest's pad (controller 2). For now: the host disconnects their controller while a guest plays. A "strict mode" that hides the host's devices is in the plan for later.
- **The guest's controller becomes an Xbox 360 pad** on the host: Xbox button prompts, and no touchpad, motion or adaptive triggers. Rumble works.
- **A game running as administrator (not measured).** Windows' integrity levels likely keep the panic-key hook from seeing keys aimed at an elevated window, while a guest's gamepad still reaches the game, so the panic key may not work there. Keyboard and mouse can't reach an elevated game at all (architecture.md, "Injection limits"). To check with such a game; if confirmed, the app should warn when the chosen window runs elevated.
- **Keyboard and mouse can't be tried on one PC**, by design: they reach the game only while its window is in front, and then the stream window doesn't have the keyboard.
- **A guest's hold was suspended twice for under 100 ms** during your run (the host's 300 ms dead-man timer). Likely the debug stream window stalling, or a window being moved, which pauses its loop; the hold came back by itself. Worth watching in M6's heartbeat work.
- **The default profile logs only to the `npm run tauri dev` terminal**; profiles started by `profiles.ps1` log to `desktop.log`.

## Follow-ups

- **Seats and input modes (your requirement; very important).** Passing and sharing controllers must feel seamless.
  - **Single player:** one input at a time, a controller or keyboard and mouse. The game sees exactly one controller (if any), never the host's own pad beside the virtual one.
  - **Multiplayer:** several controllers and/or keyboard and mouse, one per player who has one; the game sees one controller per seat.
  - **The host describes how the game expects input:** single player, or local multiplayer with a number of seats that players share out.
  - **A request names a specific seat.**
  - This takes in the host's own controller taking controller 1 (seen in M5b) and the planned strict mode and co-op seats. The protocol already carries seat ids; single player uses seat 0.
- **Editing shortcuts** (your request); the table is ready for it.
- **A PlayStation-style pad** (ViGEm's DualShock 4) as a per-session option, for games that show PlayStation prompts.
- **M6**: heartbeats, viewer restart, per-member permissions, lifecycle. **M7**: a real two-PC test of games and keyboard and mouse. **M8**: answering requests in-game.

## How to reproduce

```powershell
. .\scripts\dev.ps1
cargo test --workspace
cargo run -p couch-viewer -- --hud-preview controlling     # the HUD by eye
cargo build --release -p couch-cli -p couch-viewer
.\target\release\couch-cli.exe release-check
.\target\release\couch-cli.exe input-sink
cd apps\desktop; npm run tauri dev                          # then .\scripts\profiles.ps1
.\scripts\xinput-watch.ps1                                  # what XInput games see, on the host
```
