# ADR 0003: Video goes on one QUIC stream per epoch

**Status:** accepted (2026-10-02), to be confirmed by the M1 gates.

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

## Future

The frame header is self-describing, so other layouts stay inside the transport implementation. If temporal layers or reference-frame recovery arrive, each independently discardable unit gets its own stream.
