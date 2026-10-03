# ADR 0001: iroh carries all traffic, including media

**Status:** accepted (2026-10-02), to be confirmed by the M1 gates.

## Context

The media research favored WebRTC (`str0m`). The connectivity research favored iroh.

Members are usually behind home routers, often behind CGNAT. Some pairs can never connect directly and need a relay. The project owner will run one small relay rather than depend on third parties.

## Decision

Control, input, audio and video all travel over one iroh (QUIC) connection per pair of members.

## Consequences

- **One NAT-traversal stack.** It includes iroh's hole punching (about 9 pairs in 10 direct), port mapping, IPv6, and a relay fallback the owner can self-host.
- **Little lost from WebRTC.** Its packetizing, retransmission and encryption are already provided by QUIC streams.
- **Bitrate adaptation is our code.** It is a delay-based controller fed by receiver reports and iroh's path statistics ([protocol.md](../protocol.md)). M1 tests it under emulated loss, delay and throttling before anything else is built on it.
- **Browser viewers are not possible** without adding WebRTC later.

## Fallbacks

The media transport sits behind `VideoSender`/`VideoReceiver`, which read and write a transport-neutral `FeedbackSnapshot`.

- **If reliable streams add too much latency under loss:** move video to iroh datagrams with our own retransmission.
- **If iroh's direct paths are unstable under sustained video:** use WebRTC (`str0m`) for media, plus a TURN server on the VPS. Keep iroh for rooms and control.

## Rejected

- **`str0m` tunnelled through iroh datagrams.** Double encryption, and two congestion controllers fighting each other.
- **rust-libp2p.** Lower measured hole-punching success; WebRTC transport still alpha.
