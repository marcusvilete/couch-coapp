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

/// Each wire revision with the app's major.minor version that first carried
/// it, oldest first. **Raising [`PROTOCOL_MINOR`] means a new row here and a
/// minor version bump** (`cargo xtask bump minor`): builds of different
/// revisions refuse each other, so a patch release must never change it.
/// The test `a_new_wire_revision_comes_with_a_minor_version` holds both.
pub const REVISIONS: &[(u16, &str)] = &[(1, "0.8"), (2, "0.11"), (3, "0.11"), (4, "0.11")];

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

#[cfg(test)]
mod tests {
    use super::*;

    /// `major.minor` of a version, as numbers.
    fn major_minor(version: &str) -> (u32, u32) {
        let mut parts = version
            .split('.')
            .map(|p| p.parse::<u32>().expect("a number"));
        (parts.next().expect("major"), parts.next().expect("minor"))
    }

    /// Builds of different wire revisions refuse each other, so a new one
    /// must come with a new minor version, never in a patch: the last row of
    /// `REVISIONS` is the revision in use, and the app's version is at least
    /// the one that row names. Raise `PROTOCOL_MINOR` and this fails until
    /// both the row and `cargo xtask bump minor` are there.
    #[test]
    fn a_new_wire_revision_comes_with_a_minor_version() {
        let &(last, since) = REVISIONS.last().expect("revisions");
        assert_eq!(
            last, PROTOCOL_MINOR,
            "PROTOCOL_MINOR is {PROTOCOL_MINOR}: add its row to REVISIONS with the next minor version, and bump to it (cargo xtask bump minor)"
        );
        let app = env!("CARGO_PKG_VERSION");
        assert!(
            major_minor(app) >= major_minor(since),
            "revision {last} arrives in {since}, but the app is {app}: cargo xtask bump minor"
        );
        // Oldest first, one row per revision, versions never going back.
        for pair in REVISIONS.windows(2) {
            assert_eq!(pair[1].0, pair[0].0 + 1, "one row per revision");
            assert!(major_minor(pair[1].1) >= major_minor(pair[0].1));
        }
    }
}
