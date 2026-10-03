# M2 results: input

M2 lets a viewer drive the host's game:
- a gamepad, through a virtual Xbox 360 pad on the host;
- keyboard and mouse, through `SendInput`, while the game is in front.

**It works, with one check left for a second machine:** keyboard and mouse end to end.

Measured 2026-10-03 on one machine:
- the RTX 3060 Ti PC from M1;
- ViGEmBus 1.22.0;
- a DualSense controller;
- one Steam game, windowed.

## Checks

| Check | Result | |
|---|---|---|
| Router and forwarder tests | 35 `couch-input` tests. They cover every safety rule (holder and seat epoch, permissions, foreground, blocked keys, Guide button, release-only key-ups, held sync, dead-man, lapses, flaps) and a forwarder-to-router round trip. One `couch-net` test sends input over real loopback endpoints with video running alongside; rumble comes back | pass |
| `pad-test` in `joy.cpl` | The pad cycles every button, both sticks and both triggers. Windows lists it as an Xbox 360 controller with ids 1209:0001 | pass |
| `input-sink`: scripted keyboard and mouse | 118 keys, 5 mouse buttons, mouse motion (exact, through raw input) and the wheel all arrived, in order | pass |
| Physical pad drives the virtual pad | The DualSense's input shows on the virtual Xbox pad in `joy.cpl`. The PS (Guide) button is blocked | pass |
| A real game | It accepted the virtual pad with the development ids. Rumble went from the game to the host, then to the viewer, then to the DualSense, with both ends logging it | pass |
| The viewer skips the host's own pad | With the virtual pad plugged in, the viewer lists it (1209:0001) as a host's pad and skips it | pass |
| Viewer killed with input held | **Killing the `view` process:** released 321 ms after the kill. That is the 300 ms dead-man time after the last heartbeat, plus the 10 ms router tick and the measurement's polling. **Killing or closing the viewer window:** 116 to 160 ms, because `view` closes the connection | pass |
| Keyboard and mouse end to end | On one machine, the viewer and the game can't both have the focus, and the router only injects while the game does | deferred: needs a second PC or Windows Sandbox |
| Gates G1, G3 to G5, rerun because receiver reports now go through the datagram router | G1 p95 15.1 ms. G3 p95 83.7 ms at 19.5 Mbit/s. G4: one keyframe. G5 settled in 0.37 s | pass |

## Found and fixed

- **The viewer froze when its window was closed.** This had been there since M1. The viewer waited for its pipe writer to finish, and the writer only finished once every sender was gone, but the decode thread still held one.
  - It mattered for safety, not just usability: a frozen viewer keeps the connection open, so the host keeps the last pad state.
  - Now the viewer releases what it holds, tells the writer to stop, and exits.
- **Receiver reports used to consume every datagram on the host.** Input datagrams would have been lost. Datagrams now go through one reader that routes them by tag (`couch_net::datagrams`).

**Fixed after a code review.** A review of the finished M2 found 8 defects, and all were fixed:
- **Stopping the host left a guest's keys held.** Windows keeps injected keys down after the process that pressed them exits. Ctrl+C, Ctrl+Break and closing the console now release input first. *Verified:* Ctrl+C sent to the host's console printed `host stopping: input released` before exit.
- **The foreground check compared processes, not windows.** Another window of the same process passed: a second browser window, or anything hosted by `ApplicationFrameHost` or `explorer`. The host now compares the captured window's handle at its root, and gets that handle from the capturer itself rather than a separate title search.
- **Clicks could land outside the game.** The pointer moves relatively, so it could reach the taskbar. Button presses now need the pointer over the game window.
- **Blocked combinations looked at one seat's keys.** Two holders could build Alt+Tab between them. Held modifiers now count across seats.
- **A hung viewer kept the heartbeat going.** The main process sent it, whatever the viewer was doing. The viewer now sends `Alive` every 100 ms, and the heartbeat stops after 250 ms without one. *Verified:* freezing the viewer process released input 396 ms later, and unfreezing it made the seat active again.
- **The foreground state could be 10 ms stale.** It is now refreshed right before every input.
- **Rumble could outlive a lost "stop" by 10 s, or outlive the seat.** It now repeats every 500 ms while running, each one lasts 1 s, and losing the seat stops it.
- **`input-sink` could leave a key held.** If the sink lost focus between a key's press and its release, the release was never sent. Releases now always go out.

After the fixes, the three release scenarios measured again:
- killing `view`: 292 ms;
- killing the viewer window: 103 ms;
- closing it: 196 ms.

## Things to know

- **Games read pads only while they're in front.** On one machine, focusing the viewer leaves the game without input. In a real session the game is in front on the host's PC.
- **On one machine the game also sees your physical pad.** It reads it directly, or through Steam Input. Expect doubled input or switching button prompts.
  - Disable Steam Input for the game (Properties → Controller) when testing.
  - `pad-test` drives the virtual pad alone.
- **`joy.cpl` can stop showing a DualSense** while SDL (the viewer) or Steam has switched it to its richer report mode. Reconnecting the controller resets it.
- **Steam Input on a viewer's PC** can map a pad to mouse and keys while the viewer window is focused. The viewer would forward those.
- **Development pad ids.** `couch-cli` plugs the pad as 1209:0001 so a local viewer can skip it. Use `--real-pad-ids` for games that insist on a real Xbox 360 controller's ids, but only when no viewer runs on the same PC.

## Follow-ups

- **Alt+F4.** It isn't blocked. A member granted the keyboard can close the game. Decide in M6 whether to add it to the blocked keys.
- **A restarted viewer.** If the viewer process dies but the main app keeps the connection (M5 restarts the viewer once), the forwarder must release what the viewer held. Today `couch-cli view` closes the connection instead.
- **The panic key** (M5) uses `SeatRouter::revoke_all`.

## Reproduce

```powershell
cargo build --release -p couch-cli -p couch-viewer
$cli = '.\target\release\couch-cli.exe'
& $cli pad-test --secs 30                    # then watch joy.cpl
& $cli input-sink                            # keyboard and mouse injection, checked
& $cli release-check                         # input released on kill, freeze, close and host stop
& $cli host --source window --title "<game>" --relay none [--allow-kbm]   # prints TICKET
& $cli view <ticket> --relay none
cargo test -p couch-input -p couch-net
```
