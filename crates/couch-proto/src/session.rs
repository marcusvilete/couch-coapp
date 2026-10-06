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
    ids::{MemberId, SessionId},
    room::{MAX_ROSTER, valid_name},
};

/// Viewers a [`SessionStatus`] may list. The room's size bounds it, not the
/// host's limit, so that limit can change without a protocol change.
pub const MAX_VIEWERS_LISTED: usize = MAX_ROSTER;

/// How many players control the game at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionMode {
    /// One seat. The host starts with it and can pass it to any viewer.
    SinglePlayer,
}

/// Host to each connected member, optional: a session is live. Sent on going
/// live and on every new connection while live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAnnounce {
    pub session: SessionId,
    /// What the host called it (a name: 1 to 96 bytes, nothing invisible).
    pub title: String,
    pub mode: SessionMode,
    /// Whoever holds the seat may use keyboard and mouse, not just a gamepad.
    pub keyboard_mouse: bool,
    /// The most viewers the host takes at once.
    pub max_viewers: u8,
}

impl SessionAnnounce {
    pub fn is_valid(&self) -> bool {
        valid_name(&self.title) && self.max_viewers >= 1
    }
}

/// Host to each connected member, optional: who watches and who plays.
/// Sent after the announcement and on every change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStatus {
    pub session: SessionId,
    pub viewers: Vec<MemberId>,
    /// The viewer holding the seat; `None` while the host plays.
    pub controller: Option<MemberId>,
    /// The host took control back with the panic key; requests are declined
    /// until the host allows control again.
    pub control_paused: bool,
}

impl SessionStatus {
    pub fn is_valid(&self) -> bool {
        self.viewers.len() <= MAX_VIEWERS_LISTED
            && self.controller.is_none_or(|c| self.viewers.contains(&c))
    }
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

/// Viewer to host: may I play?
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlRequest {
    pub session: SessionId,
}

/// Viewer to host: withdraws my request, or gives control back if I have it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlCancel {
    pub session: SessionId,
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

    #[test]
    fn announcements_and_statuses_are_checked() {
        let announce = SessionAnnounce {
            session: SessionId(1),
            title: "Celeste".into(),
            mode: SessionMode::SinglePlayer,
            keyboard_mouse: false,
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
                ..announce
            }
            .is_valid()
        );

        let bia = MemberId([1; 32]);
        let status = SessionStatus {
            session: SessionId(1),
            viewers: vec![bia],
            controller: Some(bia),
            control_paused: false,
        };
        assert!(status.is_valid());
        assert!(
            !SessionStatus {
                controller: Some(MemberId([2; 32])),
                ..status.clone()
            }
            .is_valid(),
            "the controller must be a viewer"
        );
        assert!(
            !SessionStatus {
                viewers: vec![bia; MAX_VIEWERS_LISTED + 1],
                controller: None,
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
            controller: Some(MemberId([0xff; 32])),
            control_paused: true,
        };
        let len = postcard::to_stdvec(&status).unwrap().len();
        assert!(len + 2 <= crate::envelope::MAX_FRAME_LEN, "{len} bytes");
    }
}
