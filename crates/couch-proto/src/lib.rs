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
//! Protocol rules (versioning, frozen payloads) are in `docs/protocol.md`.

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
/// Stays at `/0` until the first public release.
pub const ALPN: &[u8] = b"couch-coapp/0";

/// Minor protocol revision announced in [`control::Hello`]. Peers sharing the
/// ALPN interoperate across minors by advertising features.
pub const PROTOCOL_MINOR: u16 = 0;
