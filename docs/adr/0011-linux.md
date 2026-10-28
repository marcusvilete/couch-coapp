# ADR 0011: Linux

**Status:** accepted (2026-10-28), for M16. The author's choices:
- the viewer first, the host later;
- Wayland, on KDE or GNOME;
- tested over time in WSL, a VM, on bare metal and on a Steam Deck.

## Context

Couch Co-App runs on Windows only. Friends with Linux PCs, and a Steam Deck as a handheld to play from the couch, can't join a session. M16 in `docs/architecture.md` planned Linux "behind the same traits".

What the code offers already (measured 2026-10-28):
- **The pure crates need nothing:** `couch-proto`, `couch-room`, `couch-media`, `couch-input` and `couch-net` have no Windows code.
- **The engine reaches the system through `pipeline::Backend` alone.** There's already a backend for other systems that refuses to stream, and a secret store that keeps files unencrypted there.
- **The viewer was written to allow it:**
  - its zero-copy path (D3D12VA into wgpu) is optional;
  - the decoder falls back to software without D3D11VA;
  - it skips controllers only while a host on the same PC has pads, which a Linux viewer never has.
  - **The one hard stop** is the clock frames are timed on (QPC), shared with the main process: off Windows it is a `compile_error!`.
- **Profiles** already go where Linux keeps such data: `~/.local/share/CouchCoApp`.
- **Missing:**
  - nothing ever builds or tests off Windows;
  - FFmpeg and SDL3 are pinned as Windows builds only;
  - the generated FFmpeg bindings are Windows' (C's `long` is 32 bits there, 64 on Linux).
- **The wire is the same whatever the system,** so a Linux viewer watches a Windows host with no protocol change.

A Linux *viewer* needs little from the system: a window, the GPU, sound and controllers, which SDL3 and wgpu already cover. A Linux *host* needs much more, and Wayland changes what an app may do:
- **capture** goes through a system dialog where the user picks the window;
- **input** is injected through the kernel (`uinput`);
- **an app can't tell which window is in front.**

That last point is what one of our safety rules (keyboard and mouse only while the game is in front) relies on.

## Decision

### In three parts

- **M16a, builds and tests on Linux:** the workspace builds, passes clippy and its tests on Ubuntu 24.04, in WSL and in a CI workflow on every pull request. Nothing streams yet. It finds the Windows assumptions hidden in the build and the tests.
- **M16b, the Linux viewer:** a Linux PC or a Steam Deck joins rooms, watches a Windows host, plays with its controllers, keyboard and mouse, and can be a remote-play device (M25, the device's side). Packaged as an AppImage.
- **M16c, the Linux host:** later, with its own design step first (below). Not decided here.

### Wayland, on KDE or GNOME

What Fedora, Ubuntu and SteamOS's desktop mode run. The viewer needs nothing that Wayland withholds. X11 isn't a target of its own: SDL3 and WebKitGTK run there anyway, but nothing is designed or tested for it.

### `couch-platform-linux`

A crate of its own, `#![cfg(target_os = "linux")]`, the only one that calls Linux-specific APIs (the clock now; later D-Bus, evdev, `uinput`, PipeWire), as `couch-platform-win` is the only one calling Windows'. The engine's Linux backend lives beside the Windows one, in `pipeline`.

### The clock

`CLOCK_MONOTONIC`, through `rustix` (Apache-2.0 or MIT). Like QPC, it is one clock for every process since boot, so the main process and the stream window time frames on the same scale.

### Native dependencies, pinned as on Windows

- **FFmpeg:** from the same BtbN autobuild as Windows' (the `linux64-lgpl-shared-8.1` build). The same version and source, so `deps::SOURCES` doesn't change. Still LGPL and shared, as ADR 0008 requires. It decodes with VAAPI (AMD, Intel, the Steam Deck) and NVDEC (NVIDIA). It's built against an old glibc, so it runs on most distributions.
- **SDL3:** built from source by the `sdl3` crate, at its pinned version, on Linux only. Distributions carry other versions, or none: Ubuntu 24.04 has no SDL3.
- **The FFmpeg bindings** are generated on Linux too and committed beside Windows'. `.cargo/config.toml` keeps Windows' as the default; Linux's shell script and CI set the variable that chooses Linux's.

### Secrets

The identity key and the rooms go in files only their user can read (mode 0600), as SSH keys do. They are not sealed with a password or the system keyring, for two reasons:
- **The Secret Service isn't always there.** SteamOS's game mode has none, and a keyring that isn't unlocked would stop the app at start.
- **DPAPI's protection** (against other users, and a disk read elsewhere) is what file permissions and disk encryption give on Linux.

The warning that they're stored unencrypted goes. A keyring can come later, as an option, if someone asks.

### Packaging: an AppImage

One file that runs on any distribution and on SteamOS's read-only system, without root, and that Steam can add as a non-Steam game. It carries:
- the app and `couch-viewer`;
- FFmpeg's and SDL3's libraries;
- the licence notices, with FFmpeg's source beside it, as every Windows installer does (ADR 0008, M10b).

A release build carries the relay and invite page from `release.toml`, as Windows' do (ADR 0007). `.deb` and `.rpm` packages aren't made for now.

### Testing

| Where | What it checks |
|---|---|
| WSL (Ubuntu 24.04) | building, clippy and the tests; the developer's day-to-day Linux |
| CI (GitHub Actions, `ubuntu-24.04`) | the same on every pull request, so Windows work can't break Linux unnoticed |
| A VM | the app and the viewer end to end against a Windows host, decoding in software (no GPU) |
| Bare metal | GPU decoding, controllers, latency |
| A Steam Deck | the handheld viewer: VAAPI on AMD, its built-in controls, game mode |

The rule stays: `cargo test --workspace` passes without a GPU, drivers or internet, on both systems.

### The Linux host: open questions for M16c

Not decided now:
- **Capture:** the screen-share portal through PipeWire. The user picks the window in the system's dialog, so "stream this game" becomes "pick it in the dialog". Following a game that swaps its window would need the dialog again.
- **Keyboard and mouse only while the game is in front:** Wayland gives no app the active window. This is a safety rule (CLAUDE.md), so it needs a design before any code. Candidates:
  - compositor-specific interfaces (KDE's, GNOME's);
  - a different guard;
  - keyboard and mouse off for Linux hosts.
- **Injection:** `uinput` for pads, keys and the mouse. It needs a udev rule for `/dev/uinput`.
- **The panic key:** reading the keyboards through evdev, which tells physical keys from injected ones by their device. It needs the `input` group.
- **Hiding the host's controller:** HidHide has no Linux equivalent. Grabbing the device (`EVIOCGRAB`) hides it from programs reading evdev, but not from those reading hidraw.
- **Game sound:** PipeWire, which can capture one application's streams.
- **Encoding:** NVENC through CUDA, VAAPI for AMD and Intel; frames from PipeWire as DMA-BUFs.

## Consequences

- The crate rules gain `couch-platform-linux`, beside `couch-platform-win`.
- CI builds and tests Linux on every pull request; Windows is still built and checked on the author's PC.
- Every Windows-only test is marked `#[cfg(windows)]`, never removed.
- A Linux friend gets an AppImage, as a Windows friend gets an installer. Both follow the same version and wire revision rules.
- Until M16c, a Linux PC can't host. The app says so where a Windows host's controls would be.
