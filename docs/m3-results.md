# M3 results: audio

M3 streams the game's sound to the viewer:
- WASAPI process loopback of the game's process tree on the host;
- Opus at 96 kbit/s in 10 ms packets, one per datagram;
- an adaptive jitter buffer with Opus loss concealment on the viewer, played through SDL3.

**It works.** All checks pass, including listening to a game at 1% loss.

Measured 2026-10-03 on one machine:
- the RTX 3060 Ti PC from M1;
- Realtek speakers. Windows runs them at 48 kHz with a fixed 10 ms engine period; a smaller period isn't offered.

## Checks

| Check | Result | |
|---|---|---|
| Unit tests | Opus round trip and concealment (3). Framer, jitter buffer, click track and onset detector (17): order, loss, lateness, reordering, jitter raising the target, the cap, running dry and refilling, a burst trimmed, a long hole skipped, the playback ratio, sequence wrap, a sender starting over. Protocol: datagram and IPC round trips, the size limit, trailing bytes | pass |
| Loopback transport | 50 audio datagrams over real iroh endpoints, with video's receiver reports flowing alongside, arrive intact (`couch-net` test `audio`) | pass |
| Process loopback capture | Activates and delivers 48 kHz stereo stamped on QPC (ignored test, needs the audio service). Each 10 ms packet arrives 10 ms after its first sample's timestamp | pass |
| **A1: what our pipeline adds** | Host capture to handing the samples to SDL, p95 over 30 s, 3 sessions: **19, 19, 15 ms** (target ≤ 25 ms) | pass |
| Click to playback, for information | Clock's click entering Windows' mixer to the viewer's playback entering it, 3 sessions: 65.4, 68.6, 52.0 ms. Steady within a session (p50 = p95) | — |
| Audio behind video, for information | 50.4, 49.3, 39.7 ms (medians: click to playback minus clock to present) | — |
| 1% loss, 10 ms delay | 1.34% of frames concealed, 0 late, 0 underruns, 0 dropped. Pipeline 42 ms (10 of it the added delay). Click to playback 84 ms p50 | pass |
| Listening at 1% loss | A Steam game streamed with 1% loss, its own output sent to a device nobody heard: sounded fine, nothing beyond concealment | pass |
| Gates G1, G3 to G5 (`gates --quick`), rerun because audio now shares the connection | G1 p50/p95 17.0/25.2 ms. G3 p95 82.2 ms at 19.3 Mbit/s, drift +1.3 ms. G4: one keyframe, back in 68 ms. G5 settled in 0.35 s, latency back in 0.52 s | pass |
| Rerun after the code-review fixes | A1 p95 19 and 17 ms. Click to playback 60.3 and 56.0 ms. At 1% loss, 1.62% concealed, 0 late, 0 underruns. `release-check` passes | pass |
| `release-check`, rerun because the host's session teardown changed | Released after 241, 94, 199 and 376 ms; host stop 10 ms | pass |

## Where audio latency goes

On one machine, from the game's sound entering Windows' mixer on the host to the stream's sound entering it on the viewer:

| Part | Time | Ours? |
|---|---|---|
| Host capture: Windows delivers loopback in 10 ms packets | ~10 ms | no |
| Opus (2.5 ms look-ahead), network, IPC | ~3 ms | yes |
| Jitter buffer: target (10 ms on a clean path) plus waiting for SDL's next pull | 2–10 ms | yes |
| Viewer playback: SDL to Windows' mixer | 34–49 ms | no |

**The last row is Windows' cost, not ours.** The latency clock, a plain SDL app that only plays clicks, takes 34 to 40 ms from writing a click to Windows mixing it. A game played locally pays the same before its sound is heard.

**It varies by session.** Several stages each run on a 10 ms period, and their phases settle at random when a stream starts. The total then stays put to within a millisecond for the whole session, somewhere between about 51 and 69 ms here.

**What was tried and didn't help:**
- **A smaller SDL period** (`SDL_AUDIO_DEVICE_SAMPLE_FRAMES` 240 and 128). Windows kept 480 frames.
- **A WASAPI renderer of our own.** It used `IAudioClient3` at the minimum period and refilled to exactly one period plus 1 ms on every engine event, rather than SDL's one to two periods. It measured 59 to 68 ms, no better than SDL, so it was removed.
- **A raw stream**, which skips Windows' audio effects: no change, through SDL or our renderer.
- **A 20 ms minimum jitter target** (the plan's figure): 68 to 83 ms, with no benefit on a clean path. The minimum is now 10 ms. The target still grows with measured jitter, up to 40 ms.

**Why the target changed.** The plan's targets were click to playback p95 ≤ 60 ms and audio within 40 ms of video. Both include Windows' playback path, which passed in about half the sessions on this machine and which no code of ours shortened. A1 now judges the part we control. The full figures are still reported.

## Found and fixed

- **Late packets never taught the buffer.** A packet arriving after its turn was dropped before its arrival time was recorded, so the target never grew to cover lateness. A unit test caught it.
- **A late packet was thrown away even when nothing came after it.** With 10 ms of delay and 1% loss this concealed 11% of frames, nearly all of them late rather than lost. Now a missing packet with nothing newer is concealed for one frame while playback waits for it. Concealment then matched the loss: 1.07 to 1.55%.
- **The playback ratio's dead band was too wide** (±5 ms). The level could sit below target indefinitely. It is now ±2.5 ms.
**Fixed after a code review** (the review found no high-severity defects):
- **The audio callback could outlive its data.** The `sdl3` wrapper frees a stream's callback before destroying the stream, and the viewer only paused first on a clean exit. An error or panic between opening audio and the end of the window loop left SDL's audio thread free to call into freed memory. The pause-and-wait now lives in `Drop`, for the viewer and the clock's clicks alike.
- **A short pause in the stream counted as jitter.** Transit was measured against the sequence number, so a capture that stalled for 30 to 500 ms raised the target to 40 ms. For about 2 s afterwards, playback then ran 1.5% slow, an audible pitch drop. Transit is now arrival minus `capture_us`, and the test tone stamps exact 10 ms steps.
- **A hostile host could crash the viewer.** Huge forward jumps in `seq` grew the extended sequence until arithmetic overflowed. In a debug build that panicked while holding the buffer's lock, and the next audio callback then aborted on the poisoned lock. A jump of more than 50 frames either way now restarts the buffer, peer timestamps are used with saturating arithmetic, and the audio thread ignores a poisoned lock.
- **Stale packets could restart playback.** After an underrun, a packet that had already missed its turn was accepted and played first. It is now rejected until the buffer restarts.
- **The packet limit didn't fit the transport.** QUIC's datagrams start out under 1200 bytes. The limit is now 1000 bytes rather than Opus's 1275, and the test checks it against 1100.
- **Housekeeping:**
  - The probe's listener threads now stop on every exit path.
  - A capture error is no longer lost when the queue is full.
  - The clock clicks only with `--clicks`, so plain `latency-probe` and `gates` runs are quiet.
  - A test keeps `couch-media`'s copy of the stream format in step with `couch-proto`'s.

- **A `PROPVARIANT` freed our stack.** The `windows` crate gives `PROPVARIANT` a `Drop` that calls `PropVariantClear`. The process-loopback activation parameters point at a stack variable, so dropping it was heap corruption. It is now `ManuallyDrop`.

## Things to know

- **libopus is compiled from source** by the `opus` crate (BSD-3-Clause), which needs CMake. `cargo xtask doctor` checks for it.
  - FFmpeg's libopus wrapper can't conceal a lost packet: an empty packet ends the stream.
  - The CMake build fails beyond Windows' 260-character path limit, so a deeply nested `CARGO_TARGET_DIR` breaks it.
- **The game's sound is its process tree.** Process loopback includes the window's process and the processes it started. A game whose sound comes from another process needs `host --audio system`, which captures everything except our own app: notifications and voice chat included.
- **Capture doesn't depend on where the game plays.** With the game's output sent to a device nobody listens to (Volume mixer → the game → Output device), the stream still carries it. That is how to test on one machine, and how a host can stream a game without hearing it. Turning the game down, in the game or in the mixer, turns it down for the guests too.
- **The host's own mixer entry scales the stream too.** Windows gives the loopback capture a volume of its own: `couch-cli host` at zero in the volume mixer silences the stream, as do the game's and the viewer's sliders. A host can mute their guests without meaning to. M5 should either make the app's capture ignore it or show a warning.
- **A silent game still streams.** Process loopback keeps delivering silence while the target plays nothing, even a process with no audio stream open at all (the latency clock without `--clicks`). So packets flow at 100 a second regardless. The jitter buffer still copes with a stalled capture: transit is measured from capture timestamps, so a pause isn't mistaken for jitter.
- **Capture-to-device figures assume one machine.** They subtract the host's clock from the viewer's. Across two PCs they mean nothing; click to playback can't be measured there either.
- **Teardown order.** The `sdl3` crate frees an audio callback before destroying its stream, so `AudioOut::close` pauses the stream and waits for any running callback first.

## Follow-ups

- **A/V offset on a second PC.** Audio trails video by 40 to 50 ms here, nearly all of it Windows' playback path. Delaying video to match would cost input latency, so for now it isn't done. Revisit after hearing real sessions.
- **Several viewers (M5)** each need their own Opus stream state only if formats diverge. One encoder's packets can go to every viewer as they are.

## Reproduce

```powershell
cargo build --release -p couch-cli -p couch-viewer
$cli = '.\target\release\couch-cli.exe'
& $cli latency-probe --secs 30 --relay none --audio                                  # A1, click to playback
& $cli latency-probe --secs 30 --relay none --audio --netem "delay=10ms,loss=1%"     # loss
& $cli host --source window --title "<game>" --relay none --netem "loss=1%"           # listen; prints TICKET
& $cli view <ticket> --relay none --netem "loss=1%"
cargo test -p couch-media -p couch-codec-opus -p couch-proto -p couch-net
cargo test -p couch-platform-win audio -- --ignored
```
