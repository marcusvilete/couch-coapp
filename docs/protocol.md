# Protocol

The wire protocol between room members, and the pipe between the main process and `couch-viewer`.

**Status:** draft until the M1 spike validates the media channels. Only the control envelope, `Hello` and the IPC framing are implemented (in `couch-proto`).

## Versioning

- ALPN `couch-coapp/0` until the first public release, then `/1`. Incompatible changes need a new ALPN.
- Both sides send `Hello { proto_minor, app_version, features }` first on the control stream. Optional behaviour is negotiated through `features` bits: send a message type only if the peer advertised it.
- **Released payloads are frozen.** Extend the protocol by adding message types, never by changing an existing payload. Decoders reject trailing bytes, so an accidental change fails loudly.

## Control-stream envelope

```
u32 len (big-endian) | u16 type | payload (postcard)
```

- `len` counts the type field plus the payload, and is capped at 64 KiB.
- **High bit set** (`0x8000`): the message must be understood. An unknown must-understand type closes the connection with the `version` code.
- **High bit clear**: the message is optional, and unknown optional types are skipped.

| Type | Message | Notes |
|---|---|---|
| `0x8001` | `Hello` | First message in both directions |

Message families still to come, all on this stream:
- **Admission:** `AdmitStart`, `AdmitReply`, `AdmitConfirm`, `AdmitResult`.
- **Room:** `RoomSnapshot`, `MemberJoined`/`MemberLeft`, `Presence`, `MemberAddrUpdate`, `RoomPolicy`.
- **Session directory and participation:** `SessionAnnounce`, `SessionUpdate`, `SessionEnded{reason}`, `SessionJoin`, `SessionWelcome`, `SessionLeave`.
- **Control:** `ControlRequest`, `ControlCancel`, `ControlDecision`, `SeatAssigned`, `SeatReleased`, `ControlRevoked`.
- **Media control:** `VideoConfig`, `KeyframeRequest`, `BitrateHint`.
- **Liveness:** `Ping`/`Pong` every second, `Goodbye{reason}`.

## Channels

Each pair of room members shares one iroh connection.

| Channel | QUIC primitive | Direction | Content |
|---|---|---|---|
| Control | Bidirectional stream, opened first by the dialer | both | Envelope messages above |
| Video | One unidirectional stream per epoch | host to viewer | Preamble `{session, epoch, codec, width, height, fps}`, then frames `{seq u32, flags (KEY, START), capture_ts_us u64, encode_us u16, len u32, Annex-B access unit}` |
| Input | Unidirectional stream, highest priority | viewer to host | `KeyEvent{seat_epoch, seq, hid_usage, down}`, `MouseButton{..., cum_dx, cum_dy}`, `HeldKeysSync` bitmap every second |
| `0x01` Audio | Datagram | host to viewer | `{seq, ts (48 kHz), Opus 10 ms}` |
| `0x02` Gamepad | Datagram | viewer to host | `{seat, seat_epoch, seq, buttons, triggers, sticks}` on change and every 100 ms |
| `0x03` Mouse motion | Datagram | viewer to host | `{seat, seat_epoch, seq, cumulative dx, dy, wheel}` |
| `0x04` Receiver report | Datagram | viewer to host | Every 50 ms: `{epoch, bytes received total, per-frame (seq, arrival, decode, present), audio loss, jitter, buffer}` |
| `0x05` Rumble | Datagram | host to viewer | `{seat, low, high}` |

- **Epochs.** An epoch is one keyframe plus the frames that depend on it. With an infinite GOP, every frame depends on the one before, so frames must be decoded in order anyway. Resetting a whole epoch is the only useful discard. See [ADR 0003](adr/0003-epoch-streams-for-video.md).
- **Mouse motion as running totals.** Each datagram carries the running totals, so a lost datagram's motion arrives with the next one. Button events carry the totals too, so the host applies pending motion before the click.
- **Gamepad as full snapshots.** A lost packet can never leave a button stuck.
- **Seat epochs.** Input tagged with anything but the seat's current epoch is dropped, which discards stragglers from a previous holder.

## Bitrate control (per viewer, 20 times a second)

**Inputs**
- From receiver reports:
  - Delivery rate: change in bytes received over time.
  - Outstanding bytes: sent in the epoch but not yet reported.
  - One-way delay trend: arrival time minus capture time, against its 10-second minimum. The clock offset cancels out.
- From iroh's selected path:
  - `Path::rtt()` against its 10-second minimum.
  - Growth in `PathStats.lost_packets` and `congestion_events`.
  - `Path::is_relay()`.

**Control law**
- **Overuse:** delay trend above about 10 ms, round-trip inflation above max(15 ms, 0.5 × minimum round trip), or new losses. Target becomes 0.85 × the delivery rate, held for 500 ms.
- **Otherwise:** grow 8% per second.
- **Clamp:** at least 1.5 Mbit/s and at most the lowest of the user maximum, this viewer's share of the host upload cap, and the relay limit.

**Catch-up rule**
- **Trigger:** outstanding video exceeds max(150 ms, 3 × minimum round trip) for two samples, or no report arrives for 500 ms while data is outstanding.
- **Action:** reset the epoch stream (`SKIP_AHEAD`), halve the target and force a keyframe. At most once a second.

**Starting QUIC settings** (tuned in M1)
- Timeouts: idle timeout 15 s, keep-alive 2 s.
- Streams and windows: 64 concurrent unidirectional streams; stream window 8 MB, connection window 16 MB.
- Datagrams: 64 KB send buffer, so stale audio and input drop rather than queue.
- Congestion control: the default controller, A/B-tested against BBR v3.

**Starting `h264_nvenc` settings**
- Preset p1, tune ull, `rc=cbr`, High profile.
- No B-frames, `delay=0`, `zerolatency=1`, `rc-lookahead=0`.
- `forced-idr=1` with a very long GOP; `bufsize` = bitrate / fps.

**Decoder:** single thread, `AV_CODEC_FLAG_LOW_DELAY`.

## Admission (M4a)

1. TLS handshake. iroh authenticates both endpoint ids.
2. The `after_handshake` hook rejects banned or rate-limited ids.
3. `Hello`.
4. Asymmetric SPAKE2. The dialer is A, the acceptor B. Identities are `"couch/v1" || room_id || endpoint_id`.
5. Key confirmation within 10 s.

**Derivations**
- Password input = `HKDF-SHA256(salt = room_secret, ikm = NFKC(password), info = "couch/pake-pw/v1" || room_id)`.
- Confirmation keys = `HKDF(spake2_key, salt = export_keying_material("couch-coapp pake v1", room_id))`. This binds them to the QUIC connection.
- Each side sends `HMAC(k_A or k_B, H(room_id || id_A || id_B || msg_A || msg_B))`. Compare in constant time.

**Rate limits**
- Per remote id: exponential backoff.
- Per room: 10 failures per hour pause new attempts for 10 minutes.

## Close and reset codes

- **Connection close:** `normal`, `admission_failed`, `banned`, `duplicate`, `version`, `rate_limited`, `room_full`.
- **Stream reset:** `SKIP_AHEAD`, `SESSION_END`, `VIEWER_REMOVED`.

## Viewer IPC

Framing is `u32 len (big-endian) | u8 kind | payload`. `len` counts the kind byte plus the payload and is capped at 16 MiB. The main process writes to the viewer's stdin and reads its stdout. The viewer must never print anything else to stdout.

| Kind | Direction | Meaning |
|---|---|---|
| `0x01` | main to viewer | Hello: IPC version and session parameters |
| `0x02` | main to viewer | Video frame |
| `0x03` | main to viewer | Audio packet |
| `0x04` | main to viewer | HUD state |
| `0x05` | main to viewer | Shutdown |
| `0x81` | viewer to main | Input event |
| `0x82` | viewer to main | Frame timing |
| `0x83` | viewer to main | User action (request control, leave) |

`IPC_VERSION` changes with any payload change. The viewer always ships with the app, so a mismatch means a broken install.
