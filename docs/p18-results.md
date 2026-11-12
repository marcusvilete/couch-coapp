# P18 results: a guest's pointer kept in the game window on a Linux host

## Why

Three things in one step (**your choice, 2026-11-12**):
1. **The pointer roamed** (your request at M16c-2 part 2's VM run, 2026-11-04). On a Linux host a guest's keys and clicks reached only the named game while it was in front, but the pointer itself was free: over the title bar's buttons, other windows, and into KDE's top-left corner, whose Overview takes the keys unseen.
2. **GNOME's pointer moved at about 2 fps over a still shared window** (the same run): the desktop draws the pointer into the shared picture, and a window sends a new picture only when it redraws.
3. **The invite page's logo** was an old drawing, neither today's icon nor the outline one before it.

No wire change: **version 0.34.1**, which talks to 0.33.0 and 0.34.0.

## The choice

Two ways were open (the handoff's notes): an absolute pointer we place and clamp ourselves, or the relative mouse as it is, its moves clamped against where the desktop says the pointer is. Looking closer:
- **An absolute pointer breaks mouse-look.** KWin passes no relative motion for an absolute device (`processMotionAbsolute`), nor does Mutter, so a game that locks the pointer (any shooter) would get no motion at all, and we can't tell when a game has locked it.
- **Moving the pointer back from the desktop's side** (the KWin script or GNOME extension warping it) would hold the host's own mouse too: Linux has one pointer for every mouse.

So, **your choice: clamped relative moves.** Only our moves are cut, never the pointer.

## What changed

- **The helpers say where the pointer is** (`front/kwin.js`, `front/gnome/extension.js`): each `Front` push carries the pointer's position (`at`) and the active window's client area (`area`: KWin's `clientGeometry`, Mutter's `frame_rect_to_client_rect`). Pushed while the pointer moves: KWin at most every 8 ms (a single-shot timer behind `cursorPosChanged`; KWin 5's poll every 16 ms), GNOME's pointer watcher every 16 ms (was 50), and on a window moving. `front::pointer(window)` hands them out, only while that window is in front and the helper speaks, with a count of the position's changes. An older GNOME extension sends neither: nothing is confined until it's updated (Settings → Drivers offers it, the files differing; its `metadata.json` version is 2).
- **"Over the game" means its client area** (both helpers): clicks and the wheel on its title bar or frame are dropped, as anywhere else not the game (the router's `pointer_inside`).
- **The clamp** (`couch_input::confine::Confiner`, pure): each relative move is cut, per axis, to the room between the reported position and the area's edge it goes towards, divided by the worst acceleration a desktop applies (`WORST_ACCELERATION`, 4: libinput's adaptive profile at its highest speed), less what was already sent that way since the report. Every new position starts the count again. So the pointer moves freely inside, slows only near an edge until the next report, and never crosses it, whatever the acceleration. A pointer outside the area (the host's own mouse put it there) is only let back in.
- **A locked pointer** (a game in mouse-look): our moves stop moving it. Once at least 16 units went unanswered for 200 ms (`LOCKED_MOTION`, `LOCKED_AFTER`), moves pass whole until the position changes again, which a game letting the pointer go does. Moves cut to nothing (pressed against an edge) never count as unanswered.
- **Wired in** through `KbmInjector::confine(window)`, which the input task sets beside desktop control's bounds (`Inputs::sync_confined`): the named game window for any keyboard-and-mouse holder, **never for desktop control**, nothing while no window is known. Only the Linux injector (`couch_platform_linux::kbm`) does anything with it; Windows' does nothing.
- **The pointer drawn by us** (`couch_platform_linux::cursor`, `screencast`): the capture asks the portal for the cursor as metadata where offered (`CursorMode::Metadata`, else Embedded as before) and for `SPA_META_Cursor` room up to 256 pixels each way; each buffer's pointer position (less the crop region's corner) and new shape are taken in, the pointer is blended into each picture, and a buffer with only the pointer in it makes a new frame of the last, what the pointer covered put back and the pointer drawn where it is. What the desktop wrote is checked against the room asked for before it is read.
- **The invite page's logo** is the app's icon (`deploy/invite-page/j/icon.svg`, a copy of `icon.svg` with its own first line; CLAUDE.md's icon row says so).

## Checked

- **New tests:** `couch_input::confine` (10: cut to the room at the worst acceleration, each new position giving room again and never past the edge, outside only back in, the lock found and left, moves cut to nothing never a lock, a little motion unanswered not a lock, no report passing whole, an empty area); `couch_platform_linux::cursor` (4: drawn blended and put back exactly, the hotspot and the edges, the channel orders, bad bitmaps refused); `front`'s `the_pointer_is_told_only_for_the_window_in_front`; `screencast`'s format test serializing the cursor's meta param; the scenario `a_trusted_device_controls_the_home_desktop` now checks a guest's pointer is confined to the game window and desktop control's never is.
- **Windows:** `cargo test -p couch-input -p couch-net -p couch-testkit`, `-p couch-room session`, `-p couch-engine --test sessions` (96) all pass. The live input checks (`release-check`, `input-sink`, `lifecycle-check`) weren't run: nothing on Windows' injection or teardown changed (`confine` does nothing there).
- **Linux (WSL):** fmt, clippy and the workspace's tests; with PipeWire's test source, `screencast::tests::a_pipewire_video_source_is_captured` (the stream still negotiates with the cursor's meta asked for); as root, `kbm::tests::the_devices_appear_type_and_go`.
- **The invite page:** `node deploy/invite-page/check.js`, and the page by eye in light and dark (headless Edge).

## Found while checking

- **C: filled up** mid-run (`target\debug` had grown to 183 GB: 80 GB of incremental caches, 98 GB of artifacts from older versions), stopping WSL. You agreed to `cargo clean --profile dev`. Afterwards WSL's own incremental cache, written while the disk was full, crashed rustc (`rustc_query_impl/src/incremental.rs`) until it was removed; and once `couch-media`'s test harness couldn't read random data at its start (`failed to fill whole buffer`), passing when run again. Neither was the change's.

## Still to come

- **The VM run** (`linux-host-vm-run`), on GNOME and KDE, the game named and a guest on the keyboard seat:
  - the pointer stays in the client area, windowed (never reaching the title bar's buttons) and full screen (never KDE's corner, nor another monitor); how much it slows near the edges, at a slow and a fast move (if too much, `WORST_ACCELERATION` lower, or learn it);
  - a game that locks the pointer (SuperTuxKart's menus use the pointer; a mouse-look game if one is at hand) turns freely; the host's own mouse never held;
  - title-bar clicks dropped, the game's own clicks reaching it;
  - GNOME: the pointer over a still shared window (Text Editor) moving smoothly; its shape changing (a text cursor over text, the arrow elsewhere); gone when it leaves the window; on a window share inside a larger picture, in the right place (the crop corner); KDE: the pointer still right with metadata (it was fine embedded);
  - the extension's update offered and taken (the files differing).
