//! Sessions in the room actor: the one we stream, the one we watch, and the
//! others' we hear of (docs/protocol.md, Sessions).
//!
//! **The host is the only writer** of its session's facts, sent straight to
//! each member and never relayed. So a session message counts only on the
//! current connection with its sender, a current member of the room, and
//! only about the sender's own session (or, to a host, about ours).
//!
//! Video, sound and input run in the pipelines (`crate::pipeline`); this
//! module decides who gets them. Who holds the seat is the seat router's
//! call; `couch_room::session` mirrors it.

use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet},
};

use couch_input::{HidUsage, shortcuts::Shortcut};
use couch_proto::{
    control::{KeyframeReason, KeyframeRequest},
    ids::SeatEpoch,
    ipc::{HudRole, HudSeat, HudState, ShortcutKeys, UserAction},
    session::{
        Cancel, ControlCancel, ControlRequest, EndReason, GameFocus, JoinResult, SeatKind,
        SessionAnnounce, SessionJoin, SessionLeave, SessionStatus, StreamState, ViewerControllers,
        seat_labels, seat_phrase,
    },
};
use couch_room::session::{Effect as SessionEffect, HostSession, SessionConfig};

use super::*;
use crate::{
    AnswerKeys, HostController, InputSetup, RequestView, ScreenArea, SeatView, SessionEnd,
    SessionRole, SessionView, SourceChoice, StreamStateView, StreamView, WatchEnd,
    pipeline::{
        AudioChoice, Source, ViewerOptions,
        host::{HostOptions, LocalSeat, Rates, StreamControl, StreamOptions},
        watch::{Watch, WatchOptions},
    },
};

/// How long a request must have been pending before the host's Accept or
/// Decline key answers it: long enough to have been seen on the overlay.
const ANSWER_AFTER: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct Sessions {
    hosting: Option<Hosting>,
    watching: Option<Watching>,
    /// Others' live sessions, per room, by host.
    directory: BTreeMap<RoomId, BTreeMap<MemberId, Listing>>,
}

struct Hosting {
    room: RoomId,
    session: HostSession,
    /// `None` until the session has started: plugging in the pads takes a
    /// while, and nobody hears of the session before.
    pipeline: Option<HostPipeline>,
    /// The stream running in the session, if any (M19), or `Starting`.
    stream: Option<OurStream>,
    /// Stream starts asked for so far, to tell their answers apart.
    stream_requests: u32,
    /// Stream ends the pipeline reported while a start was pending, by
    /// stream number: the starting stream may fail before its answer comes.
    early_ends: Vec<(u32, EndReason)>,
    /// The connection each viewer joined on. When it closes, the viewer
    /// has gone: its stream and input were bound to it.
    viewer_conns: BTreeMap<MemberId, ConnId>,
    /// What each viewer said it decodes when it joined (`couch_proto::codec`
    /// bits, M12c).
    viewer_codecs: BTreeMap<MemberId, u8>,
    /// The host keys' hook, in place while the stream runs, with the panic
    /// key it listens for ("Ctrl+Alt+Shift+Backspace"); `None` if it
    /// couldn't be installed.
    panic_key: Option<(Box<dyn Any + Send>, String)>,
    /// The Accept and Decline keys the hook listens for, as shown.
    answer_keys: Option<(String, String)>,
    /// The oldest pending request, the one the answer keys answer, and since
    /// when it has been the oldest (not since it arrived: a request moves to
    /// the front when the one before it goes).
    first: Option<((MemberId, SeatId), Instant)>,
    /// Where the overlay goes (the streamed window's or screen's monitor; the
    /// main screen's with nothing streamed).
    overlay_area: Option<ScreenArea>,
    /// Whether the streamed window is in front, as last reported; `None`
    /// before the first report of a stream, and always for a screen.
    in_front: Option<bool>,
    /// Our own controllers, if we chose to play through seats: their names,
    /// by controller (M23), or why they can't.
    controllers: Option<Result<Vec<String>, String>>,
    /// How loud guests hear the sound, 0 to 400% (P7): the profile's,
    /// applied to the pipeline once it has started.
    sound_level: u16,
    /// The seats whose pads Windows is still setting up (P10), and since
    /// when.
    pads_setting_up: Vec<SeatId>,
    pads_since: Option<Instant>,
    /// The seats left without a pad for the session.
    pads_missing: Vec<SeatId>,
}

struct Watching {
    room: RoomId,
    host: MemberId,
    conn: ConnId,
    session: SessionId,
    state: WatchState,
    watch: Option<Watch>,
    /// A line for the viewer's HUD after a change.
    note: String,
    /// The number of the watch's current connection
    /// (`WatchEvent::ConnectionLost`).
    link: u32,
    /// Counts joins and waits; a timeout counts only for the one it was set
    /// for.
    attempt: u32,
    /// Since when the host's connection has been gone, until watching
    /// resumes: one 30 s budget across every drop of a reconnection.
    away_since: Option<Instant>,
    /// Whether the host's game window is in front, as the host last said
    /// (`GameFocus`); assumed so until it says otherwise.
    in_front: bool,
    /// The seats we hold, each from its `SeatAssigned` to its `SeatReleased`
    /// (M18: as many as the host lets us). Never kept across connections.
    holding: BTreeSet<SeatId>,
    /// How many controllers the stream window has, as it last said; the
    /// host hears it on every change and after each join (M18).
    controllers: u8,
}

enum WatchState {
    /// Asked to watch; the reply waits for the host's answer.
    Joining(Option<Reply>),
    Watching,
    /// Asked for this seat; may hold others meanwhile (to add it, or move).
    Asking(SeatId),
    /// Holds one seat or more (`Watching::holding`).
    Controlling,
    /// The connection to the host went: the window stays open while the
    /// room redials, and watching resumes when the host announces the same
    /// session on the new connection.
    Reconnecting,
}

impl Watching {
    /// Not asking: watching, or controlling what we hold.
    fn settled(&self) -> WatchState {
        if self.holding.is_empty() {
            WatchState::Watching
        } else {
            WatchState::Controlling
        }
    }

    /// Joined: the host may hear from us.
    fn joined(&self) -> bool {
        matches!(
            self.state,
            WatchState::Watching | WatchState::Asking(_) | WatchState::Controlling
        )
    }
}

struct Listing {
    announce: SessionAnnounce,
    status: Option<SessionStatus>,
}

/// The stream in our session (M19).
enum OurStream {
    /// Asked for (the session's `request`th start); the pipeline hasn't
    /// answered yet.
    Starting { view: StreamView, request: u32 },
    Live {
        /// Its number in the pipeline: its events name it.
        number: u32,
        view: StreamView,
    },
}

/// [`Command::StartSession`]'s fields.
pub(super) struct StartSession {
    pub room: String,
    pub title: String,
    pub max_viewers: u8,
    pub input: InputSetup,
    pub controllers: Vec<HostController>,
}

/// [`Command::StartStream`]'s fields.
pub(super) struct StartStream {
    pub source: SourceChoice,
    pub name: String,
    pub fps: u32,
    pub max_mbps: u32,
    pub upload_mbps: Option<u32>,
}

/// The longest stream name the host's panel shows.
const MAX_STREAM_NAME: usize = 200;

fn end_reason(reason: EndReason) -> SessionEnd {
    match reason {
        EndReason::HostStopped => SessionEnd::HostStopped,
        EndReason::GameExited => SessionEnd::GameExited,
        EndReason::SourceLost => SessionEnd::SourceLost,
        EndReason::HostLeftRoom => SessionEnd::HostLeftRoom,
        EndReason::HostShutdown => SessionEnd::HostShutdown,
        EndReason::Error => SessionEnd::Error,
    }
}

fn mbps(v: u32) -> u32 {
    v.saturating_mul(1_000_000)
}

impl Actor {
    // ---- Connections ----

    /// The connection in use with `member` in `room`.
    pub(super) fn current_conn(&self, room: RoomId, member: MemberId) -> Option<ConnId> {
        self.rooms
            .get(&room)?
            .room
            .connections()
            .into_iter()
            .find(|(peer, _, _)| *peer == member)
            .map(|(_, conn, _)| conn)
    }

    fn send_to(&self, room: RoomId, member: MemberId, message: ControlMessage) {
        if let Some(conn) = self.current_conn(room, member) {
            self.send(conn, Outgoing::Message(message));
        }
    }

    fn broadcast(&self, room: RoomId, message: &ControlMessage) {
        let Some(entry) = self.rooms.get(&room) else {
            return;
        };
        for (_, conn, _) in entry.room.connections() {
            self.send(conn, Outgoing::Message(message.clone()));
        }
    }

    fn member_name(&self, room: RoomId, member: MemberId) -> String {
        self.rooms
            .get(&room)
            .and_then(|e| e.room.status().members.into_iter().find(|m| m.id == member))
            .map_or_else(|| "someone".into(), |m| m.name)
    }

    /// A connection with `peer` became current: if we stream in its room,
    /// it hears of it.
    pub(super) fn session_connected(
        &mut self,
        room: RoomId,
        peer: MemberId,
        conn: ConnId,
        now: Instant,
    ) {
        if self.current_conn(room, peer) != Some(conn) {
            return;
        }
        let Some(h) = self.sessions.hosting.as_ref() else {
            return;
        };
        if h.room != room || h.pipeline.is_none() {
            return;
        }
        let fx = h.session.connected(peer);
        self.apply_session(room, fx, now);
    }

    /// A connection closed (its room has already heard).
    pub(super) fn session_closed(
        &mut self,
        room: RoomId,
        conn: ConnId,
        peer: MemberId,
        now: Instant,
    ) {
        let viewer_gone = self
            .sessions
            .hosting
            .as_ref()
            .is_some_and(|h| h.room == room && h.viewer_conns.get(&peer) == Some(&conn));
        if viewer_gone {
            let fx = self.hosting_mut().map(|h| h.session.leave(peer));
            self.apply_session(room, fx.unwrap_or_default(), now);
        }
        if self
            .sessions
            .watching
            .as_ref()
            .is_some_and(|w| w.conn == conn)
        {
            self.lost_host(now);
        }
        // Without a connection to its host, a session can't be watched; it
        // is announced again on the next one. The one we wait to resume
        // stays listed.
        let resuming = self
            .sessions
            .watching
            .as_ref()
            .is_some_and(|w| w.room == room && w.host == peer);
        if !resuming
            && self.current_conn(room, peer).is_none()
            && let Some(listings) = self.sessions.directory.get_mut(&room)
            && listings.remove(&peer).is_some()
        {
            self.announce(room, now);
        }
    }

    /// The connection to the host we watch went: wait for it to come back,
    /// with the window open, unless we were only joining.
    fn lost_host(&mut self, now: Instant) {
        let Some(w) = self.sessions.watching.as_ref() else {
            return;
        };
        match w.state {
            WatchState::Reconnecting => return,
            // Asked to watch, not watching yet: the waiting command hears.
            WatchState::Joining(Some(_)) => {
                self.end_watch_refused(
                    WatchEnd::ConnectionLost,
                    "lost the connection to the host",
                    now,
                );
                return;
            }
            // A resumed join: wait again, within the same budget.
            WatchState::Joining(None)
            | WatchState::Watching
            | WatchState::Asking(_)
            | WatchState::Controlling => {}
        }
        let (room, host, session) = (w.room, w.host, w.session);
        let note = format!("Reconnecting to {}…", self.member_name(room, host));
        let wait = self.reconnect_wait;
        let w = self.sessions.watching.as_mut().expect("checked above");
        w.state = WatchState::Reconnecting;
        // The seats went with the connection.
        w.holding.clear();
        w.note = note;
        w.attempt += 1;
        let attempt = w.attempt;
        let since = *w.away_since.get_or_insert(now);
        let left = (since + wait).saturating_duration_since(now);
        tracing::info!(room = ?room, "lost the host's connection; waiting for it to come back");
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            tokio::time::sleep(left).await;
            let _ = inbox.send(Input::ReconnectTimeout(session, attempt)).await;
        });
        self.send_hud();
        self.announce(room, now);
    }

    pub(super) fn reconnect_timeout(&mut self, session: SessionId, attempt: u32, now: Instant) {
        let waiting = self.sessions.watching.as_ref().is_some_and(|w| {
            w.session == session
                && w.attempt == attempt
                && matches!(w.state, WatchState::Reconnecting)
        });
        if waiting {
            self.end_watch(WatchEnd::ConnectionLost, true, now);
        }
    }

    /// Drops listings of hosts we have no connection to (kept while we
    /// waited to resume one).
    fn forget_unreachable(&mut self, now: Instant) {
        let rooms: Vec<RoomId> = self.sessions.directory.keys().copied().collect();
        for room in rooms {
            let hosts: Vec<MemberId> = self.sessions.directory[&room].keys().copied().collect();
            let gone: Vec<MemberId> = hosts
                .into_iter()
                .filter(|h| self.current_conn(room, *h).is_none())
                .collect();
            if !gone.is_empty() {
                if let Some(listings) = self.sessions.directory.get_mut(&room) {
                    listings.retain(|h, _| !gone.contains(h));
                }
                self.announce(room, now);
            }
        }
    }

    /// The host we wait for announced its session on a new connection: the
    /// same one resumes (a new viewer, to the host); another one means ours
    /// ended.
    fn resume_watching(&mut self, room: RoomId, conn: ConnId, session: SessionId, now: Instant) {
        let Some(w) = self.sessions.watching.as_ref() else {
            return;
        };
        if w.session != session {
            self.end_watch(WatchEnd::Ended, true, now);
            return;
        }
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        let Some(watch) = w.watch.as_ref() else {
            return;
        };
        // Subscribed before asking, as when watching starts.
        let link = watch.reconnect(c.conn.clone(), &c.datagrams, &c.streams);
        let w = self.sessions.watching.as_mut().expect("checked above");
        w.conn = conn;
        w.link = link;
        w.state = WatchState::Joining(None);
        w.note = String::new();
        w.attempt += 1;
        let attempt = w.attempt;
        tracing::info!(room = ?room, "the host is back; joining its stream again");
        let codecs = self.backend.decodes();
        self.send(
            conn,
            Outgoing::Message(ControlMessage::SessionJoin(SessionJoin { session, codecs })),
        );
        self.join_timer(session, attempt);
        self.send_hud();
        self.announce(room, now);
    }

    // ---- Messages ----

    pub(super) fn session_received(
        &mut self,
        room: RoomId,
        conn: ConnId,
        message: ControlMessage,
        now: Instant,
    ) {
        let Some(peer) = self.conns.get(&conn).map(|c| c.peer) else {
            return;
        };
        let member = self
            .rooms
            .get(&room)
            .is_some_and(|e| !e.room.is_leaving() && e.room.is_member(peer));
        if !member || self.current_conn(room, peer) != Some(conn) {
            return;
        }
        let ours = |s: &Sessions, session: SessionId| {
            s.hosting.as_ref().is_some_and(|h| {
                h.room == room && h.session.id() == session && h.pipeline.is_some()
            })
        };
        match message {
            // To us, the host.
            ControlMessage::SessionJoin(m) => {
                if ours(&self.sessions, m.session) {
                    // Joining again on a new connection (it reconnected
                    // before we saw the old one close): the old stream and
                    // anything it held go first.
                    let stale = self.sessions.hosting.as_ref().is_some_and(|h| {
                        h.session.is_viewer(peer) && h.viewer_conns.get(&peer) != Some(&conn)
                    });
                    if stale {
                        let fx = self.hosting_mut().map(|h| h.session.leave(peer));
                        self.apply_session(room, fx.unwrap_or_default(), now);
                    }
                    // What its PC decodes: the host picks its stream's
                    // codec from it (M12c).
                    if let Some(h) = self.hosting_mut() {
                        h.viewer_codecs.insert(peer, m.codecs);
                    }
                    let fx = self.hosting_mut().map(|h| h.session.join(peer));
                    self.apply_session(room, fx.unwrap_or_default(), now);
                } else {
                    // Not (or no longer) ours: say so rather than leave it
                    // waiting.
                    self.send_to(
                        room,
                        peer,
                        ControlMessage::SessionJoinReply(couch_proto::session::SessionJoinReply {
                            session: m.session,
                            result: JoinResult::Ended,
                        }),
                    );
                }
            }
            ControlMessage::SessionLeave(m) if ours(&self.sessions, m.session) => {
                let fx = self.hosting_mut().map(|h| h.session.leave(peer));
                self.apply_session(room, fx.unwrap_or_default(), now);
            }
            ControlMessage::ControlRequest(m) if ours(&self.sessions, m.session) => {
                let fx = self
                    .hosting_mut()
                    .map(|h| h.session.request(peer, m.seat, now));
                self.apply_session(room, fx.unwrap_or_default(), now);
            }
            ControlMessage::ControlCancel(m) if ours(&self.sessions, m.session) => {
                let fx = self.hosting_mut().map(|h| match m.what {
                    Cancel::Request => h.session.withdraw(peer),
                    Cancel::Seat(seat) => h.session.give_back(peer, seat),
                });
                self.apply_session(room, fx.unwrap_or_default(), now);
            }
            // How many controllers a viewer has (M18): only from the viewer
            // itself, on the connection it joined on. It decides only whether
            // a seat we grant it is added or replaces its others.
            ControlMessage::ViewerControllers(m) if ours(&self.sessions, m.session) => {
                let fx = self.hosting_mut().and_then(|h| {
                    (h.session.is_viewer(peer) && h.viewer_conns.get(&peer) == Some(&conn))
                        .then(|| h.session.set_controllers(peer, m.count))
                });
                self.apply_session(room, fx.unwrap_or_default(), now);
            }
            ControlMessage::KeyframeRequest(_) => {
                if let Some(h) = self.sessions.hosting.as_ref()
                    && h.room == room
                    && h.session.is_viewer(peer)
                    && let Some(pipeline) = &h.pipeline
                {
                    pipeline.keyframe(peer);
                }
            }
            // From a host, about its own session.
            ControlMessage::SessionAnnounce(a) => {
                if !a.is_valid() {
                    return;
                }
                let session = a.session;
                let listing = Listing {
                    announce: a,
                    status: None,
                };
                self.sessions
                    .directory
                    .entry(room)
                    .or_default()
                    .insert(peer, listing);
                let resuming = self.sessions.watching.as_ref().is_some_and(|w| {
                    w.room == room && w.host == peer && matches!(w.state, WatchState::Reconnecting)
                });
                if resuming {
                    self.resume_watching(room, conn, session, now);
                }
                self.announce(room, now);
            }
            ControlMessage::SessionStatus(s) => {
                let Some(listing) = self
                    .sessions
                    .directory
                    .get_mut(&room)
                    .and_then(|l| l.get_mut(&peer))
                else {
                    return;
                };
                if !s.is_valid()
                    || listing.announce.session != s.session
                    || s.holders.len() != listing.announce.seats.len()
                {
                    return;
                }
                let was_live = listing
                    .status
                    .as_ref()
                    .is_some_and(|old| old.stream == StreamState::Live);
                let live = s.stream == StreamState::Live;
                listing.status = Some(s);
                // A stream ended: the next one's game is taken to be in front
                // until the host says otherwise, as at joining (M19). Reset
                // at the end, not when the next one starts: the host's first
                // `GameFocus` for it may come before its status.
                if was_live
                    && !live
                    && let Some(w) = self
                        .sessions
                        .watching
                        .as_mut()
                        .filter(|w| w.room == room && w.host == peer)
                {
                    w.in_front = true;
                }
                self.send_hud();
                self.announce(room, now);
            }
            ControlMessage::GameFocus(f) => {
                if !self.watching(room, peer, f.session) {
                    return;
                }
                if let Some(w) = self.sessions.watching.as_mut()
                    && w.in_front != f.in_front
                {
                    w.in_front = f.in_front;
                    self.send_hud();
                }
            }
            ControlMessage::SessionEnded(e) => {
                let known = self
                    .sessions
                    .directory
                    .get(&room)
                    .and_then(|l| l.get(&peer))
                    .is_some_and(|l| l.announce.session == e.session);
                if !known {
                    return;
                }
                if self.watching(room, peer, e.session) {
                    self.end_watch(WatchEnd::Ended, true, now);
                }
                if let Some(listings) = self.sessions.directory.get_mut(&room) {
                    listings.remove(&peer);
                }
                self.announce(room, now);
            }
            ControlMessage::SessionJoinReply(r) => {
                if !self.watching(room, peer, r.session) {
                    return;
                }
                match r.result {
                    JoinResult::Joined => {
                        let Some(w) = self.sessions.watching.as_mut() else {
                            return;
                        };
                        let reply = match &mut w.state {
                            WatchState::Joining(reply) => reply.take(),
                            _ => return,
                        };
                        w.state = WatchState::Watching;
                        w.away_since = None;
                        tracing::info!(room = ?room, host = ?peer, "watching a stream");
                        // A new viewer to the host each time: it hears how
                        // many controllers we have (M18).
                        let count = (w.controllers > 0).then_some(
                            ControlMessage::ViewerControllers(ViewerControllers {
                                session: w.session,
                                count: w.controllers,
                            }),
                        );
                        if let Some(count) = count {
                            self.send(conn, Outgoing::Message(count));
                        }
                        self.send_hud();
                        self.announce(room, now);
                        if let Some(reply) = reply {
                            let _ = reply.send(Ok(Response::Room(
                                self.view(room, now).expect("watching in it"),
                            )));
                        }
                    }
                    JoinResult::Full => self.end_watch_refused(
                        WatchEnd::Unavailable,
                        "that stream has all the viewers it takes",
                        now,
                    ),
                    JoinResult::Ended => {
                        self.end_watch_refused(WatchEnd::Ended, "that stream has ended", now);
                    }
                    JoinResult::Unavailable => self.end_watch_refused(
                        WatchEnd::Unavailable,
                        "the host can't stream to you right now",
                        now,
                    ),
                }
            }
            ControlMessage::ControlDeclined(d) => {
                if !self.watching(room, peer, d.session) {
                    return;
                }
                if let Some(w) = self.sessions.watching.as_mut()
                    && matches!(w.state, WatchState::Asking(_))
                {
                    // Back to the seats we hold, if any.
                    w.state = w.settled();
                    w.note = "The host said no, for now".into();
                    self.send_hud();
                    self.announce(room, now);
                }
            }
            ControlMessage::SeatAssigned(s) => {
                let labels = self.seat_labels();
                let Some(w) = self.sessions.watching.as_mut() else {
                    return;
                };
                if w.room != room
                    || w.host != peer
                    || matches!(w.state, WatchState::Joining(_) | WatchState::Reconnecting)
                {
                    return;
                }
                // A request for another seat stays pending, as the host keeps
                // it (`HostSession::seat_changed`): both sides agree however
                // a pass and a request cross.
                w.state = match w.state {
                    WatchState::Asking(asked) if asked != s.seat => WatchState::Asking(asked),
                    _ => WatchState::Controlling,
                };
                // Added to what we hold, or a move: the host releases the
                // others first (M18).
                w.holding.insert(s.seat);
                let label = labels
                    .get(usize::from(s.seat.0))
                    .map_or("a seat", String::as_str);
                w.note = format!("You have {}", seat_phrase(label));
                if let Some(watch) = &w.watch {
                    watch.seat_assigned(s);
                }
                self.send_hud();
                self.announce(room, now);
            }
            ControlMessage::SeatReleased(s) => {
                let labels = self.seat_labels();
                let Some(w) = self.sessions.watching.as_mut() else {
                    return;
                };
                if w.room != room || w.host != peer {
                    return;
                }
                let seat = s.seat;
                if let Some(watch) = &w.watch {
                    watch.seat_released(s);
                }
                if !w.holding.remove(&seat) {
                    return;
                }
                // While asking to move, the old seat goes first: still asking.
                if matches!(w.state, WatchState::Controlling) {
                    w.state = w.settled();
                    // Taken back, passed on or given back: the host may or
                    // may not play it now.
                    let label = labels
                        .get(usize::from(seat.0))
                        .map_or("a seat", String::as_str);
                    w.note = format!("You no longer have {}", seat_phrase(label));
                }
                self.send_hud();
                self.announce(room, now);
            }
            _ => {}
        }
    }

    fn hosting_mut(&mut self) -> Option<&mut Hosting> {
        self.sessions.hosting.as_mut()
    }

    /// The watched session's seats, as announced.
    fn watched_seats(&self) -> &[SeatKind] {
        self.sessions
            .watching
            .as_ref()
            .and_then(|w| self.sessions.directory.get(&w.room)?.get(&w.host))
            .map_or(&[], |l| &l.announce.seats)
    }

    /// What the watched session's seats are called.
    fn seat_labels(&self) -> Vec<String> {
        seat_labels(self.watched_seats())
    }

    /// Whether we watch `host`'s session `session` in `room`.
    fn watching(&self, room: RoomId, host: MemberId, session: SessionId) -> bool {
        self.sessions
            .watching
            .as_ref()
            .is_some_and(|w| w.room == room && w.host == host && w.session == session)
    }

    /// Carries out our session's effects.
    fn apply_session(&mut self, room: RoomId, effects: Vec<SessionEffect>, now: Instant) {
        let mut changed = false;
        let mut more = Vec::new();
        for effect in effects {
            match effect {
                SessionEffect::Send { to, message } => self.send_to(room, to, message),
                SessionEffect::Broadcast(message) => self.broadcast(room, &message),
                SessionEffect::StartViewer(member) => {
                    let conn = self.current_conn(room, member);
                    let started = conn.and_then(|id| {
                        let c = self.conns.get(&id)?;
                        let h = self.sessions.hosting.as_ref()?;
                        let codecs = h.viewer_codecs.get(&member).copied().unwrap_or(0);
                        h.pipeline.as_ref()?.add_viewer(
                            member,
                            codecs,
                            c.conn.clone(),
                            &c.datagrams,
                            &c.streams,
                        );
                        Some(id)
                    });
                    let mut focus = None;
                    match (started, self.sessions.hosting.as_mut()) {
                        (Some(id), Some(h)) => {
                            h.viewer_conns.insert(member, id);
                            // A new viewer hears whether the game is in front.
                            focus = h.in_front.map(|in_front| {
                                let message = ControlMessage::GameFocus(GameFocus {
                                    session: h.session.id(),
                                    in_front,
                                });
                                (id, message)
                            });
                        }
                        (None, Some(h)) => more.extend(h.session.viewer_failed(member)),
                        _ => {}
                    }
                    if let Some((id, message)) = focus {
                        self.send(id, Outgoing::Message(message));
                    }
                }
                SessionEffect::StopViewer(member) => {
                    if let Some(h) = self.sessions.hosting.as_mut() {
                        h.viewer_conns.remove(&member);
                        h.viewer_codecs.remove(&member);
                        if let Some(pipeline) = &h.pipeline {
                            pipeline.remove_viewer(member);
                        }
                    }
                }
                SessionEffect::Grant {
                    seat,
                    member,
                    permissions,
                } => {
                    if let Some(pipeline) = self.pipeline() {
                        pipeline.grant(seat, member, permissions);
                    }
                }
                SessionEffect::Revoke { seat, holder } => {
                    if let Some(pipeline) = self.pipeline() {
                        pipeline.revoke(seat, holder);
                    }
                }
                SessionEffect::RevokeViewers => {
                    if let Some(pipeline) = self.pipeline() {
                        pipeline.revoke_viewers();
                    }
                }
                SessionEffect::Claim { seat, member } => {
                    if let Some(pipeline) = self.pipeline() {
                        pipeline.claim(seat, member);
                    }
                }
                SessionEffect::HostSeats(seats) => {
                    if let Some(pipeline) = self.pipeline() {
                        pipeline.host_seats(seats);
                    }
                }
                SessionEffect::Requested(member, seat) => {
                    tracing::info!(member = ?member, ?seat, "a viewer asks for a seat");
                    let label = self
                        .sessions
                        .hosting
                        .as_ref()
                        .and_then(|h| {
                            seat_labels(&h.session.config().seats)
                                .into_iter()
                                .nth(usize::from(seat.0))
                        })
                        .unwrap_or_default();
                    let _ = self.events.send(Event::ControlRequested {
                        room: room.to_hex(),
                        member: member.to_hex(),
                        seat: label,
                    });
                }
                SessionEffect::Changed => changed = true,
            }
        }
        if !more.is_empty() {
            self.apply_session(room, more, now);
        }
        // Every change to the requests goes through here: note when the
        // oldest became the oldest.
        if let Some(h) = self.sessions.hosting.as_mut() {
            let head = h.session.requests().first().copied();
            if head != h.first.map(|(request, _)| request) {
                h.first = head.map(|request| (request, now));
            }
        }
        if changed {
            self.announce(room, now);
        }
    }

    fn pipeline(&self) -> Option<&HostPipeline> {
        self.sessions.hosting.as_ref()?.pipeline.as_ref()
    }

    // ---- Our session ----

    /// Starts our session, with nothing streamed yet (M19). Answers through
    /// `reply` once the pads are plugged in (or still being set up by Windows,
    /// waited for in the background: P10) and our controllers hidden; the
    /// error comes back too, for the caller's log.
    pub(super) fn start_session(
        &mut self,
        start: StartSession,
        reply: Reply,
        now: Instant,
    ) -> Result<(), EngineError> {
        let result = self.prepare_session(start, now);
        match result {
            Ok((session, options)) => {
                let backend = self.backend.clone();
                let inbox = self.inbox.clone();
                tokio::spawn(async move {
                    let (pipeline, events) = HostPipeline::start(backend, options).await;
                    let ready = Input::SessionReady {
                        session,
                        pipeline: Box::new(pipeline),
                        events,
                        reply,
                    };
                    let _ = inbox.send(ready).await;
                });
                Ok(())
            }
            Err(e) => {
                let text = e.to_string();
                let _ = reply.send(Err(e));
                Err(EngineError::Media(text))
            }
        }
    }

    fn prepare_session(
        &mut self,
        start: StartSession,
        _now: Instant,
    ) -> Result<(SessionId, HostOptions), EngineError> {
        let (room, _) = self.entry_mut(&start.room)?;
        if self.sessions.hosting.is_some() {
            return Err(EngineError::Refused("you have a session already"));
        }
        if self.sessions.watching.is_some() {
            return Err(EngineError::Refused("stop watching first"));
        }
        let session = SessionId(u64::from_le_bytes(random()?));
        let seats = start.input.seats();
        // Our controllers play through seats only on seats with a pad, one
        // each: any beyond those stay out.
        let mut controllers = start.controllers;
        controllers.truncate(seats.iter().filter(|s| s.has_pad()).count());
        let config = SessionConfig {
            title: start.title.trim().to_string(),
            seats: seats.clone(),
            max_viewers: start.max_viewers,
            host: (!controllers.is_empty()).then_some(self.me),
            host_controllers: controllers.len() as u8,
        };
        let host_session = HostSession::new(session, config)?;
        let options = HostOptions {
            seats,
            // Games and Steam know a controller by its ids; a made-up pair
            // leaves only the D-pad working.
            real_pad_ids: true,
            session: session.0,
            first_epoch: SeatEpoch(u32::from_le_bytes(random()?)),
            rc_trace: None,
            netem_elapsed: None,
            replace_grace: self.replace_grace,
            pad_setup_limit: couch_input::PAD_SETUP_LIMIT,
            local: (!controllers.is_empty()).then(|| LocalSeat {
                member: self.me,
                controllers: controllers.iter().map(HostController::choice).collect(),
                hide: true,
            }),
        };
        self.sessions.hosting = Some(Hosting {
            room,
            session: host_session,
            pipeline: None,
            stream: None,
            stream_requests: 0,
            early_ends: Vec::new(),
            viewer_conns: BTreeMap::new(),
            viewer_codecs: BTreeMap::new(),
            panic_key: None,
            answer_keys: None,
            first: None,
            // The main screen's, until something is streamed.
            overlay_area: self.backend.overlay_area(&Source::PrimaryScreen),
            in_front: None,
            controllers: (!controllers.is_empty())
                .then(|| Ok(controllers.iter().map(HostController::shown_name).collect())),
            sound_level: crate::store::host_level(&self.data_dir),
            pads_setting_up: Vec::new(),
            pads_since: None,
            pads_missing: Vec::new(),
        });
        Ok((session, options))
    }

    pub(super) fn session_ready(
        &mut self,
        session: SessionId,
        stream: HostPipeline,
        mut events: mpsc::UnboundedReceiver<HostEvent>,
        reply: Reply,
        now: Instant,
    ) {
        let Some(h) = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| h.session.id() == session && !h.session.is_ended())
        else {
            // Ended while it was starting.
            tokio::spawn(stream.stop());
            let _ = reply.send(Err(EngineError::Refused("ended before it started")));
            return;
        };
        let room = h.room;
        *self.panic.lock().unwrap_or_else(PoisonError::into_inner) = Some(stream.panic_handle());
        // The panic key releases straight through the input task, from the
        // hook's own thread, before the actor hears of it. Accept and
        // Decline go to the actor.
        let on_press = {
            let handle = stream.panic_handle();
            let inbox = self.inbox.clone();
            let runtime = tokio::runtime::Handle::current();
            move |key: Shortcut| {
                let input = match key {
                    Shortcut::Panic => {
                        handle.panic();
                        Input::Panic(Some(session))
                    }
                    Shortcut::AcceptRequest | Shortcut::DeclineRequest => Input::AnswerFirst {
                        session,
                        accept: key == Shortcut::AcceptRequest,
                    },
                    _ => return,
                };
                let inbox = inbox.clone();
                runtime.spawn(async move {
                    let _ = inbox.send(input).await;
                });
            }
        };
        // The user's keys, read now: a change made while live applies at
        // the next Go live.
        let shortcuts = crate::store::shortcuts(&self.data_dir);
        let keys: Vec<_> = Shortcut::HOST
            .iter()
            .map(|&s| (shortcuts.binding(s).key, s))
            .collect();
        let on_press = std::sync::Arc::new(on_press);
        let hook = |keys: &[(HidUsage, Shortcut)]| {
            let on_press = on_press.clone();
            self.backend
                .host_keys(keys, Box::new(move |key| on_press(key)))
        };
        let panic = shortcuts.binding(Shortcut::Panic);
        h.panic_key = match hook(&keys) {
            Ok(guard) => {
                h.answer_keys = Some((
                    shortcuts.binding(Shortcut::AcceptRequest).keys(),
                    shortcuts.binding(Shortcut::DeclineRequest).keys(),
                ));
                Some((guard, panic.keys()))
            }
            // Never the panic key lost for an answer key's sake: it alone.
            Err(e) => match hook(&[(panic.key, Shortcut::Panic)]) {
                Ok(guard) => {
                    tracing::error!("no keys to answer requests while streaming: {e}");
                    Some((guard, panic.keys()))
                }
                Err(e) => {
                    tracing::error!("no panic key while streaming: {e}");
                    None
                }
            },
        };
        // Our controllers play through seats, or couldn't after all.
        if let Some(Err(e)) = stream.local_seat() {
            tracing::warn!("our controllers can't play through seats: {e}");
            h.session.without_host();
            h.controllers = Some(Err(e.clone()));
        }
        stream.set_sound_level(h.sound_level);
        let start = h.session.start();
        h.pipeline = Some(stream);
        self.apply_session(room, start, now);
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if inbox.send(Input::Host(session, event)).await.is_err() {
                    return;
                }
            }
        });
        tracing::info!(room = ?room, "session started");
        // Everyone connected hears of it.
        let peers: Vec<MemberId> = self
            .rooms
            .get(&room)
            .map(|e| {
                e.room
                    .connections()
                    .into_iter()
                    .map(|(p, _, _)| p)
                    .collect()
            })
            .unwrap_or_default();
        for peer in peers {
            let fx = self
                .sessions
                .hosting
                .as_ref()
                .map(|h| h.session.connected(peer))
                .unwrap_or_default();
            self.apply_session(room, fx, now);
        }
        self.announce(room, now);
        let _ = reply.send(
            self.view(room, now)
                .map(Response::Room)
                .ok_or(EngineError::UnknownRoom),
        );
    }

    // ---- Streams in our session (M19) ----

    /// Starts streaming in our session, stopping a stream already running
    /// first. Answers through `reply` once it has started.
    pub(super) fn start_stream(
        &mut self,
        start: StartStream,
        reply: Reply,
        now: Instant,
    ) -> Result<(), EngineError> {
        match self.prepare_stream(start, now) {
            Ok((session, request, control, options)) => {
                // Queued now, in order with any stop asked for later.
                let started = control.start_stream(options);
                let inbox = self.inbox.clone();
                tokio::spawn(async move {
                    let stream = started.await;
                    let ready = Input::StreamReady {
                        session,
                        request,
                        stream,
                        reply,
                    };
                    let _ = inbox.send(ready).await;
                });
                Ok(())
            }
            Err(e) => {
                let text = e.to_string();
                let _ = reply.send(Err(e));
                Err(EngineError::Media(text))
            }
        }
    }

    fn prepare_stream(
        &mut self,
        start: StartStream,
        now: Instant,
    ) -> Result<(SessionId, u32, StreamControl, StreamOptions), EngineError> {
        let gone = EngineError::Refused("that window or screen is gone");
        let source = match &start.source {
            SourceChoice::Window { id } => {
                let id = id.parse().map_err(|_| gone)?;
                if !self.backend.window_exists(id) {
                    return Err(EngineError::Refused("that window is gone"));
                }
                Source::Window(id)
            }
            SourceChoice::Screen { id } => Source::Screen(id.parse().map_err(|_| gone)?),
        };
        let overlay_area = self.backend.overlay_area(&source);
        let h = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| h.pipeline.is_some())
            .ok_or(EngineError::NotLive)?;
        if matches!(h.stream, Some(OurStream::Starting { .. })) {
            return Err(EngineError::Refused("a stream is starting already"));
        }
        let seats = &h.session.config().seats;
        if seats.iter().any(|s| s.has_keyboard_mouse()) && !source.is_window() {
            return Err(EngineError::Refused(
                "keyboard and mouse need a window as the source: stream a window, or start \
                 a session without them",
            ));
        }
        let control = h.pipeline.as_ref().ok_or(EngineError::NotLive)?.control();
        let defaults = Rates::default();
        let max_bps = mbps(start.max_mbps.max(2));
        let options = StreamOptions {
            source,
            fps: start.fps.clamp(1, 240),
            rates: Rates {
                max_bps,
                start_bps: defaults.start_bps.min(max_bps),
                ..defaults
            },
            upload_bps: start.upload_mbps.map(mbps),
            audio: AudioChoice::Auto,
            pacing_trace: None,
        };
        let mut name = start.name.trim().to_string();
        if name.len() > MAX_STREAM_NAME {
            let mut end = MAX_STREAM_NAME;
            while !name.is_char_boundary(end) {
                end -= 1;
            }
            name.truncate(end);
        }
        let view = StreamView {
            name,
            fps: options.fps,
            max_mbps: start.max_mbps.max(2),
            upload_mbps: start.upload_mbps,
        };
        let session = h.session.id();
        let room = h.room;
        h.stream_requests = h.stream_requests.wrapping_add(1);
        let request = h.stream_requests;
        h.early_ends.clear();
        h.stream = Some(OurStream::Starting { view, request });
        h.overlay_area = overlay_area;
        h.in_front = None;
        // Viewers are told it stopped, for the moment of the switch.
        let fx = h.session.set_stream(match h.session.stream() {
            StreamState::Live => StreamState::Stopped(EndReason::HostStopped),
            other => other,
        });
        self.apply_session(room, fx, now);
        Ok((session, request, control, options))
    }

    /// The pipeline started stream `number` for start `request`: if that is
    /// still the start we wait for, everyone hears, and viewers' input goes
    /// in again (the pipeline has let it through). Otherwise it was stopped
    /// or replaced meanwhile, and the stop or the next start, queued after
    /// it in the pipeline, has ended it already.
    pub(super) fn stream_ready(
        &mut self,
        session: SessionId,
        request: u32,
        number: u32,
        reply: Reply,
        now: Instant,
    ) {
        let Some(h) = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| h.session.id() == session && !h.session.is_ended())
        else {
            let _ = reply.send(Err(EngineError::Refused("the session ended meanwhile")));
            return;
        };
        let room = h.room;
        match h.stream.take() {
            Some(OurStream::Starting { view, request: r }) if r == request => {
                h.stream = Some(OurStream::Live { number, view });
            }
            other => {
                h.stream = other;
                let _ = reply.send(Err(EngineError::Refused("stopped before it started")));
                return;
            }
        }
        // It may have failed already, before this answer came.
        let ended = h
            .early_ends
            .iter()
            .find(|(n, _)| *n == number)
            .map(|(_, reason)| *reason);
        h.early_ends.clear();
        let fx = h.session.set_stream(StreamState::Live);
        self.apply_session(room, fx, now);
        tracing::info!(room = ?room, number, "streaming");
        self.announce(room, now);
        let _ = reply.send(
            self.view(room, now)
                .map(Response::Room)
                .ok_or(EngineError::UnknownRoom),
        );
        if let Some(reason) = ended {
            self.end_stream_on_its_own(number, reason, now);
        }
    }

    /// How loud guests hear our sound (P7): remembered for the profile, and
    /// applied at once if we host. Any time, so the setting is ready before a
    /// session too.
    pub(super) fn set_stream_level(
        &mut self,
        percent: u16,
        now: Instant,
    ) -> Result<Response, EngineError> {
        let percent = percent.min(couch_media::audio::MAX_BOOST_PERCENT);
        crate::store::save_host_level(&self.data_dir, percent)
            .map_err(|e| EngineError::Media(e.to_string()))?;
        let Some(h) = self.sessions.hosting.as_mut() else {
            return Ok(Response::Done);
        };
        h.sound_level = percent;
        if let Some(pipeline) = &h.pipeline {
            pipeline.set_sound_level(percent);
        }
        let room = h.room;
        self.announce(room, now);
        Ok(Response::Room(
            self.view(room, now).ok_or(EngineError::Stopped)?,
        ))
    }

    /// Stops our stream; the session goes on (viewers stay, and keep their
    /// seats, with their input held off).
    pub(super) fn stop_stream(&mut self, now: Instant) -> Result<Response, EngineError> {
        self.end_stream(EndReason::HostStopped, None, now)?;
        Ok(Response::Done)
    }

    /// Ends our stream (all of it, or only stream `number`), its pipeline
    /// told to stop; everyone hears why.
    fn end_stream(
        &mut self,
        reason: EndReason,
        number: Option<u32>,
        now: Instant,
    ) -> Result<(), EngineError> {
        let h = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| h.pipeline.is_some())
            .ok_or(EngineError::NotLive)?;
        match (&h.stream, number) {
            (None, _) => return Err(EngineError::Refused("nothing is being streamed")),
            // A late report from a stream already replaced.
            (Some(OurStream::Live { number: n, .. }), Some(asked)) if *n != asked => {
                return Ok(());
            }
            (Some(OurStream::Starting { .. }), Some(_)) => return Ok(()),
            _ => {}
        }
        h.stream = None;
        h.in_front = None;
        h.overlay_area = self.backend.overlay_area(&Source::PrimaryScreen);
        if let Some(pipeline) = &h.pipeline {
            // Queued now, after any start asked for before.
            tokio::spawn(pipeline.control().stop_stream());
        }
        let room = h.room;
        let fx = h.session.set_stream(StreamState::Stopped(reason));
        self.apply_session(room, fx, now);
        tracing::info!(room = ?room, ?reason, "stream stopped");
        self.announce(room, now);
        Ok(())
    }

    /// Our stream ended without us asking: the session goes on, and the UI
    /// is told why.
    fn end_stream_on_its_own(&mut self, number: u32, reason: EndReason, now: Instant) {
        // A start is pending: this may be the starting stream failing before
        // its answer came (or an older one's late report). Kept for the
        // answer, which tells which.
        if let Some(h) = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| matches!(h.stream, Some(OurStream::Starting { .. })))
        {
            if h.early_ends.len() < 16 {
                h.early_ends.push((number, reason));
            }
            return;
        }
        let room = self.sessions.hosting.as_ref().map(|h| h.room);
        let ours = self.sessions.hosting.as_ref().is_some_and(
            |h| matches!(h.stream, Some(OurStream::Live { number: n, .. }) if n == number),
        );
        if !ours {
            return;
        }
        if self.end_stream(reason, Some(number), now).is_ok()
            && let Some(room) = room
        {
            let _ = self.events.send(Event::StreamEnded {
                room: room.to_hex(),
                reason: end_reason(reason),
            });
        }
    }

    pub(super) fn host_event(&mut self, session: SessionId, event: HostEvent, now: Instant) {
        let Some(h) = self
            .sessions
            .hosting
            .as_ref()
            .filter(|h| h.session.id() == session)
        else {
            return;
        };
        let room = h.room;
        match event {
            // Only the meter shows it (P7).
            HostEvent::SoundLevel(level) => {
                let db = |v: f32| v.round().clamp(couch_media::audio::SILENCE_DB, 0.0) as i8;
                let _ = self.events.send(Event::StreamLevel {
                    room: room.to_hex(),
                    peak_db: db(level.peak_db),
                    average_db: db(level.rms_db),
                });
            }
            HostEvent::Granted {
                member,
                seat,
                epoch,
                permissions,
            } => {
                if h.session.is_viewer(member) {
                    let assigned = couch_proto::control::SeatAssigned {
                        seat,
                        seat_epoch: epoch,
                        permissions,
                    };
                    self.send_to(room, member, ControlMessage::SeatAssigned(assigned));
                }
                let fx = self
                    .hosting_mut()
                    .map(|h| h.session.seat_changed(seat, Some(member)));
                self.apply_session(room, fx.unwrap_or_default(), now);
            }
            HostEvent::Revoked {
                member,
                seat,
                epoch,
            } => {
                // Our own controller's seat needs no telling.
                if member != self.me {
                    let released = couch_proto::control::SeatReleased {
                        seat,
                        seat_epoch: epoch,
                    };
                    self.send_to(room, member, ControlMessage::SeatReleased(released));
                }
                let fx = self
                    .hosting_mut()
                    .map(|h| h.session.seat_changed(seat, None));
                self.apply_session(room, fx.unwrap_or_default(), now);
            }
            HostEvent::GameFocus(in_front) => {
                tracing::debug!(in_front, "the game window's focus");
                let session = h.session.id();
                let viewers: Vec<MemberId> = h.session.viewers().to_vec();
                if let Some(h) = self.hosting_mut() {
                    h.in_front = Some(in_front);
                }
                let focus = ControlMessage::GameFocus(GameFocus { session, in_front });
                for viewer in viewers {
                    self.send_to(room, viewer, focus.clone());
                }
            }
            HostEvent::ViewerFailed { member, error } => {
                tracing::warn!(member = ?member, "no stream for a viewer: {error}");
                let fx = self.hosting_mut().map(|h| h.session.viewer_failed(member));
                self.apply_session(room, fx.unwrap_or_default(), now);
            }
            // The stream ends, not the session (M19).
            HostEvent::SourceClosed { stream } => {
                self.end_stream_on_its_own(stream, EndReason::SourceLost, now);
            }
            HostEvent::GameExited { stream } => {
                self.end_stream_on_its_own(stream, EndReason::GameExited, now);
            }
            HostEvent::SourceFollowed { .. } => {
                tracing::info!("the game opened a new window; streaming that one");
            }
            HostEvent::CaptureFailed { stream, error } => {
                tracing::error!("capture failed: {error}");
                self.end_stream_on_its_own(stream, EndReason::Error, now);
            }
            HostEvent::PadsSettingUp { seats, since } => {
                if let Some(h) = self.hosting_mut() {
                    h.pads_setting_up = seats;
                    h.pads_since = Some(since);
                }
                self.announce(room, now);
            }
            HostEvent::PadReady { seat } => {
                if let Some(h) = self.hosting_mut() {
                    h.pads_setting_up.retain(|s| *s != seat);
                }
                self.announce(room, now);
            }
            HostEvent::PadsMissing { seats } => {
                if let Some(h) = self.hosting_mut() {
                    h.pads_setting_up.retain(|s| !seats.contains(s));
                    h.pads_missing.extend(seats);
                }
                self.announce(room, now);
            }
            HostEvent::Note(text) => tracing::info!("{text}"),
            HostEvent::Seat { seat, status, .. } => tracing::debug!(?seat, ?status, "seat"),
            HostEvent::ViewerGone { member, .. } => {
                tracing::debug!(member = ?member, "a viewer's input was released");
            }
            HostEvent::Released { .. } | HostEvent::Stats(_) | HostEvent::Rumble(_) => {}
        }
    }

    /// Ends our session: input is released first, then the room hears why.
    /// Returns the pipeline for the caller to stop (or wait for), which
    /// stops the stream too, unplugs the pads and shows our controllers.
    fn end_hosting(&mut self, reason: EndReason, now: Instant) -> Option<HostPipeline> {
        let mut h = self.sessions.hosting.take()?;
        *self.panic.lock().unwrap_or_else(PoisonError::into_inner) = None;
        if let Some(pipeline) = &h.pipeline {
            pipeline.revoke_all();
        }
        for effect in h.session.end(reason) {
            if let SessionEffect::Broadcast(message) = effect {
                self.broadcast(h.room, &message);
            }
        }
        tracing::info!(room = ?h.room, ?reason, "session ended");
        self.announce(h.room, now);
        h.pipeline
    }

    pub(super) fn end_session(&mut self, now: Instant) -> Result<Response, EngineError> {
        let pipeline = self
            .sessions
            .hosting
            .is_some()
            .then(|| self.end_hosting(EndReason::HostStopped, now))
            .ok_or(EngineError::NotLive)?;
        if let Some(pipeline) = pipeline {
            tokio::spawn(pipeline.stop());
        }
        Ok(Response::Done)
    }

    fn our_session(
        &mut self,
        act: impl FnOnce(&mut HostSession) -> Result<Vec<SessionEffect>, EngineError>,
        now: Instant,
    ) -> Result<Response, EngineError> {
        let h = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| h.pipeline.is_some())
            .ok_or(EngineError::NotLive)?;
        let room = h.room;
        let fx = act(&mut h.session)?;
        self.apply_session(room, fx, now);
        Ok(Response::Done)
    }

    pub(super) fn answer_control(
        &mut self,
        member: &str,
        accept: bool,
        now: Instant,
    ) -> Result<Response, EngineError> {
        let member = MemberId::from_hex(member).ok_or(EngineError::UnknownMember)?;
        self.our_session(|s| Ok(s.answer(member, accept, now)?), now)
    }

    pub(super) fn pass_control(
        &mut self,
        member: &str,
        seat: SeatId,
        now: Instant,
    ) -> Result<Response, EngineError> {
        let member = MemberId::from_hex(member).ok_or(EngineError::UnknownMember)?;
        self.our_session(|s| Ok(s.pass(member, seat)?), now)
    }

    pub(super) fn take_back_control(
        &mut self,
        seat: Option<SeatId>,
        now: Instant,
    ) -> Result<Response, EngineError> {
        self.our_session(|s| Ok(s.take_back(seat)), now)
    }

    pub(super) fn play_seat(
        &mut self,
        seat: SeatId,
        controller: u8,
        now: Instant,
    ) -> Result<Response, EngineError> {
        self.our_session(|s| Ok(s.play(seat, controller)?), now)
    }

    /// Stops waiting for the pads Windows is still setting up (P10): the
    /// pipeline gives them up and says which seats went without.
    pub(super) fn stop_waiting_for_pads(&mut self) -> Result<Response, EngineError> {
        self.pipeline()
            .ok_or(EngineError::NotLive)?
            .stop_waiting_for_pads();
        Ok(Response::Done)
    }

    pub(super) fn allow_control(&mut self, now: Instant) -> Result<Response, EngineError> {
        // First: the host's seat may come back with a claim, which the input
        // task refuses while paused.
        if let Some(pipeline) = self.pipeline() {
            pipeline.allow_grants();
        }
        self.our_session(|s| Ok(s.allow_control()), now)
    }

    /// The panic key. The engine has already released through the
    /// pipeline's panic handle; this declines the requests and pauses
    /// control.
    pub(super) fn panic_key(&mut self, session: Option<SessionId>, now: Instant) {
        // A press during a session that has since ended (and another begun
        // meanwhile) isn't about this one.
        let Some(h) = self
            .sessions
            .hosting
            .as_mut()
            .filter(|h| session.is_none_or(|s| h.session.id() == s))
        else {
            return;
        };
        let room = h.room;
        tracing::warn!("panic key: every seat back to the host, control paused");
        if let Some(pipeline) = &h.pipeline {
            // Also refuses any grant still on its way to the input task.
            pipeline.panic();
        }
        let fx = h.session.panic();
        self.apply_session(room, fx, now);
    }

    /// The host's Accept or Decline key: answers the oldest request, once
    /// it has been the oldest for [`ANSWER_AFTER`], so a key pressed for one
    /// request never answers another that just took its place (arrived, or
    /// moved up when the one before it went).
    pub(super) fn answer_first(&mut self, session: SessionId, accept: bool, now: Instant) {
        let Some(h) = self
            .sessions
            .hosting
            .as_ref()
            .filter(|h| h.session.id() == session && h.pipeline.is_some())
        else {
            return;
        };
        let Some(&(member, seat)) = h.session.requests().first() else {
            tracing::info!("an answer key was pressed, but nobody asks");
            return;
        };
        // A first we haven't noted counts as just in.
        let seen = h.first.is_some_and(|(first, since)| {
            first == (member, seat) && now.saturating_duration_since(since) >= ANSWER_AFTER
        });
        if !seen {
            tracing::info!(member = ?member, "an answer key was pressed for a request too new at the front to have been seen; ignored");
            return;
        }
        tracing::info!(member = ?member, ?seat, accept, "answering with the host's key");
        if let Err(e) = self.our_session(|s| Ok(s.answer(member, accept, now)?), now) {
            tracing::warn!("answering with the host's key: {e}");
        }
    }

    // ---- Watching ----

    pub(super) fn watch(&mut self, room: &str, host: &str, reply: Reply, now: Instant) {
        match self.start_watching(room, host, now) {
            Ok(Some(reply_now)) => {
                let _ = reply.send(Ok(reply_now));
            }
            Ok(None) => {
                if let Some(w) = self.sessions.watching.as_mut() {
                    w.state = WatchState::Joining(Some(reply));
                }
            }
            Err(e) => {
                let _ = reply.send(Err(e));
            }
        }
    }

    /// `Some` answers at once; `None` waits for the host.
    fn start_watching(
        &mut self,
        room: &str,
        host: &str,
        now: Instant,
    ) -> Result<Option<Response>, EngineError> {
        let (room, _) = self.entry_mut(room)?;
        let host = MemberId::from_hex(host).ok_or(EngineError::UnknownMember)?;
        if host == self.me {
            return Err(EngineError::Refused("that is your own stream"));
        }
        if self.sessions.hosting.is_some() {
            return Err(EngineError::Refused("stop streaming first"));
        }
        let session = self
            .sessions
            .directory
            .get(&room)
            .and_then(|l| l.get(&host))
            .map(|l| l.announce.session)
            .ok_or(EngineError::Refused("that stream has ended"))?;
        if self.watching(room, host, session) {
            return Ok(Some(Response::Room(
                self.view(room, now).expect("watching in it"),
            )));
        }
        if self.sessions.watching.is_some() {
            self.end_watch(WatchEnd::Left, false, now);
        }
        let conn = self
            .current_conn(room, host)
            .ok_or(EngineError::Refused("not connected to the host"))?;
        let c = self.conns.get(&conn).ok_or(EngineError::Stopped)?;
        let options = WatchOptions {
            session: Some(session.0),
            viewer: ViewerOptions {
                log: Some(self.data_dir.join("viewer.log")),
                hide_stats: true,
                ..ViewerOptions::default()
            },
            measure: false,
            restart_viewer: true,
            shortcuts: ShortcutKeys {
                keys: crate::store::shortcuts(&self.data_dir).changed(),
            },
            volume: crate::store::volume(&self.data_dir),
        };
        // The subscriptions are in place before the host is asked.
        let (watch, mut events) = Watch::start(
            c.conn.clone(),
            &c.datagrams,
            &c.streams,
            self.backend.clone(),
            options,
        )
        .map_err(|e| EngineError::Media(e.to_string()))?;
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if inbox.send(Input::Watch(session, event)).await.is_err() {
                    return;
                }
            }
        });
        self.join_timer(session, 0);
        let codecs = self.backend.decodes();
        self.send(
            conn,
            Outgoing::Message(ControlMessage::SessionJoin(SessionJoin { session, codecs })),
        );
        self.sessions.watching = Some(Watching {
            room,
            host,
            conn,
            session,
            state: WatchState::Joining(None),
            watch: Some(watch),
            note: String::new(),
            link: 0,
            attempt: 0,
            away_since: None,
            in_front: true,
            holding: BTreeSet::new(),
            controllers: 0,
        });
        self.announce(room, now);
        Ok(None)
    }

    /// The host's answer to join attempt `attempt` is due within the wait.
    fn join_timer(&self, session: SessionId, attempt: u32) {
        let inbox = self.inbox.clone();
        let wait = self.join_wait;
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            let _ = inbox.send(Input::JoinTimeout(session, attempt)).await;
        });
    }

    pub(super) fn join_timeout(&mut self, session: SessionId, attempt: u32, now: Instant) {
        let joining = self.sessions.watching.as_ref().is_some_and(|w| {
            w.session == session
                && w.attempt == attempt
                && matches!(w.state, WatchState::Joining(_))
        });
        if joining {
            self.end_watch_refused(WatchEnd::Unavailable, "the host didn't answer", now);
        }
    }

    /// Watching failed before it began: the waiting command hears why.
    fn end_watch_refused(&mut self, ended: WatchEnd, why: &'static str, now: Instant) {
        if let Some(w) = self.sessions.watching.as_mut()
            && let WatchState::Joining(reply) = &mut w.state
            && let Some(reply) = reply.take()
        {
            let _ = reply.send(Err(EngineError::Refused(why)));
            self.end_watch(ended, false, now);
            return;
        }
        // Dropped later (the host lost its encoder, say).
        self.end_watch(ended, true, now);
    }

    /// Stops watching. The host hears we left unless the session or the
    /// connection is over; the UI hears why if `notify`.
    fn end_watch(&mut self, ended: WatchEnd, notify: bool, now: Instant) {
        let Some(mut w) = self.sessions.watching.take() else {
            return;
        };
        if !matches!(ended, WatchEnd::Ended | WatchEnd::ConnectionLost) {
            let leave = ControlMessage::SessionLeave(SessionLeave { session: w.session });
            self.send(w.conn, Outgoing::Message(leave));
        }
        if let WatchState::Joining(Some(reply)) =
            std::mem::replace(&mut w.state, WatchState::Watching)
        {
            let _ = reply.send(Err(EngineError::Refused("stopped watching")));
        }
        if let Some(watch) = w.watch.take() {
            tokio::spawn(watch.stop());
        }
        if notify {
            let _ = self.events.send(Event::WatchEnded {
                room: w.room.to_hex(),
                host: w.host.to_hex(),
                reason: ended,
            });
        }
        self.announce(w.room, now);
        // A listing kept while we waited to resume is no use now.
        self.forget_unreachable(now);
    }

    pub(super) fn stop_watching(&mut self, now: Instant) -> Result<Response, EngineError> {
        if self.sessions.watching.is_none() {
            return Err(EngineError::NotWatching);
        }
        self.end_watch(WatchEnd::Left, false, now);
        Ok(Response::Done)
    }

    /// Asks for `seat`: free, someone else's, or another than those we hold
    /// (added, or a move: the host decides by our controllers, M18). Asking
    /// again for another seat replaces the request.
    pub(super) fn request_control(
        &mut self,
        seat: SeatId,
        now: Instant,
    ) -> Result<Response, EngineError> {
        let labels = self.seat_labels();
        let label = labels
            .get(usize::from(seat.0))
            .cloned()
            .ok_or(EngineError::Refused("there is no such seat"))?;
        let w = self
            .sessions
            .watching
            .as_mut()
            .ok_or(EngineError::NotWatching)?;
        match w.state {
            WatchState::Joining(_) => return Err(EngineError::Refused("not watching yet")),
            WatchState::Reconnecting => {
                return Err(EngineError::Refused("reconnecting to the host"));
            }
            WatchState::Asking(asked) if asked == seat => {}
            WatchState::Controlling if w.holding.contains(&seat) => {}
            // Asking for a seat we hold while asking for another: staying
            // put, so the request is withdrawn.
            WatchState::Asking(_) if w.holding.contains(&seat) => return self.cancel_control(now),
            WatchState::Watching | WatchState::Asking(_) | WatchState::Controlling => {
                w.state = WatchState::Asking(seat);
                w.note = format!("Asked the host for {}", seat_phrase(&label));
                let (room, conn, session) = (w.room, w.conn, w.session);
                let ask = ControlMessage::ControlRequest(ControlRequest { session, seat });
                self.send(conn, Outgoing::Message(ask));
                self.send_hud();
                self.announce(room, now);
            }
        }
        Ok(Response::Done)
    }

    pub(super) fn cancel_control(&mut self, now: Instant) -> Result<Response, EngineError> {
        let w = self
            .sessions
            .watching
            .as_mut()
            .ok_or(EngineError::NotWatching)?;
        let (room, conn, session) = (w.room, w.conn, w.session);
        let cancel = |what| ControlMessage::ControlCancel(ControlCancel { session, what });
        match w.state {
            // Only the request goes: a guest asking to move keeps its seat,
            // even if the host has already declined.
            WatchState::Asking(_) => {
                w.state = w.settled();
                w.note = String::new();
                self.send(conn, Outgoing::Message(cancel(Cancel::Request)));
                self.send_hud();
                self.announce(room, now);
            }
            // Every seat we hold, given back once the host's releases arrive.
            WatchState::Controlling => {
                self.send(conn, Outgoing::Message(cancel(Cancel::Seat(None))));
            }
            WatchState::Joining(_) | WatchState::Watching | WatchState::Reconnecting => {}
        }
        Ok(Response::Done)
    }

    /// Gives back one of the seats we hold (M18), keeping the others and any
    /// request; it goes once the host's release arrives.
    pub(super) fn give_back_seat(&mut self, seat: SeatId) -> Result<Response, EngineError> {
        let w = self
            .sessions
            .watching
            .as_ref()
            .ok_or(EngineError::NotWatching)?;
        if !w.holding.contains(&seat) {
            return Err(EngineError::Refused("you don't have that seat"));
        }
        let cancel = ControlMessage::ControlCancel(ControlCancel {
            session: w.session,
            what: Cancel::Seat(Some(seat)),
        });
        self.send(w.conn, Outgoing::Message(cancel));
        Ok(Response::Done)
    }

    pub(super) fn watch_event(&mut self, session: SessionId, event: WatchEvent, now: Instant) {
        // Remembered for the profile's next stream windows (P6), even from a
        // window whose watch just ended: its last change may come after its
        // Leave.
        if let WatchEvent::Volume(volume) = event {
            if let Err(e) = crate::store::save_volume(&self.data_dir, volume) {
                tracing::warn!("saving the stream volume: {e}");
            }
            return;
        }
        let Some(w) = self
            .sessions
            .watching
            .as_ref()
            .filter(|w| w.session == session)
        else {
            return;
        };
        match event {
            WatchEvent::KeyframeNeeded { epoch } => {
                let ask = ControlMessage::KeyframeRequest(KeyframeRequest {
                    epoch,
                    reason: KeyframeReason::DecoderError,
                });
                self.send(w.conn, Outgoing::Message(ask));
            }
            WatchEvent::Action(UserAction::RequestControl(seat)) => {
                let _ = self.request_control(SeatId(seat), now);
            }
            WatchEvent::Action(UserAction::CancelControl) => {
                let _ = self.cancel_control(now);
            }
            WatchEvent::Action(UserAction::GiveBack(seat)) => {
                let _ = self.give_back_seat(SeatId(seat));
            }
            // Saved above, whatever the watch's state.
            WatchEvent::Volume(_) => {}
            // The host hears it now if we have joined, or after joining.
            WatchEvent::Controllers(count) => {
                let (joined, conn, session) = (w.joined(), w.conn, w.session);
                if let Some(w) = self.sessions.watching.as_mut() {
                    w.controllers = count;
                }
                if joined {
                    let count =
                        ControlMessage::ViewerControllers(ViewerControllers { session, count });
                    self.send(conn, Outgoing::Message(count));
                }
            }
            // The user closed the viewer.
            WatchEvent::Action(UserAction::Leave) | WatchEvent::ViewerClosed => {
                self.end_watch(WatchEnd::Left, true, now);
            }
            // It crashed, and too soon after the last time to start again.
            WatchEvent::ViewerCrashed => {
                tracing::warn!("the stream window crashed again; stopped watching");
                self.end_watch(WatchEnd::ViewerFailed, true, now);
            }
            WatchEvent::ViewerRestarted => {
                tracing::warn!("the stream window crashed and was started again");
            }
            // The room hears the connection close too; whichever is first
            // starts the wait for it to come back.
            // A late report about an earlier connection counts for nothing.
            WatchEvent::ConnectionLost { link } if link == w.link => self.lost_host(now),
            WatchEvent::ConnectionLost { .. } => {}
            WatchEvent::Timing(_)
            | WatchEvent::AudioStats(_)
            | WatchEvent::Net(_)
            | WatchEvent::Rumble(_) => {}
        }
    }

    /// The HUD of the session we watch.
    fn hud(&self) -> Option<HudState> {
        let w = self.sessions.watching.as_ref()?;
        let listing = self.sessions.directory.get(&w.room)?.get(&w.host)?;
        let status = listing.status.as_ref();
        let asked = match w.state {
            WatchState::Asking(seat) => Some(seat),
            _ => None,
        };
        let kinds = &listing.announce.seats;
        let seats = seat_labels(kinds)
            .into_iter()
            .zip(kinds)
            .enumerate()
            .map(|(i, (label, kind))| {
                let host = status.is_some_and(|s| s.host_seats.contains(&SeatId(i as u8)));
                let holder = match host {
                    true => Some(w.host),
                    false => status.and_then(|s| s.holders.get(i).copied().flatten()),
                };
                HudSeat {
                    label,
                    pad: kind.has_pad(),
                    keyboard_mouse: kind.has_keyboard_mouse(),
                    holder: holder.map(|h| self.member_name(w.room, h)),
                    host,
                    mine: w.holding.contains(&SeatId(i as u8)),
                    asked: asked == Some(SeatId(i as u8)),
                }
            })
            .collect();
        Some(HudState {
            title: listing.announce.title.clone(),
            host: self.member_name(w.room, w.host),
            role: match w.state {
                WatchState::Joining(_) | WatchState::Watching | WatchState::Reconnecting => {
                    HudRole::Watching
                }
                WatchState::Asking(_) => HudRole::Asking,
                WatchState::Controlling => HudRole::Controlling,
            },
            seats,
            host_on_seats: status.is_some_and(|s| s.host_on_seats),
            viewers: status.map_or(0, |s| s.viewers.len().min(255) as u8),
            control_paused: status.is_some_and(|s| s.control_paused),
            game_in_front: w.in_front,
            stream: status.map_or(StreamState::Waiting, |s| s.stream),
            note: w.note.clone(),
        })
    }

    fn send_hud(&self) {
        if let (Some(hud), Some(watch)) = (
            self.hud(),
            self.sessions
                .watching
                .as_ref()
                .and_then(|w| w.watch.as_ref()),
        ) {
            watch.set_hud(hud);
        }
    }

    /// After a change to the room: a viewer of ours who is no longer a
    /// member (banned, gone) is dropped at once, releasing what it held,
    /// rather than when its connection finally closes, which a peer can
    /// delay. Likewise we stop watching a host who is no longer a member,
    /// and forget such hosts' sessions.
    pub(super) fn prune_sessions(&mut self, room: RoomId, now: Instant) {
        let Some(entry) = self.rooms.get(&room) else {
            return;
        };
        let gone: Vec<MemberId> = self
            .sessions
            .hosting
            .as_ref()
            .filter(|h| h.room == room)
            .map(|h| {
                h.session
                    .viewers()
                    .iter()
                    .copied()
                    .filter(|v| !entry.room.is_member(*v))
                    .collect()
            })
            .unwrap_or_default();
        let host_gone = self
            .sessions
            .watching
            .as_ref()
            .is_some_and(|w| w.room == room && !entry.room.is_member(w.host));
        let stale: Vec<MemberId> = self
            .sessions
            .directory
            .get(&room)
            .map(|l| {
                l.keys()
                    .copied()
                    .filter(|host| !entry.room.is_member(*host))
                    .collect()
            })
            .unwrap_or_default();
        for viewer in gone {
            tracing::info!(member = ?viewer, "a viewer left the room: dropped from our stream");
            let fx = self
                .hosting_mut()
                .map(|h| h.session.leave(viewer))
                .unwrap_or_default();
            self.apply_session(room, fx, now);
        }
        if host_gone {
            self.end_watch(WatchEnd::Ended, true, now);
        }
        if !stale.is_empty() {
            if let Some(listings) = self.sessions.directory.get_mut(&room) {
                listings.retain(|host, _| !stale.contains(host));
            }
            self.announce(room, now);
        }
    }

    // ---- Rooms going away, and shutting down ----

    /// We leave `room`, or it is gone: our stream in it ends, watching in it
    /// stops, and its directory goes.
    pub(super) fn end_room_sessions(&mut self, room: RoomId, now: Instant) {
        if self
            .sessions
            .hosting
            .as_ref()
            .is_some_and(|h| h.room == room)
            && let Some(stream) = self.end_hosting(EndReason::HostLeftRoom, now)
        {
            tokio::spawn(stream.stop());
        }
        if self
            .sessions
            .watching
            .as_ref()
            .is_some_and(|w| w.room == room)
        {
            self.end_watch(WatchEnd::Left, false, now);
        }
        self.sessions.directory.remove(&room);
    }

    /// Ends our stream and waits until everything a guest held is released;
    /// stops watching.
    pub(super) async fn end_sessions_for_shutdown(&mut self, now: Instant) {
        if let Some(stream) = self.end_hosting(EndReason::HostShutdown, now) {
            let _ = tokio::time::timeout(Duration::from_secs(5), stream.stop()).await;
        }
        if let Some(mut w) = self.sessions.watching.take() {
            let leave = ControlMessage::SessionLeave(SessionLeave { session: w.session });
            self.send(w.conn, Outgoing::Message(leave));
            if let Some(watch) = w.watch.take() {
                let _ = tokio::time::timeout(Duration::from_secs(3), watch.stop()).await;
            }
        }
    }

    // ---- What the UI shows ----

    pub(super) fn session_views(&self, room: RoomId) -> Vec<SessionView> {
        let mut views = Vec::new();
        if let Some(h) = self
            .sessions
            .hosting
            .as_ref()
            .filter(|h| h.room == room && h.pipeline.is_some())
        {
            let s = &h.session;
            let (stream, streaming) = match &h.stream {
                Some(OurStream::Starting { view, .. }) => (StreamStateView::Starting, Some(view)),
                Some(OurStream::Live { view, .. }) => (StreamStateView::Live, Some(view)),
                None => (stream_view(s.stream()), None),
            };
            views.push(SessionView {
                host: self.me.to_hex(),
                title: s.config().title.clone(),
                max_viewers: s.config().max_viewers,
                viewers: s.viewers().iter().map(MemberId::to_hex).collect(),
                viewer_controllers: s.viewers().iter().map(|v| s.controllers_of(*v)).collect(),
                seats: seat_views(&s.config().seats, s.holders(), self.me)
                    .into_iter()
                    .enumerate()
                    .map(|(i, mut seat)| {
                        seat.host_controller = seat
                            .host
                            .then(|| s.host_controller_on(SeatId(i as u8)))
                            .flatten();
                        seat
                    })
                    .collect(),
                host_on_seats: s.host().is_some(),
                control_paused: s.control_paused(),
                role: SessionRole::Host,
                requests: s
                    .requests()
                    .iter()
                    .map(|(member, seat)| RequestView {
                        member: member.to_hex(),
                        seat: seat.0,
                    })
                    .collect(),
                asked_seat: None,
                panic_key: h.panic_key.as_ref().map(|(_, keys)| keys.clone()),
                answer_keys: h
                    .answer_keys
                    .clone()
                    .map(|(accept, decline)| AnswerKeys { accept, decline }),
                overlay_area: h.overlay_area,
                host_controllers: h
                    .controllers
                    .clone()
                    .and_then(Result::ok)
                    .unwrap_or_default(),
                host_homes: s.host_homes().iter().map(|seat| seat.0).collect(),
                host_controller_problem: h.controllers.clone().and_then(Result::err),
                stream,
                streaming: streaming.cloned(),
                sound_level: Some(h.sound_level),
                pads_setting_up: h.pads_setting_up.iter().map(|seat| seat.0).collect(),
                pads_setting_up_secs: h
                    .pads_since
                    .map_or(0, |since| since.elapsed().as_secs() as u32),
                pads_missing: h.pads_missing.iter().map(|seat| seat.0).collect(),
            });
        }
        for (host, listing) in self.sessions.directory.get(&room).into_iter().flatten() {
            let status = listing.status.as_ref();
            let mut asked_seat = None;
            let role = match &self.sessions.watching {
                Some(w) if w.room == room && w.host == *host => match w.state {
                    WatchState::Joining(_) => SessionRole::Joining,
                    WatchState::Watching => SessionRole::Watching,
                    WatchState::Asking(seat) => {
                        asked_seat = Some(seat.0);
                        SessionRole::Asking
                    }
                    WatchState::Controlling => SessionRole::Controlling,
                    WatchState::Reconnecting => SessionRole::Reconnecting,
                },
                _ => SessionRole::None,
            };
            let mut holders = status.map_or_else(
                || vec![None; listing.announce.seats.len()],
                |s| s.holders.clone(),
            );
            for seat in status.map_or(&[][..], |s| &s.host_seats) {
                if let Some(slot) = holders.get_mut(usize::from(seat.0)) {
                    *slot = Some(*host);
                }
            }
            views.push(SessionView {
                host: host.to_hex(),
                title: listing.announce.title.clone(),
                max_viewers: listing.announce.max_viewers,
                viewers: status.map_or_else(Vec::new, |s| {
                    s.viewers.iter().map(MemberId::to_hex).collect()
                }),
                viewer_controllers: Vec::new(),
                seats: seat_views(&listing.announce.seats, &holders, *host),
                host_on_seats: status.is_some_and(|s| s.host_on_seats),
                control_paused: status.is_some_and(|s| s.control_paused),
                role,
                requests: Vec::new(),
                asked_seat,
                panic_key: None,
                answer_keys: None,
                overlay_area: None,
                host_controllers: Vec::new(),
                host_homes: Vec::new(),
                host_controller_problem: None,
                stream: stream_view(status.map_or(StreamState::Waiting, |s| s.stream)),
                streaming: None,
                sound_level: None,
                pads_setting_up: Vec::new(),
                pads_setting_up_secs: 0,
                pads_missing: Vec::new(),
            });
        }
        views
    }
}

/// A session's stream state, for the UI.
fn stream_view(state: StreamState) -> StreamStateView {
    match state {
        StreamState::Waiting => StreamStateView::Waiting,
        StreamState::Live => StreamStateView::Live,
        StreamState::Stopped(reason) => StreamStateView::Stopped(end_reason(reason)),
    }
}

/// A session's seats with their holders, `host` included, for the UI.
fn seat_views(seats: &[SeatKind], holders: &[Option<MemberId>], host: MemberId) -> Vec<SeatView> {
    seat_labels(seats)
        .into_iter()
        .zip(seats)
        .enumerate()
        .map(|(i, (label, kind))| {
            let holder = holders.get(i).copied().flatten();
            SeatView {
                label,
                pad: kind.has_pad(),
                keyboard_mouse: kind.has_keyboard_mouse(),
                holder: holder.map(|h| h.to_hex()),
                host: holder == Some(host),
                host_controller: None,
            }
        })
        .collect()
}
