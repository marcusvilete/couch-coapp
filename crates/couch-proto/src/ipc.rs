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
pub const IPC_VERSION: u16 = 10;

/// Largest frame (kind plus payload). Sized for a high-bitrate keyframe.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// Frame kinds. Main-to-viewer kinds are below 0x80, viewer-to-main kinds above.
pub mod kinds {
    /// main -> viewer, first frame: IPC version and session parameters.
    pub const HELLO: u8 = 0x01;
    /// main -> viewer: one encoded video frame with its header.
    pub const VIDEO_FRAME: u8 = 0x02;
    /// main -> viewer: one Opus packet ([`super::AudioPacket`]).
    pub const AUDIO_PACKET: u8 = 0x03;
    /// main -> viewer: the session as the HUD shows it ([`super::HudState`]).
    pub const HUD_STATE: u8 = 0x04;
    /// main -> viewer: close the window and exit.
    pub const SHUTDOWN: u8 = 0x05;
    /// main -> viewer: the connection's path, for the HUD.
    pub const NET_STATS: u8 = 0x06;
    /// main -> viewer: whether this member holds a seat, and what it may send.
    pub const SEAT: u8 = 0x07;
    /// main -> viewer: rumble the local gamepad.
    pub const RUMBLE: u8 = 0x08;
    /// main -> viewer, right after `HELLO`: the user's shortcut keys
    /// ([`super::ShortcutKeys`]). Until it arrives, the defaults apply.
    pub const SHORTCUTS: u8 = 0x09;

    /// viewer -> main: a local input event for the host ([`super::LocalInput`]).
    pub const INPUT: u8 = 0x81;
    /// viewer -> main: decode and present times, for receiver reports.
    pub const FRAME_TIMING: u8 = 0x82;
    /// viewer -> main: something the user asked of the session ([`super::UserAction`]).
    pub const USER_ACTION: u8 = 0x83;
    /// viewer -> main: the decoder lost sync and needs a keyframe.
    pub const KEYFRAME_REQUEST: u8 = 0x84;
    /// viewer -> main: audio playback counters ([`super::AudioStats`]), every second.
    pub const AUDIO_STATS: u8 = 0x85;
}

/// Header of a [`kinds::VIDEO_FRAME`] payload; the access unit follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFrameHeader {
    pub epoch: u32,
    pub seq: u32,
    pub keyframe: bool,
    /// The stream's codec ([`crate::codec`]): the viewer opens a decoder
    /// for it, and reopens at a keyframe of another (M12c).
    pub codec: u8,
    /// Capture time on the host's clock.
    pub capture_us: u64,
    pub encode_us: u32,
    /// When the frame finished arriving, on this machine's clock.
    pub arrived_us: u64,
}

impl VideoFrameHeader {
    pub const LEN: usize = 4 + 4 + 1 + 1 + 8 + 4 + 8;

    /// Appends the header and then `access_unit`, forming a complete payload.
    pub fn write(&self, access_unit: &[u8], out: &mut Vec<u8>) {
        out.reserve(Self::LEN + access_unit.len());
        Writer(out)
            .u32(self.epoch)
            .u32(self.seq)
            .u8(u8::from(self.keyframe))
            .u8(self.codec)
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
            codec: r.u8()?,
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
    /// One local controller's whole state, by its slot (M18): a controller
    /// keeps its slot while connected, and one that connects takes the
    /// lowest free slot, so the others never move. Each one counts as
    /// touched, and as connected.
    Pad {
        controller: u8,
        pad: PadSnapshot,
    },
    /// The slots with a controller connected now, as bits (bit n: slot n),
    /// after every change (M18). A slot that went leaves its seat, if it
    /// drove one of several.
    ControllerSlots(u8),
    /// Sent every 100 ms from the viewer's window loop. While these stop (the
    /// viewer hung), the main process stops the heartbeat, so the host
    /// releases this member's input.
    Alive,
}

/// Payload of [`kinds::SEAT`] (postcard): every seat this member holds, in
/// seat order (M18: possibly several), with what each lets it send. Empty
/// while it holds none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatState {
    pub seats: Vec<HeldSeat>,
}

/// One seat in [`SeatState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldSeat {
    /// A [`crate::ids::SeatId`]'s number.
    pub seat: u8,
    pub permissions: Permissions,
    /// The controller slot driving its pad, as the main process decided
    /// (`couch_input::forward`); `None` without a pad or a controller. The
    /// window rumbles and shows by it.
    pub controller: Option<u8>,
}

impl SeatState {
    pub fn holding(&self) -> bool {
        !self.seats.is_empty()
    }

    /// Whether some held seat lets this member send `permission`.
    pub fn may(&self, permission: Permissions) -> bool {
        self.seats
            .iter()
            .any(|s| s.permissions.contains(permission))
    }
}

/// Payload of [`kinds::HUD_STATE`] (postcard): the session as the viewer's
/// HUD shows it. Sent on every change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HudState {
    /// What the host called the session; also the window's title.
    pub title: String,
    /// The host's display name.
    pub host: String,
    pub role: HudRole,
    /// The session's seats, by seat id.
    pub seats: Vec<HudSeat>,
    /// The host's own controller plays only through the seats: a seat
    /// nobody holds is empty, and the host may play none.
    pub host_on_seats: bool,
    pub viewers: u8,
    /// The host took control back with the panic key; asking is pointless
    /// until the host allows control again.
    pub control_paused: bool,
    /// The streamed window is in front on the host. While it isn't, keyboard
    /// and mouse don't reach the game, and many games ignore controllers.
    pub game_in_front: bool,
    /// Whether anything is streamed (M19). While not, the window shows black
    /// and says so; seats stay, and input is held off on the host.
    pub stream: crate::session::StreamState,
    /// A short line to show for a few seconds, such as "request declined";
    /// empty for none.
    pub note: String,
}

/// One seat as the HUD shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HudSeat {
    /// Such as "Controller 2" or "Keyboard and mouse".
    pub label: String,
    /// Its holder drives a virtual controller.
    pub pad: bool,
    /// Its holder may use keyboard and mouse.
    pub keyboard_mouse: bool,
    /// Its holder's display name, the host's included; `None` while free.
    pub holder: Option<String>,
    /// The host plays it with their own controller.
    pub host: bool,
    /// This viewer holds it.
    pub mine: bool,
    /// This viewer asked for it and waits for the host.
    pub asked: bool,
}

/// Where this viewer stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HudRole {
    Watching,
    /// Asked for control; waiting for the host.
    Asking,
    Controlling,
}

/// Payload of [`kinds::USER_ACTION`] (postcard): something the user asked of
/// the session from the viewer (a hotkey or the HUD's bar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserAction {
    /// Ask for this seat (by [`crate::ids::SeatId`]'s number).
    RequestControl(u8),
    /// Withdraw the request, or give every seat back.
    CancelControl,
    /// Give this seat back, keeping the others (M18).
    GiveBack(u8),
    /// Stop watching.
    Leave,
}

/// Payload of [`kinds::RUMBLE`] (postcard): motor strengths, 0 to 255, for
/// the controller that drives `seat` (M18).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RumbleState {
    pub seat: u8,
    pub low: u8,
    pub high: u8,
}

/// Payload of [`kinds::SHORTCUTS`] (postcard): the shortcut rows the user
/// moved off their defaults, by row id (`couch_input::shortcuts`), with the
/// HID usage of each one's key. The viewer checks them as the app does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutKeys {
    pub keys: std::collections::BTreeMap<String, u16>,
}

/// Payload of [`kinds::AUDIO_PACKET`] (postcard): an audio datagram as it
/// arrived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioPacket {
    pub seq: u32,
    /// Capture time on the host's clock.
    pub capture_us: u64,
    /// When the datagram arrived, on this machine's clock.
    pub arrived_us: u64,
    pub packet: Vec<u8>,
}

/// Payload of [`kinds::AUDIO_STATS`] (postcard): playback since the viewer
/// started. Counts are in 10 ms packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AudioStats {
    /// Packets played as received.
    pub played: u64,
    /// Packets missing at their turn and replaced by Opus's concealment.
    pub concealed: u64,
    /// Packets that arrived after their turn and were dropped.
    pub late: u64,
    /// Packets dropped to bring a too-full buffer back down.
    pub dropped: u64,
    /// Times the buffer ran dry and playback waited for it to refill.
    pub underruns: u64,
    /// Buffered audio and the buffer's current target, microseconds.
    pub buffered_us: u32,
    pub target_us: u32,
    /// Capture to handing the samples to the audio device, microseconds:
    /// the median over the last second, and the 95th percentile over the
    /// session. They compare clocks of two machines, so they only mean
    /// something when host and viewer run on one machine.
    pub capture_to_queue_us: u32,
    pub capture_to_queue_p95_us: u32,
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
            codec: crate::codec::HEVC,
            capture_us: 1_000,
            encode_us: 1_500,
            arrived_us: 9_000,
        };
        let mut payload = Vec::new();
        header.write(b"annex-b", &mut payload);
        assert_eq!(payload.len(), VideoFrameHeader::LEN + 7);
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
            LocalInput::Pad {
                controller: 1,
                pad: PadSnapshot {
                    buttons: 0x1000,
                    left_x: -32768,
                    ..Default::default()
                },
            },
            LocalInput::ControllerSlots(0b101),
        ] {
            assert_eq!(from_payload(&to_payload(&input)), Some(input));
        }
        let seat = SeatState {
            seats: vec![
                HeldSeat {
                    seat: 0,
                    permissions: Permissions::GAMEPAD,
                    controller: Some(2),
                },
                HeldSeat {
                    seat: 2,
                    permissions: Permissions::KEYBOARD | Permissions::MOUSE,
                    controller: None,
                },
            ],
        };
        assert_eq!(from_payload(&to_payload(&seat)), Some(seat.clone()));
        assert!(seat.holding() && seat.may(Permissions::KEYBOARD));
        assert!(!SeatState::default().holding());
        let mut extra = to_payload(&RumbleState {
            seat: 1,
            low: 1,
            high: 2,
        });
        extra.push(0);
        assert_eq!(from_payload::<RumbleState>(&extra), None);
    }

    #[test]
    fn session_payloads_roundtrip() {
        let hud = HudState {
            title: "Celeste".into(),
            host: "Ana".into(),
            role: HudRole::Asking,
            seats: vec![
                HudSeat {
                    label: "Controller 1".into(),
                    pad: true,
                    keyboard_mouse: false,
                    holder: Some("Bia".into()),
                    host: false,
                    mine: false,
                    asked: true,
                },
                HudSeat {
                    label: "Controller 2".into(),
                    pad: true,
                    keyboard_mouse: false,
                    holder: Some("Ana".into()),
                    host: true,
                    mine: false,
                    asked: false,
                },
            ],
            host_on_seats: true,
            viewers: 3,
            control_paused: false,
            game_in_front: false,
            stream: crate::session::StreamState::Stopped(crate::session::EndReason::GameExited),
            note: "request declined".into(),
        };
        assert_eq!(from_payload(&to_payload(&hud)), Some(hud));
        for action in [
            UserAction::RequestControl(1),
            UserAction::CancelControl,
            UserAction::Leave,
        ] {
            assert_eq!(from_payload(&to_payload(&action)), Some(action));
        }
        let keys = ShortcutKeys {
            keys: [("panic".to_string(), 0x13)].into(),
        };
        assert_eq!(from_payload(&to_payload(&keys)), Some(keys));
    }

    #[test]
    fn audio_payloads_roundtrip() {
        let packet = AudioPacket {
            seq: 7,
            capture_us: 1_000,
            arrived_us: 3_000,
            packet: vec![1, 2, 3],
        };
        assert_eq!(from_payload(&to_payload(&packet)), Some(packet));
        let stats = AudioStats {
            played: 100,
            concealed: 1,
            buffered_us: 20_000,
            ..Default::default()
        };
        assert_eq!(from_payload(&to_payload(&stats)), Some(stats));
    }

    #[test]
    fn zero_length_is_rejected() {
        assert_eq!(decode(&[0, 0, 0, 0]), Err(IpcError::Empty));
    }
}
