---
name: protocol-change
description: Checklist for adding or changing anything on Couch Co-App's wire or viewer IPC — a control message, an input-stream message, a datagram (input, audio, reports), a stream kind, or an IPC kind or payload in couch_proto. Use whenever editing crates/couch-proto (control.rs, input.rs, media.rs, ipc.rs, envelope.rs, ids.rs) or code that sends or parses those messages in couch-net, couch-cli or couch-viewer.
---

# Protocol change checklist

The wire format is in `docs/protocol.md`. Rules that hold everywhere:
- **Released payloads are frozen.** Extend the protocol by adding a message type, never by changing a released payload. Until the first public release (ALPN `couch-coapp/0`) payloads may still change, but treat each as if it were released unless the user agrees otherwise.
- **Decoders reject trailing bytes:** `control::payload`, `media::decode_payload`, `ipc::from_payload`. Use these helpers; never call `postcard::from_bytes` directly on peer data.
- **Peer data is size-limited:** 64 KiB per control or input frame, 16 MiB per IPC frame, 8 MiB per video frame. Anything a peer sends that sizes an allocation must be checked against a limit.

## Pick the right channel

| Kind of data | Channel | Where |
|---|---|---|
| Rare, must arrive, ordered | Control stream (`u32 len \| u16 type \| postcard`) | `control.rs`: `type_ids`, `ControlMessage`, `encode`, `from_frame` |
| Input that must arrive in order (keys, buttons) | Input stream, same framing | `input.rs`: `type_ids`, `InputMessage` |
| Frequent and disposable: newest state wins | Datagram, `tag u8 \| postcard` | `media.rs`: `datagram` tags; the payload struct where it belongs |
| Bulk, its own lifetime | A new unidirectional stream kind | `media.rs`: `stream_kind`; the first byte of the stream |
| Main process ↔ viewer | IPC kind, `u32 len \| u8 kind \| payload` | `ipc.rs`: `kinds` (below 0x80 to the viewer, above from it) |

A datagram must be safe to lose. Send full state or running totals, as the pad snapshots and mouse totals do, never deltas or one-off events.

**A datagram must fit about 1100 bytes**, tag included. QUIC starts with 1200-byte packets and only discovers a larger path MTU later; relay paths may never get one. A bigger datagram fails to send, and that is logged only at debug level. Cap anything variable-length below that, even if its format allows more: audio packets are capped at 1000 bytes although Opus allows 1275.

## Steps

1. **Type id.**
   - Control families: `0x00xx` session basics, `0x01xx` media control, `0x02xx` seats. Input stream: `0x03xx`.
   - Set `MUST_UNDERSTAND` (`0x8000`) if a peer that doesn't know the message can't work correctly. An unknown must-understand type closes the connection; an unknown optional one is skipped.
   - A message only some peers understand: advertise a feature bit in `Hello.features`, and send the message only to peers that set it.
2. **Payload struct** with `Serialize`/`Deserialize`.
   - No field a peer can use to force a large allocation without a check.
   - Fixed-size arrays over `Vec` when the size is bounded, like `KeyBitmap`.
   - Counters that can wrap are compared with wrapping arithmetic.
3. **Wire it into both directions.**
   - Control and input: the `match` arms in `type_id`, `encode` and `from_frame`.
   - Datagrams: receivers get them only through `couch_net::datagrams::Datagrams::subscribe(tag)`. QUIC has one datagram queue per connection, so never call `read_datagram` directly.
   - Streams: the acceptor reads the kind byte and stops streams it doesn't know.
4. **Tests in `couch-proto`:**
   - a roundtrip through the envelope (or the datagram tag);
   - trailing bytes rejected;
   - for datagrams, a size check at extreme values, under 1100 bytes (as the receiver report and audio datagram tests do);
   - an unknown type is skipped or rejected according to its bit.
5. **IPC:** bump `ipc::IPC_VERSION` on any IPC payload change, and handle the new kind in both `couch-viewer/src/pipe.rs` and `couch-cli/src/view.rs`. Logs and anything else go to stderr, never the viewer's stdout.
6. **Docs:** update `docs/protocol.md` in the same change: the type table, the channel table or the IPC table, plus any rule the receiver enforces.
7. **Verify:**
   - `cargo test -p couch-proto -p couch-net`, plus clippy.
   - **Input:** run the `input-safety-checks` skill.
   - **Video or reports:** run the `rate-control-gates` skill.
   - **Audio:** run the `audio-checks` skill.
