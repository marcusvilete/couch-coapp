# ADR 0003: Video goes on one QUIC stream per epoch

**Status:** accepted (2026-10-02). The M1 gates confirmed it on 2026-10-03; see [m1-results.md](../m1-results.md).

## Context

The encoder uses an infinite GOP: a keyframe only on demand, every other frame depending on the one before. The viewer must therefore decode frames in order. The one useful discard is a whole dependency chain, followed by a new keyframe.

## Decision

An epoch is one keyframe plus the frames that depend on it. Each epoch gets one unidirectional QUIC stream: a preamble, then length-prefixed frames.

When the host decides to skip ahead after a stall (the catch-up rule in [protocol.md](../protocol.md)), it does two things:
- resets the old stream with `SKIP_AHEAD`;
- opens a new one that starts with a keyframe.

## Consequences

- **Ordering is free.** Reliable, in-order delivery within the stream is exactly what decoding needs, so no reorder buffer is required.
- **No stream churn.** Opening a stream per frame would mean 60 to 240 new streams a second, each needing stream credit exactly when the link is congested.
- **Natural reconfiguration points.** Resolution and codec changes need a new keyframe anyway, so they start a new epoch.
- **Loss holds up later frames.** A lost packet holds up every later frame on the stream for about a round trip, until its retransmission arrives. Per-frame streams wouldn't help: the decoder needs the frames in order anyway.
  - At 1% loss, a 20 ms one-way delay and 20 Mbit/s, M1 measured a median 13 ms and a p95 40 ms above the loss-free figures.
  - The rate controller ignores these stalls ([ADR 0005](0005-media-congestion-control.md)).
  - Forward error correction is the way to remove them, if real networks need it.

## Future

The frame header is self-describing, so other layouts stay inside the transport implementation. If temporal layers or reference-frame recovery arrive, each independently discardable unit gets its own stream.
