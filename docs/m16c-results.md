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

## Still to come

- **Parts 2 and 3** (above).
- **VAAPI on real hardware:** the Steam Deck (AMD).
