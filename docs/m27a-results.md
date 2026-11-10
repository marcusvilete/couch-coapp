# M27a results: the board between stream windows

## Why

Your request (2026-11-03): friends watching want to point at things on the host's screen ("that door", "go left") instead of only saying it. ADR 0013 (2026-11-10) settled the design and split it in five parts; this is the first: the board, its wire, and drawing in the stream window. The host sees nothing over the game yet (M27b); text boxes (M27c), the host's own stream window (M27d) and pictures (M27e) come later.

A new wire revision: **version 0.28.0**, wire revision 15, so it doesn't talk to 0.27.x or earlier.

## What changed

**The ADR's open questions, settled while building:**
- **The board's own messages,** not riding in `SessionStatus`: a family of its own, `0x08xx`, all optional (`BoardOps`, `BoardUpdate`, `BoardSnapshot`, `BoardCleared`, `BoardState`, `BoardRefused`).
- **The board's logic in `couch_room::board`,** sans-IO, owned by `HostSession` (which adds what the board can't know: who watches, who holds the keyboard seat). The stream window's model (`couch-viewer`'s `board`) and editor (`editor`) are pure modules beside the egui painting (`draw_ui`).
- **A generation, not in the ADR:** every clear starts a new one, a member's batch names the one it was made in, and the host refuses an older one. Without it, a batch made before a clear the member hadn't heard of would land after it, and the member's window (which had dropped it) and everyone else's would disagree. A refused member then gets its own elements back (`BoardCleared` for it, then its elements), so what it shows of its own is the host's again. The window stamps its outbox with the generation its operations were made in, for the same reason.
- **The Settings default lives with the engine** (`drawing.json`, Settings → Drawing), so a session started from afar uses it too.
- **What a member drew stays when it stops watching,** until the host clears it.
- **Accepted batches go to every viewer but the author,** whose window drew them already.

**The pieces:**
- `couch_proto::board`: elements in the picture's own coordinates (0 to 65,535 each way), the palette of 8 and 3 widths, the limits (2,000 points a stroke; 300 elements a member, 1,500 and 200,000 points on the board; 40 batches and 60 laser points or pings a second), batches checked on their own, snapshots in parts that each fit a control frame.
- `couch_room::board::Board`: whole-batch checks (only the author's own elements, never a finished stroke extended, the limits counted on growth only, token-bucket rates counting every well-formed batch), fading elements dropped 7 s after they finish, the drawing order kept. `Replica` is a viewer's copy, which the watch pipeline keeps for a restarted stream window.
- `HostSession`: the keyboard seat's holder refused (as reported, a grant on its way, or desktop control), the board sent to a viewer as it joins, cleared when a stream ends and at the session's end; `set_drawing`, `clear_board`, `stop_drawing` (clearing theirs), `expire_board` (the actor's tick).
- The engine: the commands (`SetDrawing`, `ClearBoard`, `StopDrawing`, `Get`/`SetDrawingDefault`), `SessionView.drawing` (the host's and a session run from afar: the board's count, who drew, whom it stopped; a viewer: whether it may draw), `RemoteOp`s and `RemoteSession.drawing` for a device, the drawing keys in `Response::Shortcuts`.
- The stream window: drawing mode (Ctrl+Alt+Shift+D, or the bar's pen), the toolbar at the bottom (the tools, 8 colours, 3 widths, stays or fades, undo, redo, Done, a line of hints; wraps in a narrow window), the drawings painted over the picture, show or hide them here (Ctrl+Alt+Shift+B, or the bar's eye), a crosshair while drawing. The editor: strokes sent as they are drawn (every 30 ms), shapes dragged out with Shift for 15° lines, squares and circles, the eraser, select (click, Shift-click, a marquee) and move, copy, paste, duplicate, select all, delete, nudge, undo and redo of our own, a colour, width or fading picked applied to the selection, a long stroke going on in a new element, a ping on the middle button. Drawing mode ends whenever we may no longer draw, and forwards nothing of the keyboard or mouse; controllers keep playing. The icons are painted, not from a font, so they are alike on Windows and Linux.
- The app: a "Drawing on the stream" panel at the end of the host console (the switch, what's on the board, Clear, whom you stopped with Let them draw again), "Stop their drawing" / "Let them draw again" on each viewer's menu (the patch bay's players and the members panel), Settings → Drawing (the default, off) and the drawing keys listed in Settings → Shortcuts.

## Checked

| Check | Result |
|---|---|
| `cargo test --workspace` (Windows) | all pass: couch-proto 80, couch-room 147, couch-input 81, the stream window's 26 (2 ignored, as before), the engine's rooms 27 and sessions 90 scenarios (counts after the reviews' fixes) |
| The rooms, sessions and relay scenarios, three runs | all pass each time |
| The board's property test (`the_board_holds`) | each member changes only its own; a stopped member nothing; counts, limits and points hold; a viewer hearing every update and a newcomer from a snapshot both equal the host's board, in drawing order; fading elements leave in time |
| The session property test, with drawing ops | the keyboard seat's holder, desktop control and non-viewers never draw; updates go only to viewers; a temporary failing assert showed it reaches a taken drawing, and still a viewer on two pad seats (the drawing ops at weight 1: at 2 that reach was lost) |
| The new scenarios | `drawings_go_through_the_host_to_every_window`, `the_keyboard_seats_holder_draws_nothing`, `a_device_runs_the_board_from_afar` |
| The editor's cases (`editor/tests.rs`) | 12, from a stroke sent as drawn to the generation an outbox was made in; each checks that the window's copy of its own equals what it sent |
| `cargo clippy -D warnings`, `cargo fmt --check` | clean |
| `npx tsc --noEmit`, `npx vitest run` | clean, 66 tests |
| `npm run shots` (every scenario, three looks, three sizes, the no-jump check) | no problems; looked at `hosting-drawing`, `drawing-panel`, `drawing-menu`, `settings-drawing` |
| The stream window by eye (`viewer-shots.ps1`, the new `drawing` state, also at 480x300) | each element kind, a selection, a laser trail and a ping; the toolbar wraps in the small window |
| `scripts/check-docs.sh` | nothing stale |
| `couch-cli release-check` (release build, after the reviews' fixes) | all pass: main process killed 253 ms, window killed 98 ms, window closed 174 ms, window frozen 451 ms (limit 600; a little above its usual 350 to 400) and active again after thawing, host stopped 1 ms |
| `couch-cli input-sink` | pass: 118 keys, 5 buttons, 4 moves, 3 wheel turns through the router, in order; 13 blocked combinations held back |
| `couch-cli lifecycle-check` | all pass: window swap followed 17 ms, game exit 103 ms, window closed 5,127 ms (after the grace), stream window killed and restarted 49 ms (the seat active again after 417 ms), host killed seen 4,629 ms, a key held by a killed host released at once on the next start |
| Linux (WSL): fmt, clippy, the tests | clean, every test passes; the rooms and sessions scenarios five more times: sessions all five, rooms four, the fifth losing the two invite-expiry scenarios to WSL's clock (below) |

## Your run (2026-11-10)

Profiles on one PC: drawing in one stream window showed in the other, through the host. As expected for this part, the host sees only the console's count (the drawings over the game are M27b). **Your request after it:** the laser draws in the colour picked, not only red (`BoardOp::Laser.colour`, changed in place within wire revision 15, which hadn't left the branch); the toolbar's laser icon shows the colour picked.

## Found while checking

- **A disk full mid-run:** `target\debug` had grown to 147 GB; deleting Cargo's incremental cache (69 GB, your choice) freed it.
- **WSL's clock:** one Linux run failed `revoked_and_expired_invites_stop_working` and `an_opened_invite_is_previewed_before_joining`, which wait 2.1 s for a 1-second invite to expire. Windows' clock was 2.2 s off (`w32tm /stripchart`), and WSL's journal shows "Time jumped backwards" at that moment: the known pitfall in the `linux-checks` skill, not this change. Syncing Windows' clock fixes it.
- **The toolbar and the stats card** covered each other at the bottom: the stats card hides while drawing.
- **A preview's laser and ping** faded before the screenshot: the preview now renews them.

## The review

A fresh review agent went over the whole diff. What it found, each checked against the code:
- **High: a key could stay held on the host.** The window swallows a shortcut's key so its release stays in the window; in drawing mode that release went to the board, so the drawing key (D) stayed swallowed. Later, with the keyboard seat, pressing D went down on the host and never came up. Fixed: entering or leaving drawing mode forgets the swallowed keys and buttons, and a forwarded press is never swallowed; the key bookkeeping is now a small type of its own with a test (`a_key_forwarded_after_its_shortcut_is_always_released`).
- **Medium: a refused window could disagree with the host for good.** One board-wide generation meant stopping one member staled everyone's batches in flight; and after a refusal, batches already on their way were taken while the resync undid them in the window. Fixed: a generation per member (the newer of everyone's and its own, from one counter); a stale batch is dropped unsaid; a resync starts a new generation for that member, so what it sent meanwhile is dropped too; a window drops its outbox when its own elements are cleared.
- **Medium: refusals could make the host resend a member's whole drawing over and over.** Fixed: every batch counts toward the rates before any refusal; a refusal is said at once, and the resync comes at the next tick, at most once a second per member (`RESYNC_EVERY`).
- **Low, fixed:**
  - strokes left open forever (a seat given mid-stroke, a viewer leaving): now finished for everyone, so fading ones fade;
  - the host took drawings while nothing was streamed: now refused (`NoStream`);
  - a new editor not synced to the board's generation: synced at once;
  - whom the host stopped grew without limit, and stopping twice cleared again: kept within a room's size, and a repeat changes nothing;
  - the console's view was rebuilt for every batch: now once a tick;
  - the window's batch went on unchecked: checked (`valid_batch`) before it leaves for the host.
- **Low, kept:** undo history is forgotten when a refusal sends our own elements back (rare, and what was refused is gone anyway); the console's "Let … draw again" names the first member stopped, the others being on their menus.

Nothing found on: anyone touching another member's element, partial batches or drifting counts, frames over 64 KiB, messages from anyone but the watched host, `StopDrawing` for strangers or oneself, the window's rates, secrets in logs, the UI rules.

**A second, fresh review of the fixes** (the high finding asks for one) found them correct, and nothing high. Three low findings, each checked and fixed:
- the strokes the host finishes went out as one update, which past 64 would be dropped by every window: now in batches the checks allow (`many_open_strokes_are_finished_in_batches_the_checks_allow`);
- a snapshot didn't start a new generation: a batch a rejoining window made just before it could be taken by the host and wiped in the window by the snapshot. Now each snapshot starts one for its viewer (`a_snapshot_starts_a_generation_of_its_own`);
- a stream window that crashed mid-stroke left the stroke open on the host for good: the restarted window finishes its own strays once it has the board back (`a_stroke_left_open_by_a_crash_is_finished`).

Two notes kept: a window's batch that fails the engine's own check is dropped without a word (unreachable with today's editor, which batches through `batches`); and a member refused once a second can still have its own drawing sent back once a second, up to the board's 200,000 points, to itself alone: within the bound, though it competes with the video upload. Doubling `RESYNC_EVERY` on each resync in a row would tighten it if it ever shows.

## Still to come

- **Your run, two PCs:** draw from one stream window and watch it in the other; the host's switch, Clear and Stop; a guest given the keyboard seat leaving drawing mode; the laser and a ping; drawing while playing on a controller seat (the pad must keep playing).
- **M27b next:** the host's board overlay over the game on Windows, and the drawing key.
