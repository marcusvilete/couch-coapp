//! Framing for the pipe between the main process and `couch-viewer`:
//! `u32 len | u8 kind | payload`, big-endian.
//!
//! The main process writes to the viewer's stdin and reads its stdout, so the
//! viewer must never print to stdout; its logs go to stderr.

use thiserror::Error;

/// Bumped whenever an IPC payload changes. The viewer is always shipped with
/// the app, so a mismatch means a broken install, not an old peer.
pub const IPC_VERSION: u16 = 0;

/// Largest frame (kind plus payload). Sized for a high-bitrate keyframe.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// Frame kinds. Main-to-viewer kinds are below 0x80, viewer-to-main kinds above.
pub mod kinds {
    /// main -> viewer, first frame: IPC version and session parameters.
    pub const HELLO: u8 = 0x01;
    /// main -> viewer: one encoded video frame with its header.
    pub const VIDEO_FRAME: u8 = 0x02;
    /// main -> viewer: one Opus packet.
    pub const AUDIO_PACKET: u8 = 0x03;
    /// main -> viewer: HUD state (members, who holds control).
    pub const HUD_STATE: u8 = 0x04;
    /// main -> viewer: close the window and exit.
    pub const SHUTDOWN: u8 = 0x05;

    /// viewer -> main: a local input event for the host.
    pub const INPUT: u8 = 0x81;
    /// viewer -> main: decode and present times, for receiver reports.
    pub const FRAME_TIMING: u8 = 0x82;
    /// viewer -> main: something the user did in the HUD (request control, leave).
    pub const USER_ACTION: u8 = 0x83;
}

/// One decoded IPC frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpcFrame {
    pub kind: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IpcError {
    #[error("IPC frame length {0} exceeds the {MAX_FRAME_LEN}-byte limit")]
    TooLarge(usize),
    #[error("IPC frame length 0 has no kind byte")]
    Empty,
}

/// Appends one encoded frame to `out`.
pub fn encode(kind: u8, payload: &[u8], out: &mut Vec<u8>) -> Result<(), IpcError> {
    let len = 1 + payload.len();
    if len > MAX_FRAME_LEN {
        return Err(IpcError::TooLarge(len));
    }
    out.reserve(4 + len);
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
    Ok(())
}

/// Decodes one frame from the front of `buf`; `Ok(None)` while incomplete.
pub fn decode(buf: &[u8]) -> Result<Option<(IpcFrame, usize)>, IpcError> {
    let Some(len_bytes) = buf.get(..4) else {
        return Ok(None);
    };
    let len = u32::from_be_bytes(len_bytes.try_into().expect("slice of 4 bytes")) as usize;
    if len > MAX_FRAME_LEN {
        return Err(IpcError::TooLarge(len));
    }
    if len == 0 {
        return Err(IpcError::Empty);
    }
    let Some(body) = buf.get(4..4 + len) else {
        return Ok(None);
    };
    let frame = IpcFrame {
        kind: body[0],
        payload: body[1..].to_vec(),
    };
    Ok(Some((frame, 4 + len)))
}

/// Reads one frame from a blocking reader. `Ok(None)` means clean end of
/// stream (the other process closed the pipe) at a frame boundary.
pub fn read_frame(reader: &mut impl std::io::Read) -> std::io::Result<Option<IpcFrame>> {
    use std::io::{Error, ErrorKind};

    let mut len_bytes = [0u8; 4];
    match reader.read_exact(&mut len_bytes) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_bytes) as usize;
    if len == 0 || len > MAX_FRAME_LEN {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("bad IPC frame length {len}"),
        ));
    }
    let mut kind = [0u8; 1];
    reader.read_exact(&mut kind)?;
    let mut payload = vec![0u8; len - 1];
    reader.read_exact(&mut payload)?;
    Ok(Some(IpcFrame {
        kind: kind[0],
        payload,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut buf = Vec::new();
        encode(kinds::VIDEO_FRAME, &[9; 1000], &mut buf).unwrap();
        let (frame, used) = decode(&buf).unwrap().unwrap();
        assert_eq!(used, buf.len());
        assert_eq!(frame.kind, kinds::VIDEO_FRAME);
        assert_eq!(frame.payload, vec![9; 1000]);
    }

    #[test]
    fn read_frame_reports_clean_eof() {
        let mut buf = Vec::new();
        encode(kinds::SHUTDOWN, &[], &mut buf).unwrap();
        let mut reader = buf.as_slice();
        let frame = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(frame.kind, kinds::SHUTDOWN);
        assert!(read_frame(&mut reader).unwrap().is_none());
    }

    #[test]
    fn read_frame_rejects_truncated_body() {
        let mut buf = Vec::new();
        encode(kinds::INPUT, &[1, 2, 3], &mut buf).unwrap();
        buf.pop();
        assert!(read_frame(&mut buf.as_slice()).is_err());
    }

    #[test]
    fn zero_length_is_rejected() {
        assert_eq!(decode(&[0, 0, 0, 0]), Err(IpcError::Empty));
    }
}
