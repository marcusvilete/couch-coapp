# P9 results: physical controllers told from virtual pads

## Why

Our virtual pads must carry a real Xbox 360 controller's ids (045e:028e): with made-up ids, games and Steam treat them as a generic joystick and only the D-pad works. So until P9, anything with those ids counted as possibly ours:
- a real Xbox 360 controller, or a third-party pad reporting itself as one, could never be "Your controller", nor be hidden;
- a stream window on the same PC as a running host skipped every Xbox 360 pad, a remapper's virtual pad (DS4Windows) included;
- the warning about a controller plugged into the host counted our own pads.

You asked for it to work for any controller, not only Xbox 360 ones.

## The probe

`couch-cli pad-origins` lists every gamepad SDL finds, where it sits in Windows' device tree and the origin the rule gives it. It can plug our pads (`--ours`, marked as a host's) and pads that leave no marker (`--unmarked`), standing in for another program's. It only reads: nothing is opened, hidden or moved. Your runs on 2026-10-20, with a DualSense (Bluetooth, then USB), a GameSir T4n Lite (wired, in PC mode and in a PS4 mode over Bluetooth), DS4Windows and Steam's PlayStation support:

| Question | Answer |
|---|---|
| Can the device tree tell physical from virtual? | Yes, in every run: a DualSense hangs under Bluetooth (`BTHENUM`) or a USB root hub, the GameSir under a USB root hub, every ViGEm pad under `ROOT\SYSTEM\000N`, service `ViGEmBus` |
| Does SDL give a device path to look up? | For controllers it reads directly (DualSense, the GameSir as a PS4 pad): yes. For XInput pads (the GameSir in PC mode, ours, DS4Windows's): only `XInput#0` to `XInput#3` |
| Can the tree tell our pads from DS4Windows's? | No: identical, on the same bus. A ViGEm pad's serial, the last part of its instance id (`USB\VID_045E&PID_028E\51`), can |
| Steam's PlayStation support | Changes nothing other programs see |
| DS4Windows | Doesn't hide the DualSense by default: games see both it and its virtual pad |

**XInput slots, by the driver's player light.** The Xbox 360 driver (`xusb22`) gives every Xbox-style pad a device interface, ours and other programs' included, and answers which player light it shows (undocumented requests, as community projects describe them). The light is the XInput slot. Checked three ways:

| SDL | The driver's light | ViGEm's own light | Device |
|---|---|---|---|
| `XInput#0` | 6, slot 0 | | the GameSir: USB, physical |
| `XInput#1` | 7, slot 1 | 1 | ViGEm serial 51 |
| `XInput#2` | 8, slot 2 | 2 | ViGEm serial 52 |
| `XInput#3` | 9, slot 3 | 3 | ViGEm serial 36 (unmarked) |

And from another process than the one that plugged the pads (the stream window's case): the same. ViGEm's own slot answer (`user_index`) failed every time, as its library warns; its light is right.

## What changed

**Each gamepad's origin** (`couch_platform_win::pad_origin`): its device found by SDL's path, or for `XInput#N` by the one pad showing slot N's light; then its chain of parents:
- under ViGEmBus: **ours** if its serial is in a running host's marker, or while a host is still plugging a pad, or if no serial can be read; otherwise **another program's**;
- under Bluetooth or a USB root hub: **physical**;
- under a device Windows made in software (`ROOT`, `SWD`): **another program's**;
- otherwise, or if the device doesn't carry the ids SDL gave: **unknown**, and each caller falls back to the rule before P9 (a pad with our pads' ids counts as ours). Nothing is weaker than before.

**Markers name the pad** (`virtual_pads`): each host writes its pad's marker before plugging it, as before, and adds the pad's serial once ViGEm gives one.

**The uses:**
- **"Your controller"** lists physical controllers, real Xbox 360 ones included, never a virtual pad (your choice). One whose origin can't be told is listed only without our pads' ids, as before P9 (a controller behind a USB stack we don't recognise). Pads skipped are looked at again at each listing, so one whose light wasn't set yet when it came appears.
- **Hiding** takes the same controllers as the list, every node of them in their containers, and never a node under a virtual bus.
- **A stream window on the same PC as a host** skips exactly that host's pads, and reads physical controllers and other programs' virtual pads (your choice: a guest on DS4Windows plays through it). It looks again every second.
- **The warning's count** leaves out the hosts' own pads; other programs' count, since games see them.

**Not in P9:** a remapper's virtual pad on a seat as the host's controller (M22), and several seats for the host (M23).

## Code review

A review found no way for a host's own pad to be read back, listed, followed or hidden while the device tree and the lights answer truly: every failure it could build landed on "ours" or "unknown". Fixed from it:
- **Hiding refused a controller it hid before:** one behind a USB stack whose root hub isn't named `USB\ROOT_HUB*` (some vendor drivers) has an unknown origin; the reader listed it, but hiding wanted "physical" and refused with a misleading message. Hiding now takes what the reader lists (`may_be_the_hosts`), and drops anything under a virtual bus even inside the controller's containers.
- **A light that can't be read could free its slot for another device.** Any Xbox driver interface that can't be opened or read now makes every XInput lookup unknown.
- **ViGEm known by its driver's name only:** with that unreadable, a host's pad would have passed for another program's, which a stream window reads. Its hardware id (`Nefarius\ViGEmBus\Gen1`) counts too.
- **The stream window's once-a-second look** re-examined pads already proven, about 1 ms each on its render thread, and a passing doubt could close one (giving a guest's seat back). A pad proven physical or another program's while a host is here is now settled; only pads opened with no host around, and held-back ones, are looked at again, and the markers are read once per look.
- **A marker's rename that failed left a `.partial` file for good:** retried, removed on failure, and swept when its process is gone.
- Smaller: dead code; the probe's text ("reads only", though it can plug pads); `pad-test`'s count, which now shows its own pads again beside the warning's count; the light request's version tried in the order that works; a property type checked; a pad gone between listing and a second look forgotten, never listed without ids; `survey` marked for the probe only.

**Left as it is:** the light requests are synchronous. Xbox 360's driver answers them at once; a third-party driver on the same interface that held one would stall the stream window's loop (its seat then suspends) or the host's reader (listing times out after 2 s).

## Tests

- `pad_origin`: the chains the probe recorded (a DualSense on Bluetooth, the GameSir on USB, ViGEm pads with and without a host's serial), doubt making a ViGEm pad ours (a host plugging, an unreadable serial), software buses, and unknown chains.
- `virtual_pads`: markers with serials, a marker without one meaning a pad being plugged, stale markers removed, doubt keeping one, markers from before P9 read.
- `hidhide`: a real Xbox 360 controller hidden and a virtual one with its ids beside it left alone; only virtual pads with the ids, or ids like ours of unknown origin: nothing hidden; a controller of unknown origin without our ids hidden, a virtual node in its container left alone.
- `pad_origin` also: ViGEm known by its hardware id alone; which origins may be the host's controller.
- `xinput_slots` and `device_tree`: lights to slots; listing and chains on any PC.
- The workspace's tests, clippy, fmt.

## This PC (2026-10-20)

- `release-check`: viewer killed, suspended after 300 ms; its window process killed, gone after 84 ms; window closed, gone after 181 ms; frozen, suspended after 339 ms and active again after thawing; host stopped, 0 ms. All passed.
- `input-sink`: passed. `lifecycle-check`: passed (the game's window followed in 58 ms, a key left held by a killed host released at the next start).
- `host-pad` with our pads plugged: the DualSense and the GameSir, never ours ("it's one of our virtual pads"); `couch-viewer --probe` skipped ours and read both controllers and an unmarked pad. `host-pad --feed 2`: the GameSir hidden (9 devices), shown again after.

## Your run (2026-10-20, this PC)

DS4Windows running, the DualSense chosen as your controller, alice watching:
- "Your controller" offered only the physical controllers.
- alice's stream window read the GameSir and DS4Windows's pad (skipped before P9) and skipped your seat pads, in every session.
- **XInput has 4 slots.** With 2 seats, alice had 2 controllers and could take both seats. With 3, your 3 seat pads and DS4Windows's pad took all four slots, the GameSir got none (no game or SDL sees it), and alice had one controller, so one seat (M18's rule). On two PCs this doesn't happen: the host's seat pads are on the other machine.
- On one PC, DS4Windows's pad moves with your hidden DualSense: DS4Windows is allowed past HidHide, as our app is.
