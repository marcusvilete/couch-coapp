---
name: app-manual-run
description: Set up and read a manual test of the Couch Co-App desktop app, on one PC with several profiles (the default profile plus alice, bob and carol) or on two PCs with the installer (scripts/build-installer.ps1): starting them, where every log is, what one PC can and can't show (games grab the guest's physical controller; keyboard and mouse never reach the game), how to tell a stream problem from a game's choice with xinput-watch.ps1, checking the viewer's HUD without a session, what to expect by area (sessions and streams, seats, sound, Settings and the overlay), two-PC setup, checklists per area and traps already met, and what to ask the user to try. Use when asking the user to try the app (going live, watching, passing control, the panic key, games, two PCs), when the user reports something from such a run, or when reading its logs.
---

# A manual run of the app on one PC

## Start it

From the repo root, in PowerShell:

```powershell
. .\scripts\dev.ps1
cd apps\desktop; npm run tauri dev        # the default profile; builds couch-viewer (debug) first
# a second terminal, from the repo root:
.\scripts\profiles.ps1                    # alice, bob and carol, from the same debug build
```

- **Restart everything after a code change.** Ask the user to close every app window (the default profile and each of the others) and to start both commands again. Never kill their processes yourself.
  - `tauri dev` rebuilds the app.
  - The stream window is started fresh for each Watch, so a rebuilt `couch-viewer` needs no restart. Running `cargo build -p couch-viewer` is enough.
- **A room** with the profiles in it usually exists from earlier runs. The owner is the one who can ban.

## Where the logs are

| Who | Where |
|---|---|
| alice, bob, carol (the app) | `%LOCALAPPDATA%\CouchCoApp\profiles\<name>\desktop.log` (and `desktop.err.log`). The size shown while the app runs can be stale; it is right once the app exits |
| Any profile's stream window | `...\profiles\<name>\viewer.log`, appended per window (ANSI colour codes included) |
| The default profile (the app) | only the `npm run tauri dev` terminal: ask the user to copy the lines, or repeat the test with alice hosting |

- **Log level.** `profiles.ps1` gives its instances `dev.ps1`'s default `RUST_LOG` (`couch=debug,iroh=info`). Without it, the app and its stream windows log at info level (since M9a and M9c).
- **Environment switches** reach a stream window from the **watcher's** app, so viewer-side ones go in the shell that starts that profile: `$env:COUCH_VIEWER_ZERO_COPY = '0'; .\scripts\profiles.ps1 alice` (after closing alice). Setting one before `npm run tauri dev` reaches only the default profile, which is where the host-side ones go when it hosts (`COUCH_HOST_PACING=0`, `COUCH_HOST_ZERO_COPY=0`).
- **Frame rates** (M9d): the host panel's Stream section offers 120, 60 and 30 fps, and warns when the chosen source's screen refreshes slower than the rate. The stream window's stats show the fps reached; the host's log line `capturing at N fps: frames on the GPU, at most every 4.0 ms, paced by the stream` shows the pacing.
- **Useful lines:**
  - `session started`, `streaming number=N`, `stream stopped reason=...`, `session ended reason=...` (M19), `seat status=Granted/Active/Suspended/Host`, `a viewer asks for control`, `panic key: every seat back to the host`;
  - in a viewer log, `gamepad "..."` and `skipping "...": a host on this PC has a virtual pad plugged in`;
  - in a viewer log, `GPU ... (Dx12)` and `decoding with D3D12VA (GPU frames)` (or `D3D11VA (copied)`, and why: `decoded frames go through memory: ...`).
- **The stats panel's numbers come from a debug build,** but its stream window runs without wgpu's validation, so its latency matches an installed app's (M9c).
- **A brief `Suspended` then `Active`** is the host's 300 ms dead-man timer catching a short silence from the guest; it is safe by design.

## What one PC can't show

- **Games pick the guest's physical controller.** The guest's controller (the user's DualSense) is plugged into the host's PC too, and games read it directly. Graveyard Keeper 2 (Unity) binds the player to it, and lets alice's virtual pad only move through menus with the D-pad. Steam settings don't change that.
  - So "the guest can't play the game" on one PC is not evidence of a bug.
  - Check our side with `xinput-watch.ps1` (below). A real game test needs a second PC as the guest. (HidHide hides only the *host's* controller, and only from programs other than ours; the stream window on the same PC is another program, so with the host's controller hidden it can't read it either.)
- **Keyboard and mouse never reach the game on one PC.** They go in only while the game window is in front, and then the stream window doesn't have the keyboard. That is by design.
- **Steam notifications.** Steam announces each virtual pad as it is plugged in ("Xbox 360 Controller"). Each session's start or `pad-test --real-pad-ids` shows one.
  - "XInput Controller #N" means made-up ids: the pad is a CLI test pad, or something regressed in `real_pad_ids`.
- **A Linux viewer on this PC, through WSLg** (M16b): `couch-cli host --source testpattern --relay n0` on Windows and `couch-cli view <ticket> --relay n0` in WSL (WSL can't reach Windows directly), or the app with `COUCH_PROFILE=<name> npm run tauri dev` in WSL. Its windows show on the Windows desktop. It decodes with NVDEC there and draws in software (`llvmpipe`): latency and smoothness aren't Linux's, only that it works. A real Linux run needs a VM, bare metal or the Steam Deck. Capture its windows with `PrintWindow` on WSLg's `msrdc` window, not the whole screen.
- **A virtual controller Windows is still setting up** (P10): our pads take ViGEm serials from 1, each a device Windows installs the first time. The host console then says "Windows is setting up the virtual controller for …", with a timer and Stop waiting, and the log `seat N: … Windows is still setting it up`, then `ready after N s`. This PC has every serial installed; to see it, remove one from an elevated PowerShell (`pnputil /remove-device "USB\VID_045E&PID_028E\01"`) and start a session.

## Tell a stream problem from a game's choice

With alice live and carol holding the controller, on the same PC:

```powershell
.\scripts\xinput-watch.ps1        # 60 s; prints each XInput slot when it changes
```

Ask the user to press each button and move each stick on their controller. alice's virtual pad (usually slot 0) should show every input under the right name.
- **If it does,** the stream is right and the game chose another controller.
- **If inputs are missing or wrong there,** the problem is ours: the viewer's `read_pad`, `couch_input::forward`, the router, or `couch_platform_win::vigem`.
- `joy.cpl` → "Controller (XBOX 360 For Windows)" → Properties is the same check in Windows' own panel.

**A stream window that takes many seconds to open** (or `release-check` failing with "the seat never went active"): time `couch-viewer --probe`, which starts SDL's controllers alone; it should take about 1 s. Steps of about 5 s mean a device that doesn't answer, most often a controller still paired over Bluetooth but switched off or lost (M16a: a DualShock 4 lost a year earlier cost 15 s). Removing that pairing in Settings → Bluetooth & devices fixes it; a device in an error state in Device Manager is the next suspect.

## The viewer's HUD without a session

```powershell
cargo run -p couch-viewer -- --hud-preview watching     # every state: viewer::Preview::NAMES
.\scripts\viewer-shots.ps1                              # all of them, into target\ui-shots\viewer
```

- **Check the debug build, not only release.** The app runs the debug `couch-viewer`, and egui has checks that panic only in debug builds (an undrained `TexturesDelta` crashed every stream window in M5b while release looked fine).
- **To look at it yourself:** `viewer-shots.ps1` (after `cargo build -p couch-viewer`) captures each state with PrintWindow, so other windows covering it don't matter: 1280×720 each, the crowded one also at 480×300 and at 150% (`--hud-scale`), and a pointer on a seat chip for its hover and tooltip (`--hud-preview-pointer X Y`, in points). Then read the PNGs. The preview keeps the bar up and draws a test picture behind it.
- **A still picture repaints only when asked:** egui lays a new area out unseen on its first frame, so the HUD must honour egui's repaint requests (`Hud::needs_paint`).

## The panic key

- **Only a person can press it.** It ignores injected keys, so no script can.
- **Ask the user** to press Ctrl+Alt+Shift+Backspace with a guest playing, in each of these places:
  - with the game in front;
  - with the host's own app window in front;
  - with another program in front.
- **Expect** the guest's pad to go neutral, "control paused" in the host panel, no Backspace in the game, and `panic key:` in the host's log.
- **If it fails only in our own window:** something registered the process for raw keyboard input (see `windows-ffi-pitfalls`).

## Seeing the UI yourself (M21)

- **Before asking the user,** look: `npm run shots` (in `apps/desktop`) shoots every preview scenario in dark, light and High Contrast at three sizes into `target\ui-shots\latest`, and fails on text spilling out of its box, a menu that doesn't open, or a region that moves as you navigate. Read the PNGs.
- **A state no scenario shows:** add one to `src/preview/scenarios.ts` (the fake engine's world) and, for clicks, a step to `scripts/shots.config.mjs`.
- **The real app:** start a profile of its own (`$env:COUCH_PROFILE='probe'`) with `$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS='--remote-debugging-port=9222'` in that launch's shell only, then Playwright's `chromium.connectOverCDP("http://localhost:9222")` drives and shoots it. Delete that profile's folder afterwards.
- **What only the user sees:** their own screen's scaling and text size, how it feels, two PCs, games and controllers.

## What to expect, by area

Each feature's notes sit under its area, tagged with the milestone that added it. Add a new feature's notes to the area it belongs to (or a new area), not as a section of its own.

### Rooms: a profile's backup (M13 part B), handing a room over (M13 part A)

- **Backup:** Settings → Backup opens the app's own save dialog; restoring works only in a profile with no rooms (a new `COUCH_PROFILE`), then the app starts again as that identity. A restored room catches up (its banner names who it waits for; **Stop waiting…** warns first) and an owned one asks for a new password.
- **Handing over:** **Make owner…** is in a member's menu (right-click the member, or its "⋯"), beside Ban, for members connected now. The owner confirms, then sees "Waiting for … to accept" with **Withdraw**. The member offered gets a notice and a banner with **Accept…** (a dialog for the new password, twice) and **Decline**. Once accepted, every member shows the ♛ on the new owner and a notice ("… now owns …"); the old owner's invites are refused as expired and its Invite button is gone; the new owner's invites work with the new password.
- On one PC, three profiles show it all (default, alice, bob): no game or second PC needed.

### Sessions and streams (M19), lifecycle (M6), the picture (P8)

- **Start a session** (the dialog): a title, how many can watch, the seats, and the host's controller. Nothing is captured yet. The pads are plugged in and the controller hidden at once, so a game started afterwards never sees the controller and finds the seats' pads.
- **The host console** (M21) shows LIVE or "Not streaming", Start streaming… (a dialog: windows and screens in two tabs, frame rate, quality, upload limit under it), and End session in its "⋯" (confirmed when anyone watches). While live it says what is streamed, with Stop streaming and Stream something else…; under it the request strip (always there), the patch bay (players left, seats right, a cable per held seat, dashed for a request: drag a player or one of the host's controllers onto a seat, pull a plug off a seat to take it back; the seats' ⋯ and right-click do the same) and the stream volume. While hosting, a bar at the top of every view (another room, Settings) shows the requests waiting, with Answer.
- **The stream window's controls in the app** (P11): under the card a watcher watches, a "Stream window" panel: volume and mute (in step with the window's own slider both ways), Full screen, Show window, Bar and Stats switches; above them "The game isn't in front on Ana's PC" (amber, while playing or asking) or the latest note. The card's right-click menu has the same. Show window must bring the window up from behind another window and from minimized: Windows lets it only because the app calls `AllowSetForegroundWindow` first, so if it only flashes the taskbar button, look there. Settings → Stream window turns the bar off for new windows and the open one. With alice watching on this PC the window and app overlap: drag the window to a corner to see both.
- **Watchers between streams:** the room card says "Ana isn't streaming anything yet" (or why the last stream stopped); a stream window stays open, black, with that line in the middle and "Your seat stays yours". Their input does nothing until the next stream (the host's log shows no seat change). The next stream arrives by itself, from a keyframe.
- **A session with a keyboard-and-mouse seat** streams only windows: the Stream section says so if a screen is picked.
- **The host's own controller** plays its seat between streams too, so the host can start the game with it.
- **The "game not in front" line.** With alice live and the game window behind another window, the stream windows of guests playing or asking say "The game isn't in front on Ana's PC" (watchers' don't, since M21's bar); for a player the bar stays up until it comes back. On one PC the stream window itself takes the front, so expect the line while clicking in it: that is right.
- **Closing the game** ends the stream with "The game closed", not the session (M19): viewers' windows stay, black, saying so, and the host can stream again. A game that swaps its window (some do when changing display mode) keeps streaming after a short freeze; host log: `the game opened a new window; streaming that one`.
- **A window that closes while the game runs** ends the stream after 5 s ("went away"); the session goes on.
- **Killing the host app** (Task Manager): its viewers see the connection lost after about 5 s (log: `the peer went silent; closing`).
- **A dropped connection while watching**: the stream card says "Reconnecting…", the stream window stays, and the stream comes back by itself within 30 s. The guest must ask for control again. Log: `lost the host's connection; waiting for it to come back`, then `the host is back; joining its stream again`.
- **Killing `couch-viewer.exe`**: use Task Manager's **Details** tab (End task there kills). The window comes back by itself once; a second kill within a minute ends watching ("crashed twice"). Log: `the stream window crashed and was started again`. The Processes tab's End task asks the window to close instead, which counts as leaving: watching just stops.
- **A program run as administrator** is marked "admin" in the stream dialog's list, with a warning when picked, and the console keeps warning while it is streamed. Keyboard and mouse can't reach it, and the panic key doesn't work while it is in front (measured in M6); Take back does.
- On one PC, the network can't be dropped for one pair; two PCs (turn off the guest's network for about 10 s) or the `sessions` scenarios cover it.
- **The stream window scales with bicubic** on luma (P8); the stats panel's decoder line says "…, bicubic scaling, …". `COUCH_VIEWER_SCALER=bilinear` in the watcher's environment gives the old one, to compare. The difference is small, most visible on text.
- **A soft picture is mostly the upscale and the bitrate,** not the scaler: a game in a window smaller than the watcher's screen is stretched to fit, and a stream starts at 10 Mbit/s and climbs only up to the host's quality setting. When the user reports blur, suggest the game borderless at the host's screen resolution, and the quality at 20 Mbit/s or higher (30 or 50 on a fast connection); the stats panel shows the size streamed and the bitrate.

### Seats (P1), the host on a seat (P2), several seats for one guest (M18)

**The seats themselves (P1):**
- **The session dialog's seats** (M19: one setup). Controllers 0 to 4, each its own virtual pad, and keyboard and mouse: not for guests, or a seat of their own (one player on both holds both seats; "with Controller 1" went in M24, and a saved one comes back as a seat of their own). 0 controllers and none for guests is a session to watch together (M24): no pad, nothing hidden, no panic key, and no seats or requests in the console, the guest's card or the stream window's bar. The last setup comes back (the profile's web storage). Fixed for the session.
- **A controller plugged into the host** shows a warning when it isn't chosen as the host's: it may take a player's place.
- **On this PC with profiles:** alice starts a session with 2 controllers and streams; bob and carol each ask for one. `xinput-watch.ps1` should show two virtual pads (after any physical ones), each moving only with its own guest. Steam announces each pad.
- **Moving.** A guest with one controller holding Controller 1 who asks for Controller 2 keeps the first until the host accepts; then the first goes neutral and the second follows them. Host log: `seat` lines for both.

**The host on a seat (P2).** Needs HidHide installed on the host (administrator, then a restart; installed on the user's PC on 2026-10-06) and a physical controller (theirs: a DualSense on Bluetooth, and a GameSir T4n Lite wired; since P9 a real Xbox 360 controller works too, and since M22 a remapper's pad, below; our own pads never).
- **A remapper's pad** (M22; the user has DS4Windows): "Your controllers" lists it as "Xbox 360 Controller (a remapper's virtual pad)", and the host panel as "(a remapper's)". Ticked alone, the dialog warns that games still see the physical controller behind it (the DualSense) unless the remapper hides it ("Hide DS4 controller" in DS4Windows): without that the game counts one controller too many (seen: 3 for 2 seats). It plays its seat with the remaps; `xinput-watch.ps1` shows DS4Windows's own pad "not connected" and the seat's pad moving. Toggling "Hide DS4 controller" or reconnecting the DualSense re-plugs DS4Windows's pad: it must stay hidden throughout (log: no `came back as another device` unless its id changed). Ticking both DS4Windows's pad and the DualSense behind it makes one press drive two seats (its own, and the pad's through DS4Windows): expected, and the dialog says so. **On one PC, alice's stream window may read the DualSense itself** (it reads physical controllers): with her on another seat, one press drives both seats. Give her a controller the remapper doesn't read, with "Hide DS4 controller" on before her window opens.
- **The session dialog's "Your controllers"** (M23: one check each, up to the number of controller seats). Lists the host's physical controllers when a seat has a pad; each checked one says which seat it starts on ("you start on Controller 2"), in the order listed; two of one model show as "DualSense 1" and "DualSense 2". The choice is remembered in the profile's web storage. Its line says what HidHide allows: not installed (with the download page), its configuration window open (close it, start again), or ready.
- **Several of the host's controllers** (M23): each plays its own seat; the seats list says which ("you (DualSense Wireless Controller)", "you (GameSir T4n Lite)"), and each pad seat has "Play here with…" instead of Play here: a controller moves there (its old seat goes free), or onto another of the host's seats, the two swap. A host controller that disconnects keeps its seat. On one PC: the DualSense and the GameSir both chosen, 2 controller seats; `xinput-watch.ps1` shows each moving its own pad.
- **After starting the session** the host panel says the controller is hidden for the whole session: start the game now. A game or Steam that was already running and had it open still sees it until it is reconnected once. `joy.cpl` (closed and reopened) lists only "Xbox 360 Controller", and `xinput-watch.ps1` shows the host's input in that pad's slot.
- **Steam Input off for the game** (its Properties → Controller), or one seat gets no player in it: Steam keeps a player for the hidden controller. Turning off Steam's global PlayStation support instead also works, but then the DualSense works in games only while live.
- **The seats list** shows "you (DualSense Wireless Controller)" on Controller 1 with Leave, Take back on guests' seats, and Play here on the others with a pad. Passing the host's seat to a guest leaves the host on none; the guest giving it back or leaving returns it. Take back and Leave leave the seat empty (guests see "free", or "Nobody is playing" with one seat, never "own controller"); only Play here seats the host's controller again. The panic key removes guests but the host keeps the seat they play; a seat they had passed on stays empty.
- **Ending the session** shows the controller again (log: `the host's controller is shown again`); stopping a stream doesn't. If the app was killed, it stays hidden until the next start of the app or of `couch-cli host` (log: `showed 1 controller(s) hidden by a host that crashed`). HidHide's own configuration client (Start menu) also shows and clears the list.
- **If hiding fails**, the host panel says why and the host plays outside the seats, as in P1.

**Several seats for one guest (M18):**
- **The guest's controller count** reaches the host when the guest's stream window opens its controllers: the host panel's Watching list says "bob · 2 controllers" (sessions with more than one seat). Plugging or unplugging one updates it.
- **Added or moved.** With 2 controllers, a guest on Controller 1 who asks for Controller 2 keeps both; with 1 they move, as before. The keyboard-and-mouse seat is always added, and a move keeps it. The host's "Pass another seat…" menu says "— added" or "— moves them" per seat.
- **Which controller drives which.** Each pad seat keeps the controller it got: a new seat goes to the lowest free controller, and when a second seat comes, the first keeps the controller driving it. With one pad seat, whichever controller they touch last.
- **A controller disconnecting** (unplugged, battery, sleep) with several pad seats gives its seat back at once (the host panel shows it free, the host overlay notes it); the guest's other controllers stay on their seats. With one pad seat, the seat stays: another controller or the same one reconnected carries on. The stream window's stats panel (the bar's Stats button) has an input line saying "pad A → seat 1, pad B → seat 2"; rumble goes to the controller driving that seat.
- **Giving back.** The guest's card and stream-window bar get "Give back Controller N" per seat and "Give all back"; the host's Take back stays per seat.
- **On this PC:** two physical controllers both on this PC, alice hosting 2 controller seats, bob watching (alice's own controller not chosen, or HidHide hides one from bob too). bob's window shows 2 controllers; bob asks for both; `xinput-watch.ps1` shows each of bob's controllers moving only its own pad. A DualSense and an Xbox controller work; since P9 a real Xbox 360 controller does too (only the host's own pads are skipped, by their ViGEm serials).
- **XInput has 4 slots per PC.** On one PC the host's seat pads take slots too, so with 3 or 4 seats an Xbox-style controller can get none: no game, SDL or stream window sees it, and the guest has fewer controllers, so fewer seats (seen in P9 with 3 seats and DS4Windows running). Use 2 seats, or a DualSense (read directly, not through XInput). On two PCs it doesn't happen.
- **Where a pad comes from** (P9): `couch-cli pad-origins` lists each gamepad with its device chain and origin (physical, ours, another program's virtual pad). A remapper's pad (DS4Windows) is read by a stream window beside a host, unless the host chose it as one of their controllers (M22: then it's hidden); on one PC it moves with the host's hidden DualSense, since DS4Windows is allowed past HidHide.

### Sound: the stream volume (P6), the host's stream level (P7)

- **In the stream window's bar:** Mute and a volume slider (0 to 100%, never louder than the host sends). Ctrl+Alt+Shift+`-` / `=` / `0` turn it down, up (by 10%) and mute it, showing "Volume 70%" or "Muted"; turning it up unmutes. Settings → Shortcuts lists the three, movable like the others. Only this window's sound changes: the host and other watchers hear no difference. The app's "Stream window" panel (P11) has the same Mute and slider, in step both ways.
- **Remembered per profile** (`audio.json` in its folder): the next stream window plays at the same level, even after closing the window right after a change.
- **On one PC**, every window plays through the same speakers: compare by muting one watcher's window.
- **The host panel's Stream section** has a Stream volume slider (0 to 400%, P7) and a meter of what guests hear after it (a bar and a peak mark; "silent" without sound, "loud peaks rounded off" while a boost is limited). It changes what every guest hears, never what the host hears, at once, and comes back in the next session (`host-audio.json`).
- **To play quietly while the stream (and a Discord call) stay loud:** lower the game's slider in Windows' volume mixer, then raise the stream volume until the meter is back where it was. Windows' main volume doesn't reach the capture, but lowers everything else.
- **Checking a level on one PC:** a watcher's stats panel ends its audio line with `level peak … / avg … dBFS`, the sound as it arrives (50% on the host is about 12 dB lower).

### Settings and the splash (P3), the host overlay (M8)

- **The splash** ("Player 2 joins") plays at every start for about 3 s: two controllers slide in, land on a couch, the name types out, the picture holds a second, then fades. A click or a key skips it. It animates even with Windows' animation effects off (the user's choice; their PC has them off). It covers the lobby's first load only; the engine is already running.
- **⚙ Settings** (your card at the sidebar's foot, or Ctrl+,) is a page with its own list (M21): Profile, Appearance (Like Windows, Dark, Light; the title bar follows), Shortcuts, Overlay and sounds, Stream window (P11: the session bar over the picture, on or off), Network, Drivers, Backup, About. Esc goes back. Among them:
  - **Shortcuts:** Change waits for a key (Esc cancels; Ctrl, Alt and Shift needn't be held, and are ignored). A refused key says why under its row ("Ctrl+Alt+Shift+P already does this: …", "that key can't be used", "only F1 to F12 (but F4)"). Default and Reset all put keys back. Saved in the profile's `shortcuts.json`. The panic key changes from the next session (the host panel's reminder names the key in use); the stream window's keys from the next window opened (its bar's hint names them).
  - **Network** (M7):
    - **The choices:**
      - the build's default: "Couch Co-App relay (host)" in a build with our relay built in; in a development build, iroh's public relays;
      - iroh's public relays (for testing);
      - My relay with a URL: https; `http://localhost…` for `scripts\relay.ps1`; `http://192.168.x.x…` too in development and test builds.
    - **Saving and restarting:** saved in `network.json`; "Applies when the app restarts", with Restart now (it asks once more while streaming or watching).
    - **Status:** the line under it says which relay we're connected through. A typo in the URL is refused at Save.
    - **The token:** "My relay" sends no token, so pointing it at our relay running with its token (`relay.ps1` without `-Open`) never connects. Use `-Open` for that, or a build with the relay built in.
- **Our relay on this PC** (M7). Build it once with `cargo xtask relay-tool`, then run `.\scripts\relay.ps1`: plain HTTP on `localhost:3340`, production's limits, the token from `release.toml`. Ctrl+C stops it.
  - **A development build with it built in:** in the terminal, before `npm run tauri dev`, set `$env:COUCH_BUILD_RELAY_URL='http://localhost:3340'` and `$env:COUCH_BUILD_RELAY_TOKEN='<relay_token>'`. Settings → Network then shows "Couch Co-App relay (localhost:3340)". `profiles.ps1`'s profiles share that build.
  - **Unset them afterwards,** or every later build in that terminal has them.
  - **The relay's own log** is in its window. With `$env:RUST_LOG='iroh_relay=debug'` it shows each connection, and "denied our authentication" for a client without the token.
- **Keys are checked within one profile only.** On one PC, a viewer profile's chord set to the hosting profile's panic key is swallowed by the host's hook and presses the panic key. Not a bug (on two PCs they never meet); keep them apart when testing.
- **Restart now** shows a "Restarting Couch Co-App…" screen with a spinner, goes through the normal exit (the rooms hear we go; a second or two), then starts the same profile again, which opens with the splash; the new instance waits up to 5 s for the old one's profile lock. **Release builds only** (the installed app): `npm run tauri dev` stops with its app, and the Vite server with it, so in development the app says to close it and start it again (`platform().canRestartItself`). Pressing it under `tauri dev` once ended both, leaving no app running.

**The host overlay (M8):**
- **When it shows:** only while this profile streams, something is pending or just happened, and the app's own window isn't in front (it shows the same). A corner window, top right of the monitor showing the streamed window (or the streamed screen), opened for each burst and closed when empty, so there's a short delay (about a third of a second) before the first toast.
- **What it shows:** each request ("bob asks for Controller 2"), the oldest framed in orange with its keys (Ctrl+Alt+Shift+Y accept, N decline by default); notes that fade after about 4 s (started or stopped watching, now plays or no longer plays a seat, no longer asking, control paused).
- **The keys** answer the oldest request, only once it has been pending 1 s (an immediate press does nothing; log: `too new to have been seen`). They go through the panic key's hook: a game running as administrator in front hides them from us, as for the panic key.
- **The chime:** two tones from the main window on each request, if Settings → Overlay allows. It needs the Start the session click (the webview's autoplay rule), so it only sounds for sessions started from this run of the app.
- **What to check every time:** the game keeps the focus when toasts come and go (guests' stream windows would say "the game isn't in front" otherwise, and their keyboard would stop); clicks on the overlay's corner reach the game; a stream of the whole screen doesn't show the overlay.
- **On one PC** the stream window of a watching profile is usually in front, not the game: test with the game in front and the stream windows behind it.
- **True exclusive full screen** draws over the overlay; only the chime and the keys work there.

## A full run to ask for

1. Start a session, then the game, then stream its window. Watch, then stop watching.
2. Ask for the controller and get declined. Ask again after 5 s, get accepted, and play.
3. Pass control to another viewer, then take it back.
4. The panic key, then Allow control.
5. Stop streaming and stream again (another frame rate): the viewers stay, with their seats.
6. Close the game window: everyone should hear the stream ended; the session goes on.
7. End the session.
8. Close the host's app while a guest plays.
9. Ban a viewer mid-stream.

## Two PCs

The user has a second PC on their home network. Suggest a two-PC run after any change one PC can't show:
- games reading the guest's pad;
- keyboard and mouse;
- network drops and reconnecting;
- killing a host;
- administrator windows;
- real networks and other GPUs.

### Setup

- **Roles.** Usually this PC runs the dev build (`npm run tauri dev`) and the other PC the installed app; either may host.
- **The installer** (M10a), built into `target\installer\` with FFmpeg's sources and `SOURCES.txt` (M10b). `npx --yes http-server target\installer -p 8000` serves the whole folder; hand out all of it.
  - `.\scripts\build-installer.ps1` is the release installer, on the user's relay (`couchrelay.duckdns.org`, from `release.toml`). It needs a clean, committed tree. The name carries the version and commit.
  - `-TestRelay` builds a test installer on iroh's public relays, from any tree. The name adds `-dirty` with uncommitted changes, and `-test`. Its Settings → Network says "this build has no relay of its own".
  - **Mixed builds:** a test installer can't use the user's relay (it has no token), though a release build can still reach the test build through iroh's public relays. Keep both PCs on the same kind of build; a failed connection between mixed builds proves nothing.
  - **For a LAN test of our relay**, use a copy of `release.toml` with `test = true` and `relay_url = "http://<this PC's LAN address>:3340"`. Build with `-Config <that copy>`, and run `.\scripts\relay.ps1 -Lan` here (Windows Firewall asks once).
    - **Both PCs need the same URL,** the LAN one, never `localhost`: invites and member records carry it, and the other PC must be able to dial it.
    - This PC gets it through `COUCH_BUILD_*`, as above, or by installing the same build.
  - Run the script without redirecting its stderr (`2>&1` or `*>&1` with `$ErrorActionPreference = 'Stop'` aborts on cargo's and Tauri's info lines).
  - Rebuild it after every change the other PC runs. Both sides must have the same wire revision (`PROTOCOL_MINOR`), or the member list says "Needs update" / "Update yours".
- **On the other PC:**
  - run the installer; SmartScreen warns (unsigned for good, ADR 0009): More info → Run anyway. On an administrator account Windows asks for administrator rights as it opens; then it asks: just me (`%LOCALAPPDATA%\Programs\Couch Co-App`) or everyone (Program Files);
  - answer the driver questions (ViGEmBus is needed to host games for guests' controllers; HidHide to play a seat yourself); each runs its own setup with Windows' administrator prompt; HidHide needs a restart;
  - allow the app on private networks when the firewall asks;
  - invite links open the app (`couchco://` is registered); installing again replaces the app and keeps the profile's data.
- **Its logs:** `desktop.log` (and `viewer.log`) in `%LOCALAPPDATA%\CouchCoApp\profiles\<profile>`. The app itself lives where the installer put it: `%LOCALAPPDATA%\Programs\Couch Co-App\` for just that user, `Program Files\Couch Co-App\` for everyone. Each Windows user keeps their own profiles either way.
- **Uninstalling with "Delete the application data" ticked** (P4) asks to delete `%LOCALAPPDATA%\CouchCoApp`: every profile of that Windows user, the development ones (default, alice, bob, carol) included. Before testing it on this PC, ask the user to back the folder up, or answer No.
- **Invite links** (P4): an installer built with an invite page copies `https://…/j/#…` links. Before the page is online (`.github/workflows/invite-page.yml`, after the merge), try it locally: `npx --yes http-server deploy/invite-page -p 8080`, then open the copied link with its start replaced by `http://localhost:8080/j/`.
- **The dev build and the installed app share `couchco://`.** The installer registers it for the installed app. The dev build takes it only when started with `$env:COUCH_DEV_LINKS = '1'` (P5); its registration is per user and outranks an install for everyone.
  - Without the switch, a dev start removes its own leftover registration, never another program's.
  - An app installed for just this user registers in the same per-user key, so a `COUCH_DEV_LINKS=1` start overwrites it; reinstall that app afterwards.
  - The symptom of a dev registration left behind: clicking a link with nothing open starts the debug exe, whose window says "connection refused" (no dev server).
  - By hand: `Remove-Item -Recurse HKCU:\Software\Classes\couchco`.
- **Controllers.** The guest's controller goes on the guest PC. On the host, for co-op games, either choose the host's controller in the session dialog (P2: hidden, it plays Controller 1; start the game after the session, or reconnect the controller once) or keep none plugged in: otherwise the game sees it directly, gives it a player, and a guest's pad may not work (seen in Fling to the Finish in P1). Single-player games took the guest's pad either way.
- **The link.** The stream window's HUD should say "Direct". "Relay" means the firewall or the network blocked the direct path.

### Checklists

Pick what the change touched; add the basics (watch, ask, pass control, the panic key, stop) every time.

| Area | Ask the user to | Expect |
|---|---|---|
| Gamepad in games | play a single-player game, then a co-op one, as the guest | the guest drives the game; for co-op, no controller on the host |
| Keyboard and mouse | a session with a keyboard-and-mouse seat, streaming the game's window; the guest plays with them | input reaches the game only while its window is in front on the host; the Windows key, Alt+Tab, Alt+F4, Ctrl+Esc never do |
| Alt+F4 on the guest | press it while holding the controller with keyboard, then while only watching | holding: nothing happens; watching: the stream window closes (that counts as leaving) |
| Display mode | switch the game between windowed and full screen, also from the guest | the stream goes on; the guest's input works in both (the host's log says "the game opened a new window" if the game swapped windows) |
| Game exit | quit the game | everyone gets "The game closed"; the session and the guests' seats stay, and streaming the game again works |
| Sessions before streams (M19) | start a session with the host's controller chosen, then start the game; stream; stop; change the frame rate; stream again | the game never sees the host's controller, no reconnect; the guest's window stays through the stop, black, then shows the new stream; their seat stays |
| 120 fps (M9d) | stream at 120 fps on a screen at 120 Hz or more; then at 60 with the screen at 144 or 165 Hz (Windows display settings), if it offers them | the stream window's stats show about 120, then 60 fps (before M9d, 60 on 144 Hz gave 48) |
| Network drop | turn off the guest's network for about 10 s while it plays | frozen picture, "Reconnecting…", the stream comes back within 30 s, the guest has to ask for control again |
| Host killed | end the host app in Task Manager | the guest is told the stream ended within about 5 s |
| Stream window crash | on the guest, Task Manager's **Details** tab, End task on `couch-viewer.exe`; again within a minute | it comes back by itself, still holding the controller; the second time, watching ends |
| Administrator window | run Notepad as administrator, stream it | the Stream section marks it "admin" with a warning; keyboard and mouse can't reach it; the panic key doesn't work while it is in front (Take back does) |
| Seat kinds | a session with one controller seat, then one with keyboard and mouse only, then one with a controller and the keyboard seat, both given to one guest | the guest drives the game with exactly that, nothing else |
| Watching together (M24) | a session with 0 controllers and keyboard and mouse not for guests; a guest watches | no Steam notification of a pad, the host's controller still plays the game itself, the panic key does nothing; the guest sees no seats on the card or the bar, and asking does nothing |
| Remote play (M25a) | the laptop on another network (a phone's hotspot) as a second profile in a room with the home PC; at home: Settings → Remote play, a PIN, add the laptop (codes match), on; on the laptop: "It's my PC" (codes match), Unlock, Start a session on it, a window streamed, a seat passed to the laptop itself; a friend asks and the laptop accepts; then the panic key at home | the laptop gets the home PC's console and plays; the friend sees the home PC's session as any; the home console says the laptop runs it; the panic key locks the laptop out (PIN again); a wrong PIN five times makes it wait a minute. Locally, alice can stand in for the laptop (profiles), but "from afar" needs the other network |
| Desktop control and Bring to front (M25b) | on the laptop, unlocked: start a session with the keyboard seat, stream the home PC's screen, Control the desktop (bottom of the console), click into the picture; launch a game from Steam with the mouse, Alt+Tab, the Windows key; Ctrl+Alt+Shift+M, then the Windows key; minimize the stream window. Then stream the game's window, click another window at home (or ask someone to), and Bring to front from the laptop. At home: Settings → Remote play while it lasts; the panic key | the laptop's keys and mouse reach everything at home, the pointer stays on the streamed screen; the free key gives the laptop its Windows key back, as does leaving the window; the game's window comes forward; at home the banner shows over the game (even with the overlay turned off), Add a device and Change the PIN are refused, and the panic key ends it and locks the laptop out. A guest holding the keyboard seat on a screen gets nothing in |
| The unattended PC (M25c; an installed build) | at home: Settings → Remote play, Start with Windows on, Keep Couch Co-App in the notification area on, remote play on; sign out and in; close the window; leave the PC idle past its sleep time; on the laptop, unlock; at home, Win+L; then unlock at home; Quit from the notification area icon | after signing in the app is only in the notification area; closing the window keeps it running there; the PC doesn't sleep, and its screen stays on while the laptop is unlocked; the laptop says the PC is locked within a few seconds, and stops saying so after; Quit ends it. With the notification area switch off instead: a start at sign-in opens the window, and closing it quits. On one PC: a second profile can stand in for the laptop, and `powercfg /requests` (as administrator) lists "Couch Co-App: remote play is on" |
| Several players | a co-op game (Fling to the Finish) with 2 controllers, no controller on the host; the guest asks for Controller 2 | the guest plays player 2; asking for and moving between seats works |
| The host on a seat (P2) | a session with 2 controllers and "Your controller" chosen; then start the game, and stream it; the guest asks for Controller 2; then pass Controller 1 to the guest and Play here on Controller 2; then take it back | the host plays player 1 and the guest player 2; every seat can be handed over; `joy.cpl` on the host lists only our pads; after End session the host's controller works in games again |
| Game not in front | with keyboard and mouse on, the host switches to another window | the guest's stream window says the game isn't in front; the line goes when the game is back |

### Reading the results

- **Logs.**
  - The installed app's log is `desktop.log`, and its stream window's `viewer.log`, in `%LOCALAPPDATA%\CouchCoApp\profiles\<profile>` on that PC.
  - The host's (default profile) is the `npm run tauri dev` terminal.
  - Ask for the lines around the moment, naming what to look for (`seat status=`, `lost the host's connection`, `the stream window crashed`, `the peer went silent`).
- **Traps already met** (not bugs):
  - Task Manager's **Processes** tab End task asks the window to close: that is leaving, so watching just stops. Only the Details tab kills.
  - An administrator window: Windows keeps a normal program's input and hooks from it. Expected; the warning says so.
  - Alt+F4 while only watching closes the guest's own window.
  - A host controller taking player 1 in co-op when it isn't on a seat: the session dialog warns; choosing it in "Your controller" (P2) hides it.
  - The host's controller still working in a game after the session started: the game was already running and had it open; reconnect it once (HidHide refuses only new opens), or start the game after the session next time.
  - With the host's controller on a seat, one seat short in the game (3 of 4 work, `xinput-watch` shows all 4 slots moving): Steam Input is on for the game and keeps a player for the hidden controller. Turn it off (the game's Properties → Controller). Seen with Fling to the Finish.
  - Seat order isn't XInput slot order: Windows numbers the slots itself. Tell seats apart by moving one pad at a time in `xinput-watch`.
  - A full-screen game ignoring the guest once (M5b): not seen since. On two PCs over the internet (M8) full screen worked, switched to from the guest too. If it comes back, run `xinput-watch.ps1` on the host to tell a game ignoring the pad from input not arriving.
  - A yellow border around the streamed window on a Windows 10 host: Windows draws it while capturing, and only Windows 11 lets us hide it. It isn't in the stream. (Before M8's fix, a Windows 10 host's every stream ended at its first viewer: "Toggling the capture border is not supported".)
  - "Needs update" / "Update yours" instead of a link in the member list: the two builds have different wire revisions (`PROTOCOL_MINOR`); install the same build on both.
- **Record** each run in the milestone's results file ("Your run" row), and new observations the user wants kept in the polish backlog memory.
