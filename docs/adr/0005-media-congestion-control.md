# ADR 0005: Our own QUIC congestion controller; the rate controller does congestion control

**Status:** accepted (2026-10-03), from the M1 A/B test.

## Context

Video bitrate is set by a per-viewer rate controller (`couch_media::ratecontrol`). It reads one-way delay, round-trip inflation, loss and backlog, much as GCC does in WebRTC. Underneath it, QUIC runs its own congestion controller, which limits how much may be in flight and paces packets. M1 compared noq's controllers on the impaired-network gates (netem, one machine):

| Controller | G3: 1% loss, 20 ms each way (60 s) | G4: 2 s blackout | G5: 25 → 5 Mbit/s |
|---|---|---|---|
| Cubic (noq's default) | Mean 2.0 Mbit/s, 5 catch-ups | First frame 1.4 s after the outage; 3 keyframes | Settles; max 369 ms |
| BBR v3 | Mean 3.5 Mbit/s, 11 catch-ups, p95 185 ms | First frame 1.4 s after the outage; 3 keyframes | Settles; max 148 ms |
| **Ours (`media`)** | **Mean 19 Mbit/s, no cuts, p95 83 ms** | **First frame 88 ms after; latency normal in 325 ms; 1 keyframe** | **Settles in 0.3 s; max 350 ms** |

What went wrong with the stock controllers:
- **Cubic** treats every loss as congestion. At 1% random loss and a 43 ms round trip its window averages about 12 packets, which caps throughput near 3 Mbit/s (the Mathis limit). Frames then queue in the send buffer, and the rate controller, seeing the delay, cuts the bitrate further.
- **BBR v3** keeps its model when it is app-limited, but it still:
  - holds its window to what is in flight for a round trip after each loss;
  - drains to half the BDP every few seconds to re-measure the round trip.
  Both delay frames that the path has room for.
- **Both** fill their window before an outage is noticed. QUIC declares those packets lost only once a newer packet is acknowledged, and its probe timer has backed off to 1.6 s and more by the end of a 2 s blackout. Video resumed 1.3 s after the path came back, and catch-ups piled up extra keyframes.

Moving video to datagrams (the plan's first fallback) would not have helped: QUIC datagrams use the same congestion window and pacer.

## Decision

The endpoint uses `couch_net::congestion::MediaCongestion`, and the per-viewer rate controller is the congestion control:

- **Window.** max(initial window, 2 × the fastest delivery rate of the last 10 s × max(smoothed RTT, 50 ms)). It grows at most twofold per round trip, like slow start.
- **Initial window.** 32 packets (38 KB), as Google QUIC used. With RFC 9002's 10 packets, a stream starting at 10 Mbit/s queued its first frames for four round trips.
- **Pacing.** About 8 × the delivery rate, so a frame leaves in one burst but the sender never runs at line rate.
- **Loss.** Isolated losses change nothing. A sample losing more than 20% forgets older, faster samples. Persistent congestion restarts from the initial window.
- **Silence probes.** With data outstanding and nothing acknowledged for max(4 × smoothed RTT, 100 ms), one extra packet may go out every 50 ms. The first one that gets through after an outage is acknowledged, which declares the stranded packets lost and unblocks the window.

noq's controllers stay selectable (`--congestion bbr3|cubic|newreno`) for comparison.

The rate controller changed alongside this:
- The delay signal is the minimum one-way delay over 400 ms. A retransmission delays only the frames stuck behind it on the epoch stream, while a queue delays every frame.
- Backlog is measured beyond the expected pipeline (a round trip plus the report's age).
- A catch-up aims at 0.85 × the measured delivery rate, between a quarter and half of the old rate. It no longer just halves.
- A catch-up cuts the bitrate only when the path shows congestion (round-trip inflation, loss above 2%, or delay growth). Otherwise it drops the stale video and keeps the bitrate, since the queue is ours.

## Consequences

- **Good:** full bitrate under random loss, recovery from outages in about 100 ms, and exactly one keyframe per outage.
- **Responsibility moves up.** The rate controller must react to real congestion promptly. Round-trip inflation, the loss thresholds (2% hold, 10% cut) and the catch-up rule do that; G5 shows it reacts within about 300 ms. A bug there now floods the path for longer than it would under Cubic, though never beyond twice the recent delivery rate.
- **Fairness.** A delay-based sender yields to loss-based flows (TCP downloads) sharing a bottleneck, as GCC does. Acceptable for a game stream; revisit if users report starvation.
- **Head-of-line blocking under loss is unchanged.** With 1% loss at 20 Mbit/s, the median frame arrives about 13 ms later than without loss, and p95 about 40 ms later. Avoiding that needs forward error correction, not a different congestion controller.
- **`window()` reads the wall clock.** noq asks for the window without a time, and after an outage the only thing that wakes the sender is a packet from the peer, which tells the controller nothing.
