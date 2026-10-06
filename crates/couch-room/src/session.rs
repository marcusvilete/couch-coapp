//! A session as its host runs it: who watches, who asked to play, who
//! plays, and its end (docs/protocol.md, Sessions).
//!
//! Sans-IO, like [`crate::room`]: the engine reports what happened and
//! carries out the returned [`Effect`]s.
//!
//! **The seat router decides who plays.** `couch_input::router::SeatRouter`
//! enforces every input rule and is the only judge of who holds the seat.
//! This machine asks it to hand over or take back the seat
//! ([`Effect::Grant`], [`Effect::Revoke`], [`Effect::RevokeAll`]) and only
//! mirrors the outcome: the engine reports each of the router's grants and
//! revocations to [`HostSession::seat_changed`], including those the router
//! makes alone (a holder who went quiet too long).
//!
//! **Only viewers play.** Requests come from viewers only. A grant goes only
//! to a viewer, never while control is paused (the panic key) or after the
//! end. A viewer who leaves loses its request and the seat at once.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use couch_proto::{
    control::ControlMessage,
    ids::{MemberId, Permissions, SessionId},
    room::valid_name,
    session::{
        ControlDeclined, EndReason, JoinResult, SessionAnnounce, SessionEnded, SessionJoinReply,
        SessionMode, SessionStatus,
    },
};

/// Viewers a session takes unless its host chooses otherwise.
pub const DEFAULT_MAX_VIEWERS: u8 = 4;
/// The most a host may choose: everyone else in a full room of 8. Each
/// viewer costs the host an encoder and an upload stream.
pub const MAX_VIEWERS: u8 = 7;
/// After a decline, the same viewer's next request waits this long.
pub const DECLINE_COOLDOWN: Duration = Duration::from_secs(5);

/// What the host chose when going live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    pub title: String,
    pub mode: SessionMode,
    /// Whoever plays may use keyboard and mouse, not just a gamepad.
    pub keyboard_mouse: bool,
    pub max_viewers: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("titles must be 1 to 96 bytes, with nothing invisible in them")]
    InvalidTitle,
    #[error("a session takes 1 to {MAX_VIEWERS} viewers")]
    InvalidMaxViewers,
    #[error("that member isn't watching")]
    NotViewer,
    #[error("that member hasn't asked for control")]
    NoRequest,
    #[error("control is paused; allow it again first")]
    ControlPaused,
    #[error("the session has ended")]
    Ended,
}

/// What the engine must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Send to one member, on its connection in the session's room.
    Send {
        to: MemberId,
        message: ControlMessage,
    },
    /// Send to every member connected in the session's room.
    Broadcast(ControlMessage),
    /// Start streaming to this viewer: its encoder, video and input.
    StartViewer(MemberId),
    /// Stop streaming to this viewer, releasing what it held first.
    StopViewer(MemberId),
    /// Ask the seat router to hand the seat to `member`.
    Grant {
        member: MemberId,
        permissions: Permissions,
    },
    /// Ask the seat router to give the seat back to the host: only if
    /// `holder` still holds it, or from whoever does with `None`. A request
    /// aimed at one viewer must never end another's hold.
    Revoke { holder: Option<MemberId> },
    /// Ask the seat router to give every seat back to the host at once.
    RevokeAll,
    /// A viewer asked for control: the host should notice.
    Requested(MemberId),
    /// Something the host's UI shows changed.
    Changed,
}

#[derive(Debug, Clone)]
pub struct HostSession {
    id: SessionId,
    config: SessionConfig,
    /// In the order they joined.
    viewers: Vec<MemberId>,
    /// Oldest first; one per viewer.
    requests: Vec<MemberId>,
    /// Declined viewers and when they may ask again.
    cooldowns: BTreeMap<MemberId, Instant>,
    /// Who holds the seat, as the router last reported it.
    controller: Option<MemberId>,
    paused: bool,
    ended: bool,
}

impl HostSession {
    pub fn new(id: SessionId, config: SessionConfig) -> Result<Self, SessionError> {
        if !valid_name(&config.title) {
            return Err(SessionError::InvalidTitle);
        }
        if !(1..=MAX_VIEWERS).contains(&config.max_viewers) {
            return Err(SessionError::InvalidMaxViewers);
        }
        Ok(Self {
            id,
            config,
            viewers: Vec::new(),
            requests: Vec::new(),
            cooldowns: BTreeMap::new(),
            controller: None,
            paused: false,
            ended: false,
        })
    }

    // ---- Queries ----

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    pub fn viewers(&self) -> &[MemberId] {
        &self.viewers
    }

    pub fn is_viewer(&self, member: MemberId) -> bool {
        self.viewers.contains(&member)
    }

    /// Viewers waiting for an answer, oldest first.
    pub fn requests(&self) -> &[MemberId] {
        &self.requests
    }

    /// The viewer holding the seat; `None` while the host plays.
    pub fn controller(&self) -> Option<MemberId> {
        self.controller
    }

    pub fn control_paused(&self) -> bool {
        self.paused
    }

    pub fn is_ended(&self) -> bool {
        self.ended
    }

    /// What a viewer holding the seat may send.
    pub fn permissions(&self) -> Permissions {
        if self.config.keyboard_mouse {
            Permissions::GAMEPAD | Permissions::KEYBOARD | Permissions::MOUSE
        } else {
            Permissions::GUEST_DEFAULT
        }
    }

    pub fn announce(&self) -> ControlMessage {
        ControlMessage::SessionAnnounce(SessionAnnounce {
            session: self.id,
            title: self.config.title.clone(),
            mode: self.config.mode,
            keyboard_mouse: self.config.keyboard_mouse,
            max_viewers: self.config.max_viewers,
        })
    }

    pub fn status(&self) -> ControlMessage {
        ControlMessage::SessionStatus(SessionStatus {
            session: self.id,
            viewers: self.viewers.clone(),
            controller: self.controller,
            control_paused: self.paused,
        })
    }

    // ---- What members do ----

    /// A member connected to us in the session's room: tell it we are live.
    pub fn connected(&self, member: MemberId) -> Vec<Effect> {
        if self.ended {
            return Vec::new();
        }
        vec![
            Effect::Send {
                to: member,
                message: self.announce(),
            },
            Effect::Send {
                to: member,
                message: self.status(),
            },
        ]
    }

    /// A member asks to watch.
    pub fn join(&mut self, member: MemberId) -> Vec<Effect> {
        let reply = |result| Effect::Send {
            to: member,
            message: ControlMessage::SessionJoinReply(SessionJoinReply {
                session: self.id,
                result,
            }),
        };
        if self.ended {
            return vec![reply(JoinResult::Ended)];
        }
        if self.is_viewer(member) {
            return vec![reply(JoinResult::Joined)];
        }
        if self.viewers.len() >= usize::from(self.config.max_viewers) {
            return vec![reply(JoinResult::Full)];
        }
        self.viewers.push(member);
        vec![
            Effect::StartViewer(member),
            reply(JoinResult::Joined),
            Effect::Broadcast(self.status()),
            Effect::Changed,
        ]
    }

    /// The host couldn't stream to this viewer (no encoder, say): it is
    /// dropped and told so, even after being told it joined.
    pub fn viewer_failed(&mut self, member: MemberId) -> Vec<Effect> {
        if !self.is_viewer(member) {
            return Vec::new();
        }
        let mut fx = self.drop_viewer(member);
        fx.push(Effect::Send {
            to: member,
            message: ControlMessage::SessionJoinReply(SessionJoinReply {
                session: self.id,
                result: JoinResult::Unavailable,
            }),
        });
        fx
    }

    /// A viewer stopped watching, or is gone: its connection closed, it left
    /// the room, it was banned.
    pub fn leave(&mut self, member: MemberId) -> Vec<Effect> {
        if !self.is_viewer(member) {
            return Vec::new();
        }
        self.drop_viewer(member)
    }

    fn drop_viewer(&mut self, member: MemberId) -> Vec<Effect> {
        self.viewers.retain(|v| *v != member);
        self.requests.retain(|r| *r != member);
        if self.controller == Some(member) {
            // Stopping the viewer releases what it held; the router's own
            // report of that follows.
            self.controller = None;
        }
        let mut fx = vec![Effect::StopViewer(member)];
        if !self.ended {
            fx.push(Effect::Broadcast(self.status()));
        }
        fx.push(Effect::Changed);
        fx
    }

    /// A viewer asks for control.
    pub fn request(&mut self, member: MemberId, now: Instant) -> Vec<Effect> {
        if self.ended
            || !self.is_viewer(member)
            || self.controller == Some(member)
            || self.requests.contains(&member)
        {
            return Vec::new();
        }
        let cooling = self
            .cooldowns
            .get(&member)
            .is_some_and(|until| now < *until);
        if self.paused || cooling {
            return vec![self.declined(member)];
        }
        self.cooldowns.remove(&member);
        self.requests.push(member);
        vec![Effect::Requested(member), Effect::Changed]
    }

    /// A viewer withdraws its request, or gives control back.
    pub fn cancel(&mut self, member: MemberId) -> Vec<Effect> {
        if self.requests.contains(&member) {
            self.requests.retain(|r| *r != member);
            return vec![Effect::Changed];
        }
        if self.controller == Some(member) && !self.ended {
            return vec![Effect::Revoke {
                holder: Some(member),
            }];
        }
        Vec::new()
    }

    /// The seat router handed the seat to `holder`, or back to the host.
    pub fn seat_changed(&mut self, holder: Option<MemberId>) -> Vec<Effect> {
        match holder {
            // A grant that crossed a departure, the panic key or the end:
            // the seat goes straight back.
            Some(member) if self.ended || self.paused || !self.is_viewer(member) => {
                vec![Effect::Revoke {
                    holder: Some(member),
                }]
            }
            Some(member) => {
                self.requests.retain(|r| *r != member);
                self.mirror(Some(member))
            }
            None => self.mirror(None),
        }
    }

    fn mirror(&mut self, controller: Option<MemberId>) -> Vec<Effect> {
        if self.controller == controller {
            return Vec::new();
        }
        self.controller = controller;
        let mut fx = Vec::new();
        if !self.ended {
            fx.push(Effect::Broadcast(self.status()));
        }
        fx.push(Effect::Changed);
        fx
    }

    // ---- What the host does ----

    /// Accepts or declines a viewer's request.
    pub fn answer(
        &mut self,
        member: MemberId,
        accept: bool,
        now: Instant,
    ) -> Result<Vec<Effect>, SessionError> {
        if self.ended {
            return Err(SessionError::Ended);
        }
        if !self.requests.contains(&member) {
            return Err(SessionError::NoRequest);
        }
        if accept && self.paused {
            return Err(SessionError::ControlPaused);
        }
        self.requests.retain(|r| *r != member);
        if accept {
            return Ok(vec![self.grant(member), Effect::Changed]);
        }
        self.cooldowns.insert(member, now + DECLINE_COOLDOWN);
        Ok(vec![self.declined(member), Effect::Changed])
    }

    /// Hands control to a viewer who didn't ask.
    pub fn pass(&mut self, member: MemberId) -> Result<Vec<Effect>, SessionError> {
        if self.ended {
            return Err(SessionError::Ended);
        }
        if !self.is_viewer(member) {
            return Err(SessionError::NotViewer);
        }
        if self.paused {
            return Err(SessionError::ControlPaused);
        }
        if self.controller == Some(member) {
            return Ok(Vec::new());
        }
        self.requests.retain(|r| *r != member);
        Ok(vec![self.grant(member), Effect::Changed])
    }

    /// The host plays again. Sent to the router even if no viewer seems to
    /// hold the seat: a grant may still be on its way.
    pub fn take_back(&mut self) -> Vec<Effect> {
        if self.ended {
            return Vec::new();
        }
        vec![Effect::Revoke { holder: None }]
    }

    /// The panic key: every seat back to the host, every request declined,
    /// and no more control until the host allows it again. Pressing it again
    /// releases again.
    pub fn panic(&mut self) -> Vec<Effect> {
        let mut fx = vec![Effect::RevokeAll];
        if self.ended {
            return fx;
        }
        let waiting = std::mem::take(&mut self.requests);
        fx.extend(waiting.into_iter().map(|member| self.declined(member)));
        if !self.paused {
            self.paused = true;
            fx.push(Effect::Broadcast(self.status()));
        }
        fx.push(Effect::Changed);
        fx
    }

    pub fn allow_control(&mut self) -> Vec<Effect> {
        if self.ended || !self.paused {
            return Vec::new();
        }
        self.paused = false;
        vec![Effect::Broadcast(self.status()), Effect::Changed]
    }

    /// The session is over: everyone in the room is told why. The engine
    /// stops the stream, which releases all input first.
    pub fn end(&mut self, reason: EndReason) -> Vec<Effect> {
        if self.ended {
            return Vec::new();
        }
        self.ended = true;
        self.requests.clear();
        vec![
            Effect::Broadcast(ControlMessage::SessionEnded(SessionEnded {
                session: self.id,
                reason,
            })),
            Effect::Changed,
        ]
    }

    fn grant(&self, member: MemberId) -> Effect {
        Effect::Grant {
            member,
            permissions: self.permissions(),
        }
    }

    fn declined(&self, member: MemberId) -> Effect {
        Effect::Send {
            to: member,
            message: ControlMessage::ControlDeclined(ControlDeclined { session: self.id }),
        }
    }
}

#[cfg(test)]
mod tests;
