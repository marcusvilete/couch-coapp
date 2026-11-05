---
name: linux-platform-pitfalls
description: Checklist for writing or reviewing Couch Co-App's Linux platform code — D-Bus through zbus and ashpd (the portals' one runtime, calls without time limits, a reading loop that must never await the bus), the XDG portals (screen sharing and its restore token, global shortcuts and the registered app id, inhibit), logind, PipeWire (our own sounds left out, relinking too fast, never bundling libpipewire), uinput (the evdev phys fix, no SysRq, rumble answered at once), the KWin script and GNOME extension that say which window is in front, KDE's KGlobalAccel, notifications, the AppImage, WebKitGTK and files in shared folders. Use when adding or changing code in crates/couch-platform-linux (portal, screencast, global_keys, awake, session_lock, sound, chime, notices, pads, kbm, front and its front/ scripts, launcher, autostart, gpu, stdio), couch_engine::pipeline::linux, the shell's Linux parts, xtask's appimage, or a Linux-only cfg block anywhere; and when reviewing such a change.
---

# Linux platform pitfalls

Each item below cost a review finding, a VM run or a day in M16. Check every one that applies before calling Linux code done. CLAUDE.md has the rules; this is the why and the how. Running things in WSL and its traps are the `linux-checks` skill's; a real desktop is `linux-host-vm-run`'s.

## D-Bus: zbus and ashpd

- **One portal runtime for the process.** `ashpd` keeps one D-Bus connection per process, and it dies with the tokio runtime that made it: M16c-1's host keys stopped working from a session's second Go live, each binding having run on a runtime of its own. Every portal request runs on `couch_platform_linux::portal::runtime()`. `runtime()` returns at once (a review found registering inside its `OnceLock` init blocked the actor); each task using `ashpd` awaits `portal::ready()` inside itself before its first request.
- **zbus calls have no time limit of their own.** A desktop service that never answers keeps the awaiting task for good: logind's inhibitor stayed held behind a stuck portal until each request got 10 s (part 5's review). Wrap every call: `tokio::time::timeout`, as `front::call` does with `CALL_WAIT`.
- **Never await the bus inside the loop that reads it.** zbus's socket reader *waits* when a stream's queue is full (`broadcast_direct(..).await`, 64 by default), so a call made from inside the loop reading `MessageStream::from(&bus)` can wait for its own reply behind messages nobody reads: a deadlock anyone on the bus can cause by flooding us (M16c-2 part 2's review). Keep facts you need per message (who owns a name) up to date from signals instead (`MessageStream::for_match_rule` on `NameOwnerChanged`), spawn calls that must happen (`front`'s re-arm), and poll every stream you create: an unread rule stream blocks the reader the same way.
- **Trust on the bus is the sender's unique name.** Only the bus sets it; a well-known name (`org.kde.KWin`) is checked by asking who owns it, and a unique name is never reused. A token in the body proves only that the sender read it somewhere (a KWin script's file, the extension's `Watch`).
- **A process in no logind session gets less:** polkit refuses logind's sleep inhibitor to it, and `user/self` or `session/auto` name nothing (find the user by `GetUser(uid)`). WSL is such a process; so is anything started outside the desktop.

## The portals

- **xdg-desktop-portal 1.19+ refuses global shortcuts to an app outside Flatpak that hasn't registered its app id,** and registers only an id with an installed desktop entry. So the AppImage installs its menu entry at start (`launcher`), the id is registered as the connection's first request, and a development build (no entry) never gets the host's keys.
- **The app id must never end in `.desktop`:** KDE's KGlobalAccel takes such a name for a desktop file's and drops the shortcuts (0.26.1: keys bound, never pressed). Ours is `com.couchcoapp.CouchCoApp`, not the app's identifier.
- **The desktop keeps the keys it bound for an id it knows;** the keys we offer are only the triggers. Changing them in our Settings never reaches the desktop, so Linux doesn't offer it.
- **Never call KGlobalAccel's `setShortcutKeys` or `doRegister` with made-up arguments** while investigating: a malformed key list crashed KWin, the app with it, and VirtualBox's guest helper spun until a restart. Read-only calls only (`linux-host-vm-run` lists them).
- **Global shortcuts can't tell an injected key from a real one** (`Backend::host_keys_hear_injected`): while anyone but the host has the keyboard, the answer keys answer nothing and the router refuses guests the host's chords.
- **Screen sharing's restore token is single-use:** each start spends it and gives the next. Spend it in `portal.json` too when a start takes it, or a failed start brings back a token the portal already spent. No desktop lets an app outside Flatpak revoke it, hence Choose again.
- **Every dialog may come with nobody there** (a device starting things from afar): close it after `pipeline::UNANSWERED_AFTER` and fail cleanly, as the capture and the host keys do.
- **One screen-sharing session at a time.** Two at once (a stream replaced, or stopped and started quickly, the old capture still closing) froze KDE's portal for a while: the next share and then the host's keys "didn't answer" (M16c-2's VM run). `screencast::ONE_SESSION` holds from opening a session to closing it; anything new that opens a portal session from the same app needs the same thought.
- **Restoring a remembered choice isn't instant:** GNOME took 5.3 s to answer a share it restored without a dialog (the log's "the desktop shared a window or screen after_ms"). Don't take a slow start for ours before reading that line and "the shared picture's first frame".
- **A shared window sends a picture only when it redraws,** pointer included (GNOME drew it at ~2 fps over a still window). Never assume frames keep coming: keep the last one for whoever joins (`hand_last`), and a pointer that must move smoothly needs the portal's cursor metadata.
- **A window share may come inside a larger picture,** its part marked by `SPA_META_VideoCrop` (GNOME): ask for the meta (`update_params` once the format is set) and copy only that region, or the stream shows black around the window, where it sat.
- **GNOME ends a window share when the screen locks** (the stream ends as `SourceLost`, the session goes on), and turns extensions off.
- **Notifications:** ask the server for no sound (it would be in the stream), cut what you send (16 notices, 500 bytes), escape the body where the server reads markup, close them at shutdown.

## PipeWire

- **Never bundle `libpipewire`:** it must match the desktop's daemon. `xtask appimage` fails if it is inside.
- **"Ours" is this process and its descendants, by `/proc`,** from the process ids in a node's or client's own info, bound for it: a PulseAudio client's id isn't in the registry snapshot. Loopbacks, filters, and clients owning a sink are replays: left out. Keep the chime and any new sound of the app in this process tree.
- **Relinking a program within about a second froze it** on PipeWire 1.0.5 (its sound, and others', silent; hundreds of xruns). One capture at a time, `sound::COOLDOWN` (1.5 s) after the last, through the gate; the pipeline starts the next capture before the old thread ends, so the gate takes the stream's stop.

## uinput

- **evdev 0.13.2's `with_phys` always failed** (`UI_SET_PHYS` with the wrong size): vendored with the fix (`third_party/evdev/PATCHED.md`). Keep it until a release has it; our devices are named ours by `phys`, `couch-coapp/<pid>/…`.
- **No `KEY_SYSRQ` on our keyboard, ever** (the kernel's magic SysRq listens to every keyboard; Print Screen is that key), no power or media keys, no `EV_REP` (autorepeat is the guest's).
- **A game waits on a force-feedback upload:** the pad's rumble thread answers uploads at once.
- **`/dev/uinput` writable** only through Settings → Drivers' one fixed udev rule via `pkexec`; never anything of the user's in that rule.

## The window in front (KWin script, GNOME extension)

- **Wayland tells no program which window is in front, nor where the pointer is.** Only the shell knows; our helpers only report. A helper's silence must count for nothing: each pushes every second, a push counts for `front::STALE`, the shell leaving the bus ends it at once.
- **GNOME turns extensions off at the lock screen** (without `unlock-dialog` in `session-modes`) and on again after: a new instance that knows no watcher. Say "nothing in front" in `disable()`, and expect to `Watch` again.
- **GNOME Shell's own UI takes the keys without changing the focused window** (the Alt+F2 run dialog, the overview, menus): check `Main.modalCount`, the overview and the stage's key focus too. **And the clicks:** notifications, the dock and menus aren't windows, so a stacking-order check calls the game "under the pointer" beneath them; ask the Shell what a click reaches (`global.stage.get_actor_at_pos(Clutter.PickMode.REACTIVE, x, y)`) and count it only inside the game's window actor.
- **KWin keeps its active window while something else takes the keys:** its lock screen, its own menus (Alt+F3's window menu), and full-screen effects (Overview, Present Windows) all do, and a script sees no change. The lock screen is read beside the helper (`org.freedesktop.ScreenSaver`'s `ActiveChanged`, logind's `LockedHint`); the effects' shortcuts and Alt+F3 are refused to guests; the top-left hot corner's Overview is the host's to turn off (the console says so on KDE). Anything new on the desktop that grabs the keyboard without activating a window needs the same thought.
- **A shell that restarts numbers its windows again** (GNOME Shell on X11 from 1): forget its ids when its bus owner changes, or the game's id names another window.
- **Ctrl+Alt+key is a character on Windows** (AltGr, which arrives as Ctrl and right Alt: Polish ł is AltGr+L). A Linux desktop shortcut on Ctrl+Alt (Ctrl+Alt+T, Ctrl+Alt+L) is refused to guests only on a Linux host (`filter::desktop_key`), never everywhere.
- **GNOME reads a new extension only at the next log-in** on Wayland; then the app turns it on. Compare the installed files with the build's, not a version number.
- **KWin's script API changed in 6** (`windowList`, `activeWindow`, `window*` signals, `windowAt`; 5 has `clientList`, `activeClient`, `client*`): use the old names where the new are missing. Scripts have no `setInterval`: `new QTimer()`.
- **A file in a folder others can write** (`/tmp`): never a fixed name, never written through what is there. Use the profile folder or `$XDG_RUNTIME_DIR`, removed first and created with `O_EXCL | O_NOFOLLOW` (`front::write_private`).

## The AppImage, WebKitGTK and the process

- **The stream window's stdout is the pipe:** a native library printing to it would corrupt the stream, so `couch_platform_linux::stdio` sets fd 1 aside before any library loads and points it at stderr.
- **WebKitGTK's DMA-BUF renderer blanks the window on a virtual GPU** (VirtualBox's `vmwgfx`): off there (`gpu`, unless `WEBKIT_DISABLE_DMABUF_RENDERER` is set).
- **The AppImage's GStreamer finds no plugins** (its own folders are empty, the system's are for another version): the chime plays from the engine as a PipeWire stream, not from the webview.
- **`$APPIMAGE` counts only when our own executable runs inside `$APPDIR`:** inherited from a parent, it would register links to another program.
- **Profile files are 0600 in 0700 folders** (`store::write_atomic`), the folder set 0700 at every start (the shell's log may have made it first).
- **Bringing a window to the front needs an activation token on Wayland:** Show window may not raise the stream window.
