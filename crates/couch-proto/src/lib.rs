//! Wire and IPC vocabulary shared by every Couch Co-App process.
//!
//! - [`envelope`]: control-stream framing (`u32 len | u16 type | payload`).
//! - [`control`]: typed control messages and their type ids.
//! - [`ipc`]: framing for the pipe between the main process and `couch-viewer`.
//! - [`ids`]: identifiers and permission flags that appear on the wire.
//!
//! Protocol rules (versioning, frozen payloads) are in `docs/protocol.md`.

mod bytes;
pub mod control;
pub mod envelope;
pub mod ids;
pub mod ipc;
pub mod media;

/// ALPN protocol id for connections between room members.
///
/// Stays at `/0` until the first public release.
pub const ALPN: &[u8] = b"couch-coapp/0";

/// Minor protocol revision announced in [`control::Hello`]. Peers sharing the
/// ALPN interoperate across minors by advertising features.
pub const PROTOCOL_MINOR: u16 = 0;
