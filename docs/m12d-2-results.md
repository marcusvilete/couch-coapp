# M12d results: continuing epochs (part 2)

## Why

Every recovery starts a new epoch with a keyframe: a catch-up (the viewer more than max(150 ms, 2 × the minimum round trip) behind), a resume after an outage, a stream window that fell behind decoding. Part 1 measured what a keyframe costs in Witcher 3: about 5 dB of PSNR, healing over 7 to 10 frames ([part 1](m12d-results.md)). Our video crosses reliable streams, so the host knows exactly which frames a viewer received ([ADR 0016](adr/0016-nvenc-directly-and-continuation-epochs.md)): it can tell NVENC to stop referring to the rest and carry on from a frame the viewer holds, with long-term reference frames to reach back further than the short-term references do.

**Your choices (2026-11-18):**
- a spike first, on part 1's encoder; its bar: any clear gain over a keyframe, with no decoder error;
- a stream window that falls behind decoding asks to continue rather than for a keyframe;
- after the spike: **continuations in HEVC only** (H.264 gained too little to be worth it); H.264 can be turned on later through the same per-codec bits.

Version **0.42.0**, wire revision 22: it talks to no earlier version.

## The spike

**NVENC does it at p1.** `couch-codec-nvenc` gained long-term frames (`Continuation`, per-picture marks), invalidation and a per-frame report; `tests/continuation.rs` drops runs of 20, 2 and 30 frames from one decoder, invalidates them in the encoder, goes on, and compares with a decoder given every frame. NVENC offers up to 8 long-term frames in H.264 and 7 in HEVC and marks every frame asked; after a long drop it codes a P frame predicting from the long-term frame the viewer holds (finding it alone: naming it changes nothing), and without long-term frames an IDR; after 2 frames it predicts from a short-term frame.

**The decoders follow, bit for bit.** From each continuation on, the pictures of the stream with frames left out are exactly those of the whole stream, with no error (our decoder with `AV_EF_EXPLODE`): software and D3D11VA in our decoder; software, D3D11VA and D3D12VA in FFmpeg's own (HEVC on D3D12VA at 768 lines: FFmpeg's CLI lacks our aligned frame pool, and NVIDIA's driver fails it at 720); software on Linux (WSL). Our decoder's own D3D12VA path (the stream window's default on NVIDIA) followed the gates' continuations with no decode error, but wasn't compared bit for bit; AMD's and Intel's HEVC decoders weren't tried. Also when the viewer decoded the first 4 frames past the point it continues from. VAAPI and CUDA on Linux weren't reachable (WSL has no `/dev/dri`), and Linux viewers decode H.264 only, so no continuation reaches them.

**Long-term frames take a reference away.** With `enableLTR` on, the driver's own choice predicts each frame from one short-term frame: H.264 lost about 1 dB on every frame (mandelbrot at 20 Mbit/s, PSNR p50 30.5 against 31.5; the marks' interval made no difference, LTR alone did). Asking for three references (its choice without LTR) gave it back (31.6); in HEVC, three references gained about 1 dB over part 1's stream (32.1 against 31.0).

**Worth it in HEVC.** `capture-test --gpu --recover-every 2 --drop-ms 300` leaves 300 ms of frames undecoded every 2 s, then recovers with a keyframe and a continuation in turn. Witcher 3 (your run, 1282×752, about 40 fps, 20 Mbit/s, 14 or 15 of each, points halfway between within 0.43 dB):

| after | +0 | +1 | +2 | +3 | +4 | dB lost over 10 frames |
|---|---|---|---|---|---|---|
| HEVC keyframe | −5.88 | −4.51 | −3.75 | −3.08 | −2.44 | 26.0 |
| HEVC continuation | −4.68 | −2.21 | −1.28 | −0.89 | −0.67 | 11.8 |
| H.264 keyframe | −4.90 | −4.09 | −3.21 | −2.47 | −1.83 | 19.9 |
| H.264 continuation | −5.36 | −3.08 | −2.18 | −1.66 | −1.35 | 18.1 |

None of the 28 continuations was coded intra. HEVC's loss was less than half a keyframe's; H.264's first frame was no better and its frames from +6 healed more slowly.

**How old a frame may be** (your runs, HEVC, `--recover-every 3`, 14 or 15 of each, controls within 0.26 dB): the game moves on while frames are lost, and past some age the old frame predicts worse than a fresh keyframe draws.

| frames lost | keyframe +0 | continuation +0 | keyframe, dB lost over 10 frames | continuation |
|---|---|---|---|---|
| 300 ms | −5.88 | −4.68 | 26.0 | 11.8 |
| 500 ms | −5.53 | −5.29 | 22.6 | 13.0 |
| 700 ms | −5.02 | −6.14 | 13.0 | 17.0 |

So the host continues only from a frame sent at most 500 ms before (`MAX_AGE`): catch-ups continue, and a resume after an outage (at least 500 ms of silence) mostly sends a keyframe, as before. On mandelbrot (a zoom, near the worst case for an older reference) both gained, SSIM's dip about halved.

## What changed

- **The wire** (revision 22, `docs/protocol.md` [Continuing an epoch](protocol.md#continuing-an-epoch-m12d-2)): the epoch preamble's version 1 names the frame an epoch continues from (`FrameRef { epoch, seq }`); `KeyframeReason` gains `FellBehind { holds }` and `CantContinue`; `SessionJoin.continues` says in which codecs a viewer accepts continuations (HEVC's bit alone). Viewer IPC v22: the continuation in a frame's header, the reason in a keyframe request.
- **The host's bookkeeping** (`couch_media::continuation::Continuity`, sans-IO, tested): the frames sent since the last keyframe (at most 600), which the viewer received (`couch_net::video`'s receipts: each report's `epoch_bytes_received` against where each frame ended), two long-term slots (the next mark only once the last was received, never over the one the viewer holds, at most every 100 ms), and what to invalidate to continue from a frame: those after it within 16 of the newest, and long-term ones.
- **The encoder:** `VideoEncoder` gains `continues`, `encode_marking` and `invalidate`, whose defaults mean "can't"; NVENC directly implements them when opened with `CONTINUING` (two long-term frames, three references), which the host asks for (`Backend::encoder`'s `continuing`) only for a viewer that continues in that codec. After a fallback to FFmpeg's NVENC, that viewer gets keyframes.
- **The host's encode loop** (`pipeline::host`'s `recover`): a catch-up and a resume continue from the newest frame received, a window's `FellBehind` from the frame it names, when that frame was sent at most 500 ms before and 100 encoded frames back; otherwise a keyframe, as before. `Pause` now ends the epoch and `Resume` opens the next. `COUCH_HOST_CONTINUE=0` keeps keyframes, to measure against.
- **The viewer:** the network layer delivers the old epoch's frames up to the continuation point after the new epoch begins, and the new epoch's first frame waits (up to 200 ms) for the old epoch's reader; the stream window remembers what its decoder decoded since it last broke (`couch-viewer`'s `chain`), decodes a continuation from one of those frames, asks with `CantContinue` otherwise, and when it falls behind asks to continue from its last frame.
- **`couch-cli`:** `capture-test --recover-every`/`--drop-ms`, `gates --codec hevc`.

## Checked

- **Unit and loopback tests:** `Continuity` (marks, invalidation within reach, slots freed, keyframes forgetting, receipt across a continuation, a frame never sent refused); the preamble, keyframe reasons and IPC header round trips, a malformed continuation flag refused; over loopback, the host learning what the viewer received, and a continuing epoch arriving after the old epoch's frames up to its point and naming it.
- **Three engines** (`a_window_that_fell_behind_continues_in_hevc`): B watches in HEVC and C in H.264; B's window says it fell behind at a frame, and gets an epoch continuing from it, no keyframe, the host's encoder having invalidated what it sent after; C's same ask gets a keyframe.
- **The decoders** (`cargo test -p couch-codec-nvenc --features gpu-tests --test continuation`): every case above, now asserted. And **the host's own bookkeeping against NVENC** (`the_hosts_bookkeeping_against_nvenc`, the review's ask): `Continuity` driving an encoder opened with `CONTINUING`, in HEVC, through a catch-up after 20 frames lost with the reports lagging, a window that fell behind naming a frame 9 back, two continuations from one frame, 80 frames lost (past the 16 invalidated by reach), and 120 lost (past the limit, a keyframe): all five within reach continued, and from the first on, 251 pictures were the same bit for bit in software and D3D11VA.
- **The gates in HEVC** (`couch-cli gates --quick --codec hevc`): G1 11.2 / 12.1 ms; G3 p95 46.6 ms, no catch-up; **G4 one recovery, latency back 79 ms after the outage, no keyframe at the resume** (the host's `key 0`; run by hand with its log, "continuing instead of a keyframe from (1, 578), invalidated 17"), before the age limit; with it, the 2 s outage's resume sends its keyframe, as G4 was written for (one recovery, latency back in 65 ms, G1 11.3 / 12.3 ms); G7 passes. G5 reads "settled never", as it always has in HEVC: the latency clock needs about 0.2 Mbit/s there, which the throttle never touches (M12c, why the gates default to H.264).
- **The gates in H.264** (`couch-cli gates --quick`, where nothing continues but `Pause` and `Resume` changed): every gate passes. G1 12.8 / 15.5 ms; G3 p95 110.3 ms at 17.0 Mbit/s, no catch-up (the PC's slower state part 1 met too, about 108 ms with either encoder); G4 one recovery, its keyframe at the resume, latency back 110 ms after the outage; G5 settled in 0.38 s, latency back in 0.90 s; G7 the second viewer 16.8 ms at 19.9 Mbit/s.

## Found while checking

- **With long-term frames on, NVENC predicts from one reference** unless asked for more (above): a continuing encoder asks for three.
- **HEVC's own reference choice is fewer than three:** asked for three, part 1's settings would gain about 1 dB in HEVC too, but part 1 keeps FFmpeg's stream exactly; only continuing encoders get it.
- **How far back a continuation may reach:** HEVC's picture order is 8 bits here (`log2_max_pic_order_cnt_lsb`), so a decoder places a picture by its order relative to the last it decoded. A 160-frame gap still decoded bit for bit in HEVC (H.264, whose `frame_num` is 8 bits too, gave no pictures after it: it doesn't continue), but past 256 the order is ambiguous outright, so the host continues only from a frame at most 100 encoded frames back (`MAX_AHEAD`, 1.7 s at 60 fps; none are encoded during a pause).
- **FFmpeg's CLI can't decode HEVC on D3D12VA at 720 lines** on NVIDIA (no aligned frame pool, M12c's finding); our decoder can.

## The review

A fresh review agent read the whole diff (and ran clippy and the crates' tests): no path where a viewer decodes a continuation from a frame it doesn't hold, or the host continues from one its current encoder never encoded; a malicious viewer reaches only its own stream, within bounded memory. What it found, checked against the code:

- **The order's range** (medium, held as a guard): above; `Continuity::plan` refuses a frame more than 100 encoded frames back, with a test.
- **The invalidated set in production wasn't checked against NVENC** (medium, held): the spike invalidated every frame left out; the host invalidates only those within 16 and long-term ones. The new GPU test drives the host's bookkeeping itself (above). HEVC's DPB is 6 pictures here and 16 at most.
- **Continuing encoders opened without asking the GPU** (medium, held): NVENC directly now checks invalidation and the long-term frames on open and opens its plain stream otherwise; one that won't open with long-term frames opens without before FFmpeg's.
- **A keyframe starting a continuing epoch was thrown away** (low to medium, held): the window now takes any keyframe as a fresh start, and a keyframe asked for while a catch-up or resume happens wins over continuing.
- **A continuation of a continuation could lose the frames the first needs** (low to medium, held): the network layer follows the chain of continuation points back (`ViewerState::wanted`), with a test.
- **The gates' clean-decode check got weaker** (low to medium, held): every fresh start the window asks for counts again, `CantContinue` included, as before M12d-2.
- **A long-term slot beyond two was ignored** (low, held): the host stops continuing on such a report. **The IPC header took any flag** (low, held): 0 or 1 only. **A table row and two claims** (low, held): `docs/protocol.md`'s keyframe row, and what was checked bit for bit (our D3D12VA path and AMD's and Intel's decoders weren't).
- **A window that fell behind will rarely continue from a long-term frame** (design, not held): its last decoded frame is usually older than both marks, which follow what the network read, so NVENC codes the continuing frame intra (below).

## Still to come

- **A game through the app,** HEVC between two PCs: catch-ups and a Wi-Fi gap should no longer pulse soft; the host's log says "continuing instead of a keyframe".
- **A window that fell behind** (your choice) mostly gets an intra frame: by the time it notices (200 ms), its last decoded frame is older than both long-term marks, which are invalidated with what follows it. A third slot kept older, or marks following what the window decoded, would give it a reference.
- **H.264** stays on keyframes; tuning (fresher marks, a larger buffer with four references) could earn it later, through `SessionJoin.continues`.
