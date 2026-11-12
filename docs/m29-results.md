# M29 results: HDR on a Windows host

## Why

A friend hosted with their monitor in HDR, and the author, watching, saw washed-out colours, they said even of a window that wasn't HDR (found 2026-11-09, listed in [P14's results](p14-results.md)). The author has no HDR monitor; the friend does.

**Your choices (2026-11-12):** SDR out with the right colours first (HDR end to end, 10-bit HEVC to HDR screens, a later step); highlights brighter than SDR white clipped (a roll-off only if a real game shows the need); the Virtual Display Driver installed on this PC for a virtual HDR monitor; then, once it worked, a switch in Settings, on by default.

Version **0.34.0**, no wire change: it talks to 0.33.0.

## What we found

The plan assumed Windows' 8-bit capture of an HDR desktop was wrong in general. Measured on the virtual HDR monitor (`couch-cli hdr-probe`, SDR white at 240 nits, Windows 11 25H2), it isn't:

- **SDR content comes out right in 8-bit.** Colour bars in an ffplay window, and a plain page in Edge, read exactly their values (the 75% grey at 0.749), as a window and as the whole monitor. Windows divides by the SDR white level itself.
- **Content a window presents in HDR is clipped at 80 nits.** A window presenting FP16 scRGB, as a game in HDR does (and Auto HDR and RTX HDR make SDR games do), comes out of the 8-bit capture as scRGB clipped to 1.0 and sRGB-encoded, with no division by the SDR white: everything from 80 nits up is white, and a 50% grey at SDR white reads 0.824 instead of 0.5. Blown and washed out.
- **An FP16 capture holds everything scene-referred:** SDR content raised to the white level (the 75% grey at 1.562 = 3.0 × 0.52), HDR content at its own values. Divided by the SDR white, clipped and sRGB-encoded, both come out right.
- On the SDR monitor, an FP16 capture is linear with white at 1.0 (not the gamma-encoded FP16 that Sunshine's open PR #5106 met on Advanced Color SDR displays).

So the friend's case was most likely a game in HDR, or with Auto HDR or RTX HDR on. The fix is the one designed: capture FP16 and convert it ourselves.

## What changed

- **The monitor's colour** (`couch_platform_win::monitor::colour`): whether DXGI says it is in HDR (`IDXGIOutput6::GetDesc1`, colour space G2084/BT.2020), its bits, its SDR white level (`DisplayConfigGetDeviceInfo`, `DISPLAYCONFIG_SDR_WHITE_LEVEL`) and peak brightness.
- **The capture's format is a choice** (`wgc::Format`, `Bgra8` or `Fp16`), and can change while it runs (`wgc::FormatSwitch`): the pool is remade after the next frame is delivered, so a still window keeps its picture.
- **The conversion** (`couch_platform_win::tonemap::Converter`): a pixel shader (compiled once per process with `D3DCompile`) draws each FP16 frame into the texture pool's BGRA texture in place of the copy (`TexturePool::convert`): scRGB divided by the SDR white over 80 nits, clipped, the sRGB curve. The pool stays BGRA, so nothing after it changes: no new `source`, NVENC, HEVC, the read-back and the other encoders as before. A capture to memory converts into a BGRA texture of its own first. The maths lives in `couch_media::hdr`, which the shader's test holds it to.
- **The capture follows the monitor** (`capture.rs`): FP16 only on a monitor in HDR, 8-bit everywhere else as before. A thread of its own looks every second (a look costs about 0.5 ms, never on the capture's thread) and switches the format and the white when HDR is turned on or off, the white level moves, or the window moves to another monitor.
- **The log says it**, at each capture's start and each change: the monitor's colour space, bits, SDR white and peak, and what the capture does. A friend's `desktop.log` now answers what happened.
- **Settings → Streaming** (your request): "Convert the picture of a monitor in HDR", on by default, from the next stream. Saved in `streaming.json` (in backups; `Command::GetStreaming`, `SetStreaming`), read when each stream starts (`StreamOptions::hdr`, `CaptureRequest::hdr`, `CaptureOptions::hdr`). Only where the PC can host, and not on Linux, whose capture converts nothing. `COUCH_HOST_HDR=0` turns it off for measuring whatever the setting says, and the log names which.
- **`couch-cli hdr-probe`:** the monitors' colour; one frame grabbed in both formats with what it holds; `--pattern`, a window of known scRGB patches (greys from 20 to 1,600 nits, colours at SDR white and at 1,000 nits, BT.2020's green) compared through Windows' 8-bit capture, the FP16 one and the host's own capture against what SDR should hold, then moved to an SDR monitor and back with the capture running; `--bench`, the conversion against the copy on the GPU; `--show-pattern`, for streaming it.

## Checked

- **The host's capture, patch by patch** (`hdr-probe --pattern`, the virtual monitor at SDR white 240 nits): all 24 patches within one 8-bit step of what SDR should hold, on the GPU path and the memory path. With `COUCH_HOST_HDR=0`, 5 patches off, as Windows' 8-bit capture is:

  | Patch | Shown (scRGB) | 8-bit before M29 | Host capture now | SDR should be |
  |---|---|---|---|---|
  | grey 20 nits | 0.250 | 0.537 | 0.318 | 0.320 |
  | grey 80 nits | 1.000 | 1.000 | 0.612 | 0.613 |
  | grey 160 nits | 2.000 | 1.000 | 0.835 | 0.836 |
  | grey 240 nits (SDR white) | 3.000 | 1.000 | 1.000 | 1.000 |
  | 50% grey at SDR white | 0.642 | 0.824 | 0.498 | 0.500 |
  | 50% grey at 1,000 nits | 2.675 | 1.000 | 0.949 | 0.951 |
  | BT.2020 green at SDR white | (-1.763, 3.399, -0.302) | (0, 1, 0) | (0, 1, 0) | (0, 1, 0) |

- **Following the monitor:** with one capture running, the window on the HDR monitor read 0.318 (should be 0.320), moved to the SDR monitor 0.537 (8-bit, as Windows shows it there), moved back 0.318; each switch logged, 84 frames in each 2.5 s (the pattern's own rate).
- **The shader against the reference** (`tonemap::tests::the_shader_converts_as_the_reference_does`, on Windows' software adapter, so it runs on any Windows without a GPU): every pattern value, the curve's edges, NaN and infinity, within 0.6 of a step; the area outside the frame untouched; a frame Windows made without the shader binding is copied first.
- **Cost:** the GPU's time per frame (`hdr-probe --bench`, RTX 3060 Ti, median of 200): 1080p copy 0.028 ms, conversion 0.031; 1440p 0.046 and 0.053; 4K 0.097 and 0.116.
- **SDR hosts unchanged** (`latency-probe --secs 30 --fps 60`, alternating, release build): clock to present p50 11.4 and 11.5 ms, p95 12.6 and 12.6 (new) against 11.6 and 11.4, 13.0 and 12.7 (`COUCH_HOST_HDR=0`); 59.8 to 60.0 fps, no decode errors.
- **Through NVENC:** the pattern streamed by `capture-test --psnr` at 10 Mbit/s: 264 frames in 8 s each run, PSNR p50 58.9 dB with the conversion and without, SSIM 1.000.
- **The setting:** the scenario `the_hdr_setting_reaches_the_next_streams_capture` (on by default; turned off, the next stream's capture asks for none; kept for the profile); `store`'s `hdr_conversion_is_on_unless_turned_off` (no file, an older file without the field, a damaged one: on); the front end's type check and unit tests; `npm run shots` (the new `settings-streaming` shot in every look and size, no problems, nothing jumps; Linux's Settings has no Streaming).
- **Windows:** fmt and clippy clean, the whole suite passing. **Linux (WSL):** fmt, clippy and the whole suite passing (before the reviews' fixes, which touched only Windows' crates and Settings' text).
- `check-docs.sh`: nothing stale.
- **Not run:** the rate-control gates, since nothing of rate control, the encoders or the SDR capture path changed (G1 passed in each latency run above); the input-safety live checks, since nothing of input, seats or teardown changed.

## Found while checking

- **`monitor::tests::the_primary_monitor_has_an_area_and_strangers_fall_back_to_it` failed with the virtual display installed:** Windows answered a made-up monitor handle with the virtual monitor's area, not the primary's (it answers stale handles too, as the code already said). The test now checks what holds: some monitor's area, never none.
- **Edge signed a throwaway profile into the author's Microsoft account** (`--user-data-dir` alone; its sync dialog appeared). The profile was deleted; later runs used `--inprivate`.
- **A still Edge page gave Windows.Graphics.Capture no first frame** (an animated one did), and the host's capture refuses tool windows: the pattern window is an ordinary one, presenting continuously. Both in the `windows-ffi-pitfalls` skill.

## The review

A fresh review agent over the whole change found nothing high, two medium findings and eight low ones; each was checked against the code.

- **Medium: a conversion that failed turned GPU frames off for every viewer, then failed the same way in memory**, so an HDR host whose GPU refused the shader or a view would have streamed nothing where 8-bit worked. Now a failed conversion warns once and switches that capture to 8-bit for good (`Handler::give_up_hdr`), never touching `gpu_failed`; the watcher stops. **Checking the fix found a deadlock in it:** the switch took the capture session's lock from inside the frame's delivery, which holds it, and the probe hung. The wanted format now lives outside that lock (`wgc::FormatSwitch` is an atomic), and the frame handler reads it again after delivering, so a still window is switched too. With every conversion made to fail (a temporary edit), `hdr-probe --pattern` showed one warning per capture, frames at full rate in 8-bit on both paths, and no hang.
- **Medium: a monitor in HDR whose SDR white Windows doesn't give was converted at a guessed white** (1.0), which would make every SDR window too bright, where 8-bit gets them right. Now it stays 8-bit, and the log says why.
- **Low, fixed:** a moment where the monitor can't be found no longer drops the capture to 8-bit and back (the watcher keeps its last choice); the converter's cached views no longer keep old frame textures alive (dropped on a new size, when frames stop being HDR, and when the memory path replaces its texture); the draw unbinds the stages it doesn't use, so "sets every state it uses" holds; comments on the white's ordering and the lock reworded to what the code does; `f32_to_half`'s doc says how it rounds (test pictures only); Settings' text says what Windows' capture really does (light greys and whites turn white too, not only what is brighter than white); the probe no longer underflows on a tiny monitor.
- **Low, left as it is:** the frame that asks for another format is delivered in the old one, so a still window that turns HDR on keeps its 8-bit frame until its content changes (below, Still to come); the white changes with the watcher's one-second look, so a window moved between monitors shows the wrong white for up to a second.
- Removing "strict control mode" from the Ideas row was the author's word in the handoff (2026-11-12), not part of M29.

A **second fresh review** went over the fixes: nothing high or medium; the locks sound (no sink call reaches the session's lock, the watcher holds nothing the frame thread waits on, drop order right), the white's ordering as the comment says, no view cache freeing anything NVENC holds, and the docs true. Four low holes, three fixed:
- the format switch is made before the session starts and handed to it and the frame handler (`wgc::Session::start_switched`), so a frame that fails to convert switches from the very first one, with no hand-over after the start (on a still monitor that hand-over could have waited for a frame never sent);
- a dropped FP16 frame asks for 8-bit again, so a watcher that asked for FP16 just as converting gave up can't leave the capture dropping every frame;
- a moment where the SDR white can't be read (the display configuration changing) no longer drops the capture to 8-bit and back;
- left: any error on an HDR frame gives converting up, not only the converter's own (a staging or pool failure too). 8-bit then still has the usual fallback to frames in memory, so nothing is lost but the conversion.

With every conversion made to fail again, and normally, `hdr-probe --pattern` behaved as before; fmt, clippy and the whole suite pass.

## Still to come

- **The friend's run with a real HDR monitor:** a test installer of 0.34.0 for them to host the same game as before, the author watching; their `desktop.log` names the monitor's HDR state and what the capture did. Also worth knowing from them: Windows version, GPU, and whether the game ran in HDR, Auto HDR or RTX HDR.
- **A still window when HDR is turned on:** its last frame was captured in 8-bit, and Windows sends no new one until the window changes, so its first converted frame waits for that (a game always changes).
- **A roll-off for highlights**, behind a switch, if a real game in HDR shows clipped highlights matter.
- **HDR end to end** (10-bit HEVC with PQ to viewers whose screens do HDR, the stream window presenting HDR): a milestone of its own, needing HDR screens on both ends to test.
- A Linux host's HDR (PipeWire's is young): not planned.
