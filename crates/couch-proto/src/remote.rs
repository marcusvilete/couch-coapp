//! Remote play (M25, ADR 0010): a trusted device of the host's own (the
//! *operator*) runs the host's sessions from afar.
//!
//! These pass only between the home PC and a device it trusts, on their
//! room connection, and are never relayed. The home PC acts on them only
//! from a trusted id, on its current connection, once the PIN was given on
//! that connection; and only for the operations [`RemoteOp`] lists. Nothing
//! here can change who is trusted or the PIN: that is done at the home PC.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    ids::{MemberId, SeatId, SessionId},
    session::{ControllerReport, StreamState},
};

/// The longest PIN, in bytes, on the wire. The home PC checks the rest.
pub const MAX_PIN_BYTES: usize = 64;
/// Windows a [`RemoteSources`] may list.
pub const MAX_REMOTE_WINDOWS: usize = 64;
/// Screens a [`RemoteSources`] may list.
pub const MAX_REMOTE_SCREENS: usize = 16;
/// The longest title, program or screen name in a [`RemoteSources`], and
/// the longest stream name or failure text, in bytes.
pub const MAX_REMOTE_TEXT: usize = 200;
/// Requests and guests a [`RemoteSession`] may list: the room's size.
pub const MAX_REMOTE_LISTED: usize = crate::room::MAX_ROSTER;

/// A remote-play PIN. It never prints: logs show `..`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin(pub String);

impl fmt::Debug for Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("..")
    }
}

/// Home PC to a trusted device, optional, on every room connection while
/// remote play is on: it may unlock. `wait_secs`: wrong PINs make it wait
/// this long first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteOffer {
    pub wait_secs: Option<u32>,
}

/// Trusted device to home PC: the PIN, to become its operator on this
/// connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteUnlock {
    pub pin: Pin,
}

impl RemoteUnlock {
    pub fn is_valid(&self) -> bool {
        !self.pin.0.is_empty() && self.pin.0.len() <= MAX_PIN_BYTES
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnlockResult {
    Unlocked,
    /// Wrong; the next try may come after `wait_secs` (0: at once).
    WrongPin {
        wait_secs: u32,
    },
    /// Too many wrong PINs: not tried. Try again after `secs`.
    Wait {
        secs: u32,
    },
    /// Remote play is off, or this device isn't trusted (any more).
    Off,
}

/// Home PC to the device that sent [`RemoteUnlock`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteUnlockReply {
    pub result: UnlockResult,
}

/// Operator to home PC: do this, as if from the home PC's own window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCommand {
    /// Answered by the [`RemoteReply`] with the same id.
    pub id: u32,
    pub op: RemoteOp,
}

/// What an operator may do: the host's console, and nothing else (no
/// rooms, settings or remote play itself). Its `Debug` leaves out the
/// stream's name, a window's title.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteOp {
    ListSources,
    /// In the room of the connection it came on. The home PC's own
    /// controllers never play through seats: nobody is there.
    StartSession {
        title: String,
        max_viewers: u8,
        controllers: u8,
        keyboard_mouse: bool,
    },
    StartStream {
        source: RemoteSource,
        name: String,
        fps: u32,
        max_mbps: u32,
        upload_mbps: Option<u32>,
    },
    StopStream,
    SetStreamLevel {
        percent: u16,
    },
    EndSession,
    AnswerControl {
        member: MemberId,
        accept: bool,
    },
    PassControl {
        member: MemberId,
        seat: SeatId,
        controller: Option<u8>,
    },
    TakeBackControl {
        seat: Option<SeatId>,
    },
    PlaySeat {
        seat: SeatId,
        controller: u8,
    },
    AllowControl,
    StopWaitingForPads,
    /// Desktop control (M25b): the session's keyboard-and-mouse seat goes to
    /// this operator with [`crate::ids::Permissions::DESKTOP`], or comes back.
    Desktop {
        on: bool,
    },
    /// The streamed window to the front (M25b). Only ever the streamed
    /// window: the op names none.
    BringToFront,
}

impl fmt::Debug for RemoteOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ListSources => f.write_str("ListSources"),
            Self::StartSession {
                max_viewers,
                controllers,
                keyboard_mouse,
                ..
            } => f
                .debug_struct("StartSession")
                .field("max_viewers", max_viewers)
                .field("controllers", controllers)
                .field("keyboard_mouse", keyboard_mouse)
                .finish_non_exhaustive(),
            Self::StartStream {
                source,
                fps,
                max_mbps,
                ..
            } => f
                .debug_struct("StartStream")
                .field("source", source)
                .field("fps", fps)
                .field("max_mbps", max_mbps)
                .finish_non_exhaustive(),
            Self::StopStream => f.write_str("StopStream"),
            Self::SetStreamLevel { percent } => f
                .debug_struct("SetStreamLevel")
                .field("percent", percent)
                .finish(),
            Self::EndSession => f.write_str("EndSession"),
            Self::AnswerControl { member, accept } => f
                .debug_struct("AnswerControl")
                .field("member", member)
                .field("accept", accept)
                .finish(),
            Self::PassControl {
                member,
                seat,
                controller,
            } => f
                .debug_struct("PassControl")
                .field("member", member)
                .field("seat", seat)
                .field("controller", controller)
                .finish(),
            Self::TakeBackControl { seat } => f
                .debug_struct("TakeBackControl")
                .field("seat", seat)
                .finish(),
            Self::PlaySeat { seat, controller } => f
                .debug_struct("PlaySeat")
                .field("seat", seat)
                .field("controller", controller)
                .finish(),
            Self::AllowControl => f.write_str("AllowControl"),
            Self::StopWaitingForPads => f.write_str("StopWaitingForPads"),
            Self::Desktop { on } => f.debug_struct("Desktop").field("on", on).finish(),
            Self::BringToFront => f.write_str("BringToFront"),
        }
    }
}

impl RemoteOp {
    /// Its text fits [`MAX_REMOTE_TEXT`]; the home PC checks the rest as it
    /// checks its own window's commands.
    pub fn is_valid(&self) -> bool {
        match self {
            Self::StartSession { title, .. } => title.len() <= MAX_REMOTE_TEXT,
            Self::StartStream { name, .. } => name.len() <= MAX_REMOTE_TEXT,
            _ => true,
        }
    }
}

/// A window or screen, by the id [`RemoteSources`] gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteSource {
    Window(i64),
    Screen(i64),
}

/// Home PC to the operator: how a [`RemoteCommand`] went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteReply {
    pub id: u32,
    pub result: RemoteResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteResult {
    Done,
    Sources(RemoteSources),
    /// Refused or failed, said as the home PC's window would say it.
    Failed(String),
}

/// What the home PC could stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteSources {
    pub windows: Vec<RemoteWindow>,
    pub screens: Vec<RemoteScreen>,
    /// Whether virtual controllers can be plugged in (ViGEmBus).
    pub virtual_pads: bool,
}

/// Its `Debug` leaves out the title and the program.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteWindow {
    pub id: i64,
    pub title: String,
    pub app: String,
    pub elevated: bool,
    pub refresh_hz: Option<u32>,
}

impl fmt::Debug for RemoteWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteWindow")
            .field("id", &self.id)
            .field("elevated", &self.elevated)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteScreen {
    pub id: i64,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    pub refresh_hz: Option<u32>,
}

impl RemoteSources {
    /// Cut to the limits, as the home PC sends it: at most
    /// [`MAX_REMOTE_WINDOWS`] windows and [`MAX_REMOTE_SCREENS`] screens,
    /// each text at most [`MAX_REMOTE_TEXT`] bytes.
    pub fn capped(mut self) -> Self {
        self.windows.truncate(MAX_REMOTE_WINDOWS);
        self.screens.truncate(MAX_REMOTE_SCREENS);
        for w in &mut self.windows {
            cut(&mut w.title);
            cut(&mut w.app);
        }
        for s in &mut self.screens {
            cut(&mut s.name);
        }
        self
    }

    pub fn is_valid(&self) -> bool {
        self.windows.len() <= MAX_REMOTE_WINDOWS
            && self.screens.len() <= MAX_REMOTE_SCREENS
            && self
                .windows
                .iter()
                .all(|w| w.title.len() <= MAX_REMOTE_TEXT && w.app.len() <= MAX_REMOTE_TEXT)
            && self.screens.iter().all(|s| s.name.len() <= MAX_REMOTE_TEXT)
    }
}

/// Cuts `text` to [`MAX_REMOTE_TEXT`] bytes, at a character boundary.
pub fn cut(text: &mut String) {
    if text.len() > MAX_REMOTE_TEXT {
        let mut end = MAX_REMOTE_TEXT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

/// Home PC to each operator, on unlocking and whenever it changes: what
/// the host's console shows that members don't otherwise hear
/// (announcements and statuses carry the rest).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteState {
    /// The home PC's session, if it has one.
    pub session: Option<RemoteSession>,
    /// Windows is locked at the home PC (M25c): nothing gets in until
    /// someone unlocks it there, and a capture shows black or freezes.
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteSession {
    pub session: SessionId,
    /// A stream is starting.
    pub starting: bool,
    pub stream: StreamState,
    pub streaming: Option<RemoteStream>,
    /// Oldest first.
    pub requests: Vec<RemoteRequest>,
    /// Each viewer's controllers and which drives which seat, as it last
    /// said (P12).
    pub guests: Vec<RemoteGuest>,
    /// How loud guests hear it, in percent (P7).
    pub sound_level: u16,
    pub pads_setting_up: Vec<SeatId>,
    pub pads_setting_up_secs: u32,
    pub pads_missing: Vec<SeatId>,
    /// The home PC's own controllers on seats, and the seat each plays.
    pub host_controllers: Vec<String>,
    pub host_homes: Vec<SeatId>,
    /// Whether the home PC has its panic key.
    pub panic_key: bool,
    /// The operator controlling the desktop, if one does (M25b).
    pub desktop: Option<MemberId>,
    /// Whether the streamed window is in front, once the home PC knows;
    /// `None` for a screen (M25b).
    pub in_front: Option<bool>,
}

/// Its `Debug` leaves out the name, a window's title.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteStream {
    pub name: String,
    pub fps: u32,
    pub max_mbps: u32,
    pub upload_mbps: Option<u32>,
    /// A screen rather than a window (M25b): nothing to bring to the front.
    pub screen: bool,
}

impl fmt::Debug for RemoteStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteStream")
            .field("fps", &self.fps)
            .field("max_mbps", &self.max_mbps)
            .field("screen", &self.screen)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRequest {
    pub member: MemberId,
    pub seat: SeatId,
    pub controller: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteGuest {
    pub member: MemberId,
    pub report: ControllerReport,
}

impl RemoteState {
    /// What the operator takes: lists within the room's size, texts within
    /// [`MAX_REMOTE_TEXT`], and each guest's report as a host checks one.
    pub fn checked(mut self) -> Option<Self> {
        if let Some(s) = &mut self.session {
            let text = |t: &str| t.len() <= MAX_REMOTE_TEXT;
            if s.requests.len() > MAX_REMOTE_LISTED
                || s.guests.len() > MAX_REMOTE_LISTED
                || s.host_controllers.len() > crate::session::MAX_PAD_SEATS
                || s.host_homes.len() > crate::session::MAX_PAD_SEATS
                || s.pads_setting_up.len() > crate::session::MAX_SEATS
                || s.pads_missing.len() > crate::session::MAX_SEATS
                || !s.host_controllers.iter().all(|c| text(c))
                || !s.streaming.as_ref().is_none_or(|st| text(&st.name))
            {
                return None;
            }
            let guests = std::mem::take(&mut s.guests);
            for g in guests {
                s.guests.push(RemoteGuest {
                    member: g.member,
                    report: g.report.checked()?,
                });
            }
        }
        Some(self)
    }
}

/// Either way: the operator is locked out (the home PC says why), or it
/// locks itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteLock {
    pub reason: LockReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LockReason {
    /// The operator locked it.
    ByOperator,
    /// Someone at the home PC pressed the panic key: the PIN again.
    Panic,
    /// The home PC no longer trusts this device.
    Removed,
    /// The PIN was changed at the home PC: the new one, please.
    PinChanged,
    /// Remote play was turned off at the home PC.
    Off,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{GuestController, SeatDriver};

    #[test]
    fn a_pin_never_prints() {
        let unlock = RemoteUnlock {
            pin: Pin("123456".into()),
        };
        assert!(!format!("{unlock:?}").contains("123456"));
        assert!(unlock.is_valid());
        assert!(
            !RemoteUnlock {
                pin: Pin(String::new())
            }
            .is_valid()
        );
        assert!(
            !RemoteUnlock {
                pin: Pin("9".repeat(MAX_PIN_BYTES + 1))
            }
            .is_valid()
        );
    }

    /// Window titles stay out of logs, as everywhere else.
    #[test]
    fn titles_never_print() {
        let title = "Secret Project - Notepad";
        let shown = [
            format!(
                "{:?}",
                RemoteOp::StartStream {
                    source: RemoteSource::Window(1),
                    name: title.into(),
                    fps: 60,
                    max_mbps: 20,
                    upload_mbps: None,
                }
            ),
            format!(
                "{:?}",
                RemoteWindow {
                    id: 1,
                    title: title.into(),
                    app: title.into(),
                    elevated: false,
                    refresh_hz: None,
                }
            ),
            format!(
                "{:?}",
                RemoteStream {
                    name: title.into(),
                    fps: 60,
                    max_mbps: 20,
                    upload_mbps: None,
                    screen: false,
                }
            ),
        ];
        for s in shown {
            assert!(!s.contains("Secret"), "{s}");
        }
    }

    #[test]
    fn sources_are_cut_to_their_limits() {
        let window = RemoteWindow {
            id: -7,
            title: "é".repeat(MAX_REMOTE_TEXT),
            app: "Celeste.exe".into(),
            elevated: false,
            refresh_hz: Some(60),
        };
        let sources = RemoteSources {
            windows: vec![window; MAX_REMOTE_WINDOWS + 3],
            screens: Vec::new(),
            virtual_pads: true,
        };
        assert!(!sources.is_valid());
        let capped = sources.capped();
        assert!(capped.is_valid());
        assert_eq!(capped.windows.len(), MAX_REMOTE_WINDOWS);
        assert!(capped.windows[0].title.len() <= MAX_REMOTE_TEXT);
    }

    #[test]
    fn a_state_is_checked_as_a_host_checks_reports() {
        let guest = |slot: u8| RemoteGuest {
            member: MemberId([1; 32]),
            report: ControllerReport {
                controllers: vec![GuestController {
                    slot,
                    name: "Xbox Wireless Controller".into(),
                }],
                seats: vec![SeatDriver {
                    seat: SeatId(0),
                    controller: Some(0),
                }],
            },
        };
        let state = |guests| RemoteState {
            session: Some(RemoteSession {
                session: SessionId(1),
                starting: false,
                stream: StreamState::Live,
                streaming: None,
                requests: Vec::new(),
                guests,
                sound_level: 100,
                pads_setting_up: Vec::new(),
                pads_setting_up_secs: 0,
                pads_missing: Vec::new(),
                host_controllers: Vec::new(),
                host_homes: Vec::new(),
                panic_key: true,
                desktop: None,
                in_front: None,
            }),
            locked: false,
        };
        assert!(state(vec![guest(0)]).checked().is_some());
        // Seat 0 driven by a controller the guest doesn't have.
        assert!(state(vec![guest(1)]).checked().is_none());
        assert!(
            RemoteState {
                session: None,
                locked: true
            }
            .checked()
            .is_some()
        );
    }
}
