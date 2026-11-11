# M27c results: text boxes

## Why

Your request (2026-11-03, ADR 0013): those watching should write notes on the stream, not only draw, and text copied elsewhere should paste as a note. M27a carried the board between stream windows and M27b put it over the game at the host; this third part adds text boxes to both editors. The host's own stream window (M27d) and pictures (M27e) come later.

**Version 0.30.0**, wire revision 17 (a new element kind and field): it doesn't talk to 0.29.x or earlier. Viewer IPC 18.

## What changed

**For the user**, in the stream window's drawing mode and on the host's board over the game alike:
- **The Text tool** (T, the toolbar's T, between the ellipse and the eraser). A click starts a note there; typing shows in every other window as it goes; Enter starts a new line; Esc (or Ctrl+Enter) finishes it and selects it, to move at once; a click away finishes it too. While the Text tool is on, the width buttons are the text's sizes (small, medium, large) and the hints line says how to write.
- **Changing a note:** a double click on your own note with Select, or a click on it with the Text tool, puts the caret there; one Ctrl+Z undoes the whole edit. An emptied note goes away. Colour, size and fading picked while typing restyle it.
- **Typing goes through the system's text input**, so accents, dead keys (´ then e), AltGr's characters and input methods (Win+. for emoji, Japanese or Chinese IMEs) work: SDL's text input in the stream window (on only while a note is typed in, so no input method comes up over a game), and a hidden field on the board page. What an input method is composing shows underlined at the caret, and its candidates appear beside it.
- **Ctrl+V pastes text copied elsewhere as a new note** at the pointer (cleaned: line breaks unified, tabs as spaces, control characters dropped; cut at 500 characters, and said). Your own drawings copied with Ctrl+C still paste as drawings: the copy puts the copied notes' text (or nothing) on the clipboard, and a paste is ours while the clipboard still holds that. Pasting while typing puts the text in the note at the caret.
- Notes are painted over a soft black shadow so they read over any game; the stream window's `typing` preview state and the page's `board-typing` shot show one being typed.

**Settled while building** (also under the ADR's status):
- **A text box is two points and its text** (`ElementKind::Text`, `Element.text`): its top-left corner and the bottom-right corner as its author's window measured it. So picking, moving, the marquee, nudging and keeping inside the picture all work as for a box, unchanged. Other windows lay the text out with their own font from the first point; the second only says where the box is.
- **Each editor measures with its own font** (`editor::Measure`: egui's in the stream window, installed at its first paint since egui's fonts exist only then; the canvas's on the page), and **the shared cases with a fixed estimate** (half an em a character), so both editors' cases compute the same corners (within one unit: Rust rounds in `f32`, TypeScript in `f64`).
- **A note isn't `done` while typed**, so a fading one doesn't fade mid-word; finishing sends it again with `done`. Points are a stroke's alone: the host refuses `Append` on a note, so whatever finishes one left open (the host's `finish_open` when its author goes or is refused, a window's `finish_strays` after a restart) sends the box again, finished.
- **Text weighs a point a byte** (`Element::load`) against a batch's 4,000 items, a snapshot part's weight and the board's 200,000 points, so the limits that keep every frame under 64 KiB hold for text too (tested with the widest characters).
- **Characters are Unicode scalar values on both sides** (Rust's `chars`, `Array.from` in TypeScript), never UTF-16 units: the 500-character limit, the caret and Backspace count alike.

**The pieces:**
- `couch_proto::board`: `ElementKind::Text`, `Element.text`, `MAX_TEXT_CHARS` (500), `LINE_HEIGHT` (1.25), `text_size` and `TEXT_SIZE_NAMES`, `valid_text`, `clean_text`, `Element::load`; wire revision 17, IPC 18.
- `couch_room::board`: text counted by load, `finish_open` finishing a note with a `Put`; the property test puts notes (and empty ones, refused) and finishes members' open elements.
- `couch_input::shortcuts`: `DrawTool::Text`, T in `DRAWING_KEYS` (so Settings → Shortcuts lists it).
- `couch-viewer`: the editor's typing (`Typing`, `TextKey`, `type_text`, `text_key`, `copy`, `paste`, `double_click`, `text_cursor`), `draw_ui.rs`'s `paint_text`, the frame, caret and composition, the T icon and the sizes; `viewer.rs` routing keys, text, composition, the clipboard and double clicks while drawing, and SDL's text input on only while typing, its candidates placed at the caret.
- The engine: `BoardElement.text`, `BoardElementKind::Text`, and in `boardRules.ts` `TEXT_SIZES`, `TEXT_SIZE_NAMES`, `LINE_HEIGHT`, `MAX_TEXT_CHARS` and the text tool.
- The app: `model/editor.ts` ported case for case (`validText` and `cleanText` too), `board/paint.ts`'s `paintText`, `canvasMeasure` and the caret, `BoardSurface`'s hidden field (focused all through drawing mode), its composition events, and the page's own copy and paste events for the clipboard; the preview's board has a note.

## Checked

| Check | Result |
|---|---|
| `cargo test --workspace` (Windows), twice | all pass both times: couch-proto 83, couch-room 152, couch-viewer 37, the engine's own 123, its sessions scenarios 93, rooms 27 |
| The rooms and sessions scenarios, a third run | all pass |
| The new wire tests | `text_boxes_are_checked`, `pasted_text_is_cleaned`, `text_sizes_grow`; the fullest batch and every snapshot part with the widest characters still fit a control frame; the control round trip carries a note |
| The room's tests | `a_text_box_is_typed_then_finished` (no fading while typed, `Append` refused, the box finished whole when its author goes, long notes filling the board); the property test `the_board_holds` with notes and finished members (a temporary failing assert showed it reaches a note finished by the host) |
| The shared cases | eleven new ones, in `editor/tests.rs` and `editor.test.ts` alike: a note typed and finished as one step, the caret's keys, an emptied note, editing by a double click and a click, the 500-character limit, pasting text against our own copy (and near the edge, too long, while typing), a note left open and a clear while typing (kept being typed in, after the review), a style while typing (and the eraser), and from the review a size picked for a selected note, and the 50-line limit with a double click only with Select and a Ctrl+C of nothing; plus the front end's copies of `valid_text` and `clean_text` against the wire's own cases (U+FEFF and lone surrogates too) |
| The new scenario step | `drawings_go_through_the_host_to_every_window`: C's note seen by B as it is typed, then finished, then removed |
| `cargo clippy -D warnings`, `cargo fmt --check` | clean |
| `npx tsc --noEmit`, `npx vitest run` | clean, 92 tests |
| `npm run shots -- --only board` (dark, 1200x760) | no problems; `board-typing` typed by real key events through the hidden field: the canvas-measured frame fits the text |
| `viewer-shots.ps1` (`typing`, `drawing`) | the egui-measured frame fits the text, the composition underlined with the caret after it |
| `scripts/check-docs.sh` | nothing stale |
| After the review's fixes (Windows) | clippy clean, `cargo test --workspace` all pass; couch-viewer 39, the front end 94; `npm run shots` every scenario, look and size: no problems (the new `board-typing` shot clicks relative to the canvas, above the toolbar at 760x520) |
| `couch-cli release-check` (release build, after the review's fixes) | all pass: main process killed 258 ms, window killed 87 ms, window closed 161 ms, window frozen 376 ms (limit 600) and active again after thawing, host stopped 9 ms |
| `couch-cli input-sink` | pass: 118 keys, 5 buttons, 4 moves, 3 wheel turns through the router, in order; 13 blocked combinations held back |
| `couch-cli lifecycle-check` | all pass: window swap followed 26 ms, game exit 110 ms, window closed 5,272 ms (after the grace), stream window killed and restarted 41 ms (the seat never suspended), host killed seen 4,584 ms, a key held by a killed host released at once on the next start |
| Linux (WSL): fmt, clippy, the tests, the front end | all clean, every test passes; the front end's 92; after the review's fixes again all clean, the front end's 94 |

## Found while checking

- **The disk filled** during the first workspace build (3 GB free): links failed (LNK1180), and one left a corrupt PDB behind (LNK1285 on the next try). Deleting `target\debug\incremental`, rustc's cache, freed about 30 GB. Later a test run hung 48 minutes in two `link.exe` of couch-net's tests, the binaries the full disk had hit; with their PDBs deleted, the run passed. Not the change's; noted for the next long builds.

## The review

A fresh review agent went over the whole change. **Nothing high:** no key, text or click reaches the host as input while typing or after drawing mode ends, SDL's text input goes off whenever drawing mode or typing does, the host's checks refuse every malformed note, and the frame budgets hold. Each finding was checked against the code; all held but the third, which is yours to decide:

- **Medium, fixed:** picking a size for a selected note changed its text but not its box (picking, erasing and moving used the old one). Both editors now measure it again (`remeasured`); a shared case.
- **Medium, fixed:** a clear of our own elements while typing (also what the host's resync after a refused batch looks like: a full board, too fast) ended typing silently, losing what was typed since, and the next letters became tool keys. Now the note stays being typed in: it goes out whole at the next key or when finished, and its old self leaves the history with the rest. The shared case says so (it said the opposite before).
- **Medium, kept as built (your choice, 2026-11-10):** Ctrl+V of text copied elsewhere since your last Ctrl+C makes a note at once, seen by everyone: if that text was private (a password, a link), a habitual Ctrl+V meant for drawings shows it. As the ADR asked and as Excalidraw does. The alternatives weighed were pasting text only with the Text tool, or as a note kept to yourself until finished.
- **Medium, fixed:** the board page didn't paint an input method's composition until it was committed, and a composition cut short could show at the next note. It now repaints as it composes, and shows one only while a note is typed in.
- **Low, fixed:** a double click entered a note with any tool (the pen drew two dots first): now with Select only, as documented; a note left emptied could come back with its old self in a cleared history (fixed with the above); the front end's text checks took U+FEFF as a space and kept lone UTF-16 surrogates (the host's JSON would have refused every batch of that note), now as Rust's; the page's keys blocked astral characters typed directly; a Ctrl+C with nothing selected emptied the system clipboard (now it leaves it alone, both editors); letters went nowhere after a click on the toolbar's background (the field takes the focus back); notes had no limit on lines, and a member could make every window lay out hundreds of thousands (now 50 lines, `MAX_TEXT_LINES`, checked by the host); the stream window would have panicked on a malformed element of its own from the host (it now ignores invalid elements); a doc comment had slipped onto the wrong function; protocol.md overstated two things.

## Your run (2026-11-11), and what it asked for

**It worked** ("seems to be working fine"), and you asked for two things more, both built in this branch (your choices: the collapse handle, in this PR):
- **Text editing as other text tools do:** selecting text (Shift with any move, a drag, a double click on a word, Ctrl+A), Ctrl+Left and Right by word, Ctrl+Home and End, Ctrl+Backspace and Delete by word, typing and pasting over the selection, Ctrl+C and Ctrl+X of selected text; Up and Down now keep the caret's place across (measured), and Up on the first line or Down on the last go to its ends, as text fields do. In both editors, on three new shared cases (`words_and_the_selection_move_as_in_text_fields`, `up_and_down_keep_the_place_across_and_selections_replace`, `dragging_and_double_clicking_select_text`); a word is letters, digits and `_` (Rust's `is_alphanumeric`), a run of punctuation a word of its own, as Windows' text fields, and Ctrl+Delete takes the spaces after a word too.
- **The toolbar out of the way:** you found notes moved behind it couldn't be picked again, nor that area drawn on. A tab on its top edge (a chevron) folds it into a pill at the bottom centre with the tool and colour picked; a click on the pill, or Tab when not typing (a new drawing key, `DrawAction::ToggleToolbar`, the window's own), shows it again. The stream window remembers it for its life, the board overlay on this PC (its page's storage). Previews: the stream window's `drawing-hidden` state, shots `board-tools-hidden` and `board-typing-selected` (typed and selected by real key events).

**A second review** (a fresh agent, over the text editing and the toolbar): nothing high; every finding checked against the code and held:
- **Medium, fixed:** a double click on an empty line (or past a line's end after spaces) selected the line break, so typing joined the lines. A line break is now a kind of its own: a double click never selects one (an empty line selects nothing), and word moves stop at line breaks as text fields do (Ctrl+Right at a line's end goes just past the break, Ctrl+Left at a line's start just before it, Ctrl+Delete there takes the break alone). A shared case, `word_moves_stop_at_line_breaks_and_an_empty_line_selects_nothing`.
- **Low, fixed:** a held Tab toggled the toolbar on every repeat (now once); the stream window's pill and toolbar shared one egui area, laid out a frame late after a toggle (now each its own); a triple click selected the word again in the stream window only (now double clicks only, as the page); a drag selecting in a long line laid out every prefix of it per mouse move (now a binary search: a few layouts); an input method's composition cut short by a click could land in the next note on the page (the field is blurred, ending it); Alt with the text keys moved the caret on the page only (now neither); the stream window's pill was clickable only inside its margin; a doc comment had slipped.
- **Checked after these fixes:** clippy clean; `cargo test --workspace` all pass; the front end's 98; `npm run shots`, every scenario, look and size and the no-jump check: no problems; Linux (WSL): fmt, clippy, every test and the front end's 98, clean.
- **Kept, as they are:** moves go by character (Unicode scalar value), not by what reads as one letter: a decomposed "é" or a joined emoji can be split by Left, Right or Backspace, in both editors alike. And Ctrl+A, C, X and V follow the key's place on the keyboard, as every drawing key does (on AZERTY, Ctrl+A is the key marked Q).

**Your second run (2026-11-11), with the text editing, the toolbar's hiding and the review's fixes: passed** ("I think it looks perfect").

## Still to come

- **Two PCs:** your runs were on one PC; text boxes between two (another window's font laying out a note measured by this one) are still to see.
- M27d, the host's own stream window; M27e, pictures; then P16.
