//! A session as its host runs it: who watches, who asked to play which
//! seat, who plays, and its end (docs/protocol.md, Sessions).
//!
//! Sans-IO, like [`crate::room`]: the engine reports what happened and
//! carries out the returned [`Effect`]s.
//!
//! **The seat router decides who plays.** `couch_input::router::SeatRouter`
//! enforces every input rule and is the only judge of who holds each seat.
//! This machine asks it to hand over or take back seats ([`Effect::Grant`],
//! [`Effect::Revoke`], [`Effect::RevokeViewers`]) and only mirrors the outcome:
//! the engine reports each of the router's grants and revocations to
//! [`HostSession::seat_changed`], including those the router makes alone (a
//! holder who went quiet too long).
//!
//! **Only viewers play, as many seats as they have controllers.** Requests
//! come from viewers only. A grant goes only to a viewer, never while control
//! is paused (the panic key) or after the end. A viewer may hold several
//! seats (M18): one with a pad for each controller it says it has, plus the
//! keyboard-and-mouse seat ([`HostSession::keeps`]). A grant beyond that
//! moves the viewer, and is then preceded by revoking it from every other
//! seat, on the same ordered channel, so it never holds more. A viewer who
//! leaves loses its request and its seats at once.
//!
//! **The host may play a seat too** ([`SessionConfig::host`]): their own
//! controller, read on the host, drives a seat's pad through the router like
//! a viewer's, with the pad only. They start on the first seat with a pad,
//! hold one seat at a time, and may take a seat while control is paused (the
//! panic key gives control back to them). When a viewer lets go of the seat
//! they last held (gives it back, leaves, goes quiet) and they hold none, it
//! goes back to them through [`Effect::Claim`], which the router carries out
//! only if the seat is still free, so it can never take a seat from a viewer
//! it was just passed to. A seat the host takes back, their own included,
//! stays empty until they play one again.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use couch_proto::{
    control::ControlMessage,
    ids::{MemberId, Permissions, SeatId, SessionId},
    room::valid_name,
    session::{
        ControlDeclined, EndReason, JoinResult, SeatKind, SessionAnnounce, SessionEnded,
        SessionJoinReply, SessionStatus, StreamState, valid_seats,
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
    /// The seats players can hold, by [`SeatId`]; one is single player.
    pub seats: Vec<SeatKind>,
    pub max_viewers: u8,
    /// The host's own id, when their controller plays through a seat;
    /// `None` keeps them outside the seats, with whatever devices the game
    /// sees directly. Ignored without a seat that has a pad.
    pub host: Option<MemberId>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("titles must be 1 to 96 bytes, with nothing invisible in them")]
    InvalidTitle,
    #[error("a session takes 1 to {MAX_VIEWERS} viewers")]
    InvalidMaxViewers,
    #[error("a session has 1 to 4 controllers and at most one keyboard-and-mouse seat")]
    InvalidSeats,
    #[error("there is no such seat")]
    NoSuchSeat,
    #[error("that member isn't watching")]
    NotViewer,
    #[error("that member hasn't asked for control")]
    NoRequest,
    #[error("control is paused; allow it again first")]
    ControlPaused,
    #[error("your controller doesn't play through a seat in this session")]
    HostNotOnASeat,
    #[error("that seat has no controller")]
    NoPad,
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
    /// Ask the seat router to hand `seat` to `member`.
    Grant {
        seat: SeatId,
        member: MemberId,
        permissions: Permissions,
    },
    /// Ask the seat router to free `seat`: only if `holder` still holds it,
    /// or from whoever does with `None`. A request aimed at one viewer must
    /// never end another's hold.
    Revoke {
        seat: SeatId,
        holder: Option<MemberId>,
    },
    /// Ask the seat router to free every seat a viewer holds at once (the
    /// panic key); the one the host plays stays theirs.
    RevokeViewers,
    /// Ask the seat router to hand `seat` to `member` only if nobody holds
    /// it: the host's seat coming back to them. Refused while control is
    /// paused.
    Claim { seat: SeatId, member: MemberId },
    /// A viewer asked for a seat: the host should notice.
    Requested(MemberId, SeatId),
    /// Something the host's UI shows changed.
    Changed,
}

/// What the host last did with the seat their controller last played.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Home {
    /// They play it: if it goes free on its own (their controller went
    /// quiet, say), it comes back to them.
    Playing,
    /// They passed it to a viewer: it comes back when that viewer lets go
    /// (gives it back, leaves, goes quiet). It stays `Lent` once back with
    /// the host: telling that apart would mean trusting the router's
    /// reports, which may trail a newer pass. Taking everything back then
    /// makes it `LeftEmpty` though they play it, which only matters if their
    /// own seat goes free on its own (their controller's reader stopped).
    Lent,
    /// They took it back, or left it: it stays empty until they play a seat
    /// again.
    LeftEmpty,
}

#[derive(Debug, Clone)]
pub struct HostSession {
    id: SessionId,
    config: SessionConfig,
    /// In the order they joined.
    viewers: Vec<MemberId>,
    /// Oldest first; one per viewer, naming the seat it wants.
    requests: Vec<(MemberId, SeatId)>,
    /// Declined viewers and when they may ask again.
    cooldowns: BTreeMap<MemberId, Instant>,
    /// Who holds each seat, as the router last reported it, the host
    /// included.
    holders: Vec<Option<MemberId>>,
    /// The seat the host last held, which comes back to them when free;
    /// `None` while the host plays outside the seats.
    host_home: Option<SeatId>,
    /// What the host last did with `host_home`. Only the host's own actions
    /// change it, never the router's reports, which may be out of date.
    home: Home,
    paused: bool,
    ended: bool,
    /// Whether anything is streamed (M19): nothing at first.
    stream: StreamState,
    /// How many controllers each viewer said it has (M18; 0 until told):
    /// whether a seat with a pad is added to theirs or replaces it.
    controllers: BTreeMap<MemberId, u8>,
    /// Grants sent to the router and not yet reported back, in the order
    /// sent: they count toward a viewer's seats too, so two quick grants
    /// never give it more than its controllers allow.
    pending: Vec<PendingGrant>,
}

/// A grant on its way to the router (M18).
#[derive(Debug, Clone, Copy)]
struct PendingGrant {
    seat: SeatId,
    member: MemberId,
    /// It moves the member (its other pad seats revoked first), rather than
    /// adding a seat.
    moves: bool,
}

/// The most controllers a viewer counts as having: XInput's four slots.
pub const MAX_CONTROLLERS: u8 = 4;

impl HostSession {
    pub fn new(id: SessionId, config: SessionConfig) -> Result<Self, SessionError> {
        if !valid_name(&config.title) {
            return Err(SessionError::InvalidTitle);
        }
        if !(1..=MAX_VIEWERS).contains(&config.max_viewers) {
            return Err(SessionError::InvalidMaxViewers);
        }
        if !valid_seats(&config.seats) {
            return Err(SessionError::InvalidSeats);
        }
        let host_home = config
            .host
            .and_then(|_| {
                config
                    .seats
                    .iter()
                    .position(|k| k.permissions().contains(Permissions::GAMEPAD))
            })
            .map(|i| SeatId(i as u8));
        Ok(Self {
            id,
            holders: vec![None; config.seats.len()],
            host_home,
            home: Home::Playing,
            config,
            viewers: Vec::new(),
            requests: Vec::new(),
            cooldowns: BTreeMap::new(),
            paused: false,
            ended: false,
            stream: StreamState::Waiting,
            controllers: BTreeMap::new(),
            pending: Vec::new(),
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

    /// Viewers waiting for an answer and the seat each wants, oldest first.
    pub fn requests(&self) -> &[(MemberId, SeatId)] {
        &self.requests
    }

    /// Each seat's holder, by seat, the host included; `None` while free.
    pub fn holders(&self) -> &[Option<MemberId>] {
        &self.holders
    }

    /// The host's id while their controller plays through a seat.
    pub fn host(&self) -> Option<MemberId> {
        self.host_home.and(self.config.host)
    }

    /// The seat the host plays now.
    pub fn host_seat(&self) -> Option<SeatId> {
        self.host().and_then(|host| self.seat_of(host))
    }

    fn is_host(&self, member: MemberId) -> bool {
        self.host() == Some(member)
    }

    fn has_pad(&self, seat: SeatId) -> bool {
        self.permissions(seat).contains(Permissions::GAMEPAD)
    }

    /// The first seat `member` holds, if any (the host holds one at most).
    pub fn seat_of(&self, member: MemberId) -> Option<SeatId> {
        self.seats_of(member).into_iter().next()
    }

    /// Every seat `member` holds, in seat order (M18: a viewer with several
    /// controllers may hold several).
    pub fn seats_of(&self, member: MemberId) -> Vec<SeatId> {
        self.holders
            .iter()
            .enumerate()
            .filter(|(_, h)| **h == Some(member))
            .map(|(i, _)| SeatId(i as u8))
            .collect()
    }

    /// How many controllers `member` said it has.
    pub fn controllers_of(&self, member: MemberId) -> u8 {
        self.controllers.get(&member).copied().unwrap_or(0)
    }

    /// Whether `member` would keep its other seats on being given `seat`
    /// (M18), rather than move to it:
    /// - a seat without a pad (keyboard and mouse) needs no controller: added;
    /// - a seat with a pad is added while the viewer holds fewer seats with a
    ///   pad than it has controllers; otherwise it moves, as before M18.
    ///
    /// The host's own controller holds one seat at most: never added.
    pub fn keeps(&self, member: MemberId, seat: SeatId) -> bool {
        if self.is_host(member) {
            return false;
        }
        if !self.has_pad(seat) {
            return true;
        }
        // The pad seats it holds, and those on their way to it.
        let on_way = self
            .pending
            .iter()
            .filter(|p| p.member == member)
            .map(|p| p.seat);
        let pads: BTreeSet<SeatId> = self
            .seats_of(member)
            .into_iter()
            .chain(on_way)
            .filter(|s| *s != seat && self.has_pad(*s))
            .collect();
        pads.len() < usize::from(self.controllers_of(member))
    }

    pub fn control_paused(&self) -> bool {
        self.paused
    }

    pub fn is_ended(&self) -> bool {
        self.ended
    }

    /// Whether anything is streamed (M19).
    pub fn stream(&self) -> StreamState {
        self.stream
    }

    /// What a viewer holding `seat` may send.
    pub fn permissions(&self, seat: SeatId) -> Permissions {
        self.config
            .seats
            .get(usize::from(seat.0))
            .map_or(Permissions::empty(), |kind| kind.permissions())
    }

    fn has_seat(&self, seat: SeatId) -> bool {
        usize::from(seat.0) < self.config.seats.len()
    }

    pub fn announce(&self) -> ControlMessage {
        ControlMessage::SessionAnnounce(SessionAnnounce {
            session: self.id,
            title: self.config.title.clone(),
            seats: self.config.seats.clone(),
            max_viewers: self.config.max_viewers,
        })
    }

    pub fn status(&self) -> ControlMessage {
        ControlMessage::SessionStatus(SessionStatus {
            session: self.id,
            viewers: self.viewers.clone(),
            holders: self
                .holders
                .iter()
                .map(|h| h.filter(|h| !self.is_host(*h)))
                .collect(),
            host_seat: self.host_seat(),
            host_on_seats: self.host().is_some(),
            control_paused: self.paused,
            stream: self.stream,
        })
    }

    // ---- What members do ----

    /// The host's controller can't play through a seat after all (it
    /// couldn't be hidden, say): the host plays outside the seats. Call
    /// before [`Self::start`].
    pub fn without_host(&mut self) {
        self.host_home = None;
    }

    /// The stream is up: the host's controller takes its first seat.
    pub fn start(&self) -> Vec<Effect> {
        self.reclaim().into_iter().collect()
    }

    /// The host's seat back to them, if they hold none, it is free (as far
    /// as the mirror knows: the router checks again), and they didn't take
    /// it back to leave it empty. Never while control is paused: the panic
    /// key leaves the host the seat they play, and frees the rest.
    fn reclaim(&self) -> Option<Effect> {
        let (host, home) = (self.host()?, self.host_home?);
        if self.ended
            || self.paused
            || self.home == Home::LeftEmpty
            || self.seat_of(host).is_some()
            || self.holders[usize::from(home.0)].is_some()
        {
            return None;
        }
        Some(Effect::Claim {
            seat: home,
            member: host,
        })
    }

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
        // The host plays; they never watch their own stream.
        if self.is_host(member) {
            return Vec::new();
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
        self.requests.retain(|(r, _)| *r != member);
        self.controllers.remove(&member);
        self.pending.retain(|p| p.member != member);
        // Stopping the viewer releases what it held; the router's own report
        // of that follows.
        for holder in &mut self.holders {
            if *holder == Some(member) {
                *holder = None;
            }
        }
        // Stopping the viewer frees its seat before the claim reaches the
        // router: both go to it in this order.
        let mut fx = vec![Effect::StopViewer(member)];
        fx.extend(self.reclaim());
        if !self.ended {
            fx.push(Effect::Broadcast(self.status()));
        }
        fx.push(Effect::Changed);
        fx
    }

    /// A viewer asks for `seat`, free or held by another viewer. Asking for
    /// another seat replaces its pending request.
    pub fn request(&mut self, member: MemberId, seat: SeatId, now: Instant) -> Vec<Effect> {
        if self.ended
            || !self.is_viewer(member)
            || !self.has_seat(seat)
            || self.seats_of(member).contains(&seat)
            || self.requests.contains(&(member, seat))
        {
            return Vec::new();
        }
        let cooling = self
            .cooldowns
            .get(&member)
            .is_some_and(|until| now < *until);
        // Neither leaves a request pending: the panic key declined them all,
        // and a decline is what started the cooldown.
        if self.paused || cooling {
            return vec![self.declined(member)];
        }
        self.cooldowns.remove(&member);
        self.requests.retain(|(r, _)| *r != member);
        self.requests.push((member, seat));
        vec![Effect::Requested(member, seat), Effect::Changed]
    }

    /// A viewer withdraws its request; the seat it holds, if any, stays.
    pub fn withdraw(&mut self, member: MemberId) -> Vec<Effect> {
        if !self.requests.iter().any(|(r, _)| *r == member) {
            return Vec::new();
        }
        self.requests.retain(|(r, _)| *r != member);
        vec![Effect::Changed]
    }

    /// A viewer gives `seat` back, or with `None` every seat it holds. A
    /// pending request stays: only a withdraw ends that.
    pub fn give_back(&mut self, member: MemberId, seat: Option<SeatId>) -> Vec<Effect> {
        if self.ended {
            return Vec::new();
        }
        self.seats_of(member)
            .into_iter()
            .filter(|s| seat.is_none_or(|one| one == *s))
            .map(|s| Effect::Revoke {
                seat: s,
                holder: Some(member),
            })
            .collect()
    }

    /// A viewer says how many controllers it has (M18), capped at
    /// [`MAX_CONTROLLERS`]. It decides only whether a seat is added or
    /// replaces theirs; the host still decides who plays.
    pub fn set_controllers(&mut self, member: MemberId, count: u8) -> Vec<Effect> {
        if self.ended || !self.is_viewer(member) {
            return Vec::new();
        }
        let count = count.min(MAX_CONTROLLERS);
        if self.controllers.insert(member, count) == Some(count) {
            return Vec::new();
        }
        vec![Effect::Changed]
    }

    /// The seat router handed `seat` to `holder`, or back to the host.
    pub fn seat_changed(&mut self, seat: SeatId, holder: Option<MemberId>) -> Vec<Effect> {
        if !self.has_seat(seat) {
            return Vec::new();
        }
        // The router takes grants in order: a report of `holder` on this seat
        // answers its grant, and any earlier one for the seat (overtaken).
        // A release answers none: a grant may still follow it.
        let answered = holder.and_then(|m| {
            self.pending
                .iter()
                .position(|p| p.seat == seat && p.member == m)
        });
        let moved = answered.map(|i| {
            let moves = self.pending[i].moves;
            let mut at = 0;
            self.pending.retain(|p| {
                let keep = at > i || p.seat != seat;
                at += 1;
                keep
            });
            moves
        });
        let host = holder.is_some_and(|h| self.is_host(h));
        match holder {
            // A grant that crossed a departure, the panic key or the end:
            // the seat goes straight back. The host's own needn't wait for
            // the panic key to be undone.
            Some(member) if self.ended || (!host && (self.paused || !self.is_viewer(member))) => {
                vec![Effect::Revoke {
                    seat,
                    holder: Some(member),
                }]
            }
            Some(member) => {
                if host && self.host_home != Some(seat) {
                    // Only claims and moves grant the host a seat, both to
                    // `host_home`: a report of another is out of date.
                    return vec![Effect::Revoke {
                        seat,
                        holder: Some(member),
                    }];
                }
                // A request for this seat is answered; one for another seat
                // stays, and the viewer keeps asking too.
                self.requests.retain(|r| *r != (member, seat));
                // Never more pad seats than the rule allows: a grant that
                // moves revokes the others first, so this only guards against
                // a report out of order. A grant that added keeps what is
                // held, even if the viewer's count dropped meanwhile (a seat
                // already held stays; its pad goes neutral).
                let moves = moved.unwrap_or_else(|| !self.keeps(member, seat));
                let extra = if moves {
                    self.seats_of(member)
                } else {
                    Vec::new()
                };
                let mut fx: Vec<Effect> = extra
                    .into_iter()
                    .filter(|s| *s != seat && self.has_pad(*s))
                    .map(|s| Effect::Revoke {
                        seat: s,
                        holder: Some(member),
                    })
                    .collect();
                fx.extend(self.mirror(seat, Some(member)));
                fx
            }
            None => {
                let mut fx = self.mirror(seat, None);
                fx.extend(self.reclaim());
                fx
            }
        }
    }

    fn mirror(&mut self, seat: SeatId, holder: Option<MemberId>) -> Vec<Effect> {
        let slot = &mut self.holders[usize::from(seat.0)];
        if *slot == holder {
            return Vec::new();
        }
        *slot = holder;
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
        let Some(&(_, seat)) = self.requests.iter().find(|(r, _)| *r == member) else {
            return Err(SessionError::NoRequest);
        };
        if accept && self.paused {
            return Err(SessionError::ControlPaused);
        }
        self.requests.retain(|(r, _)| *r != member);
        if accept {
            let mut fx = self.grant(member, seat);
            fx.push(Effect::Changed);
            return Ok(fx);
        }
        self.cooldowns.insert(member, now + DECLINE_COOLDOWN);
        Ok(vec![self.declined(member), Effect::Changed])
    }

    /// Hands `seat` to a viewer who didn't ask for it. A request of theirs
    /// for this seat is answered; one for another seat stays (the viewer
    /// still shows it as pending), as when the router reports the grant.
    pub fn pass(&mut self, member: MemberId, seat: SeatId) -> Result<Vec<Effect>, SessionError> {
        if self.ended {
            return Err(SessionError::Ended);
        }
        if !self.has_seat(seat) {
            return Err(SessionError::NoSuchSeat);
        }
        if !self.is_viewer(member) {
            return Err(SessionError::NotViewer);
        }
        if self.paused {
            return Err(SessionError::ControlPaused);
        }
        if self.holders[usize::from(seat.0)] == Some(member) {
            return Ok(Vec::new());
        }
        self.requests.retain(|r| *r != (member, seat));
        let mut fx = self.grant(member, seat);
        fx.push(Effect::Changed);
        Ok(fx)
    }

    /// The host takes `seat` back from a viewer, or leaves their own, or
    /// takes every seat but their own with `None`. Sent to the router even
    /// if no viewer seems to hold it: a grant may still be on its way. The
    /// seat goes free and stays free: the host plays one again only with
    /// [`Self::play`].
    pub fn take_back(&mut self, seat: Option<SeatId>) -> Vec<Effect> {
        if self.ended {
            return Vec::new();
        }
        match seat {
            Some(seat) if self.has_seat(seat) => {
                if self.host_home == Some(seat) {
                    self.home = Home::LeftEmpty;
                }
                vec![Effect::Revoke { seat, holder: None }]
            }
            Some(_) => Vec::new(),
            // The router itself tells the host's own seat apart: the mirror
            // may not know of it yet.
            None => {
                self.everything_taken_back();
                vec![Effect::RevokeViewers]
            }
        }
    }

    /// Every viewer's seat taken back: the host's, if they had passed it
    /// on, stays empty too.
    fn everything_taken_back(&mut self) {
        if self.home == Home::Lent {
            self.home = Home::LeftEmpty;
        }
    }

    /// The host plays `seat` with their own controller. A viewer on it loses
    /// it; the host's old seat goes free. Allowed while control is paused:
    /// it is the host's own.
    pub fn play(&mut self, seat: SeatId) -> Result<Vec<Effect>, SessionError> {
        if self.ended {
            return Err(SessionError::Ended);
        }
        let host = self.host().ok_or(SessionError::HostNotOnASeat)?;
        if !self.has_seat(seat) {
            return Err(SessionError::NoSuchSeat);
        }
        if !self.has_pad(seat) {
            return Err(SessionError::NoPad);
        }
        if self.holders[usize::from(seat.0)] == Some(host) {
            // Playing it already, as far as the mirror knows; if a leave is
            // still on its way, the seat comes back to them once it lands.
            if self.host_home == Some(seat) {
                self.home = Home::Playing;
            }
            return Ok(Vec::new());
        }
        // From now on only this seat comes back to the host: a claim for the
        // old one, once the router frees it, would give them two.
        self.host_home = Some(seat);
        self.home = Home::Playing;
        let mut fx = self.grant(host, seat);
        fx.push(Effect::Changed);
        Ok(fx)
    }

    /// The panic key: every seat a viewer holds freed (the one the host
    /// plays stays theirs), every request declined, and no more control for
    /// viewers until the host allows it again. Pressing it again releases
    /// again.
    pub fn panic(&mut self) -> Vec<Effect> {
        let mut fx = vec![Effect::RevokeViewers];
        if self.ended {
            return fx;
        }
        let host = self.host();
        self.pending.retain(|p| Some(p.member) == host);
        self.everything_taken_back();
        let waiting = std::mem::take(&mut self.requests);
        fx.extend(waiting.into_iter().map(|(member, _)| self.declined(member)));
        if !self.paused {
            self.paused = true;
            fx.push(Effect::Broadcast(self.status()));
        }
        fx.push(Effect::Changed);
        fx
    }

    /// A stream started or ended inside the session (M19): everyone in the
    /// room hears of it. Seats and viewers stay; the engine holds viewers'
    /// input off while nothing is streamed.
    pub fn set_stream(&mut self, stream: StreamState) -> Vec<Effect> {
        if self.ended || self.stream == stream {
            return Vec::new();
        }
        self.stream = stream;
        vec![Effect::Broadcast(self.status()), Effect::Changed]
    }

    /// Viewers may be given control again. The host's seat, if it went free
    /// meanwhile without being left empty on purpose, comes back to them.
    pub fn allow_control(&mut self) -> Vec<Effect> {
        if self.ended || !self.paused {
            return Vec::new();
        }
        self.paused = false;
        let mut fx: Vec<Effect> = self.reclaim().into_iter().collect();
        fx.extend([Effect::Broadcast(self.status()), Effect::Changed]);
        fx
    }

    /// The session is over: everyone in the room is told why. The engine
    /// stops the stream, which releases all input first.
    pub fn end(&mut self, reason: EndReason) -> Vec<Effect> {
        if self.ended {
            return Vec::new();
        }
        self.ended = true;
        self.requests.clear();
        self.pending.clear();
        vec![
            Effect::Broadcast(ControlMessage::SessionEnded(SessionEnded {
                session: self.id,
                reason,
            })),
            Effect::Changed,
        ]
    }

    /// Grants `seat`, after revoking `member` from every other seat with a
    /// pad when it moves rather than adds ([`Self::keeps`]): the router takes
    /// them in order, so a viewer that moves never holds more pad seats than
    /// it may, even with a grant still on its way. A keyboard-and-mouse seat
    /// stays: it needs no controller.
    fn grant(&mut self, member: MemberId, seat: SeatId) -> Vec<Effect> {
        // The host's seat passed on: it comes back when that viewer lets go.
        if !self.is_host(member) && self.host_home == Some(seat) {
            self.home = Home::Lent;
        }
        let moves = !self.keeps(member, seat);
        if moves {
            self.pending.retain(|p| p.member != member);
        }
        self.pending.push(PendingGrant {
            seat,
            member,
            moves,
        });
        let mut fx: Vec<Effect> = (0..self.config.seats.len())
            .map(|i| SeatId(i as u8))
            .filter(|s| moves && *s != seat && self.has_pad(*s))
            .map(|s| Effect::Revoke {
                seat: s,
                holder: Some(member),
            })
            .collect();
        // The host's keyboard and mouse reach the game directly; only their
        // controller goes through the seat.
        let permissions = if self.is_host(member) {
            Permissions::GAMEPAD
        } else {
            self.permissions(seat)
        };
        fx.push(Effect::Grant {
            seat,
            member,
            permissions,
        });
        fx
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
