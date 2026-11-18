# M28 results: a Linux host on an X11 desktop

## Why

A Steam Deck in Desktop Mode watched and played but couldn't host (the author's run, 2026-11-09): its streams failed with `Portal request didn't succeed: Other`. Desktop Mode is Plasma 6.2.5 on **X11**, and xdg-desktop-portal-kde offers screen sharing only on Wayland. Linux Mint (Cinnamon), Xfce and MATE are X11 desktops too.

[ADR 0015](adr/0015-x11-host.md) has the design and **your choices (2026-11-17)**: on X11, our own list of windows and screens, as on Windows; what is in front from the X server itself; in two parts, M28-1 the capture and M28-2 guests' keyboard seat; tested on Kubuntu's Plasma X11 session, then the Deck.

## M28-1: the capture, the list, controller seats, desktop control (0.40.0)

No wire change: **version 0.40.0**, which talks to 0.38.x and 0.39.x.

### What changed

- **Which path** (`couch_platform_linux::x11::session`): X11 only with an X display and no Wayland one, decided once and logged. On Wayland nothing changed: the portal, its restore token, the helpers.
- **The list** (`x11::windows`, `x11::screens`): the window manager's windows (`_NET_CLIENT_LIST`; docks, the desktop and our own left out, each by its title and window class), and RandR's monitors (the whole root where it lists none). `Backend::sources` gives them as on Windows, so the stream picker lists them with no dialog (`EngineInfo.shares_through_dialog` tells the picker which it is), with the sound choice under them as on Wayland.
- **The capture** (`x11::capture`): at the stream's rate, (as first built; since the first run on a real desktop, below, a window is always redirected by its capture) a screen as its rectangle of the root, a window from its own picture while a compositor composites it (Composite's pixmap of its top-level window, cut to the client), else from its rectangle on the screen (a game that bypasses the compositor, or no compositor at all), switching by itself frame by frame; through a shared-memory segment the server makes (MIT-SHM 1.2), else over the socket; the pointer drawn in from XFixes. A window closing ends the stream (the session goes on); a minimized one sends nothing until it shows again.
- **What is in front** (`x11::tracker`): a thread of its own, on its own connection, looks every 16 ms at the input focus, the windows under the pointer, and the windows asked about; `Backend::focus` and `window_exists` read its last look, which counts for a second. Viewers are told whether the game is in front from it.
- **The engine's Linux backend** takes the X11 path on an X11 session: the captured window is the game (`names_game_window` false), a stream from afar needs no answer at the PC (`shares_unattended`), a capture runs only while someone watches (as on Windows), and a window's sound is everything but this app, or the program picked.
- **Desktop control's pointer is kept on the streamed screen** (`UinputKbm::set_bounds`, X11 only): each move cut, from where the tracker last saw the pointer, by `couch_input::confine`, as P18 keeps a guest's in the game window.

### Checked

- **Against an X server in WSL** (as first built, before the run below; Xvfb at 1280×720, openbox, `xclock`; `x11::capture`'s opt-in test, the `linux-checks` skill): the window listed by its title; captured at its size (300×200) through shared memory and again over the socket, the clock's face in both; a screen captured at the screen's size; the window closed ending the capture as its source closing within the next look, the tracker then saying it is gone, and a new capture of it refused. With `xcompmgr` running, the window's frames came from its own picture (Composite); without, from its rectangle on the screen.
- **End to end in WSL:** `couch-cli host --source window --title couch-e2e` on Xvfb, `couch-cli view` in WSLg, `--relay none`, 10 s at 30 fps: 259 frames at 30.0 fps, a 640×400 window, capture to present 13.9 ms median and 21.1 ms at p95, no decode errors, no lost packets. The host's first frame came 3 ms after the capture started.
- **Found there:** a read-only shared-memory segment (read-only to the server) had every `GetImage` refused with BadAccess, and the capture gave no frames: the server writes the pictures, so it is now writable for the server and mapped read-only here, and a refusal turns shared memory off for the capture (the `linux-platform-pitfalls` skill).
- **The stream picker** (`npm run shots`, the new `linux-x11-host` scenario): the windows and screens with the sound choice under them, set apart from the list, at every size; the no-jump checks for it and Wayland's picker pass.
- **Clippy and the tests:** clean on Windows and Linux (WSL, the whole workspace: 44 test binaries); the engine's rooms and sessions scenarios on Windows; the front end's type check and unit tests.

## M28-2: guests' keyboard seat, the host's keys (the same branch and version)

Built before the first run on a real X11 desktop (**your choice, 2026-11-18**), so that run checks both parts.

### What changed

- **In front, from the X server** (`x11::tracker`): the input focus on the game's window or a child, **and no other client's keyboard grab**. A grab leaves the focus where it was, so the tracker selects focus events on the game's window: the server sends it `FocusOut` with mode Grab as another client grabs the keyboard (a lock screen, KRunner, an overview, a menu, a run dialog), and `FocusIn` with Ungrab as it ends. The game grabbing its own child is the game's (detail Inferior), and events another client sent count for nothing. logind's `LockedHint` is read beside it. A window not yet watched is never in front; a grab already held when the watch starts (at the stream's start) isn't seen.
- **The pointer:** over the game when the deepest window under it is the game's window or a child (its title bar isn't); a guest's pointer kept in the game's client area from the tracker's pointer and the window's area (P18's `confine`, no helper needed).
- **The keyboard seat for guests on any X11 desktop** where `/dev/uinput` is writable: KDE, GNOME's X11 session, Cinnamon, Xfce, MATE; no extension (Settings → Drivers no longer offers GNOME's there).
- **The host's keys** (`x11::keys`): the portal first, as on Wayland (KDE's works on X11: the Deck's did); where it fails or nobody answers its dialog, a passive grab of each Ctrl+Alt+Shift chord on the root window, four times for Caps Lock and Num Lock, a key another program holds left out (the console shows the keys in force), a held key's repeats and sent events dropped. It hears injected keys as the portal does, so the guards stand: Accept and Decline answer nothing while anyone else has the keyboard, and guests are refused the chords.
- **Blocked for guests on a Linux host:** the X server's own Ctrl+Alt with the keypad's plus and minus (its video modes) and divide and multiply (breaking or killing a grab, where a server allows it).

### Checked

- **Against Xvfb in WSL** (`--ignored x11::`, with and without `xcompmgr`): another client's `GrabKeyboard` takes the window out of front at once and its `UngrabKeyboard` brings it back; the pointer warped over the window counts as over it and off it as not; the client area read (300×200); the focus moved to the root takes it out of front. The host's keys: Ctrl+Alt+Shift+Backspace typed through XTEST (`xdotool`, as a `uinput` key reaches the server) reported once, with Ctrl+Alt+Shift+Y, which another client held, left out of the keys in force. Unit tests for a grab and its end, the game's own grab, sent events, and repeats.
- **Clippy and the tests** as for M28-1, again after M28-2.

## The first run on a real X11 desktop (2026-11-18)

The VM (Ubuntu 26.04) had only Wayland sessions; GNOME 50 has no X11 session at all. Plasma's comes as a package: `plasma-session-x11` with `kwin-x11` (6.6.4 beside Plasma 6.6.6; apt took it, 12 packages, nothing removed). Plasma on X11 is the nearest to the Deck's Desktop Mode (Plasma 6.2.5 on X11). The test AppImage (0.40.0, main's d1f27a8 with the branch) ran there, your Windows app the guest. KWin wasn't compositing (its `Compositing.active` false, no `_NET_WM_CM_S0` owner).

**Worked:** our list of windows and screens, a window streamed with no dialog; a guest's keys and mouse reaching the game, and stopping, said "not in front", while another window was; the blocked combinations (Alt+Space and the rest) and the host's chords refused to the guest; the panic key at the PC; desktop control's pointer kept on the streamed screen. Killed mid-game (`pkill -9`), nothing of ours left: no process, no virtual device.

**Found, and fixed:**
- **Windows over the game were streamed.** With no compositor, the design read the window's rectangle on the screen, so whatever lay over it showed (it could have been the host's chat). The capture now redirects the window itself (Composite's automatic redirection, as OBS does), compositor or not, and reads its own picture; the X server undoes it when the capture drops or the process goes. Once redirected, a frame whose own picture can't be read is skipped, never taken from the screen (one was, the stream's last as the game's window went: 'the window is gone' now names the request that found it so). The Xvfb test now opens a red window over the captured one, with and without a compositor: none of it in the window's frames, all of it on the screen.
- **Bring to front said "Windows wouldn't bring that window to the front".** The Linux backend didn't have it, and the refusal named Windows. On X11 it now asks the window manager as a taskbar does (`_NET_ACTIVE_WINDOW`, source 2, restoring a minimized window); on Wayland a stream is never a window, so the button never shows; the refusal says "your PC". Openbox in Xvfb ignores the request whoever sends it (`xdotool windowactivate` too), so the VM checks it.

**The second try, and a third finding** (the same day): windows over the game stayed out of the stream, compositor or not; Bring to front brought the game back, minimized too; guests' keys stopped behind KRunner and the lock screen, in full screen too. But **switching SuperTuxKart between windowed and full screen ended the stream each time.** The log (the "gone" line now naming its request) showed `GetGeometry` failing: the game had destroyed its window, closing its X connection and opening a new one, and its new window had the very same id (the server gave the new connection the old one's slot). Now:
- **A window gone waits a moment for the same id:** up to 2 s, then the capture ends. A window by that id again is captured on only if it is the same game's (its `_NET_WM_PID`, else its window class), redirected anew; another program's is never captured, and the capture ends. A replacement made between two frames shows as the window no longer being redirected, and goes through the same check.
- **A new window of the game's process is followed** by the pipeline as on Windows (`Backend::game_process` on X11, by `_NET_WM_PID`).
- **The tracker watches a window by that id anew:** its `DestroyNotify` (now selected too) or a look finding it gone drops its focus events' selection, so the new window's grabs are seen.
- Tested against Xvfb: a "game" reopening its connection gets the same id, and the capture goes on with its picture; a window by that id with another process id ends the capture without one frame of it.

**Tried again in the VM:** switched a few times, the stream kept up. Four switches came back by the same id within half a second ("the game's window is back"); one took longer than 2 s, the capture ended, and the pipeline followed the game's process to its new window at once (another id), as on Windows. Quitting the game ended the stream as "the game exited".

**SuperTuxKart lost the guest's controller after the switch,** by itself (still so after the fix: only restarting the game brings it back): its log shows both controllers "unplugged" as it rebuilt its video, never plugged again, then `Error in handleJoystick() map::at` for every pad event. Our pads stayed plugged in until the session ended. A physical controller would go the same way; restarting the game finds them again.

## The review

A fresh review agent read the whole change (2026-11-18): nothing of high severity; the unsafe mapping, the event queues, the keys' repeats and the fallback to the X server's keys held. Each finding was checked against the code; fixed:
- **A window by the game's id from another program could get a guest's keys** (medium): the capture refused it, but the tracker selected it anew after its `DestroyNotify` and counted it in front, for up to the capture's 2 s and the pipeline's 5 s. The tracker now remembers whose each watched window was when first seen (`_NET_WM_PID`, else `WM_CLASS`) and watches a window by that id again only if it is the same program's; until it can tell, not at all.
- **A window replaced between two frames by one a compositor redirects** (medium, privacy): the capture caught a replacement only when it couldn't name the pixmap, and a compositor's window it can. The capture now selects the window's `DestroyNotify` on its own connection and looks before and after each read; a picture read across a destruction is dropped, and whatever bears the id is asked whose it is first.
- **Bring to front waited on the X server on the room actor** (medium): now on a thread of its own.
- **A redirection refused at the start was never tried again** (low to medium, privacy): tried again each second, so a compositor that stops compositing never leaves the capture on the screen for long.
- **The focus arriving under another client's grab** (`FocusIn` with mode WhileGrabbed: a switcher holding the keyboard) now counts as grabbed, failing closed.
- **Pointer grabs elsewhere** (a window manager's move, a picker): the tracker selects the window's crossing events; `LeaveNotify` with mode Grab takes the pointer off the game until `EnterNotify` with Ungrab. A pointer grab begun while the pointer was elsewhere still isn't seen (no crossing reaches the game then).
- **A lost connection cleared nothing for a second**: now the looks and the pointer go at once.
- **`window_exists` could block an async worker for 300 ms** with the pipeline's locks held: it returns at once now (a window not looked at counts as there; the capture notices one gone).
- **The X11 path from the environment alone:** only this PC's own display counts (`:0`, `unix:0`; never `ssh -X`'s), and a session that isn't the active one (a switch to a console or another user) counts as locked (logind's `Active` beside `LockedHint`): nothing in front, on Wayland too.
- And the nits: the filter's doc comment, a log line for a picture too large to stream, the host's own stream window left out of the list of windows (it would capture itself), this page's and the ADR's wording.

Left as they are: a portal failing just as a session's keys are dropped can grab and report keys briefly (tagged with the session: churn only); a press after a missed release is swallowed once (only a grab broken mid-press, by keys now blocked for guests).

## Still to come

- **Not tried in the VM:** the Overview (Meta+W) and a menu over the game with a guest typing (KRunner and the lock screen were); a stream started from Windows with nobody at the VM; the guest's pointer kept in the game window with VirtualBox's mouse integration off; a desktop without the shortcuts portal (Xfce, say) for the host's keys from the X server (Xvfb only so far).
- **The Steam Deck** in Desktop Mode: VAAPI, a real game, latency, and what redirecting the game's window costs where nothing composited it before.
- Later, measured: Damage to skip unchanged pictures; a window's own sound by its process.
