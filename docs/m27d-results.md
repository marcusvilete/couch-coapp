# M27d results: the host's own stream window

## Why

Your request (2026-11-03, ADR 0013): the host should see what friends draw and draw back. M27b put the board over the game on Windows, but nothing of ours can lie over a game in exclusive full screen (it leaves full screen, often minimizing), and a Linux host has no board over the game at all. This fourth part gives the host a stream window of its own stream, at the PC, where the board shows and the host draws. Pictures (M27e) come next.

**Version 0.31.0**, the same wire revision 17: it talks to 0.30.0. Viewer IPC 19.

**Your choices (2026-11-11):**
- over a game in exclusive full screen, the drawing key opens the window **in front, in drawing mode** (the game leaves full screen), and as that drawing ends the game is brought back to the front;
- on a Linux host the window opens from the console only: no drawing key through the portal yet (it would bring the desktop's shortcut dialog back, and need a VM run).

## What changed

**For the user:**
- **"Your stream window"**, a switch in the console's drawing panel (Windows and Linux): a stream window opens with the stream as those watching see it, on a monitor other than the game's when there is one. Its bar says "Your own stream, as those watching see it", has Close for Leave, no volume and no seats; its pen draws as the host, and what it draws shows in every viewer's window and over the game, and theirs in it. It stays open across a stream stopping and the next starting, and closes with the session.
- **Over a game in exclusive full screen** (Windows), the drawing key opens it (or brings it forward), already drawing; Esc or Done there brings the game back. The console's full-screen line says so.
- **While it is in front**, the drawing key turns its drawing mode on and off (the low-level hook takes the key before the window hears it).
- **On Windows it is left out of what is streamed**, so streaming the whole screen doesn't stream it. A Linux share can't leave a window out: the console says to keep it on a screen not shared, or share a window.

**Inside:**
- `couch_platform_win::board_window::exclude_from_capture` (`SetWindowDisplayAffinity` with `WDA_EXCLUDEFROMCAPTURE`), called by the stream window on its own window (Windows refuses it for another process's).
- **The stream window's `--own-stream`** (`WindowOptions::own_stream`): no controllers (`InputCapture::new(.., with_pads)`), no sound, no input sent (`local` cleared), the HUD's own words (`Hud::own`: Close, no volume, "you" in the centre card, no asking), the toolbar's "Esc: done" (`Drawing::own`), and the `own` preview state for `viewer-shots.ps1`.
- **Viewer IPC 19:** `WindowRequest::Drawing(bool)` (the host's key for its own window; on waits up to 3 s for what the window's own key needs, its first picture above all, then says why not) and `WindowState.drawing`.
- **The host pipeline's own feed** (`HostPipeline::open_own`, `own_keyframe`, `close_own`, `own_encode_loop`): every stream's frames at 30 fps at most (a frame early waits its turn unless a newer one replaces it, so a still picture's last frame shows), H.264 at 4 to 20 Mbit/s by the picture's size, no rate control, never on the network; encoded frames over a channel of 8, the next a keyframe when it is full. It keeps the capture running as a viewer would (`State::watched`, `stop_unwatched`) and takes no share of the upload. GPU frames go to NVENC as for any viewer, under the same rules.
- **`pipeline::own_window`**: the engine's task for the window (like `watch`, without the network): Hello, shortcuts, the HUD (dropped when unchanged), board news, frames; from it only drawing, keyframe asks, its state and Leave are read, the rest dropped unread.
- **`rooms::sessions::own_window`**: `Command::OwnStreamWindow { open }` (refused from afar), the window's events, its HUD on every `announce`, `OwnWindowView` in the session's view (`open`, `drawing`, `left_out`), and the host's two windows as one author: `host_drew` takes a batch from either through `HostSession::draw` and sends the new `couch_room` effect `HostDrew` (a taken host batch) only to the other window; `to_host_windows` sends what the session tells the host to both.
- **The drawing key** (`board_overlay::host_draw_key`): our own window in front toggles its drawing; over exclusive full screen (drawing on, something live, no desktop control) it opens or raises the window, drawing, and marks it `by_key`, so the game comes back as that drawing ends or the window closes.
- **Backend:** `foreground_process`, `own_window_area` (another monitor's work area), `own_window_left_out`.
- **Console:** `OwnWindow` in `HostConsole.tsx`, the Linux line; scenarios `hosting-own-window` and `linux-host`'s view, shots `own-window` and `own-window-linux`.

## Checked

- **Windows:** `cargo fmt`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace`: all pass. Rooms (27) and sessions (94) scenarios three times each: all pass.
- **Linux (WSL, Ubuntu 24.04):** `cargo fmt --check`, clippy with `-D warnings`, `cargo test --workspace`: all pass; rooms and sessions three times: all pass.
- **The new scenario** `the_hosts_own_stream_window`: opened, its own frames (the first a keyframe), no seats in its HUD and the host as author; opened again only raises it; a key and a seat request from it reach nothing (no injector call, no request); its drawing reaches B's window and the board over the game once; the overlay's drawing reaches it once (two elements, not three); B's reaches it; the key while it is in front toggles its drawing; closed by the console; over exclusive full screen the key opens a second one, raised and drawing, nothing over the game, and Esc in it brings the game back; the key again asks it to draw again; closed by the host while drawing by key brings the game back; a stream stopped and started again keeps it, with the next stream's frames; the session's end closes it.
- **`couch-room`:** `the_host_draws_as_itself_to_every_viewer` checks `HostDrew` once for a host batch and never for a viewer's; the session property test checks `HostDrew` comes exactly when the host's batch was taken.
- **Front end:** `npx tsc --noEmit`, `npx vitest run` (98 pass); `npm run shots` for the new shots and the drawing ones: no problems, nothing jumps. Looked at: `own-window` (dark, 1200x760), `own-window-linux` (light, 960x640).
- **Live input checks** (`input-safety-checks`, with your go-ahead): `release-check` all pass, at the usual times (viewer's process killed: suspended after 262 ms; its window killed: gone after 75 ms; closed: 173 ms; frozen: suspended after 376 ms, active again after thawing; host stopped: 1 ms); `input-sink` pass (118 keys, 5 buttons, 4 moves, 3 wheel turns in order, 13 blocked combinations held back); `lifecycle-check` all pass (window swap followed after 31 ms, game exit 92 ms, source lost 5.19 s, stream window restarted 37 ms, host killed: lost after 4.59 s, a key held at a kill released 0 ms after the next start).
- **Your first run (2026-11-11):** "seems to be working".
- **The stream window by eye:** `viewer-shots.ps1 -Only own`: the bar with Close, no volume, "Your own stream, as those watching see it", and the toolbar's "Esc: done".

## Found while checking

- The words "back to the game" in the toolbar's hints, the Done button's tip, the pen's tip and the note on entering drawing mode didn't fit the host's own window: they say "done" and "stop drawing" there now.

## The review

A fresh review agent read the whole change. It found no safety break: nothing from the host's own window reaches the seat router, a forwarder, an injector or a request; the echo never sends a host batch back to the window that drew it, never goes on the wire, and no viewer can cause one; the window is refused from afar. Its findings, checked against the code:

- **Possibly high, fixed:** over a game in exclusive full screen the key comes through the hook while the game is in front, so the engine isn't the process in front, and the window's own raise (after `AllowSetForegroundWindow`) would be refused, as `bring_to_front` found in M25b: the window would draw behind the game, and the key, not finding it in front, could never end that drawing. The engine now brings it forward itself, finding its window by process (`Backend::window_of_process`, `process::window_of`) and tapping F24 first (`bring_to_front`), as soon as the window shows (`Own::raise`). The scenario checks it.
- **Medium, fixed:** the own window's encoder, failing to take GPU frames (NVENC's session limit, say), set the stream's `gpu_input_off`, turning GPU frames off for every guest. It now reads its own frames back alone (`gpu_off`, per stream).
- **Medium, fixed:** during desktop control the own window entered drawing mode and every batch was refused with the keyboard seat's words. Its HUD now says it is stopped then, and the window refuses up front: "Not while a device of yours controls this PC's desktop".
- **Low, fixed:**
  - after a dropped frame, units already encoded went out leaning on it: now nothing goes out until a keyframe;
  - a key request the window turned down left `by_key` set, so a later drawing ended by hand brought the game forward: it counts only if drawing begins within 5 s of the key;
  - the session's end skipped bringing the game back: `end_hosting` closes the window after input is released and the end is sent;
  - opening the window started a new host generation, staling a batch of the board over the game on its way: it now gets the board in the generation as it is (`HostSession::host_snapshot`), alone;
  - a refusal reached both windows: it now goes to the window that drew;
  - a window that stops reading could block its task: each write has 2 s, closing 5 s.
- **Low, kept:**
  - the two windows each pick new element ids, kept apart by hearing each other's batches; batches from both within that moment could share an id (one mouse makes it unlikely), noted in the `drawing-change` skill;
  - the board over the game asking for the board whole (its page loading) still starts a new generation, staling a batch of the own window on its way: rare, and a stroke loses a few points at most;
  - `left_out` is true on any Windows, though `WDA_EXCLUDEFROMCAPTURE` needs Windows 10 2004 or later (the viewer logs a refusal);
  - two presses of the key before the window answers toggle the same way twice.

**A second fresh review went over the fixes** (your rule since M22, after a high finding). Nothing high; it confirmed the raise (no loop, no other process's window: the child's handle keeps its id), the guests' GPU frames left alone, the drop logic, the routing, the generation kept, and the teardown order (`revoke_all`, the end sent, then the window, then the pipeline). Its findings:
- **Medium, fixed:** the 2 s limit on each write held from the start, so a cold start (the window's DLLs scanned) could close the window it had just opened: the limit is 15 s until the window first speaks, 2 s after.
- **Medium, fixed:** the key's 5 s were counted from the key, though the window's own 3 s wait for its picture begins only once it runs: they now count from the window's first word. And the key pressed while the window already draws (opened from the console) makes that drawing the key's.
- **Low, fixed:** one failed read-back ended the window (now a frame skipped, three in a row end it, as for the stream's read-backs); the window's HUD read desktop control as held where the session refuses once it is given (now the same); a key pressed to an open window with a picture waited 3 s to say why it can't draw (now at once); the window opened by a stream started by title went to the primary monitor's side, not the captured window's; CLAUDE.md now says refusals go to the window that drew, and names the own window's encoder as the exception to `gpu_input_off` and to one read-back per frame; the scenario now checks a refusal reaching the window that drew, and the game brought back as the session ends while the key's drawing is on.
- **Low, kept:** `raise` stays asked for on a backend that can't find windows (Linux, where nothing raises: harmless); the reverse of the generation case above; the window is one more NVENC session (8 with 7 viewers, GeForce's cap on current drivers), now in CLAUDE.md; the scenario doesn't see duplicated points, nor the desktop-control HUD or the own encoder leaving `gpu_input_off` alone (the fake's GPU frames could, later).

## Still to come

- **More of your run, if wanted:** the own window over a real game, windowed and in exclusive full screen (the key bringing it up drawing, and Esc giving the game its full screen back), on another monitor, with a friend drawing; streaming the whole screen with the window on it (it shouldn't show).
- **A Linux host's own window** on a real desktop (the VM): it isn't left out of a shared screen, which the console says.
- **A drawing key on Linux** through the global-shortcuts portal, if wanted later (your choice for now: the console only).
- M27e, pictures; then P16.
