# M16b results: the Linux viewer

## Why

ADR 0011's second part: a Linux PC or a Steam Deck joins rooms, watches and plays a Windows host, and runs a PC of yours from afar (M25, the device side). A Linux PC still can't host (M16c).

M16b comes in two parts:
- **part 1, here (version 0.24.0):** the viewer works on Linux;
- **part 2:** the AppImage that carries it to friends.

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

## Still to come

- **M16b part 2:**
  - the AppImage and its notices, including the bundled system libraries' source packages;
  - the Linux section of the self-hosting guide.
- **The author's tests:**
  - a VM (software decoding);
  - bare metal (VAAPI or NVDEC, latency, the GPU's Vulkan);
  - the Steam Deck (VAAPI, its controls, game mode).
- **Later:** HEVC on Linux, and Show window on Wayland, which needs an activation token to raise a window.
