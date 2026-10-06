---
name: input-safety-checks
description: Verify that Couch Co-App's remote input stays safe after a change, with `couch-cli release-check`, `input-sink`, `lifecycle-check` and the router tests: the host must release a viewer's keys, buttons and pad whenever a session ends badly, and inject only what the seat router allows. Use after changing couch_input (router, forward, filter), couch_net::input or datagrams, couch-viewer's input, pipe or shutdown code, couch_engine::pipeline (host, watch), the engine's rooms::sessions, couch-cli's host/view, couch-platform-win's sendinput, held_input, markers, keymap, vigem, foreground, game or panic_key code, couch_engine::pipeline::source, couch_input::shortcuts, or anything on the viewer/host teardown path; and before reporting or committing such a change.
---

# Input safety checks

**What the checks protect.**
- Remote input is remote control of someone's PC.
- Windows keeps an injected key down until something releases it, even after the injecting process exits. So any path that ends a session without releasing leaves a key or button held on the host.

**The rules live in `couch_input::router::SeatRouter`** (docs/protocol.md, "Input"). Its tests are the first guard. Never weaken or delete one to make a change pass. If a rule seems wrong, say so and ask the user.

## Run

```powershell
. .\scripts\dev.ps1
Get-Process couch-* -ErrorAction SilentlyContinue | Stop-Process -Force
cargo test -p couch-input -p couch-net        # router, forwarder, filter; input over loopback
cargo test -p couch-room session              # the host's session machine and its property test
cargo test -p couch-engine --test sessions    # handoffs, panic key, viewers leaving, all with a recording pad
cargo build --release -p couch-cli -p couch-viewer
.\target\release\couch-cli.exe release-check  # about 1 minute
.\target\release\couch-cli.exe input-sink     # a few seconds
.\target\release\couch-cli.exe lifecycle-check  # about 1 minute
```

- **Tell the user first.** `release-check` and `lifecycle-check` start several hosts, each plugging a virtual pad (a Steam notification each), and open windows that take the focus.

- **Close leftovers first.** A running `couch-cli.exe` blocks the release build ("Access is denied"), and a stray viewer confuses the checks.
- **Leave the machine alone while they run.** Both commands open windows. `input-sink` types into its own window, and stops if that window loses the foreground. `release-check` starts and ends viewer windows. Don't click around.
- **Run these from a console.** `release-check` sends Ctrl+Break to a host it started in its own process group, so it needs a console it shares with that host. The PowerShell tool qualifies.
- **Exit code** 0 means everything passed.

## What `release-check` does

Each scenario starts a host (test pattern) and a viewer, waits for the seat to go active, does one thing, and times the host's `EVENT` line against that moment. Both sides read the same QPC clock.

| Scenario | Expected release | Limit | Typical |
|---|---|---|---|
| Viewer's main process (`view`) killed | `suspended`: nothing tells the host, so its 300 ms dead-man timer fires | 350 ms | 250 to 300 ms |
| Viewer window process killed | `gone`: `view` closes the connection | 350 ms | 70 to 120 ms |
| Viewer window closed (WM_CLOSE, as its X button sends) | `gone`, and the viewer process exits | 350 ms | 130 to 200 ms |
| Viewer window process frozen | `suspended`: `view` stops the heartbeat after 250 ms without the viewer's `Alive`, then the dead-man fires. After thawing, the seat must go active again | 600 ms | 350 to 400 ms |
| Host stopped (Ctrl+Break) | `stopped`, and the host exits | 500 ms | under 5 ms |

Ctrl+C and closing the host's console run the same code path as Ctrl+Break. Ctrl+C was verified by hand in M2.

**If a scenario fails**
- **No release at all:** some path ends without calling the router's release, or without reaching it.
  - Check the exits of the input task, `input_loop` in `couch_engine::pipeline::host`: it must release on every one, including when the `HostStream` is dropped.
  - Check the viewer's shutdown: it must send `Outgoing::Close`, not wait for its channels to close.
  - Check `Forwarder::tick`'s liveness gate.
- **Late:** look at the timer constants first:
  - `router::DEAD_MAN`;
  - `forward::{PAD_REFRESH, VIEWER_QUIET}`;
  - the viewer's `ALIVE_EVERY`;
  - `couch_engine::pipeline::host`'s `INPUT_TICK`.

  Their sum sets each limit above. Change a limit only with the user's agreement.
- **Error before acting:** usually leftovers, or the seat never went active (a slow ViGEm plug can take seconds).

## What `input-sink` and `lifecycle-check` do

- **`input-sink`** plays a guest holding the seat: every key, button, move and wheel turn goes through a `SeatRouter` into the `SendInput` injector, into `couch-viewer --input-sink` as the game, and must arrive in order. Then the blocked combinations (Windows, Alt+Tab, Alt+Esc, Ctrl+Esc, Ctrl+Shift+Esc, Alt+F4) must not, beyond their modifiers. A Windows key that got through would open Start and take the focus, which stops the run.
- **`lifecycle-check`**, times on QPC:

| Scenario | Expected | Limit |
|---|---|---|
| `fake-game` swaps its window | host `followed` | 1 s |
| `fake-game` exits | host `game-exited` | 3 s |
| Its window closes, the process runs on | host `source-lost`, after the grace | 5 to 6 s |
| The stream window (`view --restart-viewer`) killed | `view` `viewer-restarted`; the seat active again if it was suspended | 2 s |
| Host killed | `view` reports `connection-lost` (no `Alive` for 5 s) | 5.5 s |
| A helper holding F24 through `SendInputInjector` is killed | F24 stays down; the next `host` start releases it | 1 s after start |

  - The last one checks that F24 really was left down first ("nothing to show" otherwise), and lets go of F24 whatever happens.
  - A stale-key failure means the held-input marker (`couch_platform_win::held_input`) wasn't written, read or acted on: check `%LOCALAPPDATA%\CouchCoApp\held-input\`.

## What isn't covered automatically; ask the user

- **`pad-test`:** run it and watch `joy.cpl` (Game Controllers → the Xbox 360 controller → Properties). Needs ViGEmBus. Each run plugs a pad, and Steam announces it: say so before running it repeatedly.
- **A guest's pad reaches the host:** with a session live and a guest holding the controller, `.\scripts\xinput-watch.ps1` on the host must show every button and stick under the right name in the virtual pad's slot. That is the check for our side; a game ignoring the pad on one PC is usually the game binding to the guest's physical controller (the `app-manual-run` skill).
- **A real game:**
  - put it in windowed mode and turn off Steam Input for it;
  - run the host on the game window and the viewer;
  - with the game focused, the pad should drive it, and rumble lines should appear in both the host and `view` output;
  - on one PC the game may pick the physical controller instead; only two PCs settle it.
- **Pad identity.** The app's virtual pad must report a real Xbox 360 controller's ids (`real_pad_ids: true` in `rooms::sessions`): with made-up ids, games and Steam treat it as a generic joystick and only the D-pad works ("XInput Controller #1" in Steam's notification). A viewer on the same PC skips Xbox 360 pads while a host here has a pad plugged in (`couch_platform_win::virtual_pads` markers). After touching `vigem.rs`, `virtual_pads.rs` or the viewer's `open_pad`, check that `couch-viewer --probe` lists a plugged host pad as skipped and that the marker disappears when it is unplugged.
- **Keyboard and mouse end to end:** needs a second PC or Windows Sandbox. On one machine the viewer holds the focus, so the router correctly keeps keyboard and mouse away from the game.
- **The panic key** (Ctrl+Alt+Shift+Backspace) ignores injected keys by design, so only a person can press it. After touching `couch_platform_win::panic_key`, the shortcuts table or the hook's wiring in `rooms::sessions`, ask the user to stream from the app with a guest playing and press it on the physical keyboard, once with the game in front and once with the host's own app window in front. The guest's pad must go neutral at once, the host panel must show "control paused", and the game must not see a Backspace.
  - The `Chord` unit tests cover the logic, including a guest's injected modifier key-ups (which clear Windows' shared down bit) not blocking it, and a Backspace held before the chord staying the game's. `the_panic_key_takes_control_back_until_allowed` covers the wiring with a fake hook, and every end path checks the hook goes.
  - Never log or block inside the hook callback: every key on the PC waits for it.
- **The user's DualSense going silent in `joy.cpl`** while SDL or Steam holds it in enhanced mode is normal.

## After the checks

- Report the `release-check` table and any change from the typical values.
- If behaviour changed on purpose, update `docs/protocol.md` ("Input") and the milestone's results file.
