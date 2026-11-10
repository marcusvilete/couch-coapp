# M27b results: the host over the game

## Why

Your request (2026-11-03, refined 2026-11-10 in ADR 0013): the host should see what friends draw on the stream over the game itself, and draw there too, with a key of their own that takes the focus and gives it back. M27a carried the board between stream windows; this is the second part, on Windows. Text boxes (M27c), the host's own stream window (M27d, for exclusive full screen and Linux hosts) and pictures (M27e) come later.

**Version 0.29.0**, wire revision 16 (an element's fade time, from your one-PC run): it doesn't talk to 0.28.x or earlier.

## What changed

**Your choice (2026-11-10):** a session to watch together (M24), which installs no hook of the host's keys, gets one with **the drawing key alone, while drawing is on**. CLAUDE.md's M24 rule now names that exception.

**Settled while building** (also under the ADR's status):
- **The engine places the board window; the app only makes it.** `couch_engine::board::BoardWindows` (in `src-tauri`) creates a transparent, click-through, never-activated window left out of captures, waiting off screen. From then on the engine lays it over exactly what is captured every 33 ms (a window's DWM frame bounds, a whole screen; `couch_platform_win::board_window`), parks it off screen rather than hiding it, and switches its styles itself with `SetWindowLongPtr`. Tauri's own setters would `ShowWindow` it, M8's pitfall.
- **It lies over the game only when there is something to see:** its page has loaded, the game is in front, no game is in exclusive full screen, and something is drawn or a laser or ping is passing. So no transparent window sits over a game for nothing (a full-window one over a borderless game can cost the game its independent flip).
- **The host is an author like any other** (`HostSession::set_me`): its batches go through `draw` under the same checks, refused while desktop control lasts; it hears the board as `Effect::HostBoard`, inside the host, never on the wire. No message changed.
- **The host's drawing key shares Ctrl+Alt+Shift+D with the stream window's**, as the ADR wanted: the one pair of rows allowed one key (`Shortcut::same_meaning`). It is hooked, and listed in Settings, only where the host draws over the game.
- **One editor's rules for both:** `bindings/boardRules.ts` (palette, widths, limits, timings, the drawing keys with their codes and actions) is written by `cargo test -p couch-engine` from `couch_proto::board` and `couch_input::shortcuts`, so the front end's editor reads the same tables as the stream window's.

**Measured before relying on it:** `WindowFromPoint` sees through a window with `WS_EX_TRANSPARENT` and `WS_EX_LAYERED` (a test form over another: it returned the window below). So with the board click-through over the game, `foreground::pointer_over` still finds the game, and a keyboard guest's clicks still count. (A first try had WinForms drop the style when its opacity was set, and seemed to say the opposite.)

**The pieces:**
- `couch_room::session`: `set_me`, the host's batches through `draw`, `Effect::HostBoard` for every board message the host should hear (updates from viewers, clears, its own refusals and resyncs), `host_board` (a snapshot in a generation of the host's own), `marks_until` and `board_has_elements` for the overlay.
- `couch_platform_win::board_window`: the capture's area, placing and parking the window, click-through on or off.
- The engine: `rooms::sessions::board_overlay` (the window's life, the follower, drawing mode and every way it ends, the seatless session's hook), `HostDraw`, `HostBoard` (also the page's word that it has loaded), `EndHostDrawing`, `ShowBoardOverlay`, `Event::HostBoard`, `SessionView.board_overlay`; the drawing key in the host's hook.
- The app: the board page (`board.html`, `src/board/`), its board model and editor ported from the stream window's (`model/board.ts`, `model/editor.ts`), painted on a canvas as the stream window paints (`board/paint.ts`), the main window passing the board to it and its drawing to the engine (`board/useBoardOverlay.ts`), and in the console's drawing panel "Show drawings over the game", the key's hint, and a line for why the key did nothing or a game in exclusive full screen. "5 drawings by you" for the host's own.
- The preview: `preview.html?board` (the board while drawing, over a stand-in game), scenarios `hosting-drawing-over-game` and `hosting-drawing-refused`.

## Checked

| Check | Result |
|---|---|
| `cargo test --workspace` (Windows) | all pass: couch-room 150, the engine's own 123, its sessions scenarios 92 (two new), rooms 27, relay 2 |
| The rooms, sessions and relay scenarios, three runs | all pass each time |
| The session property test, with `HostDraw` | the host draws only with drawing on, never during desktop control, its drawing reaching every viewer, nothing of the board to the host on the wire; a temporary failing assert showed it reaches a host's drawing taken |
| The new unit tests | `the_host_draws_as_itself_to_every_viewer`, `the_host_draws_nothing_during_desktop_control`, `the_overlay_shows_while_something_is_drawn_or_passing`, `the_two_drawing_keys_may_share_a_key_and_nothing_else_may`, the engine's `ops_cross_the_boundary_unchanged`, `news_names_members_by_id_and_says_why` |
| The new scenarios | `the_host_draws_over_the_game` (after the review, also: the key refused with the game off screen, drawing ended when the game goes off screen, a window gone by itself replaced; the window opened and parked, laid over the game when something is drawn, parked when the game goes behind or a game is in exclusive full screen; the key refused with drawing off and over full screen; drawing taking the focus only once the page has loaded; the host's drawing in a viewer's window; ended by Done, the key, the focus going elsewhere (the game not brought back) and the panic key; hidden, then opened to draw; closed with the stream and the session), `watching_together_hooks_the_drawing_key_alone_while_drawing_is_on` |
| The editor's shared cases in the front end | the 12 of `couch-viewer`'s `editor/tests.rs`, case for case, in `model/editor.test.ts`, all pass; the board model's 3 in `model/board.test.ts` |
| `cargo clippy -D warnings`, `cargo fmt --check` | clean |
| `npx tsc --noEmit`, `npx vitest run` | clean, 81 tests |
| `npm run shots` (every scenario, three looks, three sizes, the no-jump check) | no problems; looked at `board-drawing` (1200x760, 760x520: wraps), `drawing-over-game`, `drawing-refused` |
| `scripts/check-docs.sh` | nothing stale |
| `couch-cli release-check` (release build, after the review's fixes) | all pass: main process killed 253 ms, window killed 85 ms, window closed 174 ms, window frozen 349 ms (limit 600) and active again after thawing, host stopped 10 ms |
| `couch-cli input-sink` | pass: 118 keys, 5 buttons, 4 moves, 3 wheel turns through the router, in order; 13 blocked combinations held back |
| `couch-cli lifecycle-check` | all pass: window swap followed 52 ms, game exit 97 ms, window closed 5,239 ms (after the grace), stream window killed and restarted 38 ms (the seat never suspended), host killed seen 4,585 ms, a key held by a killed host released at once on the next start |
| Linux (WSL): fmt, clippy, the tests | first run: fmt and clippy clean, two failures, both under "Found while checking"; after the review's fixes: fmt and clippy clean, everything passes but the two 1-second invite-expiry scenarios in the full run (the known WSL clock pitfall, as in M27a); then rooms and sessions three more runs, all pass; after the one-PC run's changes (wire revision 16): fmt, clippy and every test clean, rooms and sessions three runs all pass |

## Found while checking

- **Seats: a transient breach after the panic key, older than M27b.** On Linux the session property test found a sequence (a pass, desktop control, the panic key, Allow, passes) after which the router briefly holds one viewer on two pad seats with no controllers reported. It uses no drawing op; pinned to `main`'s test, it fails there too: adding `HostDraw` only changed which random sequences proptest tries. **The cause:** the panic key drops viewers' pending grants from the session's books while their reports are still on the way; after Allow, a new grant of the same seat to the same viewer is answered by the old report, and the mirror then trails the router. It mends itself at the next report (`seat_changed` revokes the extra seat), so a viewer holds two pad seats for at most one report's delay, and only after a panic racing reports in flight. Keeping them as cancelled pending grants isn't right either: the pipeline refuses grants on their way after the panic key with no report at all, which would leave entries a later report answers wrongly. The fix needs reports matched to grants by something like the router's epoch: **a polish step of its own, P16** (your choice, 2026-11-10: after the whole of drawing), not in this branch. Its seed is not committed (it would fail every run).
- **On Linux, the shortcut list leaves the drawing key out** (it is hooked only where the host draws over the game), so `shortcuts_list_the_panic_key_first`, which counted every row, failed: it now states the rule per system.

- **Your one-PC run (2026-11-10): "The drawing window couldn't be opened", and rectangles flickering when the key was pressed.** The board window shares the main window's webview data folder, and WebView2 refuses a second webview on one folder unless its environment's options are the same: the main window sets `scrollBarStyle: fluentOverlay`, the board didn't. It now takes those options from the main window's own configuration. And the engine tried again on every change and tick, each try flashing a window: now a window that can't be made is tried once, and again only when you ask (the key, the switch) or a new stream starts (`a_board_window_that_cant_be_made_is_not_tried_over_and_over`).
- **A sound scenario failed under the whole suite's load, twice in the session** (`the_hosts_stream_level_scales_the_sound_and_is_remembered`: a level window straddling the drop to 0%), then passed six times alone. It has no board window and no drawing host: a timing flake of its own, not this change's. **Fixed after the merge, in the test:** the fake game clicks every 500 ms and the meter reads every 250 ms, so every other reading is silent at any level; the test took the first silent reading after setting 0% as the change, which could be a gap from before it, still queued under load. It now waits for two silent readings in a row (one of any two holds a click unless the level is 0); six full-suite runs then passed.

## Your one-PC run (2026-11-10)

**Passed** (your word, after the last changes: "it is perfect").

After the fix above it worked: your drawing over the game, guests' drawings over it, the refusals. What you found, and what changed:
- **The panic key** was always meant to end only *your* drawing over the game (the game back in front, control paused), never guests' drawing: the checklist I gave you was unclear, not the code.
- **Different icons** in your board's toolbar and Alice's stream window (the laser, fading): the board used Lucide's icons, the stream window paints its own. The board now draws the stream window's, point for point (`board/icons.tsx` from `draw_ui.rs`), Done included as plain text.
- **Your fading drawings never faded on your screen.** A fading stroke stays still a while, then fades; the board page painted only when something changed or moved, and nothing moves while it stays, so it never painted the fade. It now wakes itself when the next fade is due (`BoardView.nextFade`). The stream window had the same flaw, hidden by video frames: a still picture brings none (Windows' capture sends frames only on change), so it wakes the same way (`Drawing::fade_due`).
- **Your request: how long before a stroke fades, the 4 s as the default.** Your choice: the author picks it per stroke. Both toolbars offer 2s, 4s, 8s and 15s beside the fade toggle (picking one turns fading on, and restyles what is selected); each element carries its `stay` (one of those, anything else refused), every window fades it after it, and the host keeps it 3 s past its fade. **Wire revision 16** (and viewer IPC 17): 0.29.0 no longer talks to 0.28.0. **After trying it, your choice: one fade button** instead of four: it shows the fading line, plus the seconds once fading; a click goes round stays, 2, 4, 8, 15 s, and the mouse wheel over it steps longer or shorter, stopping at either end; one width either way, so nothing moves (`Editor::step_fade` and `stepFade`, a shared case `the_fade_button_goes_round_and_the_wheel_steps`; in the stream window the wheel reaches it through `Drawing::wheel`). A shared case in both editors (`picking_how_long_turns_fading_on_and_restyles_the_selection`), `a_longer_stay_is_kept_longer` on the host's board, `each_element_fades_after_its_own_stay` in the stream window.

## The review

A fresh review agent went over the whole diff. **Nothing high:** only the host's physical key begins drawing (no command, remote op or injected key can); a guest's keys and mouse can't reach the focused board; nothing of the board goes to the host on the wire; tao rewrites the board's styles only through its own setters, which nothing calls; the shortcut exception covers one pair only; the front end's editor matches the Rust one. What it found, each checked against the code:
- **Medium, fixed:**
  - **the 0.7 s focus grace counted the page's load:** a window opened by the key itself (drawings hidden, or a stream just live) could end drawing before it began. The follower now counts only once the board was given the focus;
  - **a board window that went (Alt+F4 while drawing, a webview crash) was never noticed:** drawing ended, but the dead window stayed and nothing showed for the rest of the stream. The follower now reports it gone and the engine opens another; the app refuses closing the board any way but the engine's;
  - **drawing with nothing to see:** a game minimized (borderless games often do when they lose the focus) let the key focus a board parked off screen, swallowing every key unseen. The key is now refused with the game off screen, and drawing ends if the board can't show;
  - **ending over a screen streamed gave the keys to nothing:** only a game's window was brought back. Now the window in front when drawing began is, on a screen.
- **Low, fixed:** the board window closed by its label could destroy a newer one made since (now closed by its handle); placing it waited on the app's UI thread (now asynchronous; changing its style still waits, only at drawing's start and end, as the comment now says); the host's board news was made on every system and while no board window was open (now only where the host draws over the game, and while its window is there); the page's send timer ran for the window's whole life (now only while drawing and until the last operation has gone); docs that weren't true (the capabilities line, the M8 rule pointing to its exception, a screen streamed always counting as the game in front).
- **Skills:** a new one, `drawing-change` (your go-ahead, 2026-11-10): where each piece of drawing lives, how the two editors stay alike, the checks per part, the pitfalls met in M27a and M27b.
- **Low, kept:**
  - **a shortcut row the user can't see still clashes:** on Linux the drawing key isn't listed, yet moving another row onto D, with the stream window's key moved off it, is refused as taken by "Draw over the game"; and on Windows a 0.28 profile that had put another row on D loses that key to the new row (logged). Rare, and the message names the row;
  - **the main window's permissions reach the board window by label** (`setPosition`, `setIgnoreCursorEvents`, `close`): defence in depth only, as the main window already holds `engine_command`.

**Two races the fixes' runs found:** under the whole suite's load, `the_host_draws_over_the_game` failed twice in four runs. (1) The follower's exclusive-full-screen answer, kept 500 ms, could end a drawing begun just after a game left full screen: while drawing it now asks every tick. (2) The page's "loaded" could reach the engine before the engine heard the window was made, and was ignored: it now counts while the window is opening. Then four and three runs of the suite passed.

## Still to come

- **A two-PC run** of drawing over the game (a guest on the keyboard seat held off while you draw, a real exclusive full-screen game), with M27a's two-PC run.
- **M27c next:** text boxes, in both editors.
