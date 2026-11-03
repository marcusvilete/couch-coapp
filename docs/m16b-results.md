# M16b results: the Linux viewer

## Why

ADR 0011's second part: a Linux PC or a Steam Deck joins rooms, watches and plays a Windows host, and runs a PC of yours from afar (M25, the device side). A Linux PC still can't host (M16c).

M16b comes in two parts:
- **part 1 (version 0.24.0):** the viewer works on Linux;
- **part 2 ([below](#part-2-the-appimage)):** the AppImage that carries it to friends. Packaging alone: it carries 0.24.0.

No wire change: 0.24.0 talks to 0.23.x.

**The author's choices (2026-10-28):**
- an AppImage bundling WebKitGTK and GTK;
- Lucide's icon font for the stream window's bar;
- H.264 only on Linux for now.

## What changed (part 1)

**A system that can't host says so, and refuses**
- **`Backend::can_host`,** true for Windows' backend only, reaches the UI as `EngineInfo.can_host`, beside `EngineInfo.platform`.
- **Refused before anything is set up or announced** (`check_can_host`): `StartSession`, `StartStream`, and the home PC's side of remote play (`SetRemotePlay { on: true }`, `SetRemotePin`, `AddRemoteDevice`).
- **What still works:** watching, playing, and the device side of remote play (accepting a PC, unlocking it, running its sessions, desktop control).
- **Start with Windows is offered on Windows only** (`null` elsewhere).
- **Why this was needed:** before, nothing refused hosting on Linux. A session was announced to the room, and only its stream failed.

**The Linux backend watches**
- **Off Windows, `ViewerOnly`** replaces `Unsupported`: it starts the stream window and refuses everything hosting needs.
- **Starting the stream window is one function for both systems** (`pipeline::viewer_process`). Only Windows' no-console flag differs.

**Hardware decoding on Linux** (`couch_codec_ffmpeg::decoder`)
- **Each system's list of hardware decoders, tried in order:** D3D11VA on Windows; VAAPI (AMD, Intel, the Deck), then CUDA (NVDEC) on Linux; else software.
- **Any hardware frame is copied to memory as NV12,** told apart by its hardware frames context.
- **A decoder that FFmpeg quietly drops to software says so:** its kind and the stream window's label follow the frames.
- **YUV420P and YUVJ420P** are interleaved into NV12 by chunks, not byte by byte.
- **`couch-cli probe`** reports each of the system's hardware decoders. One available is enough.
- **The software decoder's thread count is unchanged** until it's measured on the Deck.

**The stream window's look on Linux**
- **Text:** the system's fonts, from the paths distributions use (Noto, else Cantarell or DejaVu).
- **Icons:** Lucide's font, staged beside the stream window.
  - **Pinned:** `lucide-static` 1.52.0, the version of the app's `lucide-react`, in `LINUX_DEPS`.
  - **Matching the app:** its icons are the ones the app's own stream-window panel uses.
  - **Checked:** an `xtask` test compares the HUD's code points with the pinned font's `codepoints.json`.
- **Windows keeps Segoe.**

**The shell on Linux**
- **Release builds drop `WEBKIT_INSPECTOR*`,** as they drop `WEBVIEW2_*`: either would open a port through which any program could drive the app.
- **Restart now starts `$APPIMAGE`** when there is one. The executable inside the AppImage's mount goes away with the old process.
- **An AppImage registers `couchco://` links at start,** its main profile only. The deep-link plugin's `.desktop` handler points at `$APPIMAGE`, and is rewritten when that path changes.

**The UI follows `canHost`**
- **Hidden:**
  - Start a session (the empty state explains instead);
  - Settings → Overlay and sounds, and Drivers;
  - the host's shortcuts;
  - Remote play's home-PC parts, now their own `HomePc` component.
- **Kept:** the device side.
- **Neutral wording where Linux shows it:** "Like the system", "after reinstalling your system".
- **A `linux-viewer` preview scenario.**

**Secrets on Linux, as ADR 0011 decided:** every profile file is written 0600, through `store::write_atomic`, in folders made 0700 (the profile's own included). A temporary file a crash left behind is set to 0600 again.

## Checked

- **On Linux (WSL):**
  - clippy, and the whole suite;
  - the new tests: profile file modes, and Lucide's code points against the pinned font.
- **End to end, Linux watching Windows:**
  - **The set-up:** `couch-cli host --source testpattern` on Windows, `couch-cli view` in WSL, through iroh's public relay, then direct.
  - **The result,** for 20 s: 59–61 fps, 1,122 frames, no decode errors or lost packets, arrival to on-screen 11.7 ms median and 17.6 ms at the 95th percentile, and sound with no underruns.
  - **Decoded with NVDEC (copied):** VAAPI was absent and CUDA started, the new path. WSL drew it in software (`llvmpipe`), as it has no hardware Vulkan.
- **The stream window's bar in WSLg:** Lucide's icons and the system's text, laid out as on Windows.
- **The app on WebKitGTK in WSLg:** it starts and draws the first-run screen in the app's look; `light-dark()` works there. Its profile is in `~/.local/share/CouchCoApp`.
- **Windows:**
  - clippy;
  - the scenarios, including `a_viewer_only_platform_refuses_to_host_and_still_watches`;
  - `npm run shots`, tsc and vitest.

## The review

A review agent found no way to start hosting on a system that can't, and the device side of remote play intact. It found, all fixed:
- **The profile folder wasn't reliably 0700:** the shell's log made it first, with the default mode, and an older profile kept its files' loose modes. The engine now sets 0700 on it at every start. A folder only its owner can enter protects everything inside, the webview's data and the logs included.
- **`$APPIMAGE` was trusted blindly:** inherited from a parent that is an AppImage, it would have registered invite links to another program. Now it counts only when our own executable runs inside `$APPDIR`.
- **The tray icon was built on Linux,** whose indicator library may be missing; its switch is hidden there anyway. Only Windows builds it now, and closing the window or starting in the background asks whether an icon exists.
- **Remote play left on** by an older build's profile stayed on, its off switch hidden. On a system that can't host it now loads as off; the file is left as it was.
- **The decoder's fallback to software was one way:** it now follows each frame, back to hardware too.
- **A full-range picture** (YUVJ420P) is now said once in the log; the shader takes limited range.
- **A native library printing to stdout** could have corrupted the pipe on Linux: the stream window now sets its stdout aside for the pipe before any library loads, and points fd 1 at stderr (`couch_platform_linux::stdio`).
- **Nits:**
  - the TypeScript type is `SystemPlatform`, beside the front end's own `Platform`;
  - the icon test counts all nine icons;
  - Cantarell's bold is among the Linux fonts.

**Rechecked after the fixes:**
- **Linux:** clippy, the whole suite, and watching the Windows host again through the new pipe output: 59.8 fps, 0 decode errors, 13.9 ms median.
- **Windows:** clippy, the scenarios, and `npm run shots` (no problems).
- **Before the fixes,** the live input checks on Windows passed: `release-check` 5 of 5, `lifecycle-check` 6 of 6, and `input-sink`.

## Found while checking

- **CI's runner raced `one_stream_starts_at_a_time`:** it sends two stream starts at once and expects the second refused while the first starts. On a fast runner the fake capture started at once, the first stream was live before the second arrived, and that one started a second stream, as the engine allows. The fake's capture now takes 300 ms to start in that scenario (`capture_slow`), so the second always arrives during the first; 8 Linux runs and 5 Windows runs then passed.
- **WSL can't reach a Windows host directly** (its network address translation, and the firewall). Through a relay it connects, then goes direct.
- **Screenshots of WSLg windows:** `PrintWindow` on WSLg's `msrdc` window, never the whole screen while the author is using the PC.

## Part 2: the AppImage

**How it's built**
- **`bash scripts/build-appimage.sh`** on Ubuntu 24.04 (WSL's included), from a git clone, runs `cargo xtask appimage`: `build-installer.ps1`'s twin, in Rust so the relay's token never passes through a shell, and its parsing has tests.
  - **The same checks and options:** `release.toml` (`COUCH_RELEASE_CONFIG` reaches Windows' own from WSL), refused inside the repository, the addresses checked; a clean tree for a release build; a warning for an unpushed commit; the same version in the three files. `--test-relay`, `--config`, `--invite-page`, `--no-invite-page`.
  - **The build's values** are set for Tauri's build alone, as on Windows.
- **Tauri's own AppImage bundler** (linuxdeploy, with its GTK plugin), from `tauri.appimage.conf.json`: `couch-viewer`, Lucide's font, `LICENSE` and the notices go into `usr/bin`, beside the app, where the app finds them. Two things it needed:
  - **FFmpeg's libraries on `LD_LIBRARY_PATH`:** linuxdeploy doesn't follow our `$ORIGIN` rpath. It then copies them into `usr/lib` and sets both binaries' runpath to `$ORIGIN/../lib`.
  - **Our binaries stripped** for it (`CARGO_PROFILE_RELEASE_STRIP=debuginfo`): the release profile's line tables made them 210 MB and 97 MB, and the AppImage 175 MiB. Now it's 132 MB.
- **It bundles twice:** the notices list what the bundler copied, known only after it has, and must be inside. The second bundle is checked to carry them, with the same runtime.
- **Into `target/appimage/`:** `Couch-Co-App_<version>_<commit>.AppImage`, the notices, FFmpeg's two source archives and `SOURCES.txt`.

**What it carries**
- **280 files from Ubuntu 24.04, out of 98 source packages:** WebKitGTK, GTK, GLib, GStreamer's core (WebKitGTK links it), ICU, and their kin, with the GTK plugin's loaders, input methods, schemas and typelibs.
- **The system's own:** glibc, libstdc++, the GPU's drivers (GL, EGL, Vulkan, DRM), X11 and xcb, fontconfig, freetype and harfbuzz stay the system's, as AppImages do. It needs glibc 2.39 or newer.
- **Its runtime is the static type2-runtime,** which needs no libfuse2: it ran in WSL without extracting.

**The notices on Linux** (`cargo xtask notices`; in an AppImage, `notices/system_libraries.rs`)
- **Each system's own crates:** cargo-about runs for the system being packaged (`--target`), so Linux's notices list GTK's and WebKitGTK's bindings and not Windows' (546 crates there, 475 on Windows).
- **SDL3 with the hidapi, yuv2rgb and fdlibm code built into it,** from `sdl3-src`'s sources, on both systems: Windows' SDL3.dll carries them too, and its notices lacked them. `sdl3-src` must stay at Windows' pinned SDL3 version, checked.
- **Lucide's font** (ISC).
- **The AppImage's own parts:** the runtime (MIT), with musl, libfuse (LGPL-2.1, with the runtime's source at its commit), squashfuse, zstd and zlib built into it, `AppRun.wrapped` (AppImageKit, MIT), linuxdeploy's `AppRun` script and the GTK plugin's start-up hook (both MIT). Their texts are pinned in `xtask/licences/appimage/`, and the build fails if the runtime names other parts.
- **Every system file traced to its package:** `dpkg -S` by name, the path sharing most of the file's path winning (the bundler moves libraries into `usr/lib`). Per source package: its version, its address on Launchpad, the binary packages and files, and the package's copyright file. A file no package installed fails the build. 2.1 MB of notices in all.

**Checked**
- **In WSL, from a clone:** a test AppImage built end to end; `ldd` finds every library inside it or among the system's own; the app starts from it, goes online through the relay, and draws its first screen in the app's look; the stream window from it draws its bar with Lucide's icons.
- **The AppImage's stream window watching this PC's Windows host:** its `couch-viewer` and libraries, started by `couch-cli view` (the engine's own way of starting it) through iroh's public relay, then direct. For 15 s: 60.1 fps, 854 frames, no decode errors or lost packets, arrival to on-screen 7.4 ms median and 9.1 ms at the 95th percentile, no sound underruns; decoded with NVDEC (copied), FFmpeg loaded from inside the AppImage. Not yet: the AppImage's app itself in a room, which needs someone at its window.
- **The notices' sections,** and Windows' notices with the new SDL3 parts.
- **xtask's new tests** (options, `release.toml`'s reading as the PowerShell script reads it, the addresses, the path matching), on both systems.

**The review (part 2)**

A review agent found no way for the relay token to leak and nothing against CLAUDE.md's rules. Its findings, all fixed:
- **Addresses the app can't read passed the check** (a bad port, a space): `check_address` now parses with the `url` crate, the parser the app's `RelayUrl` uses, so a release can't quietly fall back to iroh's relays.
- **Only `usr/lib` and `usr/share` were traced:** the whole AppImage is now, with an explicit list of our own files and its tooling's; anything else must come from a package, or the build fails.
- **The final checks read the folder, not the AppImage,** and an older bundle's file could have been taken: the bundle folder is cleared before each bundle, the notices are unpacked from the final AppImage itself, the two bundles must hold the same files, and a build that changes tracked files (`Cargo.lock`) fails a release.
- **SDL3's own maths functions** (Sun's fdlibm, in Windows' SDL3.dll, which links no C library) ask to keep their notice: now in, on both systems.
- **"Ubuntu 24.04" was said but not checked:** the build now requires it (`VERSION_ID`), as the notices, `SOURCES.txt` and the glibc 2.39 promise assume.
- **AppRun was misnamed:** it is linuxdeploy's script (MIT, now in), and AppImageKit's binary is `AppRun.wrapped`.
- **Hardening:** dpkg's diversion lines can't be taken for packages; a library moved into `usr/lib` must be the one the system's loader finds by that name (`ldconfig -p`); `release.toml` is refused inside any git work tree (WSL reaches Windows' checkout), a key set twice is refused, a byte-order mark read, errors name a line by number rather than echo it, and the configuration's debug form hides the token; a relative `--config` is the caller's; `CARGO_TARGET_DIR` elsewhere is refused; an xtask test keeps `sdl3-src` at Windows' SDL3 pin.
- **Docs:** the self-hosting steps in order, with `npm ci` and the clone's own folder; the skill's copy command.

**Found while checking**
- **The stream window crashed at its end in WSL when NVDEC decoded** (a segfault after FFmpeg's "cuCtxDestroy failed"), whether packaged or not; in software it ended cleanly. Its decode thread dropped the decoder, and with it CUDA's context, while the process was already exiting. Fixed in 0.24.1, below.

## 0.24.1: the stream window ends cleanly with NVDEC

- **The window now stops its decoder before it ends** (`DecoderStop`): it tells the decode thread to stop as soon as its loop ends, alongside releasing input and the GPU's last work, then waits for it, at most a second, before the GPU goes and before returning; on an error or a panic too, as the guard drops. The decode thread, waiting for video, looks every 100 ms (`next_message`), so a pipe that stays open can't hold it. On Windows the decoder, and a decoded frame still waiting, now also go before the GPU device they may hold.
- **The review** found nothing of substance; its three small points are in: the stop starts early, so the two one-second waits (the GPU's and the decoder's) overlap rather than add up against the main process's 2 s for a closing window; the guard covers the early exits; the waiting frame goes before the GPU.
- **Checked:**
  - **In WSL with NVDEC:** a stream window ended by a signal exits cleanly 3 times of 3 (status 0, was 139), in about 180 ms, and in 50 to 110 ms once the stop starts early; in software too. Watching the Windows host: 60.1 fps, no decode errors, and a clean end.
  - **On Windows:** the stream window's tests and clippy; the session scenarios (78); `release-check` 5 of 5 (the window closed: gone after 207 ms, 166 ms after the review's change), `lifecycle-check` 6 of 6, `input-sink`, before and after the review's change.
- **Found while checking:** SDL took 10 to 15 s to start on this PC, which failed every `release-check` scenario before it acted ("the seat never went active"). The DualSense was paired but not yet connected, and the author's QuadCast microphone (a HID device too) may have been in an error state. Once the controller was on, SDL started in 0.1 s. `couch-viewer --probe`, timed, tells this apart from a real failure.

## Still to come

- **Handing the AppImage out**, with the Windows installer of the same commit, and seeing it run on another Linux PC.
- **The author's tests:**
  - a VM (software decoding);
  - bare metal (VAAPI or NVDEC, latency, the GPU's Vulkan);
  - the Steam Deck (VAAPI, its controls, game mode).
- **Later:** HEVC on Linux, and Show window on Wayland, which needs an activation token to raise a window.
