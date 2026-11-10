---
name: drawing-change
description: Change drawing on the stream (M27, ADR 0013) without the two editors drifting apart or the board's guards weakening — where each piece lives (couch_proto::board, couch_room::board and HostSession's board, the stream window's board.rs, editor.rs and draw_ui.rs, the front end's model/board.ts, model/editor.ts, board/paint.ts and BoardSurface, the engine's board.rs and rooms::sessions::board_overlay, couch_platform_win::board_window), how to keep the stream window's egui editor and the board overlay's TypeScript one alike (one rules file, one set of cases), which checks each part needs, and the pitfalls already met. Use when adding a tool, a key, an element kind, a limit or a board message, changing either editor or how either paints, changing the host's board over the game (its window, its follower, drawing mode), and for M27c (text boxes), M27d (the host's own stream window) and M27e (pictures).
---

# A change to drawing on the stream

Drawing on the stream is ADR 0013's, built in parts: M27a the board between stream windows (`docs/m27a-results.md`), M27b the host's board over the game (`docs/m27b-results.md`); M27c text boxes, M27d the host's own stream window and M27e pictures to come. The wire is `docs/protocol.md`'s "Drawing on the stream"; CLAUDE.md's Sessions rules ("Drawing on the stream", "The host's board over the game") are the law.

## Where it lives

| Piece | Rust | Front end |
|---|---|---|
| Elements, operations, limits, palette, widths, timings | `couch_proto::board` | `bindings/boardRules.ts` (generated, below) and the ts-rs types `BoardElement`, `BoardOpView`, `BoardNewsView` (`couch_engine::board`) |
| The host's board and its checks | `couch_room::board::Board`, `HostSession::draw`, `board_tick`, `set_drawing`, `clear_board`, `stop_drawing`, `host_board` | none: the host's page draws through `Command::HostDraw` |
| The drawing keys | `couch_input::shortcuts::DRAWING_KEYS`, `DrawTool`, `DrawAction` | `boardRules.ts`'s `DRAWING_KEYS`, `TOOLS`; `BoardSurface`'s `drawingKey` |
| What a window shows (fading, laser, pings) | `couch-viewer/src/board.rs` (`BoardView`) | `model/board.ts` (`BoardView`) |
| The editor (tools, selection, undo, copy and paste, batching) | `couch-viewer/src/editor.rs` | `model/editor.ts` |
| Painting and the toolbar | `couch-viewer/src/draw_ui.rs` (egui) | `board/paint.ts` (canvas), `board/BoardSurface.tsx`, `board/board.css` |
| The shared cases | `couch-viewer/src/editor/tests.rs` | `model/editor.test.ts` (and `board.test.ts` for `board.rs`'s) |
| The host's board over the game (M27b, Windows) | `rooms::sessions::board_overlay` (window life, follower, drawing mode), `couch_platform_win::board_window`, `Backend::board_area`, `place_board`, `board_interactive`; `src-tauri`'s `BoardWindow` | `board/main.tsx` (the page), `board/useBoardOverlay.ts` (the main window's relay), `sessions/HostConsole.tsx`'s `OverGame` |

## The two editors stay alike

The stream window's editor is egui in Rust, the board overlay's is TypeScript: two copies, held together three ways.
- **One rules file.** `boardRules.ts` is written by `cargo test -p couch-engine` (`couch_engine::board::rules_ts`, test `the_board_rules_are_written_for_the_front_end`) from `couch_proto::board` and `couch_input::shortcuts`. Never hard-code a limit, colour, width, timing or key in the front end: add it to the Rust table, to `rules_ts`, and run the test. A new `DrawAction` or `DrawTool` needs a line in `rules_ts`'s `action_ts` and `tool_name`.
- **One set of cases.** Every case in `editor/tests.rs` has its twin in `editor.test.ts`, same name in words, same steps, same numbers. A change to one editor's behaviour changes both editors and both case files in the same PR; a new behaviour gets a case in both.
- **The same painting.** `paint.ts` follows `draw_ui.rs`'s `element_shapes`: the same widths (`line_width`), see-through highlighter (0.38), arrow heads, laser trail (two strokes, a white head), ping (a ring pulsing twice in its time, the name below). Compare `preview.html?board` (shot `board-drawing`) with the stream window's `drawing` HUD state (`viewer-shots.ps1`).

Port by translation, not by rewrite: keep the function names (`press`, `drag`, `release`, `take_batch` → `takeBatch`), their order and their comments, so a diff of one shows where the other changes.

## Rules that are easy to break

- **Only the author's own:** elements are keyed by the connection's member (or the host's `set_me` id), never by a member named in a message. Editors select, erase, move and undo only `me`'s.
- **Generations:** a batch carries the generation it was made in (the outbox's, not the latest); a clear of ours or everyone's, and every snapshot, starts a new one; a stale batch is dropped unsaid. After a clear of ours (a snapshot among them) an editor calls `cleared`, then `finishStrays`; after any other news, `sync`.
- **Batches are taken whole or refused,** and refusals must never make the host resend a member's drawing over and over (`resync_due`, at most once a second).
- **The keyboard seat's holder draws nothing,** nor the host while desktop control lasts; drawing input never enters the input path (the stream window's `InputCapture::set_drawing`; the board page's keys never leave it).
- **The host's board over the game** is the M8 overlay rule's one exception: only the host's own key at the PC begins drawing mode; never a command, the webview or a device. Keep every end in place (the key, Esc or Done, the focus going once the board had it, the board unable to show, the panic key, drawing off, the stream stopping, desktop control, the end).
- **A wire change** (a new op, element kind, message or field) is a new wire revision: the `protocol-change` skill. Pictures (M27e) go on a stream of their own kind, never the control stream.

## Checks

| Touched | Run |
|---|---|
| `couch_proto::board`, `couch_room::board`, `HostSession`'s board | `cargo test -p couch-proto -p couch-room` (the property tests `the_board_holds` and the session's; a temporary failing assert must show they still reach a drawing taken, a viewer's and the host's), and the `room-security-checks` skill |
| Either editor or board model | `cargo test -p couch-viewer` and `npx vitest run` (the shared cases on both sides); `viewer-shots.ps1` (`drawing`) and `npm run shots -- --only board` |
| The stream window's drawing mode, the host's drawing mode, the drawing keys | the `input-safety-checks` skill (its drawing paragraphs), its live checks with the author's go-ahead |
| The engine's commands, events or the board window | `cargo test -p couch-engine` (rooms and sessions three times: `drawings_go_through_the_host_to_every_window`, `the_host_draws_over_the_game`, `watching_together_hooks_the_drawing_key_alone_while_drawing_is_on`), the regenerated bindings committed |
| The console's panels, Settings | the `front-end-change` skill, `npm run shots` (`drawing-panel`, `drawing-over-game`, `drawing-refused`, `drawing-menu`, `settings-drawing`) |
| Anything | Linux through the `linux-checks` skill (CI runs it) |

**What only a person can check:** the host's drawing key (the hook ignores injected keys), the board's look over a real game, and two PCs. The `app-manual-run` skill's "Drawing on the stream" lists what to ask for.

## Pitfalls already met

- **A swallowed shortcut key across drawing mode** (M27a review): the window swallows a shortcut's key so its release stays in the window; in drawing mode that release went to the board, and the key later stuck down on the host. Entering or leaving drawing mode forgets every swallowed key and button.
- **Sharing in TypeScript where Rust clones:** an op queued in the outbox and the element on the board must be separate objects (`cloneElement`, `copyOp`), or `merge` growing an outbox stroke also grows the shown one twice. Tests that replay sent ops must replay copies (`hostsCopy`).
- **Rust's `signum(0.0)` is 1, `Math.sign(0)` is 0:** a square or circle dragged straight down or across needs `dx < 0 ? -1 : 1`.
- **`WindowFromPoint` and click-through windows:** it sees through a window with both `WS_EX_TRANSPARENT` and `WS_EX_LAYERED` (measured in M27b), so the board over the game leaves `pointer_over` finding the game. A first test seemed to say otherwise: WinForms had dropped the style when its opacity was set. Check a window's real style before believing a probe.
- **tao's setters show the window** (`ShowWindow`): the engine changes the board's styles itself (`SetWindowLongPtr`) and places it with `SetWindowPos` and `SWP_ASYNCWINDOWPOS` (a move posted, never waiting on the app's UI thread); changing a style still waits on it, so only at drawing's start and end.
- **One webview data folder, one set of environment options** (the author's one-PC run of M27b): the board window shares the main window's data folder, and WebView2 refuses a second webview there unless its environment options match (the main window sets `scrollBarStyle: fluentOverlay`). `src-tauri`'s `BoardWindow::open` copies them from the main window's configuration; keep it so for any new one. And a window that can't be made is tried once, never on every change (each try flashed a window): `BoardOverlay::failed`.
- **A new webview is white until its page loads:** the board shows and takes the focus only after its page has said so (`HostBoard`), and that word can reach the engine before the engine hears the window was made: it counts while the window is opening.
- **Cached answers end things:** the follower kept the exclusive-full-screen answer 500 ms, and a drawing begun just after a game left full screen ended at once. While drawing it asks every tick.
- **The focus grace counts from the focus given,** not from the key: opening a window and loading its page can take longer than the grace.
- **A board window can go** (Alt+F4, a webview crash): the follower reports it gone and the engine opens another; the app refuses closing it any other way, and closes only the window it made (by handle, never by label).
- **Text kept even when empty** in the console uses a no-break space: an edit's old text must hold it too.
