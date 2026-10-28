# M16a results: building and testing on Linux

## Why

The first part of M16 ([ADR 0011](adr/0011-linux.md)): the workspace builds, passes clippy and passes its tests on Linux, so a Linux viewer (M16b) can follow, and work on Windows can't break Linux unnoticed. Nothing streams on Linux yet.

Version 0.23.2, the same wire revision as 0.23.1: nothing changes on Windows.

## What changed

**`couch-platform-linux`,** a new crate that compiles to nothing elsewhere, as `couch-platform-win` does: `CLOCK_MONOTONIC` through `rustix`. Three users read it on Linux, which must agree, since they compare each other's frame times:
- the stream window's `now_us`;
- the engine's backend for systems without streaming;
- `couch-cli host`.

**Native dependencies:**
- **FFmpeg:** the same BtbN build of the same source as Windows', for Linux (`linux64-lgpl-shared-8.1`). It's pinned by its SHA-256 from BtbN's published checksums, the same list that carries our Windows pin. Its libraries are staged beside the binaries.
- **The binaries find those libraries by an `$ORIGIN` rpath.** It's written as `DT_RPATH` (`--disable-new-dtags`): the newer `DT_RUNPATH` reaches only direct dependencies, and libavcodec needs libswresample.
- **SDL3** is built from source by the `sdl3` crate, at Windows' pinned version (3.4.16), and linked in.
- **FFmpeg's bindings** are generated on Linux and committed beside Windows'. `scripts/dev.sh` chooses them; `.cargo/config.toml` keeps Windows' as its default. `xtask`'s test `each_systems_bindings_match_its_pin` fails on both systems until each file matches its pin.

**Tools:**
- `scripts/linux-deps.sh`: the system packages.
- `scripts/dev.sh`: the shell's environment.
- `xtask`:
  - `fetch-deps`, `stage-dlls` and `doctor` per system;
  - the drivers on Windows only;
  - `notices` says it's Windows-only until M16b.
- **`couch-cli` on Linux keeps `probe`, `host` and `view`.** The rest measure or drive Windows' capture, sound and input. `host` stops on Ctrl+C or SIGTERM there.

**CI:** `.github/workflows/linux.yml`, on Ubuntu 24.04 for every pull request and push to `main`:
- clippy;
- the tests;
- the front end's type check and unit tests.

**Licence policy:**
- `cargo deny` checks Linux's dependencies too. Licences pass.
- Four "unmaintained" notices from Tauri's Linux stack (gtk-rs's `proc-macro-error`; `zbus`'s `zbus_names`, `zvariant` and `zvariant_derive`, via the single-instance plugin) are recorded as accepted, with reasons. They aren't vulnerabilities, and only Tauri can move off them.

## Checked

**On Linux** (Ubuntu 24.04 in WSL):
- the whole workspace builds, the Tauri app included;
- clippy with `-D warnings`;
- every test passes, with the rooms (27) and sessions (76) scenarios run repeatedly;
- the front end's `tsc` and vitest (50);
- `ldd` finds every library of `couch-viewer` and `couch-cli` with no library path set;
- the clock test reads the clock from a second process.

**On Windows:**
- clippy;
- `cargo deny`;
- the whole suite, with the rooms and sessions scenarios run repeatedly.

## Found while checking

- **The tests' port picker looped forever on Linux.** It held an IPv4 socket on a port while binding IPv6 to it. On Windows an IPv6 socket covers IPv6 only; on Linux it covers IPv4 too, so every port looked taken. The rooms tests hung for 37 minutes before this was found.
- **Engines restarting on their own port lost it sometimes:** rarely on Windows, often on Linux. The picker probed with port 0, and the OS could hand a parallel test the port a restarting engine had just let go. Test ports now come from a counter in a range no OS picks by itself (`couch_testkit::net::free_udp_port`), still checked free on both IPv4 and IPv6.
- **A newcomer asking to watch while its connections settle is refused** ("lost the connection to the host"). When two members dial each other at once, one connection replaces the other; a watch request waiting on the replaced one fails (`rooms::sessions`, `lost_host` in `WatchState::Joining`). In the app, Watch works on a second click.
  - Seen once in six Linux runs of `a_newcomer_sees_a_session_already_live`, never on Windows.
  - The scenario now waits for the connections to settle before watching, since it is about the newcomer seeing the session.
  - **The race itself is left for a session change of its own**, with the room-security and input-safety checks.
- **A desktop control scenario raced its own waits** (`desktop_control_ends_with_the_operator`, once in six Linux runs). After remote play was switched off and on, it waited for the device to see "an offer", and the old offer, its withdrawal still on the way, passed the wait. It now waits for the offer to go first, as for the device removed and added back; 20 runs then passed.
- **WSL's wall clock jumped back and forth by 33.7 s** every few seconds (`journald`: "Time jumped backwards"). The author's Windows clock was 33.7 s fast (`w32tm /stripchart` against time.windows.com): WSL pushed Windows' time in, Ubuntu's own sync set the right one. Invite expiry rightly uses the wall clock, so `revoked_and_expired_invites_stop_working`, with its 1-second invite, once saw an expired invite accepted (once in eight runs in WSL).
  - The app is right: on a real clock an invite expires on time, and a clock half a minute off doesn't matter for invites lasting a week.
  - The test is kept as it is. CI's runners and real Linux PCs keep a sane clock. The cure is syncing Windows' clock; `wsl --shutdown` doesn't help.
- **The first clock test compared with `/proc/uptime`,** which counts time asleep where `CLOCK_MONOTONIC` doesn't. It would have failed on any Linux PC after a suspend.
- **A full disk on the author's PC:** `target\debug` had reached 225 GB, and a build ran out of space and corrupted itself. `cargo clean --profile dev` freed it, keeping the release build and the installer.

## The review

A review agent found nothing that changes Windows or weakens a safety rule, and confirmed the three clock users agree. It found:
- the rpath (`DT_RUNPATH`), fixed;
- the clock test's sleep problem, fixed;
- Linux missing from `cargo deny`, fixed;
- `notices` panicking on Linux, fixed;
- Linux's bindings able to go stale unnoticed, fixed by the test;
- the workflow's toolchain step, cache and environment, fixed;
- the restart race in the tests, fixed as above.

## Still to come

M16b, the Linux viewer:
- `launch_viewer` and `decodes()` in a Linux backend;
- VAAPI and NVDEC decoding; `probe`'s "D3D11VA decode" line then needs Linux's own;
- Tauri on WebKitGTK;
- the AppImage and its notices;
- tests in a VM, on bare metal and on a Steam Deck.
