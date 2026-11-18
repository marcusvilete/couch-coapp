# M12d results: NVENC directly (part 1)

## Why

Since M1, every bitrate change has cost a keyframe: FFmpeg 8.1's NVENC wrapper reconfigures with `resetEncoder` and `forceIDR` on any change of the bitrate (`libavcodec/nvenc.c`, `reconfig_encoder`, our pin n8.1.3). So the rate controller batches raises into steps of at least 15%, at most one a second, and every cut costs one too. M12d calls NVENC itself ([ADR 0016](adr/0016-nvenc-directly-and-continuation-epochs.md)).

**Your choices (2026-11-18):**
- measure first what a keyframe costs, before building anything;
- reference frame invalidation in M12d's scope, with its wire revision;
- Windows hosts' GPU frames only (FFmpeg keeps frames in memory, AMD, Intel, software and Linux);
- two pull requests: this one, the encoder; then a spike on it, then the continuation epochs (part 2);
- the spike's bar for part 2: any clear gain over a keyframe, with no decoder error;
- a stream window that falls behind decoding asks to continue rather than for a keyframe (part 2).

Version **0.41.0**, no wire change: it talks to 0.38.x, 0.39.x and 0.40.0.

## What a keyframe costs

`couch-cli capture-test` gained events (`--keyframe-every`, `--bitrate-every` with `--bitrate-to`): each frame's luma PSNR, SSIM and size against what was captured, the 10 frames after each event against the 10 before, and the same at points halfway between events, where nothing happens (the control). `--frames-csv` writes every frame.

**Witcher 3** (your run, 1282×752, NVENC fed from memory, about 30 fps since `capture-test` captures, encodes, decodes and measures in turn):

| | On the keyframe | Within 0.5 dB from | Keyframe size |
|---|---|---|---|
| Forced keyframe, H.264, 20 Mbit/s | −5.15 dB, SSIM −0.098 | frame +8 | 1.00× |
| Forced keyframe, HEVC, 20 Mbit/s | −4.38 dB, SSIM −0.076 | not by +9 (−0.66) | 1.07× |
| Bitrate step 10↔12, H.264 | −4.73 dB, SSIM −0.144 | frame +8 | 1.04× |
| Bitrate step 10↔12, HEVC | −4.72 dB, SSIM −0.131 | frame +7 | 1.43× |

The control points moved by +0.3 to +0.6 dB, so if anything the dips are understated. On test pictures (`ffplay`'s mandelbrot, full and scaled up, and `testsrc2`) the dip was 4 to 8 dB, and a bitrate step's keyframe 2.5 to 3.6 times a frame's budget on mandelbrot (the reset forgets the VBV), but 1.0 to 1.4 in the game. **The cost is the picture:** a soft frame healing over a fifth of a second or so, about once a second while the bitrate climbs, and at every cut.

## What changed

- **`couch-codec-nvenc`**, the only crate that calls NVENC directly (Windows only): it loads `nvEncodeAPI64.dll` from System32 only, checks the driver offers API 13.0, and opens a session on the capture's D3D11 device, registering the pool's BGRA textures as FFmpeg does. Its names in logs: `h264_nvenc_direct`, `hevc_nvenc_direct`.
- **FFmpeg's stream, field by field** (`encoder::configure`): p1, ultra-low-latency tuning, CBR with a one-frame VBV, an infinite GOP, no B-frames, forced IDR, the VUI FFmpeg writes for RGB input (BT.601 matrix), and FFmpeg's own defaults that our options never named (below).
- **A bitrate change reconfigures in place** (`resetEncoder` and `forceIDR` 0) where the driver supports it (`NV_ENC_CAPS_SUPPORT_DYN_BITRATE_CHANGE`; without it ours doesn't open, and FFmpeg's keeps its opening bitrate, as before), so its caps say a change costs no keyframe and the rate controller applies raises above 3% at once; the host reads the caps every tick, so a viewer that falls back to FFmpeg's gets batched raises again.
- **The engine** (`pipeline::platform`): on GPU frames, NVENC directly first, then FFmpeg's `h264_nvenc`/`hevc_nvenc` if it can't open. If it fails to encode, FFmpeg's NVENC takes that viewer's GPU frames from a keyframe (`DirectNvenc`); GPU frames stay on for the others. `COUCH_HOST_NVENC_DIRECT=0` keeps FFmpeg's, to measure against. The host's own stream window (M27d) and HEVC (M12c) take the same path.
- **The header and bindings:** nv-codec-headers' `sdk/13.0` at the commit FFmpeg's pinned build uses, vendored in `third_party/nv-codec-headers/` (MIT, in the notices); `cargo xtask gen-nvenc-bindings` generates `crates/couch-codec-nvenc/bindings/`, which xtask's test ties to the header's SHA-256.
- **`couch-cli probe`** says whether NVENC directly opens and changes the bitrate in place; **`capture-test --gpu`** encodes GPU frames with it (`--ffmpeg-nvenc` with FFmpeg's).

## Checked

- **The same stream** (`cargo test -p couch-codec-nvenc --features gpu-tests --test gpu`, this PC's NVIDIA GPU): on the same D3D11 frames, the parameter sets match FFmpeg's byte for byte and every decoded picture is identical (PSNR ∞), H.264 and HEVC, 60 frames each, barcodes read in all 60.
- **No keyframe for a bitrate change,** up or down, and the frames follow: H.264 4.0 → 8.0 → 2.0 Mbit/s, HEVC 1.7 → 2.7 → 1.2 (a 720p60 test pattern: below about 2 Mbit/s in H.264 and 1.2 in HEVC NVENC overshoots its target there, either way).
- **Textures come back to their pool** over 150 frames, a frame from another pool is refused, and an encoder opened for the new pool works.
- **A bitrate step on GPU frames** (`capture-test --gpu --bitrate-every 1 --bitrate-to 12 --bitrate-mbps 10`, 30 s each):

  | Source, codec | NVENC directly | FFmpeg's NVENC |
  |---|---|---|
  | mandelbrot scaled up, H.264 | 0 keyframes of 30 steps; −0.12 dB; 1.02× | 29 of 29; −1.23 dB at +1; 2.53× |
  | mandelbrot scaled up, HEVC | 0 of 30; −0.10 dB; 0.93× | 29 of 29; −0.53 dB; 3.22× |
  | testsrc2, H.264 | 0 of 30; within noise; 1.02× | 29 of 29; −8.48 dB; 1.11× |
  | testsrc2, HEVC | 0 of 29; −0.15 dB; 0.99× | 30 of 30; −8.97 dB; 1.42× |

  The direct encoder's dips are within what the control points move.
- **`couch-cli probe`:** "NVENC directly (M12d): encodes captured frames, bitrate changes in place".
- **The gates** (`couch-cli gates --quick`, H.264, the hosts' logs naming the encoder), NVENC directly against `COUCH_HOST_NVENC_DIRECT=0` on the same build, one run each: every gate passes with both.

  | | NVENC directly | FFmpeg's NVENC |
  |---|---|---|
  | G1 clock→present p50 / p95 | 11.5 / 13.1 ms | 11.6 / 12.6 ms |
  | G3 p95, mean bitrate, catch-ups | 79.6 ms, 19.6 Mbit/s, 0 | 79.2 ms, 19.5 Mbit/s, 0 |
  | G4 latency back, recovery keyframes | 57 ms, 1 | 74 ms, 1 |
  | G5 settled, latency back, worst frame | 0.35 s, 0.50 s, 293 ms | 0.38 s, 1.11 s, 121 ms |
  | G7 second viewer p95, bitrate | 12.9 ms, 20.0 Mbit/s | 12.9 ms, 20.0 Mbit/s |

  G5's worst frame (one frame, in one run each) is within the 100 to 420 ms M1 recorded for it; its latency came back sooner with ours.

  **After the review's fixes,** two more runs with ours passed every gate, but G3 read p95 108.9 and 108.4 ms (17.3 to 18.0 Mbit/s) and G1 a little higher (14.5 and 12.8 ms at p95). FFmpeg's NVENC, run right after as the control, read the same: G3 p95 108.5 ms at 17.5 Mbit/s, G1 15.0 ms. The PC, not the encoder: P14 met G3 at about 108 ms between runs at about 79 the same way ([p14-results](p14-results.md)).

## Found while checking

- **FFmpeg's defaults are part of the stream.** Our options name p1, `ull`, CBR and so on, but FFmpeg also sets things no option of ours names: `h264_nvenc` and `hevc_nvenc` default `refs` to 0 (the driver chose 3 reference frames here, not the 1 AVCodecContext's own default would give), `h264_nvenc`'s `s12m_tc` is on (so `enableTimeCode`, which sets the HRD's `time_offset_length`), each picture repeats the slice mode, and only H.264 gets the VUI's `bitstreamRestrictionFlag`. Found by comparing the two encoders' headers with `ffmpeg -bsf:v trace_headers` (`COUCH_NVENC_DUMP` in the GPU test).
- **NVENC's rate floor:** at 720p60 on a busy test picture, H.264 overshoots below about 2 Mbit/s, HEVC below about 1.2, with either encoder.
- **NVENC's level goes by picture size and rate, not bitrate** (the review's question): 3.1 for 720p30 and 4.1 for 1080p60 whether a session opens at 2 or 30 Mbit/s, below what H.264 High 3.1 allows at 20 Mbit/s. The same with FFmpeg's encoder, which our viewers have always decoded.
- **FFmpeg's D3D11 NVENC takes no device lock** around its calls (ADR 0016's open question): ours takes none either.

## The review

A fresh review agent read the whole diff (no high-severity findings; it confirmed the device reference released once on every path, the textures outliving their registrations, the boxed configuration's pointer surviving a reconfigure, map and unmap paired, the host's HEVC and GPU-frames fallbacks unchanged). What it found, checked against the code:

- **After a fallback, every raise would have cost a keyframe** (medium, held): the host read `bitrate_change_costs_keyframe` once, when it opened the encoder, so a viewer moved to FFmpeg's NVENC mid-stream kept unbatched raises, each a reset. Fixed: the host gives the controller the encoder's current caps every tick (`RateController::set_keyframe_costly`, with a test); `DirectNvenc`'s fallback now has a GPU test of its own (`direct_nvenc_falls_back_to_ffmpegs`, ignored without NVIDIA: a keyframe, FFmpeg's caps, the last bitrate asked for).
- **The level is chosen at the opening bitrate** (medium, checked, no change): a GPU test now opens at 2 Mbit/s, raises to 20 (H.264, 720p30) and 30 (HEVC, 1080p60), and compares a later keyframe's level with a session opened at the high rate. NVENC chooses the level by size and rate alone (3.1 and 4.1 at either bitrate), so the in-place raise is accepted and the stream is labelled as FFmpeg's always was.
- **The path without dynamic bitrate wasn't FFmpeg's** (low, held): FFmpeg never changes the bitrate there; ours reset with a keyframe, untried. Now ours doesn't open on such a driver, and FFmpeg's does, as before M12d.
- **The measuring switches** (`COUCH_NVENC_PRESET` and the rest) were silently ignored by ours (low, held): the host now warns when one is set while NVENC directly encodes.
- **The crate boundary** (low, held): CLAUDE.md now names `couch-codec-nvenc` as the second exception to "only `couch-platform-win` calls Windows APIs" (the DLL load and one `Release`), and the `windows-ffi-pitfalls` skill's description names the crate.
- **"System32 only"** (low, held as a comment): a module of the same name already loaded is reused whatever the flags; only FFmpeg's NVENC loads it besides us, and it would run that DLL anyway. Said so where it loads.
- **`capture-test`** (low, held): the "within 0.5 dB from" line now counts from the frame it stays within, events a stall made late are skipped rather than fired back to back, and `--bitrate-to` equal to `--bitrate-mbps` is refused. The numbers above came before and don't change.
- **`reduced()`'s comment** (low, not held): FFmpeg's `compute_dar` does call `av_reduce`.

## Still to come

- **Part 2, continuation epochs** (ADR 0016): first the spike on this encoder (long-term reference frames at p1, invalidation, FFmpeg's decoders following, the dip against a keyframe's in Witcher 3), then the wire revision.
- **A real game through the app,** with a friend on a real network: watch the stream while the bitrate climbs (no more soft pulses), and the host's log naming `h264_nvenc_direct`.
- **The measuring switches** (`COUCH_NVENC_PRESET`, `COUCH_NVENC_SPATIAL_AQ`, `COUCH_NVENC_MULTIPASS`) still apply to FFmpeg's NVENC only.
