//! Pictures on the board (M27e, docs/protocol.md, Drawing on the stream):
//! each picture on a unidirectional stream of its own, a member's to the
//! host (`stream_kind::PICTURE_UP`, [`PictureUpload`]) and the host's to each
//! viewer (`stream_kind::PICTURE`, [`PictureHeader`]), never the control
//! stream, whose frames are 64 KiB.
//!
//! A stream is its kind byte, its header ([`couch_proto::board::write_header`])
//! and the picture's bytes, then its end. Reading takes the header and the
//! bytes it announces, at most [`MAX_PICTURE_BYTES`], within [`READ_WITHIN`],
//! and nothing after them; whatever else is refused and the stream stopped.

use std::time::Duration;

use couch_proto::{
    board::{MAX_PICTURE_BYTES, MAX_PICTURE_HEADER, PictureHeader, PictureUpload, header_len},
    media::decode_payload,
};
use iroh::endpoint::{Connection, RecvStream, VarInt};
use serde::{Serialize, de::DeserializeOwned};

/// A picture must arrive whole this soon: a megabyte over a slow upload, with
/// room to spare. QUIC already caps how many streams a peer has open.
pub const READ_WITHIN: Duration = Duration::from_secs(60);

/// Sends one picture on a stream of its own: `kind`, `header`, `bytes`.
pub async fn send<H: Serialize>(
    conn: &Connection,
    kind: u8,
    header: &H,
    bytes: &[u8],
) -> Result<(), crate::NetError> {
    let error = |e: &dyn std::fmt::Display| crate::NetError::Connection(e.to_string());
    let mut head = vec![kind];
    head.extend_from_slice(&couch_proto::board::write_header(header));
    // Within the time a reader gives a picture: a peer that grants no room
    // or never reads holds nothing of ours past it.
    let send = async {
        let mut stream = conn.open_uni().await.map_err(|e| error(&e))?;
        stream.write_all(&head).await.map_err(|e| error(&e))?;
        stream.write_all(bytes).await.map_err(|e| error(&e))?;
        stream.finish().map_err(|e| error(&e))?;
        // Wait for the peer to have it all: dropping the stream sooner could
        // cut it short.
        let _ = stream.stopped().await;
        Ok(())
    };
    tokio::time::timeout(READ_WITHIN, send)
        .await
        .map_err(|_| crate::NetError::Connection("a picture took too long to send".into()))?
}

/// A member's picture header, from a stream positioned after its kind byte:
/// the header, well formed ([`PictureUpload::is_valid`]), and the stream
/// positioned at its bytes (P17). The host asks whether it would take the
/// picture before it reads them: [`UploadBytes::read`] or
/// [`UploadBytes::refuse`]. Both within [`READ_WITHIN`] of the stream's start.
pub async fn read_upload_header(mut recv: RecvStream) -> Option<(PictureUpload, UploadBytes)> {
    let deadline = tokio::time::Instant::now() + READ_WITHIN;
    let head = tokio::time::timeout_at(
        deadline,
        read_head(&mut recv, |h: &PictureUpload| h.is_valid().then_some(h.len)),
    )
    .await
    .ok()
    .flatten();
    let Some((header, len)) = head else {
        let _ = recv.stop(VarInt::from_u32(0));
        return None;
    };
    Some((
        header,
        UploadBytes {
            recv,
            len,
            deadline,
        },
    ))
}

/// A member's picture's bytes, not read yet (P17).
#[derive(Debug)]
pub struct UploadBytes {
    recv: RecvStream,
    len: usize,
    deadline: tokio::time::Instant,
}

impl UploadBytes {
    /// The bytes its header announced, and nothing after them.
    pub async fn read(mut self) -> Option<Vec<u8>> {
        let got = tokio::time::timeout_at(self.deadline, read_body(&mut self.recv, self.len))
            .await
            .ok()
            .flatten();
        if got.is_none() {
            let _ = self.recv.stop(VarInt::from_u32(0));
        }
        got
    }

    /// Not read: the stream stopped, so the member's sender stops too.
    pub fn refuse(mut self) {
        let _ = self.recv.stop(VarInt::from_u32(0));
    }
}

/// A picture from the host, from a stream positioned after its kind byte.
pub async fn read_picture(mut recv: RecvStream) -> Option<(PictureHeader, Vec<u8>)> {
    let read = async {
        let (header, len) =
            read_head(&mut recv, |h: &PictureHeader| h.is_valid().then_some(h.len)).await?;
        let bytes = read_body(&mut recv, len).await?;
        Some((header, bytes))
    };
    let got = tokio::time::timeout(READ_WITHIN, read).await.ok().flatten();
    if got.is_none() {
        let _ = recv.stop(VarInt::from_u32(0));
    }
    got
}

/// A header, and the length of the bytes it announces, within the limit.
async fn read_head<H: DeserializeOwned>(
    recv: &mut RecvStream,
    len_of: impl Fn(&H) -> Option<u32>,
) -> Option<(H, usize)> {
    let mut prefix = [0u8; 2];
    recv.read_exact(&mut prefix).await.ok()?;
    let header_len = header_len(prefix)?;
    debug_assert!(header_len <= MAX_PICTURE_HEADER);
    let mut header = vec![0u8; header_len];
    recv.read_exact(&mut header).await.ok()?;
    let header: H = decode_payload(&header)?;
    let len = len_of(&header)? as usize;
    (len <= MAX_PICTURE_BYTES).then_some((header, len))
}

/// `len` bytes, then the stream's end: nothing after them.
async fn read_body(recv: &mut RecvStream, len: usize) -> Option<Vec<u8>> {
    let mut bytes = vec![0u8; len];
    recv.read_exact(&mut bytes).await.ok()?;
    let rest = recv.read_to_end(0).await.ok()?;
    rest.is_empty().then_some(bytes)
}
