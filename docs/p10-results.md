# P10 results: virtual controllers on a fresh PC

## Why

A friend hosting on a fresh install of 0.17.1 (2026-10-23) got no virtual controllers in several sessions, saw them left in Device Manager after the session ended, removed them by hand, and needed two or three restarts before it worked ([the friends' run](m21-results.md)). Their `desktop.log`:
- `seat N: no virtual pad (virtual gamepad driver unavailable: ViGEmBus IOCTL VIGEM_WAIT_DEVICE_READY failed (serial 21): … 0x800701E3)` for every seat, in the first session and again after an app restart;
- moments later, the host's controller reader saw three of those very serials as working Xbox 360 controllers, "another program's virtual pad", still there after the app restarted.

No wire or IPC change: still wire revision 6. Version 0.17.3.

## What was wrong

- **Each ViGEmBus serial (1 to 64) is a device of its own to Windows,** installed the first time a pad with that serial is plugged ("new hardware found"). This PC has all 64.
- **vigem-rust, our ViGEm library, starts each client's serial search at a random serial,** so that two programs don't race for serial 1. We open a client per pad, so nearly every pad on a fresh PC was a new device for Windows to set up, up to 64 times over. The original ViGEm client (DS4Windows and most other programs) starts from 1: a PC sets up the first few once.
- **ViGEmBus's readiness check fails while Windows sets a device up** (`ERROR_DEVICE_HARDWARE_ERROR`). vigem-rust then unplugged the pad and failed the plug; the unplug didn't take, and the pad came up a moment later, owned by nobody. Our code had dropped it, and its marker with it, so it read as a remapper's pad (M22) and stayed until removed by hand.

## What changed

- **Pads take serials from 1.** vigem-rust 0.2.0 is vendored in `third_party/vigem-rust/` (MIT OR Apache-2.0, through `[patch.crates-io]`) with two changes, each marked in the code and described in `PATCHED.md`: `ClientBuilder::first_serial`, and keeping a pad whose readiness check says Windows is still setting it up, leaving readiness to the library's own probes (neutral reports until several are accepted). `couch_platform_win::vigem` connects with `first_serial(1)` and looks up the serials in use first, so ViGEmBus never refuses a plug (each refusal costs it a little kernel memory). On this PC, two pads now take serials 1 and 2.
- **A pad's serial is in its marker from the moment it is plugged,** before it takes input, so a pad Windows is still setting up is known as ours (P9's markers) all along.
- **Waiting for Windows** (`couch_input::Plugged`, `PendingPad`):
  - each pad gets 3 s, as before; a pad not taking input by then is still being set up;
  - the session starts anyway, and one background task (`pipeline::host::set_up_pads`) waits for such pads, handing each to the input task as it comes up: from then on its seat's guest drives it, from their next pad snapshot. Until then the seat can be held, and its gamepad input is dropped, as for a seat whose pad failed;
  - it gives up after 5 minutes (`couch_input::PAD_SETUP_LIMIT`), or at **Stop waiting**, unplugging the pads still waiting; their seats go without a pad for the session;
  - ending the session gives them up too, before the host's own controller shows to games again, and no pad comes up after the end.
- **The host console** says it: "Windows is setting up the virtual controller for Controller 3 and Controller 4, the first time on this PC. Guests can take those seats: their gamepad works once it's ready. 0:42", with Stop waiting; and afterwards, for a seat left without one, "Controller 4 has no virtual controller this session, so a gamepad there does nothing". Preview scenarios `hosting-pads-setting-up` and `hosting-pads-missing`.

## Checked

- Session scenarios (four new, with a fake pad that stays pending until the test lets it through): a pad set up late takes its seat when ready, a guest's press before that reaching nothing; Stop waiting unplugs the pad and none comes up later; ending the session gives a pending pad up before the host's controller shows again; the limit gives it up and says so. 62 pass, three runs in a row; rooms and relay too.
- `couch-cli pad-origins --ours 2`: serials 1 and 2, both ours.
- The input-safety checks: `release-check`, `input-sink` and `lifecycle-check` pass, times in their usual ranges.
- `cargo deny check`: licences, sources and bans pass with the vendored crate. Two new advisories (in `ctutils` and `tokio`, already on main) fail the advisories check: a security update of its own.
- `cargo xtask notices` lists vigem-rust 0.2.0 under its licence.

## The review

A fresh review agent found nothing serious, and checked the teardown order, the drop orders, the markers and the vendored crate against the original (only the marked changes differ). Fixed:
- ViGEmBus may hold the readiness check instead of answering it; the library gives up after 5 s with `ERROR_TIMEOUT`, which still unplugged the pad. That answer keeps the pad too now.
- A pad given up mid-session (Stop waiting, the limit) dropped its marker while the host's controllers were still followed: had its unplug not taken, a follow could have taken it for a remapper's and hidden it. A pad given up now unplugs at once and hands its marker to the input task, which keeps it until the host's controllers stop being followed (`PendingPad::give_up`).
- The waiting task stopped only at the input task's end: dropping the pipeline now stops it too, and so does the input task going away.
- An access-denied answer counts as final, not as Windows still setting the pad up.
- The console's timer counts from the moment plugging began, not from when the app heard of it.
- The ending scenario now proves its order: the fake records how many pads were given up at the moment the host's controller shows again.

## Still to try

Windows' first-time setup can't happen on this PC by itself: all 64 devices exist here. To see it:
- here, remove the first two, from an elevated PowerShell: `pnputil /remove-device "USB\VID_045E&PID_028E\01"` and `\02`, then start a session with two controller seats: if Windows takes long, the console says so, and the pads come up by themselves;
- on the friend's PC, with a `-TestRelay` installer of this branch.

## Your run on the PC that froze (0.18.0, 2026-10-27)

- Windows showed its "installing device" notification and took a few seconds to set the controllers up; **no freeze** this time.
- No warning in the app either, as designed: each pad gets 3 s (plus the library's own readiness check) before it counts as still being set up, and these came up within that. The warning, timer and Stop waiting appear only past it, the long first install that froze things before.

## Known limits

- A pad given up whose unplug ViGEmBus still refuses would stay behind, as before; it can't be told apart from another program's pad once the app has closed. It hasn't been seen since the plugs stopped failing that way.
- Pads are plugged one after another: on a fresh PC each new one can hold the session's start for up to 8 s (the library's 5 s readiness check, then our 3 s) before the rest are waited for in the background. Once a PC has set its pads up, each takes well under a second.
- vigem-rust is a copy now: a new release needs the two changes made again (`PATCHED.md`), unless they go upstream.
