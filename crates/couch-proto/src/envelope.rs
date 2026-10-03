//! Control-stream framing: `u32 len | u16 type | payload`, big-endian.
//!
//! `len` counts the type field plus the payload. The high bit of `type`
//! ([`MUST_UNDERSTAND`]) marks messages a receiver must not ignore.

use thiserror::Error;

/// Largest frame we accept (type field plus payload). Control messages are
/// small; anything bigger is a bug or an attack.
pub const MAX_FRAME_LEN: usize = 64 * 1024;

/// Set on message types the receiver must understand.
pub const MUST_UNDERSTAND: u16 = 0x8000;

/// One decoded control frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub msg_type: u16,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn must_understand(&self) -> bool {
        self.msg_type & MUST_UNDERSTAND != 0
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EnvelopeError {
    #[error("frame length {0} exceeds the {MAX_FRAME_LEN}-byte limit")]
    TooLarge(usize),
    #[error("frame length {0} is shorter than the type field")]
    TooShort(usize),
}

/// Appends one encoded frame to `out`.
pub fn encode(msg_type: u16, payload: &[u8], out: &mut Vec<u8>) -> Result<(), EnvelopeError> {
    let len = 2 + payload.len();
    if len > MAX_FRAME_LEN {
        return Err(EnvelopeError::TooLarge(len));
    }
    out.reserve(4 + len);
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.extend_from_slice(&msg_type.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(())
}

/// Decodes one frame from the front of `buf`.
///
/// Returns `Ok(None)` while `buf` holds less than a whole frame, otherwise the
/// frame and the number of bytes it occupied. A length over [`MAX_FRAME_LEN`]
/// is rejected as soon as the length field arrives, before buffering the body.
pub fn decode(buf: &[u8]) -> Result<Option<(Frame, usize)>, EnvelopeError> {
    let Some(len_bytes) = buf.get(..4) else {
        return Ok(None);
    };
    let len = u32::from_be_bytes(len_bytes.try_into().expect("slice of 4 bytes")) as usize;
    if len > MAX_FRAME_LEN {
        return Err(EnvelopeError::TooLarge(len));
    }
    if len < 2 {
        return Err(EnvelopeError::TooShort(len));
    }
    let Some(body) = buf.get(4..4 + len) else {
        return Ok(None);
    };
    let msg_type = u16::from_be_bytes([body[0], body[1]]);
    let frame = Frame {
        msg_type,
        payload: body[2..].to_vec(),
    };
    Ok(Some((frame, 4 + len)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn roundtrip() {
        let mut buf = Vec::new();
        encode(0x8001, b"hello", &mut buf).unwrap();
        let (frame, used) = decode(&buf).unwrap().unwrap();
        assert_eq!(used, buf.len());
        assert_eq!(frame.msg_type, 0x8001);
        assert!(frame.must_understand());
        assert_eq!(frame.payload, b"hello");
    }

    #[test]
    fn every_strict_prefix_is_incomplete() {
        let mut buf = Vec::new();
        encode(7, b"abc", &mut buf).unwrap();
        for end in 0..buf.len() {
            assert_eq!(decode(&buf[..end]).unwrap(), None, "prefix of {end} bytes");
        }
    }

    #[test]
    fn back_to_back_frames() {
        let mut buf = Vec::new();
        encode(1, b"first", &mut buf).unwrap();
        encode(2, b"", &mut buf).unwrap();
        let (a, used_a) = decode(&buf).unwrap().unwrap();
        let (b, used_b) = decode(&buf[used_a..]).unwrap().unwrap();
        assert_eq!((a.msg_type, a.payload.as_slice()), (1, &b"first"[..]));
        assert_eq!((b.msg_type, b.payload.as_slice()), (2, &b""[..]));
        assert_eq!(used_a + used_b, buf.len());
    }

    #[test]
    fn oversized_length_is_rejected_before_the_body_arrives() {
        let len = (MAX_FRAME_LEN as u32 + 1).to_be_bytes();
        assert_eq!(
            decode(&len),
            Err(EnvelopeError::TooLarge(MAX_FRAME_LEN + 1))
        );
        let payload = vec![0; MAX_FRAME_LEN];
        assert!(encode(1, &payload, &mut Vec::new()).is_err());
    }

    #[test]
    fn length_without_room_for_type_is_rejected() {
        assert_eq!(decode(&[0, 0, 0, 1, 0]), Err(EnvelopeError::TooShort(1)));
    }

    proptest! {
        #[test]
        fn roundtrip_any(msg_type: u16, payload in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let mut buf = Vec::new();
            encode(msg_type, &payload, &mut buf).unwrap();
            let (frame, used) = decode(&buf).unwrap().unwrap();
            prop_assert_eq!(used, buf.len());
            prop_assert_eq!(frame.msg_type, msg_type);
            prop_assert_eq!(frame.payload, payload);
        }

        #[test]
        fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            let _ = decode(&bytes);
        }
    }
}
