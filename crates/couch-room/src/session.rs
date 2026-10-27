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
//! **The host may play seats too** ([`SessionConfig::host`]): each of their
//! own controllers ([`SessionConfig::host_controllers`], M23), read on the
//! host, drives one seat's pad through the router like a viewer's, with the
//! pad only. Each controller has its own seat (its *home*), starting on the
//! seats with a pad in order; [`Effect::HostSeats`] tells the input task
//! which controller drives which, before any claim. The host may take a seat
//! while control is paused (the panic key gives control back to them). When
//! a viewer lets go of a controller's seat (gives it back, leaves, goes
//! quiet), it goes back to that controller through [`Effect::Claim`], which
//! the router carries out only if the seat is still free, so it can never
//! take a seat from a viewer it was just passed to. A seat the host takes
//! back, their own included, stays empty until they play one again.

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
    /// The host's own id, when their controllers play through seats;
    /// `None` keeps them outside the seats, with whatever devices the game
    /// sees directly. Ignored without a seat that has a pad.
    pub host: Option<MemberId>,
    /// How many of the host's controllers play through seats (M23), each on
    /// its own seat with a pad, in seat order; capped at the seats with a
    /// pad. Ignored without `host`.
    pub host_controllers: u8,
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
    #[error("none of your controllers in this session is that one")]
    NoSuchController,
    #[error("that seat has no controller")]
    NoPad,
    #[error("say which of their controllers moves to it: each of them plays a seat already")]
    ChooseController,
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
    /// Ask the seat router to hand `seat` to `member`; `controller`, one of
    /// a viewer's controllers by slot, is the one that should drive it
    /// (P12, P13), told to the viewer with the grant.
    Grant {
        seat: SeatId,
        member: MemberId,
        permissions: Permissions,
        controller: Option<u8>,
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
    /// it: a host controller's seat coming back to it. Refused while control
    /// is paused, and for a seat no host controller has (a late one).
    Claim { seat: SeatId, member: MemberId },
    /// The seat each of the host's controllers drives, by controller (M23):
    /// the input task routes each controller's states to its own, and takes
    /// claims for these alone. Comes before the claims and grants it
    /// concerns, on the same ordered channel.
    HostSeats(Vec<SeatId>),
    /// A viewer asked for a seat: the host should notice.
    Requested(MemberId, SeatId),
    /// Something the host's UI shows changed.
    Changed,
}

/// One host controller's seat (M23): the seat it last played, which comes
/// back to it when free, and what the host last did with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HostHome {
    seat: SeatId,
    home: Home,
}

/// What the host last did with the seat one of their controllers last
/// played.
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
    /// Each host controller's seat, by controller, all different; empty
    /// while the host plays outside the seats. Only the host's own actions
    /// change them, never the router's reports, which may be out of date.
    homes: Vec<HostHome>,
    paused: bool,
    ended: bool,
    /// Whether anything is streamed (M19): nothing at first.
    stream: StreamState,
    /// How many controllers each viewer said it has (M18; 0 until told):
    /// whether a seat with a pad is added to theirs or replaces it.
    controllers: BTreeMap<MemberId, u8>,
    /// Which controller drives each pad seat a viewer holds, by slot, as it
    /// last said (P13): a grant beyond its controllers frees the seat of
    /// the controller that moves.
    drivers: BTreeMap<MemberId, Vec<(SeatId, u8)>>,
    /// The controller a viewer's pending request names (P13).
    asked_with: BTreeMap<MemberId, u8>,
    /// Grants sent to the router and not yet reported back, in the order
    /// sent: they count toward a viewer's seats too, so two quick grants
    /// never give it more than its controllers allow.
    pending: Vec<PendingGrant>,
}

/// A grant on its way to the router (M18).
#[derive(Debug, Clone)]
struct PendingGrant {
    seat: SeatId,
    member: MemberId,
    /// The member's other pad seats it frees (revoked first), so it never
    /// holds more than its controllers (P13: only as many as that takes).
    frees: Vec<SeatId>,
    /// A later grant freed this one's seat (revoked right behind it): it no
    /// longer counts toward the member's seats, but its report still
    /// answers it, so that report frees nothing more (P13 review).
    cancelled: bool,
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
        // Each controller on its own seat with a pad, in seat order.
        let homes = config
            .seats
            .iter()
            .enumerate()
            .filter(|(_, k)| k.permissions().contains(Permissions::GAMEPAD))
            .map(|(i, _)| HostHome {
                seat: SeatId(i as u8),
                home: Home::Playing,
            })
            .take(if config.host.is_some() {
                usize::from(config.host_controllers)
            } else {
                0
            })
            .collect();
        Ok(Self {
            id,
            holders: vec![None; config.seats.len()],
            homes,
            config,
            viewers: Vec::new(),
            requests: Vec::new(),
            cooldowns: BTreeMap::new(),
            paused: false,
            ended: false,
            stream: StreamState::Waiting,
            controllers: BTreeMap::new(),
            drivers: BTreeMap::new(),
            asked_with: BTreeMap::new(),
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

    /// The host's id while their controllers play through seats.
    pub fn host(&self) -> Option<MemberId> {
        self.config.host.filter(|_| !self.homes.is_empty())
    }

    /// The seats the host plays now, in seat order.
    pub fn host_seats(&self) -> Vec<SeatId> {
        self.host()
            .map_or_else(Vec::new, |host| self.seats_of(host))
    }

    /// The seat each of the host's controllers drives, by controller (M23).
    pub fn host_homes(&self) -> Vec<SeatId> {
        self.homes.iter().map(|h| h.seat).collect()
    }

    /// The host controller whose seat `seat` is, if any.
    pub fn host_controller_on(&self, seat: SeatId) -> Option<u8> {
        self.homes
            .iter()
            .position(|h| h.seat == seat)
            .map(|k| k as u8)
    }

    fn host_seats_effect(&self) -> Effect {
        Effect::HostSeats(self.host_homes())
    }

    fn is_host(&self, member: MemberId) -> bool {
        self.host() == Some(member)
    }

    fn has_pad(&self, seat: SeatId) -> bool {
        self.permissions(seat).contains(Permissions::GAMEPAD)
    }

    /// The first seat `member` holds, if any.
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

    /// The controller `member`'s pending request names (P13).
    pub fn asked_with(&self, member: MemberId) -> Option<u8> {
        self.asked_with.get(&member).copied()
    }

    /// The pad seat `member`'s controller `controller` drives, as it said.
    fn driven_by(&self, member: MemberId, controller: u8) -> Option<SeatId> {
        self.drivers
            .get(&member)?
            .iter()
            .find(|(_, c)| *c == controller)
            .map(|(s, _)| *s)
    }

    /// The pad seats `member` holds or has on their way, but `seat`, in
    /// seat order.
    fn pad_seats_but(&self, member: MemberId, seat: SeatId) -> Vec<SeatId> {
        let on_way = self
            .pending
            .iter()
            .filter(|p| p.member == member && !p.cancelled)
            .map(|p| p.seat);
        let pads: BTreeSet<SeatId> = self
            .seats_of(member)
            .into_iter()
            .chain(on_way)
            .filter(|s| *s != seat && self.has_pad(*s))
            .collect();
        pads.into_iter().collect()
    }

    /// Which of `member`'s pad seats giving it `seat` frees (P13): none
    /// while it has a controller to spare; otherwise only as many as keep it
    /// within its controllers, normally one. First the seat `controller`
    /// drives (it moves), then from the last seat down. And whether that
    /// was a guess: more seats than that to choose from and no controller
    /// naming one of them.
    pub fn frees(
        &self,
        member: MemberId,
        seat: SeatId,
        controller: Option<u8>,
    ) -> (Vec<SeatId>, bool) {
        if self.is_host(member) || !self.has_pad(seat) {
            return (Vec::new(), false);
        }
        let pads = self.pad_seats_but(member, seat);
        let room = usize::from(self.controllers_of(member)).saturating_sub(1);
        if pads.len() <= room {
            return (Vec::new(), false);
        }
        let excess = pads.len() - room;
        let moving = controller
            .and_then(|c| self.driven_by(member, c))
            .filter(|s| pads.contains(s));
        let mut order: Vec<SeatId> = moving.into_iter().collect();
        order.extend(pads.iter().rev().filter(|s| Some(**s) != moving));
        order.truncate(excess);
        (order, moving.is_none() && pads.len() > excess)
    }

    /// Whether `member` would keep its other seats on being given `seat`
    /// (M18), rather than move to it:
    /// - a seat without a pad (keyboard and mouse) needs no controller: added;
    /// - a seat with a pad is added while the viewer holds fewer seats with a
    ///   pad than it has controllers; otherwise one of its controllers moves
    ///   to it, leaving its seat ([`Self::frees`], P13; before it, every
    ///   other pad seat went).
    ///
    /// The host's controllers each keep their own seat (M23): a grant to the
    /// host never takes another of theirs; [`Self::play`] frees the moving
    /// controller's old seat itself, and their seats beyond the controllers'
    /// own are revoked as reports of them come.
    pub fn keeps(&self, member: MemberId, seat: SeatId) -> bool {
        if self.is_host(member) {
            return true;
        }
        if !self.has_pad(seat) {
            return true;
        }
        // The pad seats it holds, and those on their way to it.
        self.pad_seats_but(member, seat).len() < usize::from(self.controllers_of(member))
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
            host_seats: self.host_seats(),
            host_on_seats: self.host().is_some(),
            control_paused: self.paused,
            stream: self.stream,
        })
    }

    // ---- What members do ----

    /// The host's controllers can't play through seats after all (one
    /// couldn't be hidden, say): the host plays outside the seats. Call
    /// before [`Self::start`].
    pub fn without_host(&mut self) {
        self.homes.clear();
    }

    /// The session is up: the input task hears which controller drives
    /// which seat, then each host controller takes its seat.
    pub fn start(&self) -> Vec<Effect> {
        if self.host().is_none() {
            return Vec::new();
        }
        let mut fx = vec![self.host_seats_effect()];
        fx.extend(self.reclaim());
        fx
    }

    /// Each host controller's seat back to it, if it is free (as far as the
    /// mirror knows: the router checks again) and the host didn't take it
    /// back to leave it empty. Never while control is paused: the panic key
    /// leaves the host the seats they play, and frees the rest.
    fn reclaim(&self) -> Vec<Effect> {
        let Some(host) = self.host() else {
            return Vec::new();
        };
        if self.ended || self.paused {
            return Vec::new();
        }
        self.homes
            .iter()
            .filter(|h| h.home != Home::LeftEmpty && self.holders[usize::from(h.seat.0)].is_none())
            .map(|h| Effect::Claim {
                seat: h.seat,
                member: host,
            })
            .collect()
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
        self.drivers.remove(&member);
        self.asked_with.remove(&member);
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
        self.request_with(member, seat, None, now)
    }

    /// A viewer asks for `seat` for one of its controllers (P13): granted,
    /// that controller plays it, leaving its own seat if the viewer has no
    /// controller to spare. Asking again for the same seat changes nothing,
    /// another controller included.
    pub fn request_with(
        &mut self,
        member: MemberId,
        seat: SeatId,
        controller: Option<u8>,
        now: Instant,
    ) -> Vec<Effect> {
        if self.ended
            || !self.is_viewer(member)
            || !self.has_seat(seat)
            || self.seats_of(member).contains(&seat)
        {
            return Vec::new();
        }
        // A request stays as the host first saw it: its controller can't
        // change under the host's Accept (P13 review). Withdraw to change it.
        if self.requests.contains(&(member, seat)) {
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
        match controller {
            Some(c) => self.asked_with.insert(member, c),
            None => self.asked_with.remove(&member),
        };
        vec![Effect::Requested(member, seat), Effect::Changed]
    }

    /// A viewer withdraws its request; the seat it holds, if any, stays.
    pub fn withdraw(&mut self, member: MemberId) -> Vec<Effect> {
        if !self.requests.iter().any(|(r, _)| *r == member) {
            return Vec::new();
        }
        self.requests.retain(|(r, _)| *r != member);
        self.asked_with.remove(&member);
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

    /// A viewer says which of its controllers drives each pad seat it holds
    /// (P13, by slot): which seat a controller moving to another leaves.
    /// Only seats it holds or has on their way count ([`Self::frees`]).
    pub fn set_drivers(&mut self, member: MemberId, drivers: &[(SeatId, u8)]) {
        if self.ended || !self.is_viewer(member) {
            return;
        }
        self.drivers.insert(member, drivers.to_vec());
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
        let freed = answered.map(|i| {
            let frees = self.pending[i].frees.clone();
            let mut at = 0;
            self.pending.retain(|p| {
                let keep = at > i || p.seat != seat;
                at += 1;
                keep
            });
            frees
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
                if host && self.host_controller_on(seat).is_none() {
                    // Only claims and moves grant the host a seat, each to a
                    // controller's own: a report of another is out of date.
                    return vec![Effect::Revoke {
                        seat,
                        holder: Some(member),
                    }];
                }
                // A request for this seat is answered; one for another seat
                // stays, and the viewer keeps asking too.
                if self.requests.contains(&(member, seat)) {
                    self.requests.retain(|r| *r != (member, seat));
                    self.asked_with.remove(&member);
                }
                // Never more pad seats than the rule allows: a grant that
                // frees a seat revokes it first, so this only guards against
                // a report out of order. A grant that added keeps what is
                // held, even if the viewer's count dropped meanwhile (a seat
                // already held stays; its pad goes neutral).
                let extra = freed.unwrap_or_else(|| self.frees(member, seat, None).0);
                let held = self.seats_of(member);
                let mut fx: Vec<Effect> = extra
                    .into_iter()
                    .filter(|s| *s != seat && self.has_pad(*s) && held.contains(s))
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
        let controller = self.asked_with.remove(&member);
        if accept {
            // The controller they asked with moves (P13); without one, a
            // guess (a request with one controller needs none).
            let mut fx = self.grant(member, seat, controller);
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
        self.pass_with(member, seat, None)
    }

    /// Hands `seat` to a viewer for its controller `controller` (P12, P13),
    /// which then plays it, leaving its own seat if the viewer has no
    /// controller to spare. Refused with [`SessionError::ChooseController`]
    /// when a seat must be freed and nothing says which.
    pub fn pass_with(
        &mut self,
        member: MemberId,
        seat: SeatId,
        controller: Option<u8>,
    ) -> Result<Vec<Effect>, SessionError> {
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
        if self.frees(member, seat, controller).1 {
            return Err(SessionError::ChooseController);
        }
        if self.requests.contains(&(member, seat)) {
            self.requests.retain(|r| *r != (member, seat));
            self.asked_with.remove(&member);
        }
        let mut fx = self.grant(member, seat, controller);
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
                for h in self.homes.iter_mut().filter(|h| h.seat == seat) {
                    h.home = Home::LeftEmpty;
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

    /// Every viewer's seat taken back: the host's they had passed on stay
    /// empty too.
    fn everything_taken_back(&mut self) {
        for h in self.homes.iter_mut().filter(|h| h.home == Home::Lent) {
            h.home = Home::LeftEmpty;
        }
    }

    /// The host plays `seat` with their controller `controller` (M23: one of
    /// several). Allowed while control is paused: it is the host's own.
    /// - On a seat another of their controllers has, the two swap seats:
    ///   the router's holders don't change (the host holds both either way),
    ///   only which controller drives which.
    /// - Otherwise the controller moves there: a viewer on it loses it, and
    ///   the controller's old seat goes free.
    pub fn play(&mut self, seat: SeatId, controller: u8) -> Result<Vec<Effect>, SessionError> {
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
        let k = usize::from(controller);
        let old = self
            .homes
            .get(k)
            .ok_or(SessionError::NoSuchController)?
            .seat;
        // Held by the host as far as the mirror knows, with no viewer's
        // grant of it still on its way (that grant would take it from them).
        let held = |s: SeatId| {
            self.holders[usize::from(s.0)] == Some(host)
                && !self.pending.iter().any(|p| p.seat == s && p.member != host)
        };
        if old == seat {
            // Its own seat: played again (it comes back once free), or
            // played already, as far as the mirror knows.
            self.homes[k].home = Home::Playing;
            if held(seat) {
                return Ok(Vec::new());
            }
            let mut fx = self.grant(host, seat, None);
            fx.push(Effect::Changed);
            return Ok(fx);
        }
        let mut fx = Vec::new();
        if let Some(j) = self.homes.iter().position(|h| h.seat == seat) {
            // Another controller's seat: they swap, each with what the host
            // last did with its new seat.
            self.homes.swap(j, k);
            self.homes[k].home = Home::Playing;
            fx.push(self.host_seats_effect());
            if !held(seat) {
                fx.extend(self.grant(host, seat, None));
            }
            // The other controller's new seat, if free, comes back to it.
            fx.extend(self.reclaim());
        } else {
            // From now on only the new seat comes back to this controller:
            // the old one, freed here, is no controller's (a late claim for
            // it is refused).
            self.homes[k] = HostHome {
                seat,
                home: Home::Playing,
            };
            fx.push(self.host_seats_effect());
            // Whatever the mirror says: a claim of it may have reached the
            // router before its report reached us.
            fx.push(Effect::Revoke {
                seat: old,
                holder: Some(host),
            });
            fx.extend(self.grant(host, seat, None));
        }
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
        self.asked_with.clear();
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

    /// Viewers may be given control again. Each host controller's seat, if
    /// it went free meanwhile without being left empty on purpose, comes
    /// back to it.
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
        self.asked_with.clear();
        self.pending.clear();
        vec![
            Effect::Broadcast(ControlMessage::SessionEnded(SessionEnded {
                session: self.id,
                reason,
            })),
            Effect::Changed,
        ]
    }

    /// Grants `seat`, after revoking `member` from the pad seats it must
    /// give up to stay within its controllers ([`Self::frees`], P13: the
    /// one `controller` drives first): the router takes them in order, so a
    /// viewer never holds more pad seats than it may, even with a grant
    /// still on its way. A keyboard-and-mouse seat stays: it needs no
    /// controller.
    fn grant(&mut self, member: MemberId, seat: SeatId, controller: Option<u8>) -> Vec<Effect> {
        // A host controller's seat passed on: it comes back to it when that
        // viewer lets go.
        if !self.is_host(member) {
            for h in self.homes.iter_mut().filter(|h| h.seat == seat) {
                h.home = Home::Lent;
            }
        }
        // Beyond its controllers: the seat of the one that moves goes first
        // (P13), on the same ordered channel, so the router never sees it
        // on more pad seats than its controllers.
        let (frees, _) = self.frees(member, seat, controller);
        for p in &mut self.pending {
            if p.member == member && frees.contains(&p.seat) {
                p.cancelled = true;
                p.frees.clear();
            }
        }
        self.pending.push(PendingGrant {
            seat,
            member,
            frees: frees.clone(),
            cancelled: false,
        });
        let mut fx: Vec<Effect> = frees
            .into_iter()
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
            controller: controller.filter(|_| !self.is_host(member)),
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
