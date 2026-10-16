//! Wire and IPC vocabulary shared by every Couch Co-App process.
//!
//! - [`envelope`]: control-stream framing (`u32 len | u16 type | payload`).
//! - [`control`]: typed control messages, their type ids, close codes.
//! - [`admission`]: the password check's messages.
//! - [`room`]: member records, the owner's policy, snapshots, presence.
//! - [`session`]: sessions (a member streaming to others) and control requests.
//! - [`invite`]: invite links.
//! - [`ipc`]: framing for the pipe between the main process and `couch-viewer`.
//! - [`ids`]: identifiers and permission flags that appear on the wire.
//! - [`media`]: video framing and datagram tags.
//! - [`input`]: input stream messages and input datagrams.
//!
//! Protocol rules (the wire revision, changing payloads) are in
//! `docs/protocol.md`.

pub mod admission;
mod bytes;
pub mod control;
pub mod envelope;
pub mod ids;
pub mod input;
pub mod invite;
pub mod ipc;
pub mod media;
pub mod room;
pub mod session;

/// ALPN protocol id for connections between room members.
///
/// Stays at `/0`: there is no public binary release (ADR 0009), and
/// [`PROTOCOL_MINOR`] catches incompatible builds.
pub const ALPN: &[u8] = b"couch-coapp/0";

/// The wire revision announced in [`control::Hello`] (M10a).
///
/// Payloads change in place (for good, ADR 0009), so builds talk only to
/// builds of the same revision: a peer with another one is refused right
/// after `Hello`, and the app says who needs to update. **Bump it with
/// every change to what members exchange** (the `protocol-change` skill).
///
/// 1: the first installer (0.8.0). Builds before it sent 0 and didn't check.
/// 2: HEVC (M12c): `SessionJoin` says which codecs the viewer decodes, and an
/// epoch's preamble may name HEVC.
/// 3: sessions before streams (M19): `SessionStatus` says whether anything is
/// streamed, and an epoch may end with `STREAM_END`.
/// 4: several seats for one viewer (M18): `ViewerControllers`, a viewer on
/// several seats in `SessionStatus`, and giving back one seat.
pub const PROTOCOL_MINOR: u16 = 4;

/// Video codecs on the wire: in an epoch's preamble and the viewer's IPC
/// frame header, and as bits in what a viewer decodes (`SessionJoin`).
pub mod codec {
    pub const H264: u8 = 1;
    /// HEVC Main, 8-bit (M12c).
    pub const HEVC: u8 = 2;

    /// What a viewer decodes, as bits: H.264 always.
    pub const DECODES_H264: u8 = 1 << 0;
    pub const DECODES_HEVC: u8 = 1 << 1;
}
