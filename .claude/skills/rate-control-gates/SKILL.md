---
name: rate-control-gates
description: Run and judge Couch Co-App's streaming gates (G1 latency, G3 loss, G4 blackout, G5 throttle, G7 one viewer's bad path not touching another's) with `couch-cli gates`, and debug a failure from the rate-control and frame traces. Use after changing couch_media::ratecontrol, couch_net::congestion, couch_net::video, QUIC transport settings, encoder bitrate or keyframe handling, the upload share, the host pipeline (`couch_engine::pipeline::host`), or the viewer's decode loop; when asked to measure streaming latency or check for a regression; and before reporting or committing such a change.
---

# Rate-control gates

`couch-cli gates` runs five `latency-probe` scenarios (G7 is two runs) on this machine and judges each from its traces. The scenarios use the netem test transport, so no admin rights, second machine or internet are needed.

| Gate | Scenario | Pass |
|---|---|---|
| G1 | direct, unimpaired, 30 s | clock to present p50 ≤ 25 ms, p95 ≤ 35 ms |
| G3 | 1% loss, 20 ms each way; 10 min (2 with `--quick`) | p95 ≤ G1 p95 + 2 × RTT + 20 ms; last minute's p95 within 10 ms of the best minute's; mean bitrate > 15 Mbit/s |
| G4 | 2 s blackout at 10 s | latency back to baseline ≤ 1 s after the outage; exactly 1 recovery keyframe |
| G5 | link throttled 25 → 5 Mbit/s at 10 s | bitrate within 2.5–6 Mbit/s for good ≤ 2 s after; latency back ≤ 2 s |
| G7 | two viewers, 30 s, run twice: unimpaired, then only the host's traffic to the first viewer throttled to 5 Mbit/s (netem `to=<port>`) | the second viewer's p95 within 5 ms of its unimpaired run, and its median received bitrate ≥ 90% of it |

How the timing checks work:
- **Event times are exact.** G4 and G5 time from the netem schedule, which the host's trace records.
- **"Latency back"** is the first moment the median of the next 30 frames is within 5 ms of the median over the 2 s before the event.

Every gate also needs a full run with 0 decode errors. Two checks are regression guards rather than plan targets:
- **G3 mean bitrate.** Cubic passed G3's latency target while collapsing to 2 Mbit/s.
- **G5 latency back.** A bitrate can settle while a queue stays standing.

## Run

```powershell
. .\scripts\dev.ps1
Get-Process couch-* -ErrorAction SilentlyContinue | Stop-Process -Force
cargo build --release -p couch-cli -p couch-viewer
.\target\release\couch-cli.exe gates --quick      # about 6 min; without --quick about 14
```

- **Stop leftover processes first.** A running `couch-cli.exe` makes the release link fail with "Access is denied". A leftover latency-clock window can be captured instead of the new one.
- **Build both binaries.** `couch-cli` starts the `couch-viewer.exe` next to it, so a stale viewer gives stale numbers.
- **Quick or full.** Use `--quick` while iterating. Before reporting or committing, do a full run, since G3's drift check needs the 10 minutes.
- **Other flags:**
  - `--only g4,g5` repeats one scenario.
  - `--congestion bbr3|cubic|newreno` runs an A/B against noq's controllers.
  - `--out DIR` changes where traces go (default `target/gates`).
- **From a tool call.** The full run exceeds the 10-minute foreground limit. Run it with `run_in_background`, redirect output to a file in the scratchpad, and wait for the completion notice rather than polling.
- **Keep the machine quiet.** Two windows open at the top left: the clock, and the viewer below it. Leave them visible and keep the machine otherwise idle; the numbers are wall-clock.
- **Exit code** 0 means every check passed. Each gate leaves `gN-rc.csv`, `gN-view.csv` and `gN.log` in the output folder.

## Judge against the reference, not just the threshold

A pass that is clearly worse than the reference is still a regression to explain. Below is the spread this command measured on the M1 build (2026-10-03, RTX 3060 Ti; G4 and G5 over six runs). Fuller results are in `docs/m1-results.md`.

| Gate | Normal range |
|---|---|
| G1 | p50 12–14 ms (12.3–13.0 since M9c, 11.5 since M9d's pacing), p95 12–18 ms; bitrate steps up to 20 Mbit/s about 9 s in |
| G3 | p95 82–84 ms, p50 about 50 ms; mean bitrate 19–20 Mbit/s; no catch-ups; drift within ±2 ms |
| G4 | first frame 45–100 ms after the outage, with latency back on that frame; 1 keyframe |
| G5 | catch-up 0.3–0.4 s after the throttle, to about 4 Mbit/s; settled at 0.3–0.4 s; latency back after 0.7–0.9 s; worst frame 100–420 ms; p95 42–45 ms |

G5's worst frame varies most between runs. Repeat a borderline gate with `--only` before calling it a regression.

## Many viewers on one PC

The gates use one or two viewers. After changing the host pipeline, encoding or rate control, also run `.\scripts\viewers.ps1 -Screen` with 4 viewers and with `-Viewers 7`. `-Screen` captures the primary screen, so frames take the GPU path (M9b); the test pattern (without `-Screen`) is made in memory and doesn't. Reference from M9c (`docs/m9c-results.md`; frames on the GPU on both sides):
- **4 viewers:** about 60 fps each, capture to present p50 4.5 to 5.5 ms and p95 about 8 ms, 18.7 Mbit/s each, slowest encode about 9.5 ms; the stream windows at 23% of one core.
- **7 viewers:** 59 to 60 fps each, p50 5.4 to 6.8 ms and p95 about 9 ms, 18.7 to 19.9 Mbit/s each, slowest encode 10 to 11 ms; the windows at 37%.
- **The viewers copying frames** (`-CopyFrames`, as before M9c): 7 viewers at p50 about 14 ms and p95 about 24 ms, windows at about 270%. A default run near those numbers means the windows aren't decoding on the GPU: check their "decoding with" log line.
- **The host copying frames** (`-CopyThroughMemory`, as before M9b): 7 viewers fall to about 40 fps and p50 about 70 ms. Check the host's "GPU frames" log line.
- **4 viewers at 120 fps** (`-Fps 120`, M9d): 114 to 116 fps each, p50 2.0 to 2.8 ms and p95 4.0 to 4.6 ms. Without pacing (`COUCH_HOST_PACING=0`) about 117 fps and p95 3.8 to 4.0: pacing costs a little on this 240 Hz screen, where the old interval was already right. A p95 near 10 ms means held frames go out before the fresher one arrives (`couch_media::pacing`'s wait).

Every rate controller should stay near its 20 Mbit/s target.

A controller falling to the 1.5 Mbit/s floor with many viewers means host load is being read as congestion again (`SendDelays`).

## When a check fails

1. **Read the gate's `actions:` line.** It lists every controller action timed from the event: the outage start for G4, the throttle for G5, the run start otherwise. It shows bitrates in Mbit/s:
   - `set` changes the bitrate;
   - `catch-up` resets the epoch and sends a keyframe;
   - `pause` and `resume` bracket an outage.
2. **Read `gN-rc.csv`**, one row per controller tick (50 ms). Columns:
   - `qpc_us`, `applied_bps`, `target_bps`, `delivery_bps`, `send_bps`, `outstanding_bytes`;
   - `rtt_us`, `min_rtt_us`, `delay_trend_us`;
   - `sent_packets`, `lost_packets`, `congestion_events`, `cwnd_bytes`;
   - `report_age_ms`, `relayed`, `netem_us`, `action`.
3. **Work out why the controller acted.** Overuse is any of:
   - `delay_trend_us` > 10 000;
   - `rtt_us − min_rtt_us` > max(15 000, `min_rtt_us` / 2);
   - loss over the last second > 10%, from the deltas of `lost_packets` and `sent_packets`.

   The full control law is in `docs/protocol.md` under "Bitrate control".
4. **Read `gN-view.csv`**, one row per presented frame: `presented_us`, `barcode_us`, `capture_us`, `arrived_us`, `decoded_us`, `epoch`, `seq`.
   - Latency is `presented_us − barcode_us`. A `barcode_us` of 0 means the barcode wasn't read.
   - Both files use QPC microseconds on one clock. `qpc_us − netem_us` is when the netem schedule started, so `at10s=` events happen 10 s after it.
5. **For analysis beyond the table,** extend `crates/couch-cli/src/gates.rs`, which has CSV and percentile helpers. This machine has no Python. In PowerShell, set `[cultureinfo]::CurrentCulture = 'en-US'` first: the pt-BR locale prints decimals with commas.

Failure patterns seen in M1:

| Symptom | Usual cause |
|---|---|
| G3 mean bitrate collapses; `cwnd_bytes` shrinks after losses | QUIC's congestion controller is reacting to random loss. `MediaCongestion` must ignore isolated losses (ADR 0005). |
| G3 has repeated `set` cuts while `delay_trend_us` stays low | Retransmission stalls are being read as queueing. The delay signal must be the minimum over 400 ms. |
| G4's first frame comes more than 1 s after the outage | QUIC probe-timeout backoff. `MediaCongestion`'s silence probes should cap it. |
| G4 has a `catch-up` right after `resume` (2 or more keyframes) | Data queued behind a transport that is still waking up. A resume must count toward the catch-up cooldown. |
| G5 catch-up lands far below the link, or latency spikes to about 900 ms | The catch-up target ignores the delivery rate. It should be 0.85 × `delivery_bps`, between a quarter and half of the old bitrate. |
| A catch-up fires at stream start | The transport window is still opening. Catch-ups should cut only when the path shows congestion. |

## Rules

- **Never loosen a threshold to make a change pass.** If a target looks wrong, say so and ask the user.
- **Stop and tell the user before switching transport.** If G3 to G5 can't be met by changing rate control, the plan's fallback is video over datagrams with our own retransmission (or WebRTC if iroh's paths themselves fail).
- **Update the docs when a deliberate change moves the numbers.** Update `docs/m1-results.md`, or that milestone's results file. Also update `docs/protocol.md` and the ADR if the control law changed, and report the numbers before and after.
- **Other checks aren't part of this command.** Run them when their area changes; commands are in `docs/m1-results.md` under "Reproduce":
  - G1b: on-screen latency, `latency-probe --screen`;
  - G2: a 30-minute soak with relays on;
  - G6: relay only, through iroh's public relays (needs internet), or through our relay on this PC (M7, no internet): `cargo xtask relay-tool` once, `.\scripts\relay.ps1`, then `latency-probe --relay http://localhost:3340 --relay-token <release.toml's relay_token> --relay-only`. On loopback it shows the path and the app's relay bitrate cap work, not real-network latency;
  - the audio-datagram timing test.
