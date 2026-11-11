# M27e results: pictures on the board

## Why

Your request (2026-11-03, ADR 0013): besides drawing, friends watching can put a picture on the stream (a map, a screenshot, a meme) where everyone sees it, behind a switch of the host's. This last part of M27 does that.

**Version 0.32.0, wire revision 18:** it doesn't talk to 0.31.x or earlier. Viewer IPC 20.

**Your choices (2026-11-11):**
- a picture's size is set by **dragging its corners** (resize handles), not the width buttons;
- turning pictures **off takes them off the board**, for everyone, and stops new ones;
- pictures **fade like any element** (the toolbar's stays/fades);
- they come by **pasting and by dropping a file**.

## What changed

**For the user:**
- **A switch in the console's drawing panel**, "Let them put pictures on the board" (off, and disabled while drawing is off), and its Settings default under Drawing (off). A device running the session from afar has it too (`RemoteOp::SetPictures`).
- **Ctrl+V of a picture** (a screenshot, an image copied in a browser) in drawing mode, when the clipboard holds no text, in the stream window, the board over the game and the host's own window; **dropping a picture file** onto them while drawing. The picture lands where the pointer was, fitting 40% of the picture each way, selected.
- **Its corners resize it** (the one picture selected shows a handle at each), its shape kept, inside the picture. It moves, copies, erases, undoes and fades as any element.
- **Made smaller at the sender:** at most 1,280 pixels on its longer side and 1 MiB, a photo staying a JPEG, anything else a PNG (or a JPEG if too large and with nothing see-through). What a photo carries, such as where it was taken, is left out. Not allowed, or not a picture: the window says so.

**Inside:**
- **The wire** (`couch_proto::board`): `ElementKind::Picture` and `Element.picture` (the SHA-256 of its bytes), `BoardState.pictures`, `PictureAnswer` (`0x0807`), refusals `PicturesOff`, `BadPicture`, `TooManyPictures`, two stream kinds (`0x03` up, `0x04` down: two because a kind has one subscriber per connection and two members may host each other), `PictureUpload` and `PictureHeader` behind a length-prefixed header; `RemoteOp::SetPictures`, `RemoteDrawing.pictures`. `docs/protocol.md` has it all, "Pictures (M27e)".
- **`couch_media::picture`** on the `image` crate (PNG, JPEG, and BMP for a Windows clipboard's screenshot): `prepare` (orientation, scaling, encoding until it fits), `check` and `decode` with limits on sides and memory.
- **`couch_net::pictures`:** a picture on a stream of its own, read whole within 60 s, nothing after it.
- **The host's board** (`couch_room::board`): pictures' bytes kept by id while an element shows them, `put_picture` (an upload taken as a batch of one `Put`, under every batch check and the pictures' own), a batch's `Put` of a picture only moving one already there, `set_pictures` (off takes them off). **`HostSession`:** `put_picture` always answers a viewer, `refuse_picture`, `Effect::SendPicture` and `HostPicture`, each viewer sent each picture once (`pictures_sent`), `host_pictures` for a window that opens.
- **The engine** (`rooms::sessions::pictures`): a reader per viewer, from its join, on the connection it joined on, one picture at a time, decoded off the actor; the `Gate` holding a window's later batches until its picture is answered (or 75 s), for a member and for each of the host's windows; `Command::HostPicture` (the board page's, base64), `Command::SetPictures`, `Event::HostPicture`; the watch pipeline's picture reader (session and hash checked) and its store for a restarted window; the own window's IPC.
- **Both editors** (kept alike, three shared cases): an outbox of operations and pictures (`take_picture` hands a picture once what came before it has gone), `paste_picture`, `handles`, the `Resize` gesture, a picture touched anywhere in its box. **The stream window:** `pictures.rs` (decoding threads, the clipboard through `SDL_GetClipboardData`, dropped files), textures, `paint_picture`. **The board page:** `board/pictures.ts` (a canvas makes the picture, SubtleCrypto its id), the store, paste and drop, `paintPicture`, two events with the main window.
- **Also:** rustix 1.1.6 (RUSTSEC-2026-0486, which failed `cargo deny` on main too).

## Checked

- **Windows:** `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace` (44 test binaries): all pass. `cargo deny check`: advisories, bans, licences ok. `cargo xtask notices`: 480 crates, `image`'s with their texts.
- **`couch-media`:** a small picture stays a PNG, a large one is scaled to its longer side, a photo stays a JPEG and fits, a large opaque PNG becomes a JPEG and a see-through one shrinks, a clipboard bitmap with nothing in its alpha is opaque, a photo is turned as taken and loses its EXIF, only PNG and JPEG come from the wire, a picture cut short is refused, and **the decode bomb**: a 1-by-1 PNG whose header says 60,000 by 60,000 is refused before anything that size is made.
- **`couch-room`:** the board's pictures (only by upload, moved but never brought in by a batch, held while shown, freed with the last, fading out with their bytes), their limits (3 each, 16 and 8 MiB on the board), off taking them off; the session's (each viewer sent each picture once, a newcomer its bytes, erased and put back sent again, the host's own, refusals always answered); **both property tests** take pictures and the switch, with invariants on bytes held and counts, and temporary failing asserts showed they reach a taken upload, a taken move, and a viewer's picture (the session test still reaching a viewer on two pad seats and the host's drawing).
- **The new scenario** `pictures_go_through_the_host_with_their_bytes`: refused while pictures are off, nothing left; on, a paste then a move at once taken in that order; a newcomer gets the element moved and its bytes; junk bytes refused, never sent on; off takes it off both windows.
- **Front end:** `npx tsc --noEmit`, `npx vitest run` (101 pass, the three shared picture cases among them); `npm run shots -- --only drawing`: no problems, nothing jumps. Looked at: `drawing-panel` and `settings-drawing` (dark, 1200x760).
- **Live input checks** (`input-safety-checks`, your go-ahead): `release-check` all pass (viewer's process killed: suspended after 254 ms; its window killed: gone after 81 ms; closed: 189 ms; frozen: suspended after 367 ms, active again after thawing; host stopped: 0 ms); `input-sink` pass (118 keys, 5 buttons, 4 moves, 3 wheel turns in order, 13 blocked combinations held back); `lifecycle-check` all pass (window swap followed after 105 ms, game exit 103 ms, source lost 5.14 s, stream window restarted 38 ms, host killed: lost after 4.61 s, a key held at a kill released 0 ms after the next start). All within their usual ranges: a picture pasted or dropped reaches only the board, never the input path.
- **Linux (WSL, Ubuntu 24.04):** `cargo test --workspace`: all 44 test binaries pass; `cargo fmt --check` and clippy with `-D warnings` clean (after dropping three needless `mut`s in the new scenario, which Linux's clippy caught); the pictures scenario passes there too.

## Not checked yet, and what only a person can check

- **By hand, at one PC:** paste a screenshot (Win+Shift+S, then Ctrl+V) and a picture copied in a browser into a stream window in drawing mode; drop a photo file on it; resize by a corner; undo and redo; turn pictures off in the console. The same over the game with the drawing key (paste and drop on the board page), and in your own stream window.
- **Two PCs:** a friend's picture reaching you and a third window, and a newcomer's.
- **The previews** show no picture yet (the board preview and the stream window's HUD preview): a picture there needs its bytes made at run time.

## Your one-PC run (2026-11-11)

- Dropping pictures into the stream window and pasting a selection copied in Paint worked, and so did pasting a copied picture file over the game. **A picture file copied in Explorer didn't paste into the stream window:** Explorer puts a list of files on the clipboard, which SDL doesn't read on Windows. Now the stream window pastes it as a dropped file: on Windows read by `couch_platform_win::clipboard::copied_files` (`CF_HDROP`, `DragQueryFileW`), on Linux from SDL's `text/uri-list` (`pictures::uri_list`, with a test); the first PNG, JPEG or BMP among them, before the clipboard's text (a Linux file manager puts the path there too).

- **Your question: could shapes have the handles too?** Yes, rectangles and ellipses (their four corners, resized freely; Shift makes a square or a circle, as when drawing) and lines and arrows (a handle at each end; Shift in 15° steps), in both editors (`Editor::handles` for any selected element, the `Resize` gesture with the handle pressed), two shared cases each side (`a_boxs_corner_resizes_it_freely_and_shift_makes_it_square`, `a_lines_ends_move_by_their_handles_and_shift_steps_the_angle`). Strokes and text boxes have none: a text box's size is the width buttons'. No wire change (an ordinary `Put`).

- **Seen once, not reproduced:** on the first `npm run tauri dev` after these changes, the board over the game couldn't be turned on (stream windows worked); closing and starting it again fixed it, and its terminal showed nothing suspect. Not tracked down: a dev-server hiccup loading the board page is the likely cause, but if it comes back on a clean start (or in an installed build), it's a bug to chase.

## The review's findings

A fresh review agent found nothing weakening the input or room rules, and eight robustness issues, all real. Fixed:
- **The stream window kept every picture's decoded image** (up to 6.5 MB each) while its store counted only encoded bytes, and decoded each on a thread of its own: now one decoding thread, images kept only for the pictures shown (decoded again if one comes back), at most 32 pictures kept.
- **A host could make a viewer keep any number of pictures** by naming them on its board: the stream window's and the watch pipeline's stores keep shown pictures only up to the board's limit (16), whatever a host names.
- **The board page's store was never trimmed**, its images never closed, and `prepare` could leave its bitmap open on an error: trimmed past 32, closed, and `prepare` closes in a `finally`.
- **A newcomer's pictures could be stopped by the stream acceptor's queue** (16 waiting while 4 were read), never sent again: the viewer now takes every picture stream at once and reads them 4 at a time.
- **The host's picture sends had no deadline:** within 60 s now.
- **A picture whose bytes a window lost** (long after an erase, then undone) stayed shown in that window alone: it now leaves that window's board too, with a note.

Left for later (noted for P16's neighbourhood or a polish step):
- **The host decodes an upload before the cheap refusals** (pictures off, stopped, too fast): a misbehaving member costs a read and a decode per picture, one at a time. Asking the session first means restructuring the reader around the actor.
- **A late `PictureAnswer` can open a member's gate early** (an answer after the 75 s timeout, for an id put again since): batches behind the second upload may go first and be refused, and the next resync mends the board. A sequence number per upload would close it, a wire change.

## Found while building

- `cargo update -p rustix` also downgraded unrelated crates to "Rust 1.95 compatible" versions (syn 3 to 2), which broke `bindgen` in the workspace build; `--precise` updated rustix alone.
- The session test caught a picture erased and put back counted as already sent to the viewers: the record of what each was sent is now pruned before an upload is taken.
