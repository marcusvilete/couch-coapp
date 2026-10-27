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
    /// The seats the host plays with their own controllers (it reads each
    /// controller and drives its seat's pad; M23: one seat per controller),
    /// in seat order; none of the viewers holds them. Empty while the host
    /// plays outside the seats, with whatever devices the game sees
    /// directly, as on a seat no viewer holds.
    pub host_seats: Vec<SeatId>,
    /// The host's own controllers play only through the seats, hidden from
    /// the game: with `host_seats` empty the host plays no seat, and a seat
    /// nobody holds is empty. `false` while the host plays outside the seats.
    pub host_on_seats: bool,
    /// The host took control back with the panic key; requests are declined
    /// until the host allows control again.
    pub control_paused: bool,
    /// Whether anything is streamed (M19): a session starts with nothing,
    /// and streams start and stop inside it.
    pub stream: StreamState,
}

/// Whether a session streams anything (M19). While it doesn't, viewers stay,
/// keep their seats, and their input is held off: they can't see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamState {
    /// Nothing streamed yet in this session.
    Waiting,
    Live,
    /// The last stream ended, and why; the session goes on.
    Stopped(EndReason),
}

impl SessionStatus {
    /// Every holder is a viewer, and the host's seats are seats no viewer
    /// holds, each once, only while the host plays on seats. A viewer may
    /// hold several seats (M18: one per controller, and the keyboard seat),
    /// and so may the host (M23).
    pub fn is_valid(&self) -> bool {
        self.viewers.len() <= MAX_VIEWERS_LISTED
            && self.holders.len() <= MAX_SEATS
            && self
                .holders
                .iter()
                .flatten()
                .all(|h| self.viewers.contains(h))
            && (self.host_on_seats || self.host_seats.is_empty())
            && self.host_seats.windows(2).all(|w| w[0] < w[1])
            && self
                .host_seats
                .iter()
                .all(|s| self.holders.get(usize::from(s.0)) == Some(&None))
    }

    /// Every seat `member` holds, in seat order.
    pub fn seats_of(&self, member: MemberId) -> Vec<SeatId> {
        self.holders
            .iter()
            .enumerate()
            .filter(|(_, h)| **h == Some(member))
            .map(|(i, _)| SeatId(i as u8))
            .collect()
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

/// Why a session or a stream ended. Since M19 the game exiting and its
/// window or screen going end only the stream.
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
    /// Which of the viewer's controllers asks, by slot (P13): granted, it
    /// plays the seat, leaving its own if the viewer has no controller to
    /// spare. `None` with one controller or none.
    pub controller: Option<u8>,
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
    /// Withdraw the pending request, if any; the seats held stay.
    Request,
    /// Give back this seat, or with `None` every seat held (M18).
    Seat(Option<SeatId>),
}

/// Viewer to host, optional: this viewer's controllers and which drives each
/// pad seat it holds (M18, P12), after joining and on every change. A viewer
/// holds one seat with a pad per controller (the host caps the count at 4);
/// a seat beyond that moves it. The host shows each controller as a plug of
/// its own, and a seat's cable at the controller driving it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewerControllers {
    pub session: SessionId,
    pub report: ControllerReport,
}

/// What a viewer says of its controllers (P12).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerReport {
    /// Its connected controllers, by slot, at most [`MAX_GUEST_CONTROLLERS`].
    pub controllers: Vec<GuestController>,
    /// Each pad seat it holds, in seat order, and the controller slot
    /// driving it (`None` while none does). The viewer alone decides this
    /// (`couch_input::forward`), following the host's choice when it can.
    pub seats: Vec<SeatDriver>,
}

impl ControllerReport {
    /// What a peer's report may hold: at most [`MAX_GUEST_CONTROLLERS`]
    /// controllers on distinct slots below it, their names cleaned
    /// ([`controller_name`]), and at most as many seats, each once, each
    /// driven by one of those controllers or none. `None` if it can't be
    /// read so.
    pub fn checked(mut self) -> Option<Self> {
        let max = MAX_GUEST_CONTROLLERS;
        let slots: Vec<u8> = self.controllers.iter().map(|c| c.slot).collect();
        let distinct = |v: &[u8]| v.iter().enumerate().all(|(i, a)| !v[..i].contains(a));
        let seats: Vec<u8> = self.seats.iter().map(|s| s.seat.0).collect();
        if slots.len() > max
            || self.seats.len() > max
            || !distinct(&slots)
            || !distinct(&seats)
            || slots.iter().any(|s| usize::from(*s) >= max)
            || self
                .seats
                .iter()
                .any(|s| s.controller.is_some_and(|c| !slots.contains(&c)))
        {
            return None;
        }
        for c in &mut self.controllers {
            c.name = controller_name(&c.name);
        }
        Some(self)
    }
}

/// One of a viewer's controllers (P12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestController {
    /// Its slot in the stream window, kept while it is connected.
    pub slot: u8,
    /// As the system names it ("DualSense Wireless Controller"), cleaned by
    /// [`controller_name`].
    pub name: String,
}

/// A pad seat a viewer holds and the controller slot driving it (P12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatDriver {
    pub seat: SeatId,
    pub controller: Option<u8>,
}

/// Controller slots a viewer has: its stream window's
/// (`couch_input::forward::MAX_LOCAL_CONTROLLERS`).
pub const MAX_GUEST_CONTROLLERS: usize = 8;
/// A controller's name, at most this many characters.
pub const MAX_CONTROLLER_NAME: usize = 40;

/// A controller's name as it may be shown: no control characters, spaces
/// trimmed, at most [`MAX_CONTROLLER_NAME`] characters, "Controller" if
/// nothing is left.
pub fn controller_name(raw: &str) -> String {
    let clean: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let name: String = clean.trim().chars().take(MAX_CONTROLLER_NAME).collect();
    let name = name.trim_end();
    if name.is_empty() {
        "Controller".into()
    } else {
        name.into()
    }
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
            host_seats: vec![SeatId(1)],
            host_on_seats: true,
            control_paused: false,
            stream: StreamState::Waiting,
        };
        assert!(status.is_valid());
        assert!(
            SessionStatus {
                holders: vec![Some(bia), None, None, Some(cai)],
                host_seats: vec![SeatId(1), SeatId(2)],
                ..status.clone()
            }
            .is_valid(),
            "the host may play two seats (M23)"
        );
        assert!(
            !SessionStatus {
                holders: vec![Some(bia), None, None],
                host_seats: vec![SeatId(1), SeatId(1)],
                ..status.clone()
            }
            .is_valid(),
            "each of the host's seats once"
        );
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
                host_seats: vec![],
                ..status.clone()
            }
            .is_valid(),
            "on seats, the host may hold none"
        );
        assert!(
            !SessionStatus {
                host_seats: vec![SeatId(0)],
                ..status.clone()
            }
            .is_valid(),
            "the host's seat is nobody else's"
        );
        assert!(
            !SessionStatus {
                host_seats: vec![SeatId(3)],
                ..status.clone()
            }
            .is_valid(),
            "the host's seat is a seat"
        );
        assert_eq!(status.seats_of(cai), vec![SeatId(2)]);
        assert_eq!(status.seats_of(MemberId([3; 32])), vec![]);
        assert!(
            !SessionStatus {
                holders: vec![Some(MemberId([3; 32]))],
                ..status.clone()
            }
            .is_valid(),
            "a holder must be a viewer"
        );
        let two = SessionStatus {
            holders: vec![Some(bia), Some(bia)],
            host_seats: vec![],
            ..status.clone()
        };
        assert!(two.is_valid(), "a viewer may hold two seats (M18)");
        assert_eq!(two.seats_of(bia), vec![SeatId(0), SeatId(1)]);
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
            host_seats: (0..MAX_SEATS as u8).map(SeatId).collect(),
            host_on_seats: true,
            control_paused: true,
            stream: StreamState::Stopped(EndReason::GameExited),
        };
        let len = postcard::to_stdvec(&status).unwrap().len();
        assert!(len + 2 <= crate::envelope::MAX_FRAME_LEN, "{len} bytes");
    }

    #[test]
    fn controller_names_are_cleaned() {
        assert_eq!(controller_name("  Xbox\u{0}Pad\n "), "Xbox Pad");
        assert_eq!(controller_name("\u{7}\t "), "Controller");
        let long = "é".repeat(100);
        assert_eq!(controller_name(&long).chars().count(), MAX_CONTROLLER_NAME);
    }

    fn controller(slot: u8) -> GuestController {
        GuestController {
            slot,
            name: format!("Pad\u{1b} {slot}"),
        }
    }

    fn driver(seat: u8, controller: Option<u8>) -> SeatDriver {
        SeatDriver {
            seat: SeatId(seat),
            controller,
        }
    }

    #[test]
    fn a_controller_report_is_checked() {
        let good = ControllerReport {
            controllers: vec![controller(0), controller(3)],
            seats: vec![driver(0, Some(3)), driver(2, None)],
        };
        let checked = good.clone().checked().unwrap();
        assert_eq!(checked.controllers[0].name, "Pad  0");
        assert_eq!(checked.seats, good.seats);
        let bad = [
            // Two controllers on one slot.
            ControllerReport {
                controllers: vec![controller(1), controller(1)],
                seats: vec![],
            },
            // A slot beyond the window's.
            ControllerReport {
                controllers: vec![controller(MAX_GUEST_CONTROLLERS as u8)],
                seats: vec![],
            },
            // Too many.
            ControllerReport {
                controllers: (0..=MAX_GUEST_CONTROLLERS as u8).map(controller).collect(),
                seats: vec![],
            },
            // A seat driven by a controller it doesn't have.
            ControllerReport {
                controllers: vec![controller(0)],
                seats: vec![driver(0, Some(1))],
            },
            // A seat twice.
            ControllerReport {
                controllers: vec![controller(0)],
                seats: vec![driver(1, Some(0)), driver(1, None)],
            },
        ];
        for report in bad {
            assert_eq!(report.clone().checked(), None, "{report:?}");
        }
    }

    /// The fullest report fits a control frame.
    #[test]
    fn the_fullest_controller_report_fits() {
        let report = ControllerReport {
            controllers: (0..MAX_GUEST_CONTROLLERS as u8)
                .map(|slot| GuestController {
                    slot,
                    name: "\u{10FFFF}".repeat(MAX_CONTROLLER_NAME),
                })
                .collect(),
            seats: (0..MAX_GUEST_CONTROLLERS as u8)
                .map(|s| driver(s, Some(s)))
                .collect(),
        };
        let message = ViewerControllers {
            session: SessionId(u64::MAX),
            report,
        };
        let len = postcard::to_stdvec(&message).unwrap().len();
        assert!(len < 2048, "{len} bytes");
    }
}
