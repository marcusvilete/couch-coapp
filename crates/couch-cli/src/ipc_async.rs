//! Async framing for the viewer pipe (`couch_proto::ipc`), for tokio child
//! processes.

use std::io::{self, ErrorKind};

use couch_proto::ipc::{self, IpcFrame, MAX_FRAME_LEN};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The next frame; `Ok(None)` when the other side closed the pipe.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<IpcFrame>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("bad IPC frame length {len}"),
        ));
    }
    let mut kind = [0u8; 1];
    r.read_exact(&mut kind).await?;
    let mut payload = vec![0u8; len - 1];
    r.read_exact(&mut payload).await?;
    Ok(Some(IpcFrame {
        kind: kind[0],
        payload,
    }))
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    kind: u8,
    payload: &[u8],
    scratch: &mut Vec<u8>,
) -> io::Result<()> {
    scratch.clear();
    ipc::encode(kind, payload, scratch).map_err(|e| io::Error::new(ErrorKind::InvalidInput, e))?;
    w.write_all(scratch).await?;
    w.flush().await
}
