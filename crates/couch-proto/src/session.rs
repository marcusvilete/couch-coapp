//! Sessions on the control stream: a member streaming a game to others in
//! the same room (docs/protocol.md, Sessions).
//!
//! The host is the only writer of its session's facts and sends them
//! straight to each member it is connected to; nobody relays them. So they
//! carry no signature and never name the host: the connection's
//! authenticated peer is the host, and a member acts on a session message
//! only from the session's own host.

use serde::{Deserialize, Serialize};

use crate::{
    ids::{MemberId, Permissions, SeatId, SessionId},
    room::{MAX_ROSTER, valid_name},
};

/// Viewers a [`SessionStatus`] may list. The room's size bounds it, not the
/// host's limit, so that limit can change without a protocol change.
pub const MAX_VIEWERS_LISTED: usize = MAX_ROSTER;
/// Seats with a virtual pad a session may have: XInput's four slots.
pub const MAX_PAD_SEATS: usize = 4;
/// Every seat: the pads, plus one with keyboard and mouse.
pub const MAX_SEATS: usize = MAX_PAD_SEATS + 1;

/// What a seat's holder plays with. The seat decides the permissions: the
/// host approves every holder, so handing someone a seat with keyboard and
/// mouse is the trust decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeatKind {
    /// A virtual Xbox 360 pad of its own.
    Controller,
    /// The host's keyboard and pointer, while the game window is in front.
    KeyboardMouse,
    /// Both: a pad, and keyboard and mouse.
    Both,
}

impl SeatKind {
    pub fn permissions(self) -> Permissions {
        match self {
            Self::Controller => Permissions::GAMEPAD,
            Self::KeyboardMouse => Permissions::KEYBOARD | Permissions::MOUSE,
            Self::Both => Permissions::GAMEPAD | Permissions::KEYBOARD | Permissions::MOUSE,
        }
    }

    pub fn has_pad(self) -> bool {
        matches!(self, Self::Controller | Self::Both)
    }

    pub fn has_keyboard_mouse(self) -> bool {
        matches!(self, Self::KeyboardMouse | Self::Both)
    }
}

/// What the seats are called, by seat: "Controller" (numbered when there
/// are several), "Keyboard and mouse", "Controller, keyboard and mouse".
pub fn seat_labels(seats: &[SeatKind]) -> Vec<String> {
    let pads = seats.iter().filter(|s| s.has_pad()).count();
    let mut pad = 0;
    seats
        .iter()
        .map(|kind| match kind {
            SeatKind::KeyboardMouse => "Keyboard and mouse".to_string(),
            SeatKind::Both => "Controller, keyboard and mouse".to_string(),
            SeatKind::Controller => {
                pad += 1;
                if pads > 1 {
                    format!("Controller {pad}")
                } else {
                    "Controller".to_string()
                }
            }
        })
        .collect()
}

/// A seat's label in a sentence: "the controller", "Controller 2", "the
/// keyboard and mouse".
pub fn seat_phrase(label: &str) -> String {
    if label.ends_with(|c: char| c.is_ascii_digit()) {
        label.to_string()
    } else {
        format!("the {}", label.to_lowercase())
    }
}

/// A session's seats: 1 to [`MAX_SEATS`], at most [`MAX_PAD_SEATS`] with a
/// pad, and at most one with keyboard and mouse (Windows has one keyboard
/// and one pointer).
pub fn valid_seats(seats: &[SeatKind]) -> bool {
    let pads = seats.iter().filter(|s| s.has_pad()).count();
    let kbm = seats.iter().filter(|s| s.has_keyboard_mouse()).count();
    (1..=MAX_SEATS).contains(&seats.len()) && pads <= MAX_PAD_SEATS && kbm <= 1
}

/// Host to each connected member, optional: a session is live. Sent on going
/// live and on every new connection while live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAnnounce {
    pub session: SessionId,
    /// What the host called it (a name: 1 to 96 bytes, nothing invisible).
    pub title: String,
    /// The seats players can hold, by [`SeatId`] (their index). One seat is
    /// a single-player session.
    pub seats: Vec<SeatKind>,
    /// The most viewers the host takes at once.
    pub max_viewers: u8,
}

impl SessionAnnounce {
    pub fn is_valid(&self) -> bool {
        valid_name(&self.title) && valid_seats(&self.seats) && self.max_viewers >= 1
    }
}

/// Host to each connected member, optional: who watches and who plays.
/// Sent after the announcement and on every change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStatus {
    pub session: SessionId,
    pub viewers: Vec<MemberId>,
    /// Each seat's viewer holder, by seat; `None` while no viewer holds it.
    /// As long as the announcement's seats (the receiver checks).
    pub holders: Vec<Option<MemberId>>,
    /// The seat the host plays with their own controller (it reads the
    /// controller and drives that seat's pad); none of the viewers holds it.
    /// `None` while the host plays outside the seats, with whatever devices
    /// the game sees directly, as on a seat no viewer holds.
    pub host_seat: Option<SeatId>,
    /// The host's own controller plays only through the seats, hidden from
    /// the game: with `host_seat` `None` the host plays no seat, and a seat
    /// nobody holds is empty. `false` while the host plays outside the seats.
    pub host_on_seats: bool,
    /// The host took control back with the panic key; requests are declined
    /// until the host allows control again.
    pub control_paused: bool,
}

impl SessionStatus {
    /// Every holder is a viewer, no viewer holds two seats, and the host's
    /// seat is a seat no viewer holds, only while the host plays on seats.
    pub fn is_valid(&self) -> bool {
        let held: Vec<MemberId> = self.holders.iter().flatten().copied().collect();
        self.viewers.len() <= MAX_VIEWERS_LISTED
            && self.holders.len() <= MAX_SEATS
            && held.iter().all(|h| self.viewers.contains(h))
            && held.iter().enumerate().all(|(i, h)| !held[..i].contains(h))
            && self.host_seat.is_none_or(|s| {
                self.host_on_seats && self.holders.get(usize::from(s.0)) == Some(&None)
            })
    }

    /// The seat `member` holds, if any.
    pub fn seat_of(&self, member: MemberId) -> Option<SeatId> {
        self.holders
            .iter()
            .position(|h| *h == Some(member))
            .map(|i| SeatId(i as u8))
    }
}

/// Host to its viewers, optional: whether the streamed window is in front
/// on the host. Keyboard and mouse reach the game only while it is, and many
/// games ignore controllers in the background. Sent on a change (coalesced)
/// and to each new viewer; never for a screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameFocus {
    pub session: SessionId,
    pub in_front: bool,
}

/// Why a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EndReason {
    HostStopped,
    /// The game exited (M6 watches the process).
    GameExited,
    /// The captured window or screen went away.
    SourceLost,
    HostLeftRoom,
    HostShutdown,
    Error,
}

/// Host to each connected member, optional: the session is over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnded {
    pub session: SessionId,
    pub reason: EndReason,
}

/// Member to host: let me watch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionJoin {
    pub session: SessionId,
    /// The codecs this viewer decodes in hardware, as [`crate::codec`]
    /// bits; H.264's is always set (M12c). The host picks the stream's.
    pub codecs: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinResult {
    /// In: video and audio follow.
    Joined,
    /// The host's viewer limit is reached.
    Full,
    /// No such session (any more).
    Ended,
    /// The host couldn't start a stream for this viewer, such as when no
    /// encoder could be opened.
    Unavailable,
}

/// Host to member: the answer to [`SessionJoin`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionJoinReply {
    pub session: SessionId,
    pub result: JoinResult,
}

/// Viewer to host: I stopped watching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLeave {
    pub session: SessionId,
}

/// Viewer to host: may I play on this seat? It may be free or held by
/// another viewer; the host decides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlRequest {
    pub session: SessionId,
    pub seat: SeatId,
}

/// Viewer to host: withdraws my request, or gives my seat back. Which one is
/// explicit: a withdraw that crosses a decline must never cost the seat the
/// viewer plays on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlCancel {
    pub session: SessionId,
    pub what: Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cancel {
    /// Withdraw the pending request, if any; the seat held stays.
    Request,
    /// Give back the seat held, if any.
    Seat,
}

/// Host to viewer: your request was declined (or dropped: the host took
/// control back with the panic key).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlDeclined {
    pub session: SessionId,
}

#[cfg(test)]
mod tests {
    use super::*;

    use SeatKind::*;

    #[test]
    fn seats_are_checked() {
        assert!(valid_seats(&[Controller]));
        assert!(valid_seats(&[KeyboardMouse]));
        assert!(valid_seats(&[Both]));
        assert!(valid_seats(&[
            Controller,
            Controller,
            Controller,
            Controller,
            KeyboardMouse
        ]));
        assert!(!valid_seats(&[]));
        assert!(!valid_seats(&[Controller; MAX_PAD_SEATS + 1]), "five pads");
        assert!(
            !valid_seats(&[KeyboardMouse, Controller, KeyboardMouse]),
            "two keyboard seats"
        );
        assert!(!valid_seats(&[Both, KeyboardMouse]), "two keyboard seats");
        assert_eq!(Controller.permissions(), Permissions::GUEST_DEFAULT);
        assert_eq!(seat_labels(&[Controller]), ["Controller"]);
        assert_eq!(
            seat_labels(&[Controller, Controller, KeyboardMouse]),
            ["Controller 1", "Controller 2", "Keyboard and mouse"]
        );
        assert!(!KeyboardMouse.permissions().contains(Permissions::GAMEPAD));
    }

    #[test]
    fn announcements_and_statuses_are_checked() {
        let announce = SessionAnnounce {
            session: SessionId(1),
            title: "Celeste".into(),
            seats: vec![Controller],
            max_viewers: 4,
        };
        assert!(announce.is_valid());
        assert!(
            !SessionAnnounce {
                title: "a\u{202E}b".into(),
                ..announce.clone()
            }
            .is_valid()
        );
        assert!(
            !SessionAnnounce {
                max_viewers: 0,
                ..announce.clone()
            }
            .is_valid()
        );
        assert!(
            !SessionAnnounce {
                seats: vec![],
                ..announce
            }
            .is_valid()
        );

        let bia = MemberId([1; 32]);
        let cai = MemberId([2; 32]);
        let status = SessionStatus {
            session: SessionId(1),
            viewers: vec![bia, cai],
            holders: vec![Some(bia), None, Some(cai)],
            host_seat: Some(SeatId(1)),
            host_on_seats: true,
            control_paused: false,
        };
        assert!(status.is_valid());
        assert!(
            !SessionStatus {
                host_on_seats: false,
                ..status.clone()
            }
            .is_valid(),
            "a host on a seat plays on seats"
        );
        assert!(
            SessionStatus {
                host_seat: None,
                ..status.clone()
            }
            .is_valid(),
            "on seats, the host may hold none"
        );
        assert!(
            !SessionStatus {
                host_seat: Some(SeatId(0)),
                ..status.clone()
            }
            .is_valid(),
            "the host's seat is nobody else's"
        );
        assert!(
            !SessionStatus {
                host_seat: Some(SeatId(3)),
                ..status.clone()
            }
            .is_valid(),
            "the host's seat is a seat"
        );
        assert_eq!(status.seat_of(cai), Some(SeatId(2)));
        assert_eq!(status.seat_of(MemberId([3; 32])), None);
        assert!(
            !SessionStatus {
                holders: vec![Some(MemberId([3; 32]))],
                ..status.clone()
            }
            .is_valid(),
            "a holder must be a viewer"
        );
        assert!(
            !SessionStatus {
                holders: vec![Some(bia), Some(bia)],
                ..status.clone()
            }
            .is_valid(),
            "nobody holds two seats"
        );
        assert!(
            !SessionStatus {
                holders: vec![None; MAX_SEATS + 1],
                ..status.clone()
            }
            .is_valid()
        );
        assert!(
            !SessionStatus {
                viewers: vec![bia; MAX_VIEWERS_LISTED + 1],
                holders: vec![None],
                ..status
            }
            .is_valid()
        );
    }

    /// The largest status still fits one control frame.
    #[test]
    fn a_full_status_fits_a_control_frame() {
        let status = SessionStatus {
            session: SessionId(u64::MAX),
            viewers: vec![MemberId([0xff; 32]); MAX_VIEWERS_LISTED],
            holders: vec![Some(MemberId([0xff; 32])); MAX_SEATS],
            host_seat: Some(SeatId(u8::MAX)),
            host_on_seats: true,
            control_paused: true,
        };
        let len = postcard::to_stdvec(&status).unwrap().len();
        assert!(len + 2 <= crate::envelope::MAX_FRAME_LEN, "{len} bytes");
    }
}
