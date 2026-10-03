---
name: audio-checks
description: Verify Couch Co-App's game audio after a change, with `couch-cli latency-probe --audio` (gate A1, click to playback, loss) and the audio unit tests, and know what to ask the user to listen for. Use after changing couch_media::audio (framer, jitter buffer, click track), couch-codec-opus, couch_platform_win::audio (WASAPI loopback), couch_net::audio, couch-cli's host_audio/host/view audio paths, couch-viewer's audio.rs, pipe.rs or clock.rs, the audio datagram or IPC payloads, or QUIC/datagram settings; and before reporting or committing such a change.
---

# Audio checks

**The path** (docs/architecture.md, "Data paths"; docs/protocol.md, "Audio"):
1. The host captures the game's process tree with WASAPI process loopback, in 10 ms packets stamped with QPC time.
2. It encodes them with Opus at 96 kbit/s and sends one datagram per 10 ms.
3. The viewer runs them through an adaptive jitter buffer (10 to 40 ms), then decodes or conceals on SDL's audio thread, and SDL hands the samples to Windows.

docs/m3-results.md has the reference numbers and what was tried.

## Run

```powershell
. .\scripts\dev.ps1
Get-CimInstance Win32_Process -Filter "Name='couch-cli.exe' or Name='couch-viewer.exe'" | Select-Object ProcessId, CommandLine
cargo test -p couch-media -p couch-codec-opus -p couch-proto -p couch-net
cargo test -p couch-platform-win audio -- --ignored          # live loopback capture
cargo build --release -p couch-cli -p couch-viewer
$cli = '.\target\release\couch-cli.exe'
& $cli latency-probe --secs 30 --relay none --audio                                # run twice: results vary by session
& $cli latency-probe --secs 30 --relay none --audio --netem "delay=10ms,loss=1%"
```

- **Look for leftovers first.** A running `couch-cli.exe` (often a host the user started for a listening test) makes the release build fail with "failed to remove file". The old binary then runs against a new viewer, and the results are wrong without any obvious error. Ask the user to stop it (Ctrl+C in its console) rather than killing their session.
- **It clicks through the speakers.** `--audio` starts the latency clock with `--clicks`, and the viewer plays them back too. Say so before running. Plain `latency-probe` and `gates` are silent.
- **Run each clean measurement at least twice.** See "Reading the results".

## Reading the results

The probe prints:

| Line | Meaning | Normal on the M3 PC |
|---|---|---|
| `audio capture -> handed to the device` p95 | **Gate A1**: what our pipeline adds, from the host's capture timestamp to the moment SDL gets the samples, over the whole session. Judged only on unimpaired runs | 14 to 21 ms; limit 25 |
| `audio click -> playback` | The clock's click entering Windows' mixer, to the viewer's playback of it entering the mixer. Both are loopback-captured by the probe, so the loopback path cancels out. Information only | 52 to 69 ms, steady within a session (p50 ≈ max) |
| `buffer X ms (target Y)` | The jitter buffer's average level and its current target | 10 / 11 clean; up to 20 / 13 at 1% loss |
| `concealed / late / dropped / underruns` | Frames concealed; packets arriving after their turn; packets dropped to trim a full buffer; times it ran dry | Clean: all 0. At 1% loss, about 1–1.6% concealed, 0 late, 0 underruns |
| `audio behind video by` | Medians: click to playback minus clock to present. Information only | 40 to 50 ms |

**Why click to playback isn't the gate.** About 35 to 50 ms of it is Windows' playback path on the viewer: SDL's one or two queued 10 ms periods plus the engine's own processing.
- The plain latency clock, an SDL app that only plays clicks, takes 34 to 40 ms from write to mix.
- Several 10 ms stages settle their phases at random when a stream starts, so the total differs by up to about 15 ms between sessions and stays put within one.
- A custom WASAPI renderer, raw (effects-free) streams and smaller SDL periods were all tried in M3 and made no difference. Don't re-try them without new information.

**Red flags**
- **A1 above 25 ms on a clean run:** the jitter target grew (check `target`), or packets wait somewhere new (IPC, decoding, the framer).
- **`late` above 0 on a clean run, or concealment well above the loss rate:** the buffer misjudges lateness. Check `JitterBuffer::push`'s transit computation and the "missing with nothing newer" rule.
- **`underruns` climbing without a network stall:** the host's capture stalled, or packets stopped. Process loopback normally delivers silence continuously, even from a silent process.
- **The HUD's buffer stuck well above target:** the playback ratio isn't reaching SDL (`SDL_SetAudioStreamFrequencyRatio` in the viewer's callback).
- **0 clicks:** the clock didn't get `--clicks`, it has no audio device, or a stale `couch-cli.exe` ran (see Run).

**Capture-to-device figures need one machine.** They subtract the host's clock from the viewer's. Across two PCs only the jitter-buffer and concealment counts mean anything.

## What isn't covered automatically; ask the user

**Listening at 1% loss**, with a game that has music or speech:
- On one machine, both the game and the viewer play. **Muting the game doesn't help**: the in-game volume and the volume mixer's sliders all silence the stream too. Those are the game's own slider and the `couch-cli` host's (the loopback capture has its own volume), as well as the viewer's.
- Instead have the user send the game's output to a device they can't hear: Settings → System → Sound → Volume mixer → the game → Output device. Capture doesn't depend on the output device.
- Command: `host --source window --title "<game>" --relay none --netem "loss=1%"`, then `view <ticket> --relay none --netem "loss=1%"`.
- Ask whether anything beyond faint smoothing is audible: clicks, dropouts, warbling, or pitch drifting. A pitch drift means the playback ratio is working too hard.

**Changing the default output device mid-stream:** SDL should follow it. Worth a quick try after changing the viewer's audio code.

## After the checks

- Report the A1 p95, click to playback (noting the per-session spread), the loss-run counts, and any change from the normal ranges.
- If behaviour changed on purpose, update docs/protocol.md ("Audio") and the milestone's results file.
- If the transport or rate control changed too, also run the `rate-control-gates` skill. If teardown changed, run `input-safety-checks`.
