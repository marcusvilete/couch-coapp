---
name: input-safety-checks
description: Verify that Couch Co-App's remote input stays safe after a change, with `couch-cli release-check`, `input-sink` and the router tests: the host must release a viewer's keys, buttons and pad whenever a session ends badly, and inject only what the seat router allows. Use after changing couch_input (router, forward, filter), couch_net::input or datagrams, couch-viewer's input, pipe or shutdown code, couch_engine::pipeline (host, watch), the engine's rooms::sessions, couch-cli's host/view, couch-platform-win's sendinput, keymap, vigem or foreground code, or anything on the viewer/host teardown path; and before reporting or committing such a change.
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
```

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

## What isn't covered automatically; ask the user

- **`pad-test`:** run it and watch `joy.cpl` (Game Controllers → the Xbox 360 controller → Properties). Needs ViGEmBus.
- **A real game:**
  - put it in windowed mode and turn off Steam Input for it;
  - run the host on the game window and the viewer;
  - with the game focused, the pad should drive it, and rumble lines should appear in both the host and `view` output.
- **Keyboard and mouse end to end:** needs a second PC or Windows Sandbox. On one machine the viewer holds the focus, so the router correctly keeps keyboard and mouse away from the game.
- **The user's DualSense going silent in `joy.cpl`** while SDL or Steam holds it in enhanced mode is normal.

## After the checks

- Report the `release-check` table and any change from the typical values.
- If behaviour changed on purpose, update `docs/protocol.md` ("Input") and the milestone's results file.
