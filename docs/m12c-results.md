# M12c results: HEVC

## Why

The internet test with a friend after M9f showed that bitrate is the limit: about 20 Mbit/s most of the time, dipping to 8–12, and at 120 fps each frame gets half the bits it would at 60. HEVC gives a better picture than H.264 at the same bitrate, and NVIDIA's GPUs encode and decode it in hardware.

## What changed

- **Each viewer gets its own codec, chosen when it joins** (`pipeline::host::choose_codec`):
  - HEVC if its `SessionJoin` says it decodes HEVC, the host is NVIDIA and gives NVENC GPU frames, and `COUCH_HOST_CODEC` isn't `h264`;
  - otherwise H.264, as before. AMD and Intel hosts stay on H.264 until their hardware can be tested (M12a).
- **A viewer says what it decodes:** `SessionJoin.codecs`, a bitmask. HEVC is set when the GPU's D3D11 video device lists the HEVC Main decoder profile (`couch_platform_win::video_caps`), unless `COUCH_VIEWER_HEVC=0`.
- **`hevc_nvenc`** has NVENC's H.264 options plus the Main profile (8-bit). It takes the same D3D11 textures as `h264_nvenc`; the GPU test gives the same pictures as frames from memory.
- **If HEVC fails** to open or to encode, that viewer falls back to H.264, with a keyframe. The others keep their codec, and GPU frames stay on.
- **The wire:**
  - the epoch preamble's codec byte takes 2 for HEVC (it was always 1);
  - viewer IPC v8 carries the codec in each video frame;
  - `PROTOCOL_MINOR` 2.
- **The stream window** reopens its decoder when a keyframe brings another codec. Its stats line names the codec ("HEVC, D3D12VA (GPU frames)").
- **`couch-cli capture-test --codec h264|hevc --psnr`** measures picture quality: the luma PSNR of each decoded frame against the captured one.

## A bug in FFmpeg's D3D12VA: HEVC at 720p wouldn't decode

The first spike decoded HEVC with D3D11VA but not with D3D12VA, the viewer's zero-copy path (M9c): "Could not create the texture", then "hardware accelerator failed to decode picture".

- **Mapped by size:** heights with `h mod 32` from 1 to 16 failed (720, 900, 1070, 1200, 2160, 360); 1080 worked.
- **Found in FFmpeg's source:** D3D12VA's frame pool is the picture's exact size (`ff_d3d12va_common_frame_params`). D3D11VA's (`dxva2.c`) is aligned to 128 for HEVC. NVIDIA's driver refuses the exact sizes. FFmpeg 8.1-latest and 9.0 have the same code and fail the same way (tried, your choice).
- **Fix:** the decoder supplies its own pool, aligned to 128, in `get_format` (`aligned_d3d12_frames`). A decoded texture can now be larger than the picture, so `FrameImporter` copies the picture's extent.
- **Checked:** the `zero_copy` tests compare D3D12VA's frames with D3D11VA's byte for byte, for H.264 and HEVC at 1280x720, 1920x1080 and 1682x1070: 441 frames each, 0 bytes differ.

## Picture quality: H.264 against HEVC at the same bitrate

`capture-test --psnr` on a borderless ffplay window (1280x720, 60 fps), started afresh for each run, so both codecs see the same frames. One run each, 8 s; about 48 fps were captured (what ffplay drew). Luma PSNR, dB (higher is better; 3 dB is about half the noise); the bitrate actually sent in brackets.

| Picture | Asked | H.264 p50 (p5) | HEVC p50 (p5) |
|---|---|---|---|
| Mandelbrot zoom (detail, colour) | 3 Mbit/s | 30.6 (28.2) [2.7] | **31.6 (30.2)** [2.8] |
| | 5 | 32.2 (30.8) [4.1] | **32.9 (31.0)** [4.0] |
| | 10 | 35.0 (33.8) [8.3] | **35.5 (33.6)** [6.9] |
| | 20 | 39.5 (37.7) [16.0] | **39.7 (38.3)** [12.8] |
| Game of Life (fine, random detail) | 3 | 24.3 (22.1) [3.1] | 24.2 (22.1) [3.0] |
| | 5 | 24.8 (22.4) [4.3] | 24.9 (22.3) [4.2] |
| | 10 | 26.1 (23.1) [8.2] | **26.9 (23.3)** [7.7] |
| | 20 | 29.2 (24.7) [16.3] | **31.9 (25.5)** [13.9] |
| testsrc2 (moving shapes, gradients) | 3 | 39.3 (38.8) [2.4] | **39.9 (39.7)** [2.2] |
| | 5 | 41.8 (41.0) [3.9] | **43.1 (42.0)** [3.5] |
| | 10 | 46.6 (45.6) [7.8] | **49.6 (48.6)** [7.1] |
| | 20 | 53.4 (52.5) [16.0] | **56.8 (55.3)** [15.1] |

- **HEVC is as good or better everywhere, and usually with fewer bits.** On the Mandelbrot at 20 Mbit/s it matches H.264's picture with 20% fewer bits; on testsrc2 it is 3 dB better at 10 Mbit/s with 9% fewer.
- **The gain is smaller than HEVC's usual 30–40%,** as expected with NVENC's fastest preset (p1) and no lookahead or B-frames: the low-latency settings leave HEVC's larger blocks and better prediction less room. Random detail (Life) gains least at low bitrates; both codecs are starved there.
- A game is the real test (your run, below).

**Measuring PSNR needs NVENC's colour matrix.** The first PSNR run read about 20 dB at every bitrate: FFmpeg's NVENC wrapper converts RGB with BT.601, whatever colorspace we set (`nvenc.c`), and the measure assumed BT.709. `capture-test` now uses the matrix the stream declares (below). A Mandelbrot zoom left running between runs also skewed it (each run saw a deeper zoom, slower to draw), hence the restart per run.

## Latency and load: no change

Alternating runs on one release build, HEVC (the default) against `COUCH_HOST_CODEC=h264`.

**One viewer, `latency-probe --secs 20` (the clock, 1088x184), two rounds:**

| | HEVC | H.264 |
|---|---|---|
| 60 fps, clock to present p50 / p95 | 11.0 / 15.1, 11.0 / 15.1 ms | 11.6 / 15.8, 11.4 / 12.5 ms |
| 120 fps | 10.6 / 14.1, 10.6 / 11.3 ms | 10.8 / 15.1, 10.8 / 11.4 ms |
| Maximum | 35–41 ms | 23–24 ms |

HEVC's maximum is higher: the clock's bitrate climbs in steps, each an encoder reset with a keyframe (FFmpeg's NVENC wrapper, M1), and HEVC's keyframes take a few ms more to decode on this picture. It doesn't show at p95.

**Four viewers of the screen with the clock below (`viewers.ps1 -Screen -Clock`, 20 s, 1080p):**

| | HEVC | H.264 |
|---|---|---|
| 60 fps: fps, capture to present p50 / p95 | 60.0–60.1, 8.0–8.3 / 10.8–11.0 ms | 60.1, 7.6–8.0 / 10.0–10.2 ms |
| 120 fps | 119.3–119.5, 4.9–5.7 / 6.7–6.8 ms | 119.9–120.0, 5.0–6.0 / 6.8–7.0 ms |
| Slowest encode (60 / 120 fps) | 10.9 / 5.9 ms | 9.7 / 6.5 ms |

Within noise. NVENC took four HEVC sessions with no complaint.

## Gates

| | HEVC (clock) | H.264 (`COUCH_HOST_CODEC=h264`) |
|---|---|---|
| G1 p50 / p95 | 11.1 / 11.7 ms PASS | 11.5 / 12.3 ms PASS |
| G3 p95, mean bitrate | 37.3 ms, 19.5 Mbit/s PASS | 78.1 ms, 19.4 Mbit/s PASS |
| G4 latency back, keyframes | 90 ms, 1 PASS | 91 ms, 1 PASS |
| G5 settled, latency back | never, 0.01 s **FAIL** | 0.36 s, 0.56 s PASS |
| G7 second viewer p95 | 11.9 ms PASS | 12.8 ms PASS |

**G5's failure tests nothing.** On the latency clock, HEVC needs only about 0.3 Mbit/s whatever the target, while H.264 fills its target (10 to 20 Mbit/s). So the 5 Mbit/s throttle never touched HEVC's stream, and the controller, never short of room, kept its target at 20 Mbit/s: "settled" can't happen. G3's and G7's numbers with HEVC are low for the same reason. The gates check rate control with H.264, which passes as in M9f; `lifecycle-check` passes with HEVC (all six scenarios).

**What this means for real streams.** HEVC on a simple picture (a menu, a still scene) sends far less than its target, and the controller's target climbs to the maximum without the link being tested. When the picture gets busy, the encoder jumps to that target at once; on a slower link, the controller then backs off as in G5 (H.264: settled in 0.36 s, the worst frame 331 ms). A game in motion fills its target with either codec, as H.264 always did. Holding the target near what was actually delivered while the encoder sends less would avoid the jump: a candidate for later, if your runs with friends show stalls when a scene starts.

## Found on the way, and fixed: the stream's colours

NVENC converts RGB to YUV with BT.601, but the stream window's shader converted back with BT.709 (`couch-viewer`'s `gpu.rs`). So every NVENC stream since M1, H.264 included, showed saturated colours slightly off. Worked through the two matrices: pure red came out as (255, 25, 0), a little orange; pure green as (0, 215, 0), darker; pure blue as (0, 15, 255).

**Fixed (your choice, in M12c):** the decoder reads the matrix each picture's stream declares (`YuvMatrix` on every decoded frame: BT.470BG or SMPTE 170M is BT.601, anything else BT.709 as before), and the stream window draws with that matrix's shader. NVENC declares BT.601; our converter for the other encoders writes BT.709 and declares it. Tests: pure red through each encoder gives BT.601's luma and declaration for `h264_nvenc` and `hevc_nvenc`, BT.709's for openh264 (`roundtrip.rs`); D3D12VA's frames carry the same matrix as D3D11VA's (`zero_copy`). `capture-test --psnr` uses the declared matrix too.

## Code review

A review found no correctness, FFI or safety defects. Fixed from it:
- **HEVC went out with frames in memory too** (a test pattern, `COUCH_HOST_ZERO_COPY=0`), against the design. Now the encode loop doesn't try HEVC without GPU frames: H.264 instead, so an AMD or Intel host doesn't try a `hevc_nvenc` open per viewer, and `viewers.ps1`'s test pattern stays comparable with the M9 references.
- **`gates` sets `COUCH_HOST_CODEC=h264` for its host itself** (`latency_probe::Args::h264`), so it no longer fails G5 on an NVIDIA PC unless the switch is set by hand.
- **Three scenarios for the untested paths:** no HEVC with frames in memory; HEVC that opens but can't read the GPU frames gives H.264; HEVC failing mid-stream gives H.264 from a keyframe, and the viewer keeps watching.
- **The HEVC check runs at start on a thread of its own,** so the first `SessionJoin` doesn't make the room actor wait for a D3D11 device.
- **`couch-cli host`** takes its viewers' codecs from its own GPU (`view` doesn't say); the comment and CLAUDE.md now say to set the switches on the host for it.
- **The probe's "H.264 hardware encode"** line no longer considers `hevc_nvenc`.

Noted, not from M12c: `watching_resumes_when_the_connection_to_the_host_comes_back` timed out once in a full workspace run under load (the sessions suite took 21 s instead of 4); it passed in every other run.

## Tests

- `cargo test --workspace`, clippy: pass.
- Session scenarios (`--test sessions`): each viewer gets the best codec it decodes (one HEVC, one H.264, in one session); a failed HEVC open falls back to H.264.
- By hand: `gpu_input` (H.264 and HEVC, GPU frames against memory), `roundtrip` with `hevc_nvenc`, `open.rs` (every encoder of both codecs, twice), `zero_copy` (above), `video_caps` (this PC: HEVC decode yes).

## Your run (2026-10-15, Witcher 3, watched from alice on this PC)

- **The codec:** alice's decoder line said "HEVC, D3D12VA (GPU frames)", and H.264 with `COUCH_HOST_CODEC=h264`.
- **The colours** now match the game.
- **About 80 fps** at 120, within the 80 to 100 seen under Witcher 3 since M9f.
- **HEVC looked a little less blurry than H.264** at 20 Mbit/s.
- **Bitrate matters most:** 120 fps at 50 Mbit/s looked noticeably better than at 20; 60 fps at 20 a little better than 120 at 20 (twice the bits per frame).
- **"Still far from the source" was mostly scaling.** The game ran in a 1368x788 window, so the stream was 1368x788 too, and alice's larger stream window scaled it up with bilinear filtering. HEVC sent only about 35 of the 50 Mbit/s allowed at that size: it doesn't pad to its target as H.264 does. With the game at 1920x1080 (borderless window, and full screen) and the stream window full screen over it, 1:1, still frames looked extremely close to the source. Motion couldn't be compared: with one monitor, the game is behind the stream window.
- **`capture-test --monitor --psnr` at 10 Mbit/s,** the game on screen:

  | | HEVC | H.264 |
  |---|---|---|
  | PSNR p50 / p5 | 35.5 / **32.3** dB | 36.0 / 31.3 dB |
  | Bitrate sent | **3.0 Mbit/s** | 3.7 Mbit/s |

  The same picture with about 20% fewer bits, as on the test pictures. Only about 22 frames a second came through: `capture-test` captures, encodes, decodes and measures on one thread, through memory, and under the game each frame took about 25 ms (both codecs alike).

**Candidates for later, from this run:**
- a sharper scaler in the stream window for a stream smaller than the window (bilinear today);
- NVENC's quality settings: a slower preset than p1 and spatial adaptive quantization, each measured against its encode time with a switch;
- a friend's run with an installer: their stats say HEVC, and the dips to 8–12 Mbit/s look less soft.
