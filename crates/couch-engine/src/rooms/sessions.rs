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

use std::{any::Any, collections::BTreeMap};

use couch_input::shortcuts::{self, Shortcut};
use couch_proto::{
    control::{KeyframeReason, KeyframeRequest},
    ids::SeatEpoch,
    ipc::{HudRole, HudState, UserAction},
    session::{
        ControlCancel, ControlRequest, EndReason, JoinResult, SessionAnnounce, SessionJoin,
        SessionLeave, SessionMode, SessionStatus,
    },
};
use couch_room::session::{Effect as SessionEffect, HostSession, SessionConfig};

use super::*;
use crate::{
    SessionEnd, SessionRole, SessionView, SourceChoice, WatchEnd,
    pipeline::{
        AudioChoice, Source, ViewerOptions,
        host::{HostOptions, Rates},
        watch::{Watch, WatchOptions},
    },
};

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
    /// `None` until the stream has started: plugging in the pad takes a
    /// while, and nobody hears of the session before.
    stream: Option<HostStream>,
    /// The connection each viewer joined on. When it closes, the viewer
    /// has gone: its stream and input were bound to it.
    viewer_conns: BTreeMap<MemberId, ConnId>,
    /// The panic key's hook, in place while the stream runs; `None` if it
    /// couldn't be installed.
    panic_key: Option<Box<dyn Any + Send>>,
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
}

enum WatchState {
    /// Asked to watch; the reply waits for the host's answer.
    Joining(Option<Reply>),
    Watching,
    Asking,
    Controlling,
}

struct Listing {
    announce: SessionAnnounce,
    status: Option<SessionStatus>,
}

/// [`Command::GoLive`]'s fields.
pub(super) struct GoLive {
    pub room: String,
    pub source: SourceChoice,
    pub title: String,
    pub fps: u32,
    pub max_mbps: u32,
    pub upload_mbps: Option<u32>,
    pub max_viewers: u8,
    pub keyboard_mouse: bool,
}

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
    fn current_conn(&self, room: RoomId, member: MemberId) -> Option<ConnId> {
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
        if h.room != room || h.stream.is_none() {
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
            self.end_watch(WatchEnd::ConnectionLost, true, now);
        }
        // Without a connection to its host, a session can't be watched; it
        // is announced again on the next one.
        if self.current_conn(room, peer).is_none()
            && let Some(listings) = self.sessions.directory.get_mut(&room)
            && listings.remove(&peer).is_some()
        {
            self.announce(room, now);
        }
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
            s.hosting
                .as_ref()
                .is_some_and(|h| h.room == room && h.session.id() == session && h.stream.is_some())
        };
        match message {
            // To us, the host.
            ControlMessage::SessionJoin(m) => {
                if ours(&self.sessions, m.session) {
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
                let fx = self.hosting_mut().map(|h| h.session.request(peer, now));
                self.apply_session(room, fx.unwrap_or_default(), now);
            }
            ControlMessage::ControlCancel(m) if ours(&self.sessions, m.session) => {
                let fx = self.hosting_mut().map(|h| h.session.cancel(peer));
                self.apply_session(room, fx.unwrap_or_default(), now);
            }
            ControlMessage::KeyframeRequest(_) => {
                if let Some(h) = self.sessions.hosting.as_ref()
                    && h.room == room
                    && h.session.is_viewer(peer)
                    && let Some(stream) = &h.stream
                {
                    stream.keyframe(peer);
                }
            }
            // From a host, about its own session.
            ControlMessage::SessionAnnounce(a) => {
                if !a.is_valid() {
                    return;
                }
                let listing = Listing {
                    announce: a,
                    status: None,
                };
                self.sessions
                    .directory
                    .entry(room)
                    .or_default()
                    .insert(peer, listing);
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
                if !s.is_valid() || listing.announce.session != s.session {
                    return;
                }
                listing.status = Some(s);
                self.send_hud();
                self.announce(room, now);
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
                        tracing::info!(room = ?room, host = ?peer, "watching a stream");
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
                    && matches!(w.state, WatchState::Asking)
                {
                    w.state = WatchState::Watching;
                    w.note = "The host said no, for now".into();
                    self.send_hud();
                    self.announce(room, now);
                }
            }
            ControlMessage::SeatAssigned(s) => {
                let Some(w) = self.sessions.watching.as_mut() else {
                    return;
                };
                if w.room != room || w.host != peer || matches!(w.state, WatchState::Joining(_)) {
                    return;
                }
                w.state = WatchState::Controlling;
                w.note = "You have the controller".into();
                if let Some(watch) = &w.watch {
                    watch.seat_assigned(s);
                }
                self.send_hud();
                self.announce(room, now);
            }
            ControlMessage::SeatReleased(s) => {
                let Some(w) = self.sessions.watching.as_mut() else {
                    return;
                };
                if w.room != room || w.host != peer {
                    return;
                }
                if let Some(watch) = &w.watch {
                    watch.seat_released(s);
                }
                if matches!(w.state, WatchState::Controlling) {
                    w.state = WatchState::Watching;
                    w.note = "The controller went back to the host".into();
                    self.send_hud();
                    self.announce(room, now);
                }
            }
            _ => {}
        }
    }

    fn hosting_mut(&mut self) -> Option<&mut Hosting> {
        self.sessions.hosting.as_mut()
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
                        h.stream.as_ref()?.add_viewer(
                            member,
                            c.conn.clone(),
                            &c.datagrams,
                            &c.streams,
                        );
                        Some(id)
                    });
                    match (started, self.sessions.hosting.as_mut()) {
                        (Some(id), Some(h)) => {
                            h.viewer_conns.insert(member, id);
                        }
                        (None, Some(h)) => more.extend(h.session.viewer_failed(member)),
                        _ => {}
                    }
                }
                SessionEffect::StopViewer(member) => {
                    if let Some(h) = self.sessions.hosting.as_mut() {
                        h.viewer_conns.remove(&member);
                        if let Some(stream) = &h.stream {
                            stream.remove_viewer(member);
                        }
                    }
                }
                SessionEffect::Grant {
                    member,
                    permissions,
                } => {
                    if let Some(stream) = self.stream() {
                        stream.grant(member, permissions);
                    }
                }
                SessionEffect::Revoke { holder } => {
                    if let Some(stream) = self.stream() {
                        stream.revoke(holder);
                    }
                }
                SessionEffect::RevokeAll => {
                    if let Some(stream) = self.stream() {
                        stream.revoke_all();
                    }
                }
                SessionEffect::Requested(member) => {
                    tracing::info!(member = ?member, "a viewer asks for control");
                    let _ = self.events.send(Event::ControlRequested {
                        room: room.to_hex(),
                        member: member.to_hex(),
                    });
                }
                SessionEffect::Changed => changed = true,
            }
        }
        if !more.is_empty() {
            self.apply_session(room, more, now);
        }
        if changed {
            self.announce(room, now);
        }
    }

    fn stream(&self) -> Option<&HostStream> {
        self.sessions.hosting.as_ref()?.stream.as_ref()
    }

    // ---- Our stream ----

    /// Starts our stream. Answers through `reply` once it has started; the
    /// error comes back too, for the caller's log.
    pub(super) fn go_live(
        &mut self,
        go: GoLive,
        reply: Reply,
        now: Instant,
    ) -> Result<(), EngineError> {
        let result = self.prepare_live(go, now);
        match result {
            Ok((session, options)) => {
                let backend = self.backend.clone();
                let inbox = self.inbox.clone();
                tokio::spawn(async move {
                    let (stream, events) = HostStream::start(backend, options).await;
                    let ready = Input::LiveReady {
                        session,
                        stream: Box::new(stream),
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

    fn prepare_live(
        &mut self,
        go: GoLive,
        _now: Instant,
    ) -> Result<(SessionId, HostOptions), EngineError> {
        let (room, _) = self.entry_mut(&go.room)?;
        if self.sessions.hosting.is_some() {
            return Err(EngineError::Refused("you are streaming already"));
        }
        if self.sessions.watching.is_some() {
            return Err(EngineError::Refused("stop watching first"));
        }
        let gone = EngineError::Refused("that window or screen is gone");
        let source = match &go.source {
            SourceChoice::Window { id } => {
                let id = id.parse().map_err(|_| gone)?;
                if !self.backend.window_exists(id) {
                    return Err(EngineError::Refused("that window is gone"));
                }
                Source::Window(id)
            }
            SourceChoice::Screen { id } => Source::Screen(id.parse().map_err(|_| gone)?),
        };
        if go.keyboard_mouse && !source.is_window() {
            return Err(EngineError::Refused(
                "keyboard and mouse need a window as the source",
            ));
        }
        let session = SessionId(u64::from_le_bytes(random()?));
        let config = SessionConfig {
            title: go.title.trim().to_string(),
            mode: SessionMode::SinglePlayer,
            keyboard_mouse: go.keyboard_mouse,
            max_viewers: go.max_viewers,
        };
        let host_session = HostSession::new(session, config)?;
        let defaults = Rates::default();
        let max_bps = mbps(go.max_mbps.max(2));
        let options = HostOptions {
            source,
            fps: go.fps.clamp(1, 240),
            rates: Rates {
                max_bps,
                start_bps: defaults.start_bps.min(max_bps),
                ..defaults
            },
            upload_bps: go.upload_mbps.map(mbps),
            audio: AudioChoice::Auto,
            // Games and Steam know a controller by its ids; a made-up pair
            // leaves only the D-pad working.
            real_pad_ids: true,
            session: session.0,
            first_epoch: SeatEpoch(u32::from_le_bytes(random()?)),
            rc_trace: None,
            netem_elapsed: None,
        };
        self.sessions.hosting = Some(Hosting {
            room,
            session: host_session,
            stream: None,
            viewer_conns: BTreeMap::new(),
            panic_key: None,
        });
        Ok((session, options))
    }

    pub(super) fn live_ready(
        &mut self,
        session: SessionId,
        stream: HostStream,
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
            // Stopped while it was starting.
            tokio::spawn(stream.stop());
            let _ = reply.send(Err(EngineError::Refused("stopped before it started")));
            return;
        };
        let room = h.room;
        *self.panic.lock().unwrap_or_else(PoisonError::into_inner) = Some(stream.panic_handle());
        // The hook releases straight through the input task, from its own
        // thread, before the actor hears of it.
        let on_press = {
            let handle = stream.panic_handle();
            let inbox = self.inbox.clone();
            let runtime = tokio::runtime::Handle::current();
            move || {
                handle.panic();
                let inbox = inbox.clone();
                runtime.spawn(async move {
                    let _ = inbox.send(Input::Panic(Some(session))).await;
                });
            }
        };
        h.panic_key = match self.backend.panic_key(Box::new(on_press)) {
            Ok(hook) => Some(hook),
            Err(e) => {
                tracing::error!("no panic key while streaming: {e}");
                None
            }
        };
        h.stream = Some(stream);
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if inbox.send(Input::Host(session, event)).await.is_err() {
                    return;
                }
            }
        });
        tracing::info!(room = ?room, "went live");
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
                    .map(|h| h.session.seat_changed(Some(member)));
                self.apply_session(room, fx.unwrap_or_default(), now);
            }
            HostEvent::Revoked {
                member,
                seat,
                epoch,
            } => {
                let released = couch_proto::control::SeatReleased {
                    seat,
                    seat_epoch: epoch,
                };
                self.send_to(room, member, ControlMessage::SeatReleased(released));
                let fx = self.hosting_mut().map(|h| h.session.seat_changed(None));
                self.apply_session(room, fx.unwrap_or_default(), now);
            }
            HostEvent::ViewerFailed { member, error } => {
                tracing::warn!(member = ?member, "no stream for a viewer: {error}");
                let fx = self.hosting_mut().map(|h| h.session.viewer_failed(member));
                self.apply_session(room, fx.unwrap_or_default(), now);
            }
            HostEvent::SourceClosed => self.end_hosting_on_its_own(EndReason::SourceLost, now),
            HostEvent::CaptureFailed(e) => {
                tracing::error!("capture failed: {e}");
                self.end_hosting_on_its_own(EndReason::Error, now);
            }
            HostEvent::Note(text) => tracing::info!("{text}"),
            HostEvent::Seat { status, .. } => tracing::debug!(?status, "seat"),
            HostEvent::ViewerGone { member, .. } => {
                tracing::debug!(member = ?member, "a viewer's input was released");
            }
            HostEvent::Released { .. } | HostEvent::Stats(_) | HostEvent::Rumble(_) => {}
        }
    }

    /// The stream ended without us asking: the UI is told why.
    fn end_hosting_on_its_own(&mut self, reason: EndReason, now: Instant) {
        let room = self.sessions.hosting.as_ref().map(|h| h.room);
        if let Some(stream) = self.end_hosting(reason, now) {
            tokio::spawn(stream.stop());
        }
        if let Some(room) = room {
            let _ = self.events.send(Event::SessionEnded {
                room: room.to_hex(),
                reason: end_reason(reason),
            });
        }
    }

    /// Ends our stream: input is released first, then the room hears why.
    /// Returns the stream for the caller to stop (or wait for).
    fn end_hosting(&mut self, reason: EndReason, now: Instant) -> Option<HostStream> {
        let mut h = self.sessions.hosting.take()?;
        *self.panic.lock().unwrap_or_else(PoisonError::into_inner) = None;
        if let Some(stream) = &h.stream {
            stream.revoke_all();
        }
        for effect in h.session.end(reason) {
            if let SessionEffect::Broadcast(message) = effect {
                self.broadcast(h.room, &message);
            }
        }
        tracing::info!(room = ?h.room, ?reason, "stream ended");
        self.announce(h.room, now);
        h.stream
    }

    pub(super) fn stop_live(&mut self, now: Instant) -> Result<Response, EngineError> {
        let stream = self
            .sessions
            .hosting
            .is_some()
            .then(|| self.end_hosting(EndReason::HostStopped, now))
            .ok_or(EngineError::NotLive)?;
        if let Some(stream) = stream {
            tokio::spawn(stream.stop());
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
            .filter(|h| h.stream.is_some())
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
        now: Instant,
    ) -> Result<Response, EngineError> {
        let member = MemberId::from_hex(member).ok_or(EngineError::UnknownMember)?;
        self.our_session(|s| Ok(s.pass(member)?), now)
    }

    pub(super) fn take_back_control(&mut self, now: Instant) -> Result<Response, EngineError> {
        self.our_session(|s| Ok(s.take_back()), now)
    }

    pub(super) fn allow_control(&mut self, now: Instant) -> Result<Response, EngineError> {
        let done = self.our_session(|s| Ok(s.allow_control()), now)?;
        if let Some(stream) = self.stream() {
            stream.allow_grants();
        }
        Ok(done)
    }

    /// The panic key. The engine has already released through the stream's
    /// panic handle; this declines the requests and pauses control.
    pub(super) fn panic_key(&mut self, session: Option<SessionId>, now: Instant) {
        // A press during a stream that has since ended (stopped and gone
        // live again meanwhile) isn't about this one.
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
        if let Some(stream) = &h.stream {
            // Also refuses any grant still on its way to the input task.
            stream.panic();
        }
        let fx = h.session.panic();
        self.apply_session(room, fx, now);
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
                log: Some(self.viewer_log.clone()),
                hide_stats: true,
                ..ViewerOptions::default()
            },
            measure: false,
        };
        // The subscriptions are in place before the host is asked.
        let (watch, mut events) = Watch::start(
            c.conn.clone(),
            &c.datagrams,
            &c.streams,
            &*self.backend,
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
        let inbox = self.inbox.clone();
        let wait = self.join_wait;
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            let _ = inbox.send(Input::JoinTimeout(session)).await;
        });
        self.send(
            conn,
            Outgoing::Message(ControlMessage::SessionJoin(SessionJoin { session })),
        );
        self.sessions.watching = Some(Watching {
            room,
            host,
            conn,
            session,
            state: WatchState::Joining(None),
            watch: Some(watch),
            note: String::new(),
        });
        self.announce(room, now);
        Ok(None)
    }

    pub(super) fn join_timeout(&mut self, session: SessionId, now: Instant) {
        let joining = self
            .sessions
            .watching
            .as_ref()
            .is_some_and(|w| w.session == session && matches!(w.state, WatchState::Joining(_)));
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
    }

    pub(super) fn stop_watching(&mut self, now: Instant) -> Result<Response, EngineError> {
        if self.sessions.watching.is_none() {
            return Err(EngineError::NotWatching);
        }
        self.end_watch(WatchEnd::Left, false, now);
        Ok(Response::Done)
    }

    pub(super) fn request_control(&mut self, now: Instant) -> Result<Response, EngineError> {
        let w = self
            .sessions
            .watching
            .as_mut()
            .ok_or(EngineError::NotWatching)?;
        match w.state {
            WatchState::Watching => {
                w.state = WatchState::Asking;
                w.note = "Asked the host for the controller".into();
                let (room, conn, session) = (w.room, w.conn, w.session);
                let ask = ControlMessage::ControlRequest(ControlRequest { session });
                self.send(conn, Outgoing::Message(ask));
                self.send_hud();
                self.announce(room, now);
            }
            WatchState::Joining(_) => return Err(EngineError::Refused("not watching yet")),
            WatchState::Asking | WatchState::Controlling => {}
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
        let cancel = ControlMessage::ControlCancel(ControlCancel { session });
        match w.state {
            WatchState::Asking => {
                w.state = WatchState::Watching;
                w.note = String::new();
                self.send(conn, Outgoing::Message(cancel));
                self.send_hud();
                self.announce(room, now);
            }
            // Given back once the host's release arrives.
            WatchState::Controlling => self.send(conn, Outgoing::Message(cancel)),
            WatchState::Joining(_) | WatchState::Watching => {}
        }
        Ok(Response::Done)
    }

    pub(super) fn watch_event(&mut self, session: SessionId, event: WatchEvent, now: Instant) {
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
            WatchEvent::Action(UserAction::RequestControl) => {
                let _ = self.request_control(now);
            }
            WatchEvent::Action(UserAction::CancelControl) => {
                let _ = self.cancel_control(now);
            }
            // The user closed the viewer (or it died): either way, gone.
            WatchEvent::Action(UserAction::Leave) | WatchEvent::ViewerClosed => {
                self.end_watch(WatchEnd::Left, true, now);
            }
            WatchEvent::ConnectionLost => self.end_watch(WatchEnd::ConnectionLost, true, now),
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
        Some(HudState {
            title: listing.announce.title.clone(),
            host: self.member_name(w.room, w.host),
            role: match w.state {
                WatchState::Joining(_) | WatchState::Watching => HudRole::Watching,
                WatchState::Asking => HudRole::Asking,
                WatchState::Controlling => HudRole::Controlling,
            },
            controller: status
                .and_then(|s| s.controller)
                .map(|c| self.member_name(w.room, c)),
            viewers: status.map_or(0, |s| s.viewers.len().min(255) as u8),
            keyboard_mouse: listing.announce.keyboard_mouse,
            control_paused: status.is_some_and(|s| s.control_paused),
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
            .filter(|h| h.room == room && h.stream.is_some())
        {
            let s = &h.session;
            views.push(SessionView {
                host: self.me.to_hex(),
                title: s.config().title.clone(),
                keyboard_mouse: s.config().keyboard_mouse,
                max_viewers: s.config().max_viewers,
                viewers: s.viewers().iter().map(MemberId::to_hex).collect(),
                controller: s.controller().map(|c| c.to_hex()),
                control_paused: s.control_paused(),
                role: SessionRole::Host,
                requests: s.requests().iter().map(MemberId::to_hex).collect(),
                panic_key: h
                    .panic_key
                    .is_some()
                    .then(|| shortcuts::binding(Shortcut::Panic).keys()),
            });
        }
        for (host, listing) in self.sessions.directory.get(&room).into_iter().flatten() {
            let status = listing.status.as_ref();
            let role = match &self.sessions.watching {
                Some(w) if w.room == room && w.host == *host => match w.state {
                    WatchState::Joining(_) => SessionRole::Joining,
                    WatchState::Watching => SessionRole::Watching,
                    WatchState::Asking => SessionRole::Asking,
                    WatchState::Controlling => SessionRole::Controlling,
                },
                _ => SessionRole::None,
            };
            views.push(SessionView {
                host: host.to_hex(),
                title: listing.announce.title.clone(),
                keyboard_mouse: listing.announce.keyboard_mouse,
                max_viewers: listing.announce.max_viewers,
                viewers: status.map_or_else(Vec::new, |s| {
                    s.viewers.iter().map(MemberId::to_hex).collect()
                }),
                controller: status.and_then(|s| s.controller).map(|c| c.to_hex()),
                control_paused: status.is_some_and(|s| s.control_paused),
                role,
                requests: Vec::new(),
                panic_key: None,
            });
        }
        views
    }
}
