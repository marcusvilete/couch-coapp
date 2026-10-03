//! Framing for the pipe between the main process and `couch-viewer`:
//! `u32 len | u8 kind | payload`, big-endian.
//!
//! The main process writes to the viewer's stdin and reads its stdout, so the
//! viewer must never print to stdout; its logs go to stderr.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    bytes::{Reader, Writer},
    ids::Permissions,
    input::PadSnapshot,
};

/// Bumped whenever an IPC payload changes. The viewer is always shipped with
/// the app, so a mismatch means a broken install, not an old peer.
pub const IPC_VERSION: u16 = 1;

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
    /// main -> viewer: the connection's path, for the HUD.
    pub const NET_STATS: u8 = 0x06;
    /// main -> viewer: whether this member holds a seat, and what it may send.
    pub const SEAT: u8 = 0x07;
    /// main -> viewer: rumble the local gamepad.
    pub const RUMBLE: u8 = 0x08;

    /// viewer -> main: a local input event for the host ([`super::LocalInput`]).
    pub const INPUT: u8 = 0x81;
    /// viewer -> main: decode and present times, for receiver reports.
    pub const FRAME_TIMING: u8 = 0x82;
    /// viewer -> main: something the user did in the HUD (request control, leave).
    pub const USER_ACTION: u8 = 0x83;
    /// viewer -> main: the decoder lost sync and needs a keyframe.
    pub const KEYFRAME_REQUEST: u8 = 0x84;
}

/// Header of a [`kinds::VIDEO_FRAME`] payload; the access unit follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFrameHeader {
    pub epoch: u32,
    pub seq: u32,
    pub keyframe: bool,
    /// Capture time on the host's clock.
    pub capture_us: u64,
    pub encode_us: u32,
    /// When the frame finished arriving, on this machine's clock.
    pub arrived_us: u64,
}

impl VideoFrameHeader {
    pub const LEN: usize = 4 + 4 + 1 + 8 + 4 + 8;

    /// Appends the header and then `access_unit`, forming a complete payload.
    pub fn write(&self, access_unit: &[u8], out: &mut Vec<u8>) {
        out.reserve(Self::LEN + access_unit.len());
        Writer(out)
            .u32(self.epoch)
            .u32(self.seq)
            .u8(u8::from(self.keyframe))
            .u64(self.capture_us)
            .u32(self.encode_us)
            .u64(self.arrived_us);
        out.extend_from_slice(access_unit);
    }

    /// Splits a payload into its header and access unit.
    pub fn read(payload: &[u8]) -> Option<(Self, &[u8])> {
        let mut r = Reader(payload);
        let header = Self {
            epoch: r.u32()?,
            seq: r.u32()?,
            keyframe: r.u8()? != 0,
            capture_us: r.u64()?,
            encode_us: r.u32()?,
            arrived_us: r.u64()?,
        };
        Some((header, r.rest()))
    }
}

/// Payload of [`kinds::FRAME_TIMING`]: what happened to one presented frame.
/// All times are microseconds on this machine's clock except `capture_us`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameTiming {
    pub epoch: u32,
    pub seq: u32,
    pub capture_us: u64,
    pub arrived_us: u64,
    pub decoded_us: u64,
    pub presented_us: u64,
    /// The time read from a latency-clock barcode in the frame; 0 if none.
    pub barcode_us: u64,
}

impl FrameTiming {
    pub const LEN: usize = 4 + 4 + 8 * 5;

    pub fn write(&self, out: &mut Vec<u8>) {
        Writer(out)
            .u32(self.epoch)
            .u32(self.seq)
            .u64(self.capture_us)
            .u64(self.arrived_us)
            .u64(self.decoded_us)
            .u64(self.presented_us)
            .u64(self.barcode_us);
    }

    pub fn read(payload: &[u8]) -> Option<Self> {
        let mut r = Reader(payload);
        Some(Self {
            epoch: r.u32()?,
            seq: r.u32()?,
            capture_us: r.u64()?,
            arrived_us: r.u64()?,
            decoded_us: r.u64()?,
            presented_us: r.u64()?,
            barcode_us: r.u64()?,
        })
    }
}

/// Payload of [`kinds::NET_STATS`], sent a few times a second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NetStats {
    pub rtt_us: u32,
    /// The path goes through a relay server.
    pub relayed: bool,
    /// Video received over the last interval.
    pub receive_bps: u32,
}

impl NetStats {
    pub const LEN: usize = 4 + 1 + 4;

    pub fn write(&self, out: &mut Vec<u8>) {
        Writer(out)
            .u32(self.rtt_us)
            .u8(u8::from(self.relayed))
            .u32(self.receive_bps);
    }

    pub fn read(payload: &[u8]) -> Option<Self> {
        let mut r = Reader(payload);
        Some(Self {
            rtt_us: r.u32()?,
            relayed: r.u8()? != 0,
            receive_bps: r.u32()?,
        })
    }
}

/// Payload of [`kinds::INPUT`] (postcard): one local input event. Mouse
/// motion and wheel are deltas; the main process keeps the running totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalInput {
    /// A USB HID keyboard usage (SDL scancodes are the same numbers).
    Key {
        usage: u16,
        down: bool,
    },
    /// A [`crate::input::mouse_button`] code.
    MouseButton {
        button: u8,
        down: bool,
    },
    MouseMotion {
        dx: i32,
        dy: i32,
    },
    /// In 1/120 notches.
    Wheel {
        dx: i32,
        dy: i32,
    },
    /// The local gamepad's whole state; neutral when it disconnects.
    Pad(PadSnapshot),
    /// Sent every 100 ms from the viewer's window loop. While these stop (the
    /// viewer hung), the main process stops the heartbeat, so the host
    /// releases this member's input.
    Alive,
}

/// Payload of [`kinds::SEAT`] (postcard).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatState {
    pub holding: bool,
    pub permissions: Permissions,
}

/// Payload of [`kinds::RUMBLE`] (postcard): motor strengths, 0 to 255.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RumbleState {
    pub low: u8,
    pub high: u8,
}

/// The postcard encoding of an IPC payload.
pub fn to_payload<T: Serialize>(value: &T) -> Vec<u8> {
    postcard::to_stdvec(value).expect("serializing to a Vec cannot fail")
}

/// Decodes a postcard IPC payload, rejecting trailing bytes.
pub fn from_payload<T: serde::de::DeserializeOwned>(payload: &[u8]) -> Option<T> {
    crate::media::decode_payload(payload)
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
    fn video_frame_payload_roundtrip() {
        let header = VideoFrameHeader {
            epoch: 2,
            seq: 77,
            keyframe: true,
            capture_us: 1_000,
            encode_us: 1_500,
            arrived_us: 9_000,
        };
        let mut payload = Vec::new();
        header.write(b"annex-b", &mut payload);
        assert_eq!(
            VideoFrameHeader::read(&payload),
            Some((header, &b"annex-b"[..]))
        );
    }

    #[test]
    fn frame_timing_roundtrip() {
        let timing = FrameTiming {
            epoch: 1,
            seq: 2,
            capture_us: 3,
            arrived_us: 4,
            decoded_us: 5,
            presented_us: 6,
            barcode_us: 7,
        };
        let mut buf = Vec::new();
        timing.write(&mut buf);
        assert_eq!(buf.len(), FrameTiming::LEN);
        assert_eq!(FrameTiming::read(&buf), Some(timing));
    }

    #[test]
    fn net_stats_roundtrip() {
        let stats = NetStats {
            rtt_us: 42_000,
            relayed: true,
            receive_bps: 2_000_000,
        };
        let mut buf = Vec::new();
        stats.write(&mut buf);
        assert_eq!(buf.len(), NetStats::LEN);
        assert_eq!(NetStats::read(&buf), Some(stats));
    }

    #[test]
    fn input_payloads_roundtrip() {
        for input in [
            LocalInput::Key {
                usage: 0x04,
                down: true,
            },
            LocalInput::MouseMotion { dx: -3, dy: 9 },
            LocalInput::Pad(PadSnapshot {
                buttons: 0x1000,
                left_x: -32768,
                ..Default::default()
            }),
        ] {
            assert_eq!(from_payload(&to_payload(&input)), Some(input));
        }
        let seat = SeatState {
            holding: true,
            permissions: Permissions::GAMEPAD,
        };
        assert_eq!(from_payload(&to_payload(&seat)), Some(seat));
        let mut extra = to_payload(&RumbleState { low: 1, high: 2 });
        extra.push(0);
        assert_eq!(from_payload::<RumbleState>(&extra), None);
    }

    #[test]
    fn zero_length_is_rejected() {
        assert_eq!(decode(&[0, 0, 0, 0]), Err(IpcError::Empty));
    }
}
