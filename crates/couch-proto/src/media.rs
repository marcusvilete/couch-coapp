//! Media framing on the wire (docs/protocol.md, "Channels").
//!
//! Video travels on one unidirectional stream per epoch: an [`EpochPreamble`],
//! then frames, each a [`FrameHeader`] followed by its access unit. Datagrams
//! start with a one-byte tag from [`datagram`] and carry a postcard payload.

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::bytes::{Reader, Writer};

/// First byte of every unidirectional stream: what the stream carries.
pub mod stream_kind {
    pub const VIDEO_EPOCH: u8 = 0x01;
    pub const INPUT: u8 = 0x02;
}

/// Largest access unit accepted from a peer.
pub const MAX_FRAME_LEN: u32 = 8 * 1024 * 1024;

/// Stream reset codes.
pub mod reset_code {
    /// The host skipped ahead to a new epoch after a stall.
    pub const SKIP_AHEAD: u32 = 1;
    pub const SESSION_END: u32 = 2;
    pub const VIEWER_REMOVED: u32 = 3;
}

/// Opens a video epoch stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpochPreamble {
    pub session: u64,
    pub epoch: u32,
    /// [`crate::codec`]: 1 = H.264, 2 = HEVC.
    pub codec: u8,
    pub width: u16,
    pub height: u16,
    pub fps: u16,
}

impl EpochPreamble {
    /// Including the stream-kind byte.
    pub const LEN: usize = 1 + 1 + 8 + 4 + 1 + 2 + 2 + 2;
    const VERSION: u8 = 0;
    pub const CODEC_H264: u8 = crate::codec::H264;
    pub const CODEC_HEVC: u8 = crate::codec::HEVC;

    pub fn write(&self, out: &mut Vec<u8>) {
        Writer(out)
            .u8(stream_kind::VIDEO_EPOCH)
            .u8(Self::VERSION)
            .u64(self.session)
            .u32(self.epoch)
            .u8(self.codec)
            .u16(self.width)
            .u16(self.height)
            .u16(self.fps);
    }

    /// Parses a preamble, including the stream-kind byte.
    pub fn read(buf: &[u8; Self::LEN]) -> Option<Self> {
        let mut r = Reader(buf);
        if r.u8()? != stream_kind::VIDEO_EPOCH || r.u8()? != Self::VERSION {
            return None;
        }
        Some(Self {
            session: r.u64()?,
            epoch: r.u32()?,
            codec: r.u8()?,
            width: r.u16()?,
            height: r.u16()?,
            fps: r.u16()?,
        })
    }
}

/// Precedes each access unit on an epoch stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub seq: u32,
    pub keyframe: bool,
    /// Capture time on the host's clock, microseconds.
    pub capture_us: u64,
    pub encode_us: u32,
    pub len: u32,
}

impl FrameHeader {
    pub const LEN: usize = 4 + 1 + 8 + 4 + 4;
    const FLAG_KEY: u8 = 1;

    pub fn write(&self, out: &mut Vec<u8>) {
        Writer(out)
            .u32(self.seq)
            .u8(if self.keyframe { Self::FLAG_KEY } else { 0 })
            .u64(self.capture_us)
            .u32(self.encode_us)
            .u32(self.len);
    }

    /// Parses a header; `None` if it announces more than [`MAX_FRAME_LEN`].
    pub fn read(buf: &[u8; Self::LEN]) -> Option<Self> {
        let mut r = Reader(buf);
        let header = Self {
            seq: r.u32()?,
            keyframe: r.u8()? & Self::FLAG_KEY != 0,
            capture_us: r.u64()?,
            encode_us: r.u32()?,
            len: r.u32()?,
        };
        (header.len <= MAX_FRAME_LEN).then_some(header)
    }
}

/// Datagram tags.
pub mod datagram {
    pub const AUDIO: u8 = 0x01;
    pub const GAMEPAD: u8 = 0x02;
    pub const MOUSE_MOTION: u8 = 0x03;
    pub const RECEIVER_REPORT: u8 = 0x04;
    pub const RUMBLE: u8 = 0x05;
}

/// Audio on the wire: Opus, 48 kHz stereo, one 10 ms frame per datagram.
pub mod audio {
    pub const SAMPLE_RATE: u32 = 48_000;
    pub const CHANNELS: u16 = 2;
    /// Samples per channel in one packet (10 ms).
    pub const FRAME_SAMPLES: usize = 480;
    pub const FRAME_US: u64 = 10_000;
    /// Largest packet accepted. Below Opus's own 1275-byte limit so that a
    /// datagram fits QUIC's initial 1200-byte packets, before path MTU
    /// discovery and on relays. 10 ms at Opus's 510 kbit/s maximum is 638
    /// bytes; our 96 kbit/s packets are 120.
    pub const MAX_PACKET: usize = 1000;
}

/// Datagram [`datagram::AUDIO`], host to viewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDatagram {
    /// One more for every packet. A gap is a lost packet, which the viewer
    /// conceals; the host doesn't send while its source is silent.
    pub seq: u32,
    /// Capture time of the packet's first sample on the host's clock,
    /// microseconds, like a video frame's `capture_us`.
    pub capture_us: u64,
    /// One Opus packet of one 10 ms frame, at most [`audio::MAX_PACKET`] bytes.
    pub packet: Vec<u8>,
}

impl AudioDatagram {
    /// Decodes a payload, rejecting packets longer than Opus allows.
    pub fn decode(payload: &[u8]) -> Option<Self> {
        decode_payload::<Self>(payload).filter(|d| d.packet.len() <= audio::MAX_PACKET)
    }
}

/// Sent by a viewer every 50 ms; the host's rate controller runs on it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReceiverReport {
    /// The newest epoch the viewer has seen.
    pub epoch: u32,
    /// Video bytes received in that epoch (headers included). Measures what
    /// is still in flight; reset epochs never arrive, so they don't count.
    pub epoch_bytes_received: u64,
    /// All video bytes received on this connection, for the delivery rate.
    pub total_bytes_received: u64,
    pub frames_received: u64,
    /// Arrival minus capture time for recent frames, microseconds. The clock
    /// offset between machines is unknown but constant, so trends are valid.
    pub delays: Vec<DelaySample>,
    pub decode_errors: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelaySample {
    pub seq: u32,
    pub one_way_us: i64,
}

/// Most delay samples per report, keeping it inside one datagram.
pub const MAX_DELAY_SAMPLES: usize = 32;

/// `tag` followed by the postcard encoding of `message`.
pub fn encode_datagram<T: Serialize>(tag: u8, message: &T) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend(postcard::to_stdvec(message).expect("serializing to a Vec cannot fail"));
    out
}

/// Splits a datagram into its tag and payload.
pub fn datagram_tag(buf: &[u8]) -> Option<(u8, &[u8])> {
    buf.split_first().map(|(tag, rest)| (*tag, rest))
}

/// Decodes a datagram payload. Trailing bytes are rejected, as on the control
/// stream, so an accidental payload change fails loudly.
pub fn decode_payload<T: DeserializeOwned>(payload: &[u8]) -> Option<T> {
    match postcard::take_from_bytes(payload) {
        Ok((value, [])) => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preamble_roundtrip() {
        let p = EpochPreamble {
            session: 9,
            epoch: 3,
            codec: EpochPreamble::CODEC_H264,
            width: 1920,
            height: 1080,
            fps: 60,
        };
        let mut buf = Vec::new();
        p.write(&mut buf);
        assert_eq!(buf.len(), EpochPreamble::LEN);
        assert_eq!(
            EpochPreamble::read(buf.as_slice().try_into().unwrap()),
            Some(p)
        );
    }

    #[test]
    fn frame_header_roundtrip_and_size_limit() {
        let h = FrameHeader {
            seq: 42,
            keyframe: true,
            capture_us: 123_456_789,
            encode_us: 2_100,
            len: 50_000,
        };
        let mut buf = Vec::new();
        h.write(&mut buf);
        assert_eq!(
            FrameHeader::read(buf.as_slice().try_into().unwrap()),
            Some(h)
        );

        let mut big = Vec::new();
        FrameHeader {
            len: MAX_FRAME_LEN + 1,
            ..h
        }
        .write(&mut big);
        assert_eq!(FrameHeader::read(big.as_slice().try_into().unwrap()), None);
    }

    #[test]
    fn receiver_report_fits_a_datagram() {
        let report = ReceiverReport {
            epoch: 1,
            epoch_bytes_received: u64::MAX,
            total_bytes_received: u64::MAX,
            frames_received: u64::MAX,
            delays: vec![
                DelaySample {
                    seq: u32::MAX,
                    one_way_us: i64::MIN
                };
                MAX_DELAY_SAMPLES
            ],
            decode_errors: u32::MAX,
        };
        let datagram = encode_datagram(datagram::RECEIVER_REPORT, &report);
        assert!(datagram.len() < 1100, "{} bytes", datagram.len());
        let (tag, payload) = datagram_tag(&datagram).unwrap();
        assert_eq!(tag, datagram::RECEIVER_REPORT);
        assert_eq!(decode_payload::<ReceiverReport>(payload), Some(report));
    }

    #[test]
    fn audio_datagram_roundtrip_and_limits() {
        let audio = AudioDatagram {
            seq: u32::MAX,
            capture_us: u64::MAX,
            packet: vec![0xAB; audio::MAX_PACKET],
        };
        let datagram = encode_datagram(datagram::AUDIO, &audio);
        // The largest packet fits a datagram in QUIC's initial 1200-byte
        // packets, like the receiver report.
        assert!(datagram.len() < 1100, "{} bytes", datagram.len());
        let (tag, payload) = datagram_tag(&datagram).unwrap();
        assert_eq!(tag, datagram::AUDIO);
        assert_eq!(AudioDatagram::decode(payload), Some(audio.clone()));

        let too_long = AudioDatagram {
            packet: vec![0; audio::MAX_PACKET + 1],
            ..audio.clone()
        };
        let datagram = encode_datagram(datagram::AUDIO, &too_long);
        assert_eq!(AudioDatagram::decode(&datagram[1..]), None);

        let mut trailing = encode_datagram(datagram::AUDIO, &audio);
        trailing.push(0);
        assert_eq!(AudioDatagram::decode(&trailing[1..]), None);
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut datagram = encode_datagram(datagram::RECEIVER_REPORT, &ReceiverReport::default());
        datagram.push(0);
        let (_, payload) = datagram_tag(&datagram).unwrap();
        assert_eq!(decode_payload::<ReceiverReport>(payload), None);
    }
}
