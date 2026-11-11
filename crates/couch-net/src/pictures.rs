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

/// A member's picture, from a stream positioned after its kind byte: the
/// header, well formed ([`PictureUpload::is_valid`]), and its bytes.
pub async fn read_upload(recv: RecvStream) -> Option<(PictureUpload, Vec<u8>)> {
    read(recv, |h: &PictureUpload| h.is_valid().then_some(h.len)).await
}

/// A picture from the host, from a stream positioned after its kind byte.
pub async fn read_picture(recv: RecvStream) -> Option<(PictureHeader, Vec<u8>)> {
    read(recv, |h: &PictureHeader| h.is_valid().then_some(h.len)).await
}

async fn read<H: DeserializeOwned>(
    mut recv: RecvStream,
    len_of: impl Fn(&H) -> Option<u32>,
) -> Option<(H, Vec<u8>)> {
    let read = async {
        let mut prefix = [0u8; 2];
        recv.read_exact(&mut prefix).await.ok()?;
        let header_len = header_len(prefix)?;
        debug_assert!(header_len <= MAX_PICTURE_HEADER);
        let mut header = vec![0u8; header_len];
        recv.read_exact(&mut header).await.ok()?;
        let header: H = decode_payload(&header)?;
        let len = len_of(&header)? as usize;
        if len > MAX_PICTURE_BYTES {
            return None;
        }
        let mut bytes = vec![0u8; len];
        recv.read_exact(&mut bytes).await.ok()?;
        // Nothing after the bytes: the stream ends there.
        let rest = recv.read_to_end(0).await.ok()?;
        rest.is_empty().then_some((header, bytes))
    };
    let got = tokio::time::timeout(READ_WITHIN, read).await.ok().flatten();
    if got.is_none() {
        let _ = recv.stop(VarInt::from_u32(0));
    }
    got
}
