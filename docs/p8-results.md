# P8 results: picture quality

## Why

Your Witcher 3 runs in M12c and with friends looked soft. M12c's notes named two candidates: a sharper scaler in the stream window, and NVENC's quality settings (a slower preset, spatial AQ, multipass). P8 tried both, each measured against itself turned off on the same build (the `performance-checks` skill).

## The stream window's scaler

The stream window drew the picture with one bilinear read per pixel. Two others were tried:
- **Bicubic** (Catmull-Rom) on luma, in 9 bilinear reads per pixel; chroma stays bilinear (it is half the size, and the eye barely sees it).
- **AMD's FSR 1** (EASU to upscale, RCAS to sharpen), ported to WGSL: two more passes, through an RGB picture.

Each by `COUCH_VIEWER_SCALER` (`fsr` went with FSR); the stats panel's decoder line names the one running ("…, bicubic scaling, …").

**Their cost** (a 1280x720 test pattern into a 1920x1080 window, arrive to present, p50, two runs each, alternating):

| Scaler | p50 | fps |
|---|---|---|
| bilinear | 0.88 / 0.89 ms | 60 |
| bicubic | 0.88 / 0.87 ms | 60 |
| FSR 1 | 1.01 / 1.01 ms | 60 |

**By eye** (2026-10-20, your run): Witcher 3 in a 1280x800 window, streamed into a full-screen stream window (about 1620x1011 of it), switching scaler every 20 s with beeps to tell them apart. "It is hard to tell the difference and all 3 looked kind of blurry, I think bicubic was the best from what I could tell, especially for text."

**Kept:** bicubic, the default; bilinear behind `COUCH_VIEWER_SCALER=bilinear`. **FSR 1 dropped** (your choice): it cost more and looked no better, and it would have added AMD's code and notice to every build.

## NVENC's quality settings

Measured with `capture-test --psnr` (now with SSIM too) on FFmpeg's moving Mandelbrot at 1280x720, 10 Mbit/s, 10 s per setting, two passes. Luma PSNR and SSIM, p5 / p50; encode time p50 / p95. The two passes agree within run-to-run noise, so one pass is shown:

| Codec | Setting | PSNR (dB) | SSIM | Encode (ms) | Mbit/s |
|---|---|---|---|---|---|
| H.264 | today (p1) | 29.05 / 34.65 | 0.945 / 0.975 | 3.4 / 4.9 | 8.0 |
| H.264 | p4 | 29.47 / 34.90 | 0.950 / 0.976 | 4.0 / 5.2 | 7.9 |
| H.264 | p7 | 29.63 / 34.89 | 0.952 / 0.977 | 5.5 / 8.3 | 8.5 |
| H.264 | spatial AQ | 29.63 / 34.62 | 0.954 / 0.977 | 3.5 / 4.6 | 8.0 |
| H.264 | multipass 1 | 29.92 / 35.97 | 0.955 / 0.981 | 3.7 / 5.1 | 8.3 |
| HEVC | today (p1) | 29.82 / 35.50 | 0.955 / 0.979 | 3.2 / 4.7 | 7.0 |
| HEVC | p4 | 30.37 / 36.49 | 0.962 / 0.983 | 4.3 / 6.1 | 6.8 |
| HEVC | p7 | 30.27 / 36.58 | 0.961 / 0.983 | 4.8 / 6.5 | 7.0 |
| HEVC | spatial AQ | 29.56 / 35.28 | 0.956 / 0.980 | 3.3 / 5.0 | 6.9 |
| HEVC | multipass 1 | 29.07 / 35.94 | 0.950 / 0.981 | 3.5 / 5.0 | 7.9 |

Two candidates stood out, H.264 multipass 1 (+1.1 to 1.3 dB) and HEVC p4 (+0.9 to 1.0 dB). p7 gains no more than p4 for more time; spatial AQ moves SSIM a little and PSNR not at all; HEVC multipass spends 12% more bits for its gain.

**Their cost:**
- **H.264 multipass 1:** latency unchanged on one viewer (`latency-probe`); 7 viewers of the screen (`viewers.ps1 -Viewers 7 -Screen -Clock`) held 60 fps, capture to present p50 9 → 11.5 ms; `gates --quick` all passed (G1 11.6 / 12.2 ms, G3 p95 78.5 ms, G4 93 ms, G5 0.33 s, G7).
- **HEVC p4:** 4 viewers held 60 fps, p50 8.3 → 10.9 ms; 7 viewers fell from 60 fps (p50 9.6 ms) to 48 fps (p50 32 ms, p95 53 ms): the encoder ran out of time.

**Kept:** neither (your choice). HEVC p4 breaks a full room; H.264 multipass costs every viewer 2.5 ms for a gain only H.264 viewers see (HEVC goes to every viewer that decodes it). The option sets are unchanged.

**Kept for measuring,** applied over whichever option set opens, and named in the caps line ("measuring …"):
- `COUCH_NVENC_PRESET=p1..p7`;
- `COUCH_NVENC_SPATIAL_AQ=1`, with `COUCH_NVENC_AQ_STRENGTH=1..15`;
- `COUCH_NVENC_MULTIPASS=0..2` (none, a quarter, full resolution).

## What softens the picture, then

Not the scaler, nor the encoder's settings, as much as:
- **the upscale:** a game window smaller than the viewer's screen is stretched to fit. Running the game borderless at the screen's own resolution gives the stream the most pixels;
- **the bitrate:** every stream starts at 10 Mbit/s (or the quality setting, if lower) and climbs as the path allows, up to the quality setting in the host panel. 20 Mbit/s is recommended; 30 or 50 on a fast connection. M12c found 50 Mbit/s against 20 the largest difference of all.

The `app-manual-run` skill now says so.

## Also in P8

- **`capture-test --psnr` prints SSIM** (luma, 8x8 blocks): PSNR alone rewarded settings that smooth detail away.
- **The stream window's shader is validated in the tests** (wgpu's own `naga`, a dev-dependency), so a WGSL mistake fails `cargo test` rather than a window.
- **M12a and M12b, by probing** (in `docs/architecture.md`): Witcher 3 (DirectX 11) and Left 4 Dead 2 (DirectX 9) in exclusive full screen were both captured as a window and as the monitor on Windows 11, so M12b's Desktop Duplication goes to the bottom, waiting for a report of a black stream. M12a is split: pacing for hosts with frames in memory can be built and measured here; the rest waits for AMD and Intel hardware.

## Code review

A review found the shader's weights and edges right (they sum to 1, a 1:1 picture comes out exact, the edges clamp), every encoder still opening through `avcodec_open2`, and no FSR left. Fixed from it:
- **A switch's value went to FFmpeg as typed:** `COUCH_NVENC_PRESET=p04` passed the check and would have failed every NVENC open, sending the measurement to another encoder. The number read goes now; a strength without spatial AQ says it's ignored.
- **The bicubic path also made a bilinear read** it then threw away; the read is now one or the other.
- `capture-test` shows the encoder's options (and so "measuring …") on its first line; the scaler's switch ignores case; `naga` is a workspace dependency beside wgpu, so the two move together.
- The 9-read technique, a well-known one, is credited in a comment where it's used (Matt Pettineo's description).

## Tests

- `couch-viewer`: the video shader parses and validates, with every entry point a pipeline asks for.
- `couch-cli`: SSIM of a picture against itself is 1; noise lowers both scores (PSNR 39.10 dB for the test's noise).
- `cargo test --workspace`, clippy, fmt, `cargo xtask notices`.

## This PC (2026-10-19/20, release build)

The tables above. One session scenario failed once while an engine was starting (a port), and then passed ten runs in a row; nothing in P8 touches it.
