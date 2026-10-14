# M1 results: video over iroh

M1 is the decision gate: does video over iroh (QUIC) meet the latency and robustness targets, or does media move to datagrams or WebRTC? **It does, and neither fallback is needed.** Getting there took one change the plan didn't foresee: our own QUIC congestion controller ([ADR 0005](adr/0005-media-congestion-control.md)).

Measured 2026-10-03 on one machine:
- RTX 3060 Ti, NVENC H.264;
- 60 fps capture of the latency clock window (1088×184) unless noted;
- host and viewer as separate processes.

Network impairments come from the netem test transport (see [architecture.md](architecture.md#measuring-latency)).

## Gates

| Gate | Check | Target | Result | |
|---|---|---|---|---|
| G1 | Clock to present, by barcode, direct loopback | p50 ≤ 25 ms, p95 ≤ 35 ms | p50 11.8–13.8 ms, p95 14.1–17.7 ms over four runs | pass |
| G1b | Clock and viewer side by side on screen | median ≤ 50 ms | median 4–17 ms, max 25 ms over two runs | pass |
| G2 | 30 min at 20 Mbit/s, relays enabled | no disconnects, decoder errors or path flaps | Over 107,846 frames at 59.9 fps: direct path throughout, 0 path changes, 0 decoder errors, 0 lost packets, ran to the end. Bitrate averaged 19.97 Mbit/s. Latency p50 13.7 ms in every 5-minute block, p95 15–18 ms. | pass |
| G3 | 1% loss and 20 ms each way | p95 ≤ baseline + 2 × RTT + 20 ms ≈ 121 ms; no drift over 10 min | 60 s: p95 81.6 and 83.1 ms at a mean of 19 Mbit/s. 10 min: p95 83.4 ms; every minute p50 49–53 ms, p95 82–84 ms, 19–20 Mbit/s | pass |
| G4 | 2 s blackout | back to baseline within 1 s, exactly one keyframe | first frame 75 ms after the outage; median latency back within 2 ms of baseline after 208 ms; one keyframe | pass |
| G5 | Throttle 25 → 5 Mbit/s | bitrate settles within 2 s | One catch-up 0.28 s after the drop, to 3.8 Mbit/s; worst frame 331 ms. Then 4.0–5.3 Mbit/s. | pass |
| G6 | Relay only, iroh's public relay, 720p30 test pattern | works at ≤ 2 Mbit/s, HUD shows the relay | 30 fps at 1.9–2.0 Mbit/s, no loss. HUD: "Relay rtt 241.6 ms 1.9 Mbit/s". Clock to present p50 128–134 ms. | pass |
| — | Audio-sized datagrams while a 1 MB keyframe is in flight | gain ≤ 2 ms | loopback: p95 gain 0.9 ms | pass |

Notes on the gates:
- **Builds.** G2 and the 10-minute G3 ran on the build before the last two changes (the initial window, and catch-ups that keep the bitrate when the path isn't congested). Those changes only act at stream start and on catch-ups, and neither run had a catch-up. Everything else was re-run on the final build.
- **G1** includes about 9 ms that is the clock app's own render and compositing before capture.
- **G3 latency.** Loss costs latency even at full bitrate: the median rises from 36 to 49 ms and p95 from 42 to 83 ms. A lost packet holds up the frames behind it for a round trip ([ADR 0003](adr/0003-epoch-streams-for-video.md)).
- **G6 latency** is mostly the relay's distance from here: a 240 ms round trip.
- **Timing of G4 and G5.** The figures above estimate when the outage ended or the throttle began from the traces. `couch-cli gates`, added after M1, reads the exact times from the netem schedule. Over six runs on the final build it measured:
  - G4: first frame 45–100 ms after the outage;
  - G5: a catch-up 0.3–0.4 s after the throttle, and a worst frame of 100–420 ms.
- **Datagrams behind a slow link.** On a 25 Mbit/s link, audio waits in the link's queue behind a 1 MB keyframe: about 83 ms at p50. Our keyframes are much smaller (a one-frame VBV, about 42 KB at 20 Mbit/s). Pacing video below the link rate would trade video latency for audio latency; revisit in M3 if audio suffers.

## Congestion controller A/B

Same rate controller, three QUIC congestion controllers. These runs came before the final tuning of the initial window and catch-ups.

| | G3: 1% loss, 20 ms (60 s) | G4: 2 s blackout | G5: throttle to 5 Mbit/s |
|---|---|---|---|
| Cubic (noq's default) | mean 2.0 Mbit/s; p95 102 ms; 5 catch-ups | first frame 1.4 s after the outage; 3 keyframes | max 369 ms |
| BBR v3 | mean 3.5 Mbit/s; p95 185 ms; 11 catch-ups | first frame 1.4 s after the outage; 3 keyframes | max 148 ms |
| **Ours (`media`, now the default)** | **mean 19.0 Mbit/s; p95 83 ms; no cuts** | **88 ms; 1 keyframe** | **max 350 ms** |

Why, in short: Cubic shrinks its window on random loss, which caps it near 3 Mbit/s. BBR v3 holds its window during loss recovery and drains it to re-measure the round trip. And QUIC's probe timer backs off to seconds during an outage. Details in [ADR 0005](adr/0005-media-congestion-control.md).

## What changed during M1

- **Congestion control moved into the rate controller.** QUIC now runs `MediaCongestion`, which:
  - ignores isolated losses;
  - starts with a 32-packet window;
  - sends silence probes after an outage.
- **Rate-controller fixes:**
  - The delay signal is the minimum over 400 ms, so frames stuck behind a retransmission no longer count as queueing.
  - Backlog is measured beyond the round trip and the report's age.
  - Catch-ups aim just below the delivery rate, and cut the bitrate only when the path shows congestion.
  - A resume counts toward the catch-up cooldown.
- **Outage handling:** pause when reports stop with video outstanding, and resume with one keyframe.
- **Measurement tools:**
  - the latency clock with a CRC-checked barcode, and `latency-probe`;
  - `screen-scan`;
  - the netem transport;
  - `--rc-trace` and `--timings-csv`;
  - relay and direct status in the viewer HUD.

## Known limits and follow-ups

- **Bitrate changes cost a keyframe.** FFmpeg's NVENC wrapper forces an IDR on every bitrate change, so increases are batched (steps of at least 15%, at most one a second). Fix in M9 by calling NVENC directly.
- **Loss adds latency.** At 1% loss, about 30% of 20 Mbit/s frames contain a lost packet. Forward error correction would remove the round-trip wait if real networks need it.
- **Fairness.** The rate controller is delay-based, so it yields to TCP downloads sharing a bottleneck.
- **One machine only.** These are loopback numbers. Real-network and NAT tests come with our relay (M7); a test with a friend can come earlier.
- **Capture timestamps.** Windows.Graphics.Capture's frame timestamp runs ahead of delivery, so "capture to present" understates latency. Use the barcode figures. (Fixed in P5: the capture time is never later than delivery.)
- **Font notices.** The viewer's HUD font comes with egui. Its licenses (OFL 1.1, Ubuntu Font Licence) allow bundling but must ship in the installer's notices (M10).

## Reproduce

`couch-cli gates` runs G1 and G3 to G5 and judges them (`--quick` cuts G3 to 2 minutes). The commands below run each gate by hand.

```powershell
cargo build --release -p couch-cli -p couch-viewer
$cli = '.\target\release\couch-cli.exe'
& $cli gates                                                                           # G1, G3, G4, G5, judged
& $cli latency-probe --secs 30 --relay none --screen                                   # G1, G1b
& $cli latency-probe --secs 1800                                                       # G2
& $cli latency-probe --secs 600 --netem "delay=20ms,loss=1%"                           # G3
& $cli latency-probe --secs 30 --netem "at10s=blackout:2s" --netem-viewer "delay=0ms"  # G4
& $cli latency-probe --secs 30 --netem "rate=25mbit,at10s=rate:5mbit" --netem-viewer "delay=0ms"  # G5
& $cli host --source testpattern --pattern-size 1280 720 --fps 30 --relay n0 --relay-only  # G6, then:
& $cli view <ticket> --relay n0 --relay-only
cargo test --release -p couch-net --features netem --test datagrams -- --ignored --nocapture
```

Add `--congestion bbr3` or `cubic` to compare controllers, and `--rc-trace rc.csv --timings-csv frames.csv` for traces.
