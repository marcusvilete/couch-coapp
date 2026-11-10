# ADR 0013: Drawing on the stream

**Status:** accepted (2026-11-10), for M27; nothing built yet. The author's choices:
- those watching **and the host** draw, write and erase on the picture; a remote-play device too, as itself;
- **anyone but the keyboard-and-mouse seat's holder** draws (2026-11-10: a guest on a controller seat has a free mouse);
- each erases only what they made; the host clears the board, and stops one guest drawing;
- each stroke or note stays or fades, its author's choice; everyone shows or hides the drawings for themselves;
- **off by default:** the host turns drawing on per session, with a Settings default; pictures on the board have a switch of their own, off by default;
- tools like Excalidraw's, with its keys (undo and redo, copy and paste…), a laser pointer and pings, shapes and arrows, text boxes, and pictures;
- **the host draws over the game itself** (2026-11-10), in a drawing mode its own key turns on, which takes the focus from the game and gives it back;
- where nothing of ours can be over the game (exclusive full screen, a Linux host), the host sees and draws on the board in **a stream window of its own stream**.

## Context

Friends watching want to point at things on the host's screen: "that door", "go left". Today they can only say it.

What we have to build on:
- **The stream window** (`couch-viewer`) draws its HUD with egui over the video. Its rules (`hud.rs`): no popups, the bar takes clicks only while the mouse is free, and nothing animates every frame (Mailbox presentation would spin the GPU).
- **The host is the only writer of its session's facts** (`docs/protocol.md` Sessions), sent straight to each member, never relayed. Members act on a session message only from its host, on the current connection.
- **The host overlay** (M8) is a webview window over the game: unfocusable, click-through, left out of the capture, never over a game in exclusive full screen (P14: any window there makes the game leave full screen, often minimizing it). It may call no engine command.
- **Keyboard and mouse reach the game only while it is in front;** losing the front releases what a guest held (`SeatRouter::set_foreground`). Desktop control (M25b) is the exception: it injects wherever the pointer is.
- **A Linux host has no overlay** (GNOME lets no window of ours stay over a game; notices go to the desktop's notifications).

## Decision

### In parts

1. **M27a, the board between stream windows.** The wire, the host as hub, the stream window's drawing mode with every tool but text and pictures, the console's switches, remote play's ops. The host sees nothing over the game yet; the console counts what is on the board.
2. **M27b, the host over the game (Windows).** The board overlay over the captured window, showing the board; the host's drawing key making it a drawing surface.
3. **M27c, text boxes**, in both editors, and text pasted from the clipboard.
4. **M27d, the host's own stream window**, for exclusive full screen and Linux hosts.
5. **M27e, pictures**, behind their own switch.

Each part is a release of its own. M27a carries the wire revision; a later part that changes the wire takes another.

### The board

- **The board is the host's, in memory only.** It belongs to the session, is cleared when a stream ends (a new stream is a new picture), and is gone at the session's end. Nothing is written to disk.
- **An element** is a stroke (pen or highlighter), a shape (line, arrow, rectangle, ellipse), a text box, or a picture. It has:
  - an id: its author (a member) and a number the author picked. The host keys elements by **the connection's member**, never by a member named in the message, so nobody can touch another's element;
  - a colour from a fixed palette of 8, a width of 3, and whether it **fades**;
  - points in **the picture's own coordinates**, 0 to 65,535 on each axis, so every window places them alike, whatever its size. Widths and text sizes are fractions of the picture's height.
- **Fading:** a fading element stays 4 s after it is finished, then fades out over 1 s. Each window times it from when it arrived (no clocks to agree on). The host drops it from the board a little later, so a newcomer never gets it.
- **Passing marks are never on the board:**
  - the **laser pointer** is a trail that fades within 1 s;
  - a **ping** is a ring pulsing for 2 s, with its author's name, where they middle-clicked.

  The host fans them out like the rest, under their own rate limit, and stores nothing.

### On the wire

- **A member sends its own operations to the host:**
  - put an element (new, or replacing its own with the same id);
  - add points to its stroke in progress, about every 30 ms while drawing, so the others see the line as it is drawn;
  - remove some of its own elements;
  - a laser point or a ping.

  Operations go in batches over the control stream: they are small, and one ordered channel keeps put, append and remove in order.
- **The host checks each batch:**
  - the sender is in the session and watching;
  - drawing is on, and the host hasn't stopped them;
  - they don't hold the keyboard-and-mouse seat;
  - the limits hold.

  A batch that fails is dropped whole, and the window is told why ("the board is full", "the host stopped your drawing"). The host's own operations come from its window (or its stream window) and go through the same checks.
- **The host sends each member:**
  - the board whole when it starts watching, and when drawing is turned on;
  - each accepted batch, with its author;
  - the board's state: on or off, pictures allowed, who is stopped, and the limits.

  The host sends these straight to each member, never relayed, like every session fact.
- **Limits** (first values; the plan tunes them):
  - 2,000 points per stroke, simplified at the sender;
  - 300 elements per member, 1,500 on the board, 200,000 points on the board;
  - 500 characters per text box;
  - batches at most 40 a second per member;
  - laser points and pings at most 60 a second per member.
- **A new wire revision**, and a minor version (`couch_proto::REVISIONS`).

### The host's controls

- **Drawing on or off,** per session. It starts as Settings' default (off unless the user changes it). Turning it off clears the board.
- **Clear the board**, and **stop one member**, which also clears theirs. Both are in the console and the member's menu; nobody but the host can do either.
- **Pictures on or off,** a switch of their own, off by default (M27e).
- **Show or hide the drawings, for yourself:** in every stream window (a shortcut and the bar) and in the console for the overlay.
- **Remote play:** these switches are console ops (`RemoteOp`), so a device running the PC from afar has them, through the same checks. A device draws like any viewer, as itself.

### The stream window's drawing mode

- **A shortcut** (in `couch_input::shortcuts`, Ctrl+Alt+Shift+D by default) or the bar's pen button turns drawing mode on or off. Only while drawing is on, and never for the keyboard-and-mouse seat's holder: being given that seat ends drawing mode.
- **While it is on:**
  - the mouse is free and goes to the board, never to the game;
  - keys go to the board, never to the game;
  - a controller seat's pad keeps playing.

  Drawing input never enters the input path: the seat router never sees it.
- **A toolbar** at the bottom centre, as the HUD draws its bar, with no popups: tools, then 8 colour swatches, 3 widths, and stays or fades. The tools:
  - select;
  - pen, highlighter, laser;
  - arrow, line, rectangle, ellipse;
  - text;
  - eraser.
- **Keys while drawing, Excalidraw's.** They are a fixed group of their own in `couch_input::shortcuts`, listed in Settings → Shortcuts, not movable: they are each editor's own keys, like a text box's.
  - Ctrl+Z undo, Ctrl+Shift+Z and Ctrl+Y redo (your own only);
  - Ctrl+C, Ctrl+V and Ctrl+D to copy, paste and duplicate your own elements, and Ctrl+A to select them all;
  - Delete and Backspace delete the selection; the arrow keys nudge it (Shift further);
  - Shift draws straight lines, squares and circles;
  - V select, P pen, H highlighter, K laser, A arrow, L line, R rectangle, O ellipse, T text, E eraser;
  - Esc leaves the tool, then drawing mode.
- **Undo and redo** are each author's own, kept in the window: undo sends the inverse operation (removing what you put, putting back what you erased). What the host cleared is gone from the history. Erasing removes whole elements.
- **Text** (M27c) is typed through SDL's text input, so accents, dead keys and input methods work. A text box wraps only at the line breaks typed, so egui and the browser lay it out nearly alike.
- **Repainting:** the board is drawn with the video, and only fading elements and passing marks ask for frames between video frames (at most 30 a second, while they last).
- A Steam Deck's touch screen draws as a mouse does.

### The host over the game (Windows, M27b)

- **A second overlay window, the board overlay**, covers the captured window and follows it as it moves or resizes. Like the M8 overlay, it is:
  - left out of the capture (otherwise viewers would see every stroke twice);
  - never over a game in exclusive full screen;
  - click-through, created unfocused;
  - not shown while the game isn't in front.

  The toasts' overlay stays as it is.
- **The host's drawing key** (a host shortcut, Ctrl+Alt+Shift+D by default, through the same low-level hook as the panic key, which ignores injected keys) **turns drawing mode on**:
  - the board overlay takes the mouse and **the focus**, with its toolbar and a frame saying "Drawing: Esc to go back to the game";
  - the game isn't in front meanwhile, so a guest on the keyboard seat is held off and their keys released, as with any change of the front; some games pause or mute; controller seats keep playing;
  - Esc, the key again, or the toolbar's Done end it: the overlay is click-through again and the game is brought back to the front (`bring_to_front`). The panic key, the stream stopping and the session ending end it too.
- **This is the one exception to the M8 rule** that the overlay never takes the focus. It holds only:
  - for the board overlay;
  - in drawing mode;
  - turned on by the host's own key at the PC. Never from afar, never from the webview, never by a guest: injected keys don't reach the hook.
- **Drawing mode is refused:**
  - over a game in exclusive full screen: the window would minimize it. The key then opens the host's own stream window (M27d);
  - while desktop control lasts: the device's injected mouse would draw as the host.
- **The overlay calls no engine command,** as now. The board overlay's page sends the host's operations to the main window, which passes them on as the host's own drawing command. They are the same operations as a guest's, under the same checks.
- **Two editors:** the board overlay's is written in the front end (a canvas), the stream window's in egui. They share:
  - the wire's operations;
  - the limits;
  - the keys (one table, read by both);
  - a set of drawing cases, checked in both.

  A third copy, the stream window's egui on a transparent window over the game, was weighed: transparent wgpu windows on Windows aren't dependable (DXGI's flip model ignores alpha outside DirectComposition), so it is left for later, if ever.

### Exclusive full screen

- **Nothing of ours can be over it, nor go into its picture:**
  - any window over it makes the game leave full screen, often minimizing (found in P14);
  - drawing into the game's own frames means injecting into its renderer, as RivaTuner does. Rejected again: anti-cheat flags it, it would end up in the stream, and it means our code inside other programs.
- **So:**
  - the console says it plainly while drawing is on: "Drawings can't show over a game in exclusive full screen. Set the game to borderless or windowed full screen to see them" (on Windows 11, borderless costs most games little);
  - the host's drawing key opens the host's own stream window (M27d), to put on another monitor.

### The host's own stream window (M27d)

- **A stream window of the host's own stream:** the console's "Open the board", or the host's drawing key where the overlay can't be. Its frames come from the host pipeline locally, with no network. They come from an encoder of its own at a lower rate (30 fps, a modest bitrate), opened only while the window is.
- **It is a viewer with no seat and no input path:**
  - its drawing goes straight into the host's board as the host's own operations;
  - it can't ask for a seat;
  - its keys and mouse never reach an injector.
- **It is left out of the capture** on Windows (`SetWindowDisplayAffinity`), so a host streaming its whole screen doesn't stream it. A Linux host can't leave a window out of the portal's capture: the console says to put it on a monitor not being shared, or to share a window.
- On Linux it is the host's only view of the board until a KDE overlay (layer-shell), if ever.

### Pictures (M27e)

- **Pasted (Ctrl+V) or dropped onto the stream window or the board overlay,** while the host allows pictures.
- **Re-encoded at the sender** to at most 1,280 pixels on the longer side and 1 MB (PNG, or JPEG for photos). This also drops what a photo carries, such as its location.
- **Sent on a unidirectional stream of its own kind,** member to host, then host to each member: never through the control stream, whose frames are 64 KiB.
- **The host checks them:**
  - the kind and size;
  - it decodes each, with limits on dimensions;
  - at most 8 MB of pictures on the board, 3 per member.

  Windows decode with limits too (Rust's `image` crate, MIT or Apache-2.0, in the stream window; the browser in the overlay).

## Consequences

- **The control stream carries drawing traffic,** a few kilobytes a second per drawer at most, under the host's limits. Session control is ordered with it. A burst could delay a seat's message by milliseconds, which input never notices: input has its own channels.
- **Two editors to keep alike** (egui and the front end), held together by the shared table of keys and the shared cases.
- **The M8 overlay rule gains its one exception,** narrow and spelled out above. `CLAUDE.md`'s safety rules say so.
- **The host's own stream window costs an encode** while it is open. It is opened only on purpose, or by the key over exclusive full screen.
- **Moderation is the host's:** clear, stop, pictures off by default, and everyone can hide the board. Nothing is kept after the session.
- **Text** may wrap a little differently between egui and the browser. Text boxes keep to typed line breaks to make that small.

## Testing

- **`couch-room` (sans-IO):** the board's checks, as a property test:
  - nobody touches another's element;
  - a stopped member, or the keyboard seat's holder, changes nothing;
  - the limits hold;
  - a newcomer's board equals what the others built;
  - fading elements leave the board.
- **The sessions scenarios:** three engines drawing, with these cases:
  - drawing off and on;
  - clear;
  - stop;
  - a viewer given the keyboard seat;
  - a stream ending (the board cleared);
  - a newcomer;
  - remote play's ops.
- **The editors:** the shared cases (undo and redo, copy and paste, selection, fading), in Rust for egui's and in vitest for the front end's.
- **By eye:**
  - `--hud-preview` states for drawing mode;
  - preview scenarios for the console and the board overlay;
  - for M27b, a run over a windowed and a borderless game, and a check that drawing mode refuses over exclusive full screen (`input-safety-checks`).
- **`room-security-checks` and `input-safety-checks`** after each part. M27b also checks that a guest's keys are released when drawing mode takes the front.

## Open questions, for the parts' plans

- **Session control or its own messages:** whether the board's state rides in `SessionStatus` or a message of its own.
- **Where the board's logic lives:** a sans-IO module in `couch-room` (its checks, fading, limits), with the editor's own logic (hit testing, selection, undo) in a pure module the stream window uses.
- **The board overlay's window:**
  - whether Tauri's window calls for focusability and click-through change it in place without activating it at the wrong moment (M8's pitfall with showing hidden windows);
  - how it follows the captured window: polling its rectangle, or a WinEvent hook.
- **Stroke smoothing:** a simple spline is enough at first; a pen's pressure (SDL3's pen events) later, if wanted.
