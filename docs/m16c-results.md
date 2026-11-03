# M16c results: the Linux host

## Why

[ADR 0012](adr/0012-linux-host.md)'s first part, M16c-1: a Linux PC (KDE or GNOME on Wayland) hosts sessions with controller seats only, every safety rule unchanged on any desktop. Keyboard and mouse, desktop control and the "game in front" check are M16c-2.

M16c-1 comes in three PRs:
- **part 1, here (version 0.25.0, wire revision 13):** what a system can host, and Linux's encoders;
- **part 2:** guests' controllers through `uinput`, and the host's keys through the global-shortcuts portal;
- **part 3:** capture through the screen-sharing portal, sound from PipeWire, notifications, remote play's home PC, and turning hosting on.

**The author's choices** (ADR 0012, 2026-11-02; and 2026-11-03):
- controllers first, keyboard later;
- the host's own controller outside seats at first;
- KDE and GNOME;
- `/dev/uinput` set up with consent;
- the remote console on a Linux home PC now, desktop control later;
- sound: everything but the app, or one program picked;
- **the operator's device told what its home PC can host** (a wire change), rather than offered what then gets refused.

## Part 1: capabilities and Linux's encoders

**What a system can host** (`HostCapabilities { sessions, keyboard_seat, desktop_control }`)
- **`Backend::capabilities` replaces `can_host`.** Windows has all three. Linux's backend has none yet: part 3 turns on sessions, controller seats only.
- **Each is refused before anything is set up or announced** (`check_capability`):
  - without `sessions`: what M16b refused;
  - without `keyboard_seat`: a session asking for one, from the host's window or from afar;
  - without `desktop_control`: `RemoteOp::Desktop { on: true }` (turning it off always works);
  - the host's own controllers on seats where the system doesn't read them (`local_pads` is `None`, as on Linux).
- **The UI follows `EngineInfo.capabilities`:** Start a session as before (`sessions`); the start dialog drops the keyboard-and-mouse choice without `keyboard_seat` and says why.
- **The operator's device is told** (`RemoteState.capabilities`, wire revision 13, so 0.25.0 doesn't talk to 0.24.x): its start dialog for that PC drops the keyboard seat, and its console drops the desktop panel. The home PC refuses either way; the device only hides.
- **A `remote-console-linux` preview scenario** shows a Linux home PC run from afar.

**Linux's encoders** (`couch_codec_ffmpeg`)
- **Each system's list:** Windows unchanged; Linux `h264_vaapi` (AMD, Intel, the Deck), `h264_nvenc`, then `libopenh264`. NVIDIA's VAAPI driver only decodes, so VAAPI fails to open there and NVENC follows. No HEVC from a Linux host yet.
- **VAAPI takes only its surfaces** (`vaapi.rs`): each frame in memory is converted to NV12 (swscale, BT.709 limited range, as for every encoder) and uploaded into a surface from the encoder's own pool, on the default VAAPI device. A requested keyframe (`pict_type` I) starts an IDR there.
- **VAAPI's option sets:** CBR, one frame in flight, `low_power` first (Intel's fixed-function encoder; AMD's Mesa refuses it), then the plainest, which stands in for "the settings before M9a" in that test.

**A Linux backend** (`pipeline::linux`, replacing M16b's `ViewerOnly` on Linux; other systems keep it): the stream window, the clock, the encoders, the test pattern and the tone. `couch-cli host --source testpattern` runs on Linux.

**A race Linux showed** (once in nine runs of the session scenarios in WSL): a watch asked for whose connection closed before the host answered was refused at once ("lost the connection to the host"), even when another connection to the host was about to replace it (two members dialing each other at once keep one connection; M16a covered the case where the kept one is already there). Now such a watch waits like any reconnect (`WatchState::Reconnecting` keeps the command's reply): answered when the host announces its session on the new connection, refused when the reconnect wait runs out.

## The review

A review agent found no way to start a keyboard seat or desktop control on a host without them, the wire change complete, and VAAPI's references right. Its small points, all taken:
- **A bitrate change rebuilt VAAPI's device** (each reopen opened a new one): a reopen now shares the old encoder's device and surfaces (`VaapiFrames::share`), and a missing device stops at once rather than being tried with each option set.
- **The surface pool was fixed at 8,** and would fail every upload once exhausted: it now grows.
- **Comments said a Linux host has controller seats** before it does; and the start dialog stored a keyboard choice it had forced off, over the device's own: it stores the choice made.
- **Unverified until AMD or Intel hardware runs it:** VAAPI's plainest option set still asks for the high profile and CBR, which a driver could refuse (then that GPU falls back to software).

## Checked

- **Windows:** clippy; the protocol crate (73); the engine's tests, the rooms scenarios (27) and the session scenarios (79), the new `a_controllers_only_host_refuses_keyboard_and_desktop` included; the codec crate, every encoder opened twice without a crash; tsc.
- **Linux (WSL):** fmt, clippy, the codec crate. `couch-cli probe`: `h264_nvenc` opens (WSL reaches the NVIDIA GPU) and `libopenh264`; VAAPI has no device in WSL (no DRM render node, even with Mesa's VAAPI drivers installed), so the VAAPI path waits for AMD or Intel hardware (the Steam Deck).
- **A Linux host streaming to Windows:** `couch-cli host --source testpattern` in WSL, watched by `couch-cli view` on Windows through iroh's relay, then direct: 60.2 fps, 566 frames in 10 s, no decode errors or lost packets, sound with no underruns; the host encoded with `h264_nvenc` from frames in memory, about 5 ms a frame. (Capture to present can't be read across two machines: their clocks differ.)

## Part 2: guests' controllers and the host's keys (0.25.1)

**Virtual controllers through `uinput`** (`couch_platform_linux::pads`)
- **Each pad is made like the kernel's own Xbox 360 controller** (`xpad`): its ids (`045e:028e`, version `0x114`), name, buttons (its X is `BTN_NORTH`, its Y `BTN_WEST`), sticks with Y pointing down, triggers, and the D-pad as a hat. Guide is never sent.
- **Named ours by its `phys`,** `couch-coapp/<pid>/pad<N>` (the process's Nth pad): the stream window on the same PC reads that from sysfs and skips exactly those (`pads::is_ours`, its Linux `host_check`).
- **Rumble:** a game uploads its force-feedback effect and waits for the answer, so each pad has a thread that answers at once, follows which effect plays (and for how long), and reports the motors.
- **A crash leaves nothing held:** a `uinput` device goes with the process that made it.
- **Access:** making a device needs `/dev/uinput` writable. Settings → Drivers lists "Virtual controllers" on Linux, with Set up (`pkexec` asks for the administrator's password, then `uinput` is loaded where it's a module and one fixed udev rule is installed and applied) and the same command to run by hand (a test keeps the two the same). If this session still can't use it, Set up says to sign out and back in. Undo it by deleting `/etc/udev/rules.d/60-couch-coapp-uinput.rules`. Go live's "virtual controllers missing" warning reads it too (`Backend::virtual_pads`).
- **`couch-cli pad-test` runs on Linux,** a `uinput` pad to watch with `evtest`.

**The host's keys through the global-shortcuts portal** (`couch_platform_linux::global_keys`, `ashpd`)
- The panic key, Accept and Decline are offered to the desktop with the user's keys as Ctrl+Alt+Shift+key, the desktop's own key names. The desktop may let the user change them; **the console shows the keys the desktop names** (`HostKeyEvent::Bound`, again whenever they change).
- **Binding returns at once;** the desktop answers later. If it has no portal, doesn't open a session within 5 s, refuses the keys, or goes away, the console drops them (`HostKeyEvent::Lost`), as on Windows when the hook fails: it says so, and Take back does the panic key's work. A panic key the desktop didn't bind takes the answer keys with it.
- **One portal runtime for the process** (`couch_platform_linux::portal`): `ashpd` keeps one D-Bus connection per process, served by the runtime that made it.
- **Safe without telling injected keys apart** only because a Linux host offers no keyboard seat: no guest has keys.

**The review** found the host's keys would have stopped working from a session's second Go live on, and two smaller gaps there, all fixed:
- **The second session's keys:** each binding ran on a runtime of its own thread, and `ashpd`'s one connection per process, made on the first, died with it. Now every portal request runs on one runtime for the process (above); a test binds twice where there's no portal and sees both fail at once.
- **A failed binding wasn't shown:** the console showed the panic key whatever the desktop answered. Now the answer reaches the console (above); a session scenario has the desktop rename the keys, leave one unbound and go away.
- **Binding blocked the room actor** for up to 6 s (3 s, twice with the panic key alone) where the portal didn't answer; and a binding that timed out kept trying in the background. Now it returns at once, and a dropped binding stops at once.

Its small points, taken:
- **Rumble:** a game's repeat count and delay now set how long an effect plays; a failed read no longer ends the rumble thread (a game's next upload would have waited on it); a pad's thread is woken to stop at once rather than within 100 ms (`eventfd`).
- **The `phys` number** counts the process's pads, not seats: `pad<N>`.
- **Set up** loads `uinput` first, says when a sign-out is needed, and names pkexec's other refusal (no agent to ask with).

Left: the portal's session isn't checked on each activation (`ashpd` keeps a session's path private); only one task reads activations at a time, so they are ours.

**Found while building:** evdev 0.13.2's `with_phys` always failed (`EINVAL`): its `UI_SET_PHYS` carried a size of 1 where the kernel expects a pointer's. Vendored with the one-line fix (`third_party/evdev/PATCHED.md`), as vigem-rust is.

**Checked**
- **Windows:** clippy, the engine's tests (the session scenarios, 80, with the new `keys_the_desktop_binds_or_drops_reach_the_console`), the viewer's, the CLI's and the input crate's, tsc, vitest, `cargo deny` (the new crates: evdev, ashpd).
- **Linux (WSL):** fmt, clippy, the workspace's tests, the session scenarios eight times in a row; `couch-platform-linux`'s tests (binding twice without a portal fails at once both times); as root, the real pad: made, found ours by `phys`, B read back, a game's rumble uploaded, played (`0x80`/`0x40`) and stopped; `pad-test` seen by `evtest` exactly as `xpad`'s controller (`045e:028e` v`0x114`, its buttons).
- **Not in WSL:** the global-shortcuts portal (no desktop session there), for the VM.

## Still to come

- **Part 3** (above).
- **In the VM:** the portal's keys on KDE and GNOME (GNOME may want the app registered with the portal, as a sandboxed app is), over two Go lives in a row, with the console showing the desktop's names for them (whether a desktop gives an empty name for a key it bound, which counts as unbound, and how KDE takes Shift with a digit or symbol); a guest's pad driving a game and its rumble; Set up.
- **VAAPI on real hardware:** the Steam Deck (AMD).
