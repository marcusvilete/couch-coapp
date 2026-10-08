//! Every room's state machine (`couch_room::room`) run over iroh.
//!
//! One task, the [`Actor`], owns all room state. The accept loop, dials,
//! joins and each connection's reader and writer are tasks of their own that
//! report to it over one bounded channel, so room state is never shared or
//! locked. A peer flooding us is slowed by QUIC flow control (the readers
//! wait for the actor), and a peer that stops reading is cut off once its
//! outgoing queue fills. The admission limits are the one shared piece: the
//! endpoint's gate reads them during the TLS handshake.
//!
//! Only a room's owner admits newcomers (with an invite and the password);
//! members accept each other by the owner's signed roster.
//!
//! Sessions ride on the same connections ([`sessions`]): the one we stream,
//! the one we watch, and the others' we hear of.

mod sessions;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use couch_net::{
    Connection, Endpoint, Incoming, NetError, WeakConnectionHandle, addr,
    admission::{self, AcceptTerms, AdmissionError, JoinTerms, PasswordKey, Request},
    control::{self, ControlReceiver, ControlSender},
    datagrams::Datagrams,
    endpoint::{self, EndpointOptions, Gate, RelayChoice},
    streams::Streams,
};
use couch_proto::{
    admission::{JoinStart, MemberHello},
    control::{ALIVE_EVERY, Alive, ControlMessage, close_code},
    ids::{InviteTag, InviteToken, MemberId, RoomId, RoomNonce, SeatId, SessionId},
    invite::Invite,
    room::{PeerAddr, RoomPolicy, RoomSnapshot, Signed, valid_name},
};
use couch_room::{
    SigningKey,
    limits::AdmissionLimits,
    room::{
        ConnId, Effect, Identity, JoinDecision, LinkState, MemberDecision, Room, RoomError,
        Timings, proves_ban,
    },
    signing::{invite_tag, room_id},
};
use tokio::{
    sync::{Semaphore, broadcast, mpsc, oneshot},
    task::JoinHandle,
    time::MissedTickBehavior,
};

use crate::{
    Command, EngineError, EngineOptions, Event, InvitePreview, InviteStatus, MemberLink,
    MemberView, OtherVersion, Profile, ProfileView, Response, RoomGone, RoomView,
    pipeline::{
        Backend,
        host::{HostEvent, HostStream, PanicHandle},
        watch::WatchEvent,
    },
    store::{RememberedRoom, Settings, Store, StoreError},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a closing connection waits for its last messages to be acked.
const FLUSH_WAIT: Duration = Duration::from_secs(1);
/// After this, a close happens even if the writer is stuck behind a peer
/// that stopped reading.
const CLOSE_DEADLINE: Duration = Duration::from_secs(2);
/// Messages waiting for one peer; a peer this far behind is cut off.
const OUT_QUEUE: usize = 256;
/// Incoming connections being admitted at once; more are refused.
const MAX_PENDING_ADMISSIONS: usize = 32;
const INBOX: usize = 1024;
const ADDR_CHECK_EVERY: Duration = Duration::from_secs(2);
const PATH_CHECK_EVERY: Duration = Duration::from_secs(1);
/// How long making an invite waits for the relay when relays are configured.
const INVITE_WAIT: Duration = Duration::from_secs(10);

type Reply = oneshot::Sender<Result<Response, EngineError>>;

pub(crate) enum Input {
    Command(Command, Reply),
    Connections(String, oneshot::Sender<Vec<(String, bool)>>),
    /// The relay we are reachable through now, if any.
    HomeRelay(oneshot::Sender<Option<String>>),
    /// For tests: drop the connection with a member, as a dead path would.
    DropConnection(String, String),
    Shutdown(oneshot::Sender<()>),
    /// An accept task asks how to answer a newcomer's `JoinStart`.
    JoinQuery {
        room: RoomId,
        dialer: MemberId,
        invite: InviteTag,
        reply: oneshot::Sender<JoinAnswer>,
    },
    /// A newcomer's password check didn't end in admission.
    JoinFailed {
        room: RoomId,
        dialer: MemberId,
        invite: InviteTag,
    },
    /// An accept task asks how to answer a member's `MemberHello`.
    MemberQuery {
        room: RoomId,
        dialer: MemberId,
        policy: Box<Signed<RoomPolicy>>,
        reply: oneshot::Sender<MemberDecision>,
    },
    Admitted(Box<Admitted>),
    /// A member's build has another wire revision (`PROTOCOL_MINOR`): we
    /// can't talk until one of us updates. Seen when it dialed us.
    OtherVersion {
        member: MemberId,
        minor: u16,
        app_version: String,
    },
    DialFailed {
        room: RoomId,
        peer: MemberId,
        error: AdmissionError,
    },
    Joined {
        room: RoomId,
        result: Result<Box<JoinOutcome>, EngineError>,
        reply: Reply,
    },
    /// Making an invite was waiting for our relay (or gave up on it).
    InviteReady {
        room: RoomId,
        reply: Reply,
    },
    Received {
        conn: ConnId,
        message: ControlMessage,
    },
    Closed {
        conn: ConnId,
    },
    /// Our stream has started (or failed to: see [`sessions`]).
    LiveReady {
        session: SessionId,
        stream: Box<HostStream>,
        events: mpsc::UnboundedReceiver<HostEvent>,
        reply: Reply,
    },
    /// Something our stream reports.
    Host(SessionId, HostEvent),
    /// Something the session we watch reports.
    Watch(SessionId, WatchEvent),
    /// The host we asked to watch hasn't answered in time.
    JoinTimeout(SessionId, u32),
    /// The connection to the host we watch hasn't come back in time (for
    /// that wait).
    ReconnectTimeout(SessionId, u32),
    /// The host's panic key, for the stream it was pressed during; `None`
    /// means ours, whichever it is (`Engine::panic_key`).
    Panic(Option<SessionId>),
    /// The host's Accept or Decline key, during this stream: answers the
    /// oldest request, if it has been pending long enough to be seen.
    AnswerFirst {
        session: SessionId,
        accept: bool,
    },
}

pub(crate) enum JoinAnswer {
    Proceed {
        token: InviteToken,
        password: PasswordKey,
    },
    Refuse(u32),
    /// Refuse, showing the newcomer the owner's policy that bans it.
    Banned(Box<Signed<RoomPolicy>>),
}

pub(crate) struct Admitted {
    room: RoomId,
    peer: MemberId,
    conn: Connection,
    tx: ControlSender,
    rx: ControlReceiver,
    dialed_by_me: bool,
    /// A newcomer the owner (us) just let in, with the invite it used.
    newcomer: Option<InviteTag>,
    /// A snapshot already read from the connection.
    snapshot: Option<RoomSnapshot>,
}

pub(crate) struct JoinOutcome {
    invite: Invite,
    admitted: Admitted,
    snapshot: RoomSnapshot,
}

enum Outgoing {
    Message(ControlMessage),
    /// Flush what was sent, then close with this code.
    Close(u32),
}

struct Entry {
    room: Room,
    /// The owner's password key; only the owner admits, so only it has one.
    password: Option<PasswordKey>,
}

struct Conn {
    room: RoomId,
    peer: MemberId,
    conn: Connection,
    out: mpsc::Sender<Outgoing>,
    writer: JoinHandle<()>,
    relayed: Option<bool>,
    /// The connection's one datagram reader and stream acceptor, for the
    /// sessions on it.
    datagrams: Datagrams,
    streams: Streams,
}

pub(crate) struct Started {
    pub member_id: MemberId,
    pub inbox: mpsc::Sender<Input>,
}

struct Actor {
    endpoint: Endpoint,
    /// Whether relays are configured: invites then never carry IP addresses.
    relays: bool,
    key: SigningKey,
    me: MemberId,
    name: String,
    addr: PeerAddr,
    rooms: BTreeMap<RoomId, Entry>,
    joining: BTreeSet<RoomId>,
    conns: HashMap<ConnId, Conn>,
    next_conn: ConnId,
    limits: Arc<Mutex<AdmissionLimits>>,
    store: Store,
    events: broadcast::Sender<Event>,
    inbox: mpsc::Sender<Input>,
    timings: Timings,
    tick: Duration,
    invite_valid_for: Duration,
    next_addr_check: Instant,
    next_path_check: Instant,
    backend: Arc<dyn Backend>,
    join_wait: Duration,
    replace_grace: Duration,
    reconnect_wait: Duration,
    /// The profile's folder: the viewer window logs there, and the user's
    /// shortcut keys are read from there when going live or watching.
    data_dir: std::path::PathBuf,
    /// Shared with the engine, for the panic key.
    panic: Arc<Mutex<Option<PanicHandle>>>,
    sessions: sessions::Sessions,
    /// Members whose build we can't talk to: their wire revision and app
    /// version, until a connection with them works again.
    other_versions: BTreeMap<MemberId, (u16, String)>,
}

fn lock(limits: &Mutex<AdmissionLimits>) -> MutexGuard<'_, AdmissionLimits> {
    limits.lock().unwrap_or_else(PoisonError::into_inner)
}

fn random<const N: usize>() -> Result<[u8; N], EngineError> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|e| EngineError::Random(e.to_string()))?;
    Ok(bytes)
}

/// An invite as shared: on our invite page when the build has one, which
/// chat apps make clickable (the payload stays after `#`, which browsers
/// never send to the page's server), or as a `couchco://` link.
pub(crate) fn invite_link(invite: &Invite, page: Option<&str>) -> String {
    match page {
        Some(page) => format!("{}#{}", page.trim_end_matches('#'), invite.payload()),
        None => invite.to_link(),
    }
}

/// Wall-clock seconds, for invite expiry.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn default_name(profile: &Profile) -> String {
    if profile.is_default() {
        "Player".to_string()
    } else {
        profile.name().to_string()
    }
}

/// Loads the identity, binds the endpoint, restores remembered rooms and
/// starts the actor and the accept loop.
pub(crate) async fn start(
    profile: &Profile,
    options: EngineOptions,
    events: broadcast::Sender<Event>,
    panic: Arc<Mutex<Option<PanicHandle>>>,
) -> Result<Started, EngineError> {
    let store = Store::new(profile.data_dir(), options.secret_store.clone());
    let secret_key = store.identity()?;
    let key = SigningKey::from_bytes(&secret_key.to_bytes());
    let me = addr::member_id(secret_key.public());
    let name = store
        .settings()
        .display_name
        .filter(|n| valid_name(n))
        .unwrap_or_else(|| default_name(profile));

    let limits = Arc::new(Mutex::new(AdmissionLimits::new(options.limits.clone())));
    let gate_limits = limits.clone();
    let gate = Gate(Arc::new(move |id| {
        lock(&gate_limits)
            .id_blocked_for(addr::member_id(id), Instant::now())
            .map(|_| close_code::RATE_LIMITED)
    }));
    let relays = !matches!(options.relays, RelayChoice::Disabled);
    let mut endpoint_options = EndpointOptions::new(secret_key, options.relays.clone());
    endpoint_options.gate = Some(gate);
    endpoint_options.port = options.port;
    endpoint_options.relay_only = options.relay_only;
    let endpoint = endpoint::bind(endpoint_options)
        .await
        .map_err(|e| EngineError::Net(e.to_string()))?;

    let (inbox, inbox_rx) = mpsc::channel(INBOX);
    let now = Instant::now();
    let mut actor = Actor {
        addr: addr::peer_addr(&endpoint.addr()),
        endpoint: endpoint.clone(),
        relays,
        key,
        me,
        name,
        rooms: BTreeMap::new(),
        joining: BTreeSet::new(),
        conns: HashMap::new(),
        next_conn: 1,
        limits,
        store,
        events,
        inbox: inbox.clone(),
        timings: options.timings,
        tick: options.tick,
        invite_valid_for: options.invite_valid_for,
        next_addr_check: now + ADDR_CHECK_EVERY,
        next_path_check: now + PATH_CHECK_EVERY,
        backend: options.backend,
        join_wait: options.join_wait,
        replace_grace: options.replace_grace,
        reconnect_wait: options.reconnect_wait,
        data_dir: profile.data_dir().to_path_buf(),
        panic,
        sessions: sessions::Sessions::default(),
        other_versions: BTreeMap::new(),
    };
    actor.restore_rooms(now);
    tokio::spawn(accept_loop(endpoint, inbox.clone(), me));
    tokio::spawn(actor.run(inbox_rx));
    Ok(Started {
        member_id: me,
        inbox,
    })
}

impl Actor {
    fn restore_rooms(&mut self, now: Instant) {
        for remembered in self.store.rooms() {
            let id = room_id(remembered.saved.owner, remembered.saved.nonce);
            match Room::restore(
                self.key.clone(),
                remembered.saved,
                self.timings.clone(),
                now,
            ) {
                Ok(mut room) => {
                    // Addresses change between runs; the name may have too.
                    if let Err(e) = room.set_profile(&self.name, self.addr.clone()) {
                        tracing::warn!("room {id:?}: {e}");
                    }
                    self.rooms.insert(
                        id,
                        Entry {
                            room,
                            password: remembered.password.map(PasswordKey),
                        },
                    );
                    self.save(id);
                }
                Err(RoomError::Banned | RoomError::NotMember) => {
                    let _ = self.store.forget_room(id);
                }
                Err(e) => tracing::warn!("not restoring room {id:?}: {e}"),
            }
        }
        tracing::info!(rooms = self.rooms.len(), "rooms restored");
    }

    async fn run(mut self, mut inbox: mpsc::Receiver<Input>) {
        let mut tick = tokio::time::interval(self.tick);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                input = inbox.recv() => match input {
                    Some(Input::Shutdown(done)) => {
                        self.shutdown().await;
                        let _ = done.send(());
                        return;
                    }
                    Some(input) => self.handle(input),
                    None => {
                        self.shutdown().await;
                        return;
                    }
                },
                _ = tick.tick() => self.tick(),
            }
        }
    }

    fn handle(&mut self, input: Input) {
        let now = Instant::now();
        match input {
            Input::Command(command, reply) => self.command(command, reply, now),
            Input::Connections(room, reply) => {
                let list = RoomId::from_hex(&room)
                    .and_then(|id| self.rooms.get(&id))
                    .map(|e| {
                        e.room
                            .connections()
                            .into_iter()
                            .map(|(peer, _, mine)| (peer.to_hex(), mine))
                            .collect()
                    })
                    .unwrap_or_default();
                let _ = reply.send(list);
            }
            Input::HomeRelay(reply) => {
                let _ = reply.send(self.addr.relay.clone());
            }
            Input::DropConnection(room, member) => {
                let conn = RoomId::from_hex(&room)
                    .zip(MemberId::from_hex(&member))
                    .and_then(|(room, member)| self.current_conn(room, member))
                    .and_then(|conn| self.conns.get(&conn));
                if let Some(c) = conn {
                    // As if the peer had gone silent.
                    admission::close(&c.conn, close_code::TIMED_OUT);
                }
            }
            Input::Shutdown(_) => unreachable!("handled by run"),
            Input::JoinQuery {
                room,
                dialer,
                invite,
                reply,
            } => {
                let answer = self.join_query(room, dialer, invite, now);
                let proceed = matches!(answer, JoinAnswer::Proceed { .. });
                if reply.send(answer).is_err() && proceed {
                    // Whoever asked is gone: the check never runs.
                    lock(&self.limits).cancel(dialer, room);
                    if let Some(entry) = self.rooms.get_mut(&room) {
                        entry.room.join_released(invite);
                    }
                }
            }
            Input::JoinFailed {
                room,
                dialer,
                invite,
            } => self.join_failed(room, dialer, invite, now),
            Input::MemberQuery {
                room,
                dialer,
                policy,
                reply,
            } => {
                let decision = match self.rooms.get_mut(&room) {
                    None => MemberDecision::Refuse {
                        code: close_code::UNKNOWN_ROOM,
                        proof: None,
                    },
                    Some(entry) => {
                        let (decision, fx) = entry.room.member_hello(dialer, *policy);
                        self.apply(room, fx, now);
                        decision
                    }
                };
                let _ = reply.send(decision);
            }
            Input::Admitted(admitted) => self.admitted(*admitted, now),
            Input::OtherVersion {
                member,
                minor,
                app_version,
            } => self.other_version(member, minor, app_version, now),
            Input::DialFailed { room, peer, error } => {
                tracing::debug!(room = ?room, peer = ?peer, "dial failed: {error}");
                if let AdmissionError::Incompatible { minor, app_version } = &error {
                    self.other_version(peer, *minor, app_version.clone(), now);
                }
                let Some(entry) = self.rooms.get_mut(&room) else {
                    return;
                };
                let fx = match error {
                    AdmissionError::Shown(policy) => {
                        entry
                            .room
                            .refused(peer, close_code::NOT_MEMBER, Some(*policy), now)
                    }
                    AdmissionError::Refused(code) => entry.room.refused(peer, code, None, now),
                    _ => {
                        entry.room.dial_failed(peer, now);
                        Vec::new()
                    }
                };
                self.apply(room, fx, now);
            }
            Input::Joined {
                room,
                result,
                reply,
            } => self.joined(room, result, reply, now),
            Input::InviteReady { room, reply } => {
                self.refresh_addr(now);
                let result = if self.addr.relay.is_none() {
                    Err(EngineError::NotOnline)
                } else {
                    self.make_invite(room, now)
                };
                let _ = reply.send(result);
            }
            Input::Received { conn, message } => {
                let Some(room) = self.conns.get(&conn).map(|c| c.room) else {
                    return;
                };
                if message.is_session() {
                    self.session_received(room, conn, message, now);
                    return;
                }
                if let Some(entry) = self.rooms.get_mut(&room) {
                    let fx = entry.room.received(conn, message, now);
                    self.apply(room, fx, now);
                }
            }
            Input::Closed { conn } => {
                let Some(closed) = self.conns.remove(&conn) else {
                    return;
                };
                admission::close(&closed.conn, close_code::NORMAL);
                if let Some(entry) = self.rooms.get_mut(&closed.room) {
                    let fx = entry.room.disconnected(conn, now);
                    self.apply(closed.room, fx, now);
                }
                self.session_closed(closed.room, conn, closed.peer, now);
            }
            Input::LiveReady {
                session,
                stream,
                events,
                reply,
            } => self.live_ready(session, *stream, events, reply, now),
            Input::Host(session, event) => self.host_event(session, event, now),
            Input::Watch(session, event) => self.watch_event(session, event, now),
            Input::JoinTimeout(session, attempt) => self.join_timeout(session, attempt, now),
            Input::ReconnectTimeout(session, attempt) => {
                self.reconnect_timeout(session, attempt, now);
            }
            Input::Panic(session) => self.panic_key(session, now),
            Input::AnswerFirst { session, accept } => self.answer_first(session, accept, now),
        }
    }

    fn command(&mut self, command: Command, reply: Reply, now: Instant) {
        let result = match command {
            // Answered by the engine itself.
            Command::GetInfo | Command::Ping { .. } | Command::TakeOpenedInvite => {
                Err(EngineError::Stopped)
            }
            Command::GetProfile => Ok(Response::Profile(self.profile_view())),
            Command::SetDisplayName { name } => self.set_display_name(name, now),
            Command::ListRooms => {
                let mut rooms: Vec<RoomView> = self
                    .rooms
                    .keys()
                    .filter_map(|id| self.view(*id, now))
                    .collect();
                rooms.sort_by_key(|r| r.name.to_lowercase());
                Ok(Response::Rooms { rooms })
            }
            Command::CreateRoom { name, password } => self.create(&name, &password, now),
            Command::PreviewInvite { invite } => self.preview(&invite, now),
            Command::JoinRoom { invite, password } => {
                // Answers later, from `joined`.
                self.join(&invite, &password, reply);
                return;
            }
            Command::GetInvite { room } => match self.owned(&room) {
                Ok(id) => {
                    // May answer later, once the relay is known.
                    self.invite(id, reply, now);
                    return;
                }
                Err(e) => Err(e),
            },
            Command::ChangePassword { room, password } => self.change_password(&room, &password),
            Command::RevokeInvites { room } => self.revoke_invites(&room, now),
            Command::LeaveRoom { room } => self.leave(&room, now),
            Command::BanMember { room, member } => self.ban(&room, &member, now),
            // Answered by the engine itself.
            Command::GetShortcuts
            | Command::SetShortcut { .. }
            | Command::ResetShortcuts { .. }
            | Command::GetNetwork
            | Command::SetNetwork { .. }
            | Command::GetDrivers
            | Command::InstallDriver { .. }
            | Command::ListSources => Err(EngineError::Stopped),
            Command::GoLive {
                room,
                source,
                title,
                fps,
                max_mbps,
                upload_mbps,
                max_viewers,
                input,
                controller,
            } => {
                let go = sessions::GoLive {
                    room,
                    source,
                    title,
                    fps,
                    max_mbps,
                    upload_mbps,
                    max_viewers,
                    input,
                    controller,
                };
                // Answers later, once the stream has started.
                if let Err(e) = self.go_live(go, reply, now) {
                    tracing::info!("not going live: {e}");
                }
                return;
            }
            Command::StopLive => self.stop_live(now),
            Command::Watch { room, host } => {
                // Answers later, once the host has.
                self.watch(&room, &host, reply, now);
                return;
            }
            Command::StopWatching => self.stop_watching(now),
            Command::RequestControl { seat } => self.request_control(SeatId(seat), now),
            Command::CancelControl => self.cancel_control(now),
            Command::AnswerControl { member, accept } => self.answer_control(&member, accept, now),
            Command::PassControl { member, seat } => self.pass_control(&member, SeatId(seat), now),
            Command::TakeBackControl { seat } => self.take_back_control(seat.map(SeatId), now),
            Command::PlaySeat { seat } => self.play_seat(SeatId(seat), now),
            Command::AllowControl => self.allow_control(now),
        };
        let _ = reply.send(result);
    }

    fn entry_mut(&mut self, room: &str) -> Result<(RoomId, &mut Entry), EngineError> {
        let id = RoomId::from_hex(room).ok_or(EngineError::UnknownRoom)?;
        let entry = self
            .rooms
            .get_mut(&id)
            .filter(|e| !e.room.is_leaving())
            .ok_or(EngineError::UnknownRoom)?;
        Ok((id, entry))
    }

    /// A room we own.
    fn owned(&self, room: &str) -> Result<RoomId, EngineError> {
        let id = RoomId::from_hex(room).ok_or(EngineError::UnknownRoom)?;
        let entry = self
            .rooms
            .get(&id)
            .filter(|e| !e.room.is_leaving())
            .ok_or(EngineError::UnknownRoom)?;
        if entry.room.is_owner() {
            Ok(id)
        } else {
            Err(RoomError::NotOwner.into())
        }
    }

    fn identity(&self) -> Identity {
        Identity {
            key: self.key.clone(),
            name: self.name.clone(),
            addr: self.addr.clone(),
        }
    }

    fn profile_view(&self) -> ProfileView {
        ProfileView {
            display_name: self.name.clone(),
            member_id: self.me.to_hex(),
        }
    }

    fn set_display_name(&mut self, name: String, now: Instant) -> Result<Response, EngineError> {
        let name = name.trim().to_string();
        if !valid_name(&name) {
            return Err(RoomError::InvalidName.into());
        }
        self.store.save_settings(&Settings {
            display_name: Some(name.clone()),
        })?;
        self.name = name;
        self.publish_profile(now);
        Ok(Response::Profile(self.profile_view()))
    }

    /// Our name or addresses changed: every room re-signs our record.
    fn publish_profile(&mut self, now: Instant) {
        let ids: Vec<RoomId> = self.rooms.keys().copied().collect();
        for id in ids {
            let Some(entry) = self.rooms.get_mut(&id) else {
                continue;
            };
            match entry.room.set_profile(&self.name, self.addr.clone()) {
                Ok(fx) => self.apply(id, fx, now),
                Err(e) => tracing::warn!("room {id:?}: {e}"),
            }
        }
    }

    /// Re-reads our addresses, and announces them if they changed.
    fn refresh_addr(&mut self, now: Instant) {
        let addr = addr::peer_addr(&self.endpoint.addr());
        if addr != self.addr {
            tracing::debug!(?addr, "our addresses changed");
            self.addr = addr;
            self.publish_profile(now);
        }
    }

    fn create(
        &mut self,
        name: &str,
        password: &str,
        now: Instant,
    ) -> Result<Response, EngineError> {
        if password.trim().is_empty() {
            return Err(EngineError::EmptyPassword);
        }
        let room = Room::create(
            self.identity(),
            RoomNonce(random()?),
            name.trim(),
            self.timings.clone(),
            now,
        )?;
        let id = room.id();
        self.rooms.insert(
            id,
            Entry {
                room,
                password: Some(PasswordKey::derive(id, password)),
            },
        );
        self.save(id);
        tracing::info!(room = ?id, "created a room");
        self.announce(id, now);
        Ok(Response::Room(self.view(id, now).expect("just created")))
    }

    /// Answers with a new invite. With relays configured, invites carry no
    /// IP addresses, so ours must name our relay: if it isn't known yet
    /// (just after startup), wait for it.
    fn invite(&mut self, room: RoomId, reply: Reply, now: Instant) {
        if !self.relays || self.addr.relay.is_some() {
            let _ = reply.send(self.make_invite(room, now));
            return;
        }
        let endpoint = self.endpoint.clone();
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(INVITE_WAIT, endpoint.online()).await;
            let _ = inbox.send(Input::InviteReady { room, reply }).await;
        });
    }

    fn make_invite(&mut self, room: RoomId, now: Instant) -> Result<Response, EngineError> {
        let token = InviteToken(random()?);
        let allow_ips = !self.relays;
        let valid_for = self.invite_valid_for.as_secs();
        let entry = self.rooms.get_mut(&room).ok_or(EngineError::UnknownRoom)?;
        let (invite, fx) = entry
            .room
            .create_invite(token, unix_now(), valid_for, allow_ips)?;
        self.apply(room, fx, now);
        Ok(Response::Invite {
            link: invite_link(&invite, crate::build_config::invite_page()),
        })
    }

    fn change_password(&mut self, room: &str, password: &str) -> Result<Response, EngineError> {
        if password.trim().is_empty() {
            return Err(EngineError::EmptyPassword);
        }
        let id = self.owned(room)?;
        let entry = self.rooms.get_mut(&id).expect("owned");
        let old = entry.password.replace(PasswordKey::derive(id, password));
        // Saying "changed" when the old password would be back after a
        // restart is worse than an error.
        if let Err(e) = self.try_save(id) {
            self.rooms.get_mut(&id).expect("owned").password = old;
            return Err(e.into());
        }
        tracing::info!(room = ?id, "changed the room password");
        Ok(Response::Done)
    }

    fn revoke_invites(&mut self, room: &str, now: Instant) -> Result<Response, EngineError> {
        let id = self.owned(room)?;
        let fx = self
            .rooms
            .get_mut(&id)
            .expect("owned")
            .room
            .revoke_invites()?;
        tracing::info!(room = ?id, "revoked every invite");
        self.apply(id, fx, now);
        Ok(Response::Done)
    }

    fn preview(&self, invite: &str, now: Instant) -> Result<Response, EngineError> {
        let invite = Invite::parse(invite)?;
        let room = room_id(invite.owner, invite.nonce);
        let expires_in = invite.expires.saturating_sub(unix_now());
        let status = if invite.owner == self.me {
            InviteStatus::Own
        } else if self.view(room, now).is_some() {
            InviteStatus::Member
        } else if expires_in == 0 {
            InviteStatus::Expired
        } else {
            InviteStatus::Ready
        };
        Ok(Response::InvitePreview(InvitePreview {
            room: room.to_hex(),
            name: invite.name,
            status,
            expires_in_secs: u32::try_from(expires_in).unwrap_or(u32::MAX),
        }))
    }

    fn join(&mut self, invite: &str, password: &str, reply: Reply) {
        let invite = match Invite::parse(invite) {
            Ok(invite) => invite,
            Err(e) => {
                let _ = reply.send(Err(e.into()));
                return;
            }
        };
        if password.trim().is_empty() {
            let _ = reply.send(Err(EngineError::EmptyPassword));
            return;
        }
        let id = room_id(invite.owner, invite.nonce);
        // A room we are still leaving counts as gone: joining replaces it.
        if let Some(view) = self.view(id, Instant::now()) {
            let _ = reply.send(Ok(Response::Room(view)));
            return;
        }
        if invite.owner == self.me {
            let _ = reply.send(Err(EngineError::Refused("this is your own room's invite")));
            return;
        }
        if !self.joining.insert(id) {
            let _ = reply.send(Err(EngineError::Refused("already joining this room")));
            return;
        }
        let job = JoinJob {
            endpoint: self.endpoint.clone(),
            password: PasswordKey::derive(id, password),
            invite,
            room: id,
            me: self.me,
        };
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            let result = job.run().await;
            let _ = inbox
                .send(Input::Joined {
                    room: id,
                    result,
                    reply,
                })
                .await;
        });
    }

    fn joined(
        &mut self,
        id: RoomId,
        result: Result<Box<JoinOutcome>, EngineError>,
        reply: Reply,
        now: Instant,
    ) {
        self.joining.remove(&id);
        let outcome = match result {
            Ok(outcome) => *outcome,
            Err(e) => {
                tracing::info!(room = ?id, "join failed: {e}");
                let _ = reply.send(Err(e));
                return;
            }
        };
        let JoinOutcome {
            invite,
            admitted,
            snapshot,
        } = outcome;
        if let Some(view) = self.view(id, now) {
            admission::close(&admitted.conn, close_code::DUPLICATE);
            let _ = reply.send(Ok(Response::Room(view)));
            return;
        }
        let room = match Room::join(
            self.identity(),
            invite.owner,
            invite.nonce,
            snapshot,
            self.timings.clone(),
            now,
        ) {
            Ok(room) => room,
            Err(e) => {
                admission::close(&admitted.conn, close_code::LEFT_ROOM);
                let _ = reply.send(Err(match e {
                    RoomError::Banned => EngineError::Refused("you are banned from this room"),
                    other => other.into(),
                }));
                return;
            }
        };
        tracing::info!(room = ?id, "joined a room");
        self.rooms.insert(
            id,
            Entry {
                room,
                password: None,
            },
        );
        self.save(id);
        self.admitted(admitted, now);
        self.announce(id, now);
        let _ = reply.send(Ok(Response::Room(self.view(id, now).expect("joined"))));
    }

    /// The room disappears from the UI at once. Until the others have heard
    /// (one member for a member leaving, everyone for the owner closing the
    /// room), it stays behind, hidden, and keeps trying to reach them.
    fn leave(&mut self, room: &str, now: Instant) -> Result<Response, EngineError> {
        let (id, _) = self.entry_mut(room)?;
        // Sessions end first, so their last words go out before the goodbyes.
        self.end_room_sessions(id, now);
        let (id, entry) = self.entry_mut(room)?;
        let fx = entry.room.leave(now)?;
        tracing::info!(room = ?id, "left a room");
        self.apply(id, fx, now);
        let _ = self.events.send(Event::RoomRemoved {
            room: id.to_hex(),
            reason: RoomGone::Left,
        });
        Ok(Response::Done)
    }

    /// Anyone but us, in the room now or not: a member who left, or someone
    /// who never got in, stays out too.
    fn ban(&mut self, room: &str, member: &str, now: Instant) -> Result<Response, EngineError> {
        let member = MemberId::from_hex(member).ok_or(EngineError::UnknownMember)?;
        let id = self.owned(room)?;
        let entry = self.rooms.get_mut(&id).expect("owned");
        let fx = entry.room.ban(member)?;
        tracing::info!(room = ?id, member = ?member, "banned a member");
        self.apply(id, fx, now);
        Ok(Response::Room(self.view(id, now).expect("present")))
    }

    fn join_query(
        &mut self,
        room: RoomId,
        dialer: MemberId,
        invite: InviteTag,
        now: Instant,
    ) -> JoinAnswer {
        let mut limits = lock(&self.limits);
        if limits.id_blocked_for(dialer, now).is_some() {
            return JoinAnswer::Refuse(close_code::RATE_LIMITED);
        }
        let Some(entry) = self.rooms.get_mut(&room) else {
            return JoinAnswer::Refuse(close_code::UNKNOWN_ROOM);
        };
        let Some(password) = entry.password else {
            // Only the owner admits, and only the owner has the key.
            return JoinAnswer::Refuse(close_code::PROTOCOL);
        };
        match entry.room.join_request(dialer, invite, unix_now()) {
            JoinDecision::Proceed { token } => {
                // Counted from here: a check that doesn't end in admission
                // is a failure, however it ends.
                if limits.begin(dialer, room, entry.room.is_member(dialer), now) {
                    JoinAnswer::Proceed { token, password }
                } else {
                    entry.room.join_released(invite);
                    JoinAnswer::Refuse(close_code::RATE_LIMITED)
                }
            }
            JoinDecision::Refuse(code) => JoinAnswer::Refuse(code),
            JoinDecision::Banned(proof) => JoinAnswer::Banned(proof),
        }
    }

    fn join_failed(&mut self, room: RoomId, dialer: MemberId, invite: InviteTag, now: Instant) {
        tracing::info!(room = ?room, dialer = ?dialer, "a password check failed");
        let paused = lock(&self.limits).end(dialer, room, false, now);
        if let Some(entry) = self.rooms.get_mut(&room) {
            let fx = entry.room.join_failed(invite);
            self.apply(room, fx, now);
        }
        if let Some(paused) = paused {
            let minutes = paused
                .until
                .saturating_duration_since(now)
                .as_secs()
                .div_ceil(60);
            tracing::warn!(room = ?room, minutes, "too many wrong passwords: joins paused");
            let _ = self.events.send(Event::JoinsPaused {
                room: room.to_hex(),
                minutes: u32::try_from(minutes).unwrap_or(u32::MAX),
            });
            self.announce(room, now);
        }
    }

    fn admitted(&mut self, admitted: Admitted, now: Instant) {
        let Admitted {
            room,
            peer,
            conn,
            tx,
            rx,
            dialed_by_me,
            newcomer,
            snapshot,
        } = admitted;
        // We talk again: whatever version it ran, it matches ours now.
        self.other_versions.remove(&peer);
        if newcomer.is_some() {
            // The password was right, whatever happens next.
            lock(&self.limits).end(peer, room, true, now);
        }
        let Some(entry) = self.rooms.get_mut(&room) else {
            admission::close(&conn, close_code::UNKNOWN_ROOM);
            return;
        };
        if let Some(invite) = newcomer {
            // On the roster before the connection is reported, so the
            // snapshot it gets lists it.
            match entry.room.admit(peer, invite, unix_now()) {
                Ok(fx) => {
                    tracing::info!(room = ?room, member = ?peer, "let a newcomer in");
                    self.apply(room, fx, now);
                }
                Err(code) => {
                    // Something changed during the check: a ban, the invite
                    // revoked or expired, the room filled up.
                    tracing::info!(
                        room = ?room,
                        member = ?peer,
                        "not letting a newcomer in after all: {}",
                        admission::close_name(code)
                    );
                    admission::close(&conn, code);
                    return;
                }
            }
        }
        let id = self.next_conn;
        self.next_conn += 1;
        let (out, out_rx) = mpsc::channel(OUT_QUEUE);
        let writer = tokio::spawn(writer(conn.clone(), tx, out_rx));
        tokio::spawn(reader(id, conn.weak_handle(), rx, self.inbox.clone()));
        let relayed = Some(couch_net::video::selected_path(&conn).relayed);
        self.conns.insert(
            id,
            Conn {
                room,
                peer,
                datagrams: Datagrams::start(conn.clone()),
                streams: Streams::start(conn.clone()),
                conn,
                out,
                writer,
                relayed,
            },
        );
        let Some(entry) = self.rooms.get_mut(&room) else {
            return;
        };
        let mut fx = entry.room.connected(peer, id, dialed_by_me);
        if let Some(snapshot) = snapshot {
            fx.extend(entry.room.received(
                id,
                ControlMessage::RoomSnapshot(Box::new(snapshot)),
                now,
            ));
        }
        self.apply(room, fx, now);
        // After the room's snapshot, on the same writer.
        self.session_connected(room, peer, id, now);
    }

    /// Carries out a room's effects.
    fn apply(&mut self, room: RoomId, effects: Vec<Effect>, now: Instant) {
        let mut over = None;
        let mut save = false;
        for effect in &effects {
            match effect {
                Effect::Save => save = true,
                Effect::Removed { banned } => over = Some(Some(*banned)),
                Effect::Departed => over = Some(None),
                _ => {}
            }
        }
        // On disk before anything goes out, so a policy or record version
        // that was sent is never lost in a crash and then reused.
        if save && over.is_none() {
            self.save(room);
        }
        let mut changed = false;
        for effect in effects {
            match effect {
                Effect::Dial { peer, addr } => self.spawn_dial(room, peer, addr),
                Effect::Send { conn, message } => self.send(conn, Outgoing::Message(message)),
                Effect::Close { conn, code } => self.send(conn, Outgoing::Close(code)),
                Effect::Changed => changed = true,
                Effect::Save | Effect::Removed { .. } | Effect::Departed => {}
            }
        }
        match over {
            Some(Some(banned)) => {
                let reason = if banned {
                    tracing::warn!(room = ?room, "the owner banned us from this room");
                    RoomGone::Banned
                } else {
                    tracing::info!(room = ?room, "the owner removed us or closed the room");
                    RoomGone::Removed
                };
                self.forget(room, Some(reason));
            }
            Some(None) => {
                // The UI was told when we left.
                tracing::info!(room = ?room, "the others know we left; forgetting the room");
                self.forget(room, None);
            }
            None => {
                // Someone the room no longer counts (banned, gone) loses
                // its part in sessions now, not when its connection closes.
                self.prune_sessions(room, now);
                if changed {
                    self.announce(room, now);
                }
            }
        }
    }

    fn send(&self, conn: ConnId, item: Outgoing) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        let close = match item {
            Outgoing::Close(code) => Some(code),
            Outgoing::Message(_) => None,
        };
        match c.out.try_send(item) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(conn, "a peer stopped reading; closing its connection");
                admission::close(&c.conn, close.unwrap_or(close_code::PROTOCOL));
                return;
            }
            // The writer is gone: the connection is closing, `Closed` follows.
            Err(mpsc::error::TrySendError::Closed(_)) => return,
        }
        if let Some(code) = close {
            // The writer flushes first, but a peer that stopped reading would
            // hold it there: the close happens regardless after a deadline.
            // A weak handle, so the timer never keeps a connection (or, at
            // shutdown, the endpoint's sockets) alive.
            let conn = c.conn.weak_handle();
            tokio::spawn(async move {
                tokio::time::sleep(CLOSE_DEADLINE).await;
                if let Some(conn) = conn.upgrade() {
                    admission::close(&conn, code);
                }
            });
        }
    }

    fn spawn_dial(&self, room: RoomId, peer: MemberId, peer_addr: PeerAddr) {
        let Some(entry) = self.rooms.get(&room) else {
            return;
        };
        let inbox = self.inbox.clone();
        let Some(target) = addr::endpoint_addr(peer, &peer_addr) else {
            tokio::spawn(async move {
                let error = AdmissionError::Protocol("not a valid member id".into());
                let _ = inbox.send(Input::DialFailed { room, peer, error }).await;
            });
            return;
        };
        let endpoint = self.endpoint.clone();
        let policy = entry.room.policy().clone();
        tokio::spawn(async move {
            let input = match dial_member(&endpoint, target, room, policy).await {
                Ok((conn, tx, rx, snapshot)) => Input::Admitted(Box::new(Admitted {
                    room,
                    peer,
                    conn,
                    tx,
                    rx,
                    dialed_by_me: true,
                    newcomer: None,
                    snapshot: Some(snapshot),
                })),
                Err(error) => Input::DialFailed { room, peer, error },
            };
            let _ = inbox.send(input).await;
        });
    }

    fn save(&self, room: RoomId) {
        if let Err(e) = self.try_save(room) {
            tracing::error!("could not save room {room:?}: {e}");
        }
    }

    fn try_save(&self, room: RoomId) -> Result<(), StoreError> {
        let Some(entry) = self.rooms.get(&room) else {
            return Ok(());
        };
        let remembered = RememberedRoom {
            saved: entry.room.saved(),
            password: entry.password.map(|p| p.0),
        };
        self.store.save_room(room, &remembered)
    }

    /// Drops the room for good; tells the UI why, unless it already knows.
    fn forget(&mut self, room: RoomId, reason: Option<RoomGone>) {
        self.end_room_sessions(room, Instant::now());
        self.rooms.remove(&room);
        if let Err(e) = self.store.forget_room(room) {
            tracing::error!("could not forget room {room:?}: {e}");
        }
        if let Some(reason) = reason {
            let _ = self.events.send(Event::RoomRemoved {
                room: room.to_hex(),
                reason,
            });
        }
    }

    /// `member` runs a build we can't talk to: remembered until a
    /// connection with it works again, and shown in every room we share.
    /// Anyone can dial us with any version, so only a member of a room we
    /// share is remembered: a stranger leaves nothing behind.
    fn other_version(&mut self, member: MemberId, minor: u16, app_version: String, now: Instant) {
        let shared: Vec<RoomId> = self
            .rooms
            .iter()
            .filter(|(_, e)| e.room.status().members.iter().any(|m| m.id == member))
            .map(|(id, _)| *id)
            .collect();
        if shared.is_empty() {
            return;
        }
        let seen = (minor, app_version);
        if self.other_versions.get(&member) == Some(&seen) {
            return;
        }
        tracing::info!(member = ?member, "a member runs Couch Co-App {}, wire revision {minor}: we can't talk", seen.1);
        self.other_versions.insert(member, seen);
        for room in shared {
            self.announce(room, now);
        }
    }

    fn announce(&self, room: RoomId, now: Instant) {
        if let Some(view) = self.view(room, now) {
            let _ = self.events.send(Event::RoomChanged { room: view });
        }
    }

    /// What the UI shows of a room; nothing for one we are leaving.
    fn view(&self, room: RoomId, now: Instant) -> Option<RoomView> {
        let entry = self.rooms.get(&room).filter(|e| !e.room.is_leaving())?;
        let status = entry.room.status();
        let conns: HashMap<MemberId, ConnId> = entry
            .room
            .connections()
            .into_iter()
            .map(|(peer, conn, _)| (peer, conn))
            .collect();
        let members = status
            .members
            .into_iter()
            .map(|m| MemberView {
                id: m.id.to_hex(),
                name: m.name,
                is_owner: m.is_owner,
                link: match m.link {
                    LinkState::Me => MemberLink::Me,
                    LinkState::Connected => {
                        let relayed = conns
                            .get(&m.id)
                            .and_then(|c| self.conns.get(c))
                            .and_then(|c| c.relayed);
                        if relayed == Some(true) {
                            MemberLink::Relay
                        } else {
                            MemberLink::Direct
                        }
                    }
                    LinkState::Online => MemberLink::Online,
                    LinkState::Offline => MemberLink::Offline,
                },
                // Only while we can't talk: a working connection proves the
                // versions match again.
                other_version: (m.link != LinkState::Connected)
                    .then(|| self.other_versions.get(&m.id))
                    .flatten()
                    .map(|(minor, app_version)| OtherVersion {
                        app_version: app_version.clone(),
                        newer: *minor > couch_proto::PROTOCOL_MINOR,
                    }),
            })
            .collect();
        let paused = lock(&self.limits)
            .room_paused_until(room, now)
            .map(|until| {
                u32::try_from(until.saturating_duration_since(now).as_secs() + 1)
                    .unwrap_or(u32::MAX)
            });
        Some(RoomView {
            id: room.to_hex(),
            name: status.name,
            is_owner: entry.room.is_owner(),
            members,
            joins_paused_for_secs: paused,
            sessions: self.session_views(room),
        })
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if now >= self.next_addr_check {
            self.next_addr_check = now + ADDR_CHECK_EVERY;
            self.refresh_addr(now);
        }
        if now >= self.next_path_check {
            self.next_path_check = now + PATH_CHECK_EVERY;
            let mut changed = BTreeSet::new();
            for conn in self.conns.values_mut() {
                let relayed = Some(couch_net::video::selected_path(&conn.conn).relayed);
                if relayed != conn.relayed {
                    conn.relayed = relayed;
                    changed.insert(conn.room);
                }
            }
            for room in changed {
                self.announce(room, now);
            }
        }
        let ids: Vec<RoomId> = self.rooms.keys().copied().collect();
        for id in ids {
            if let Some(entry) = self.rooms.get_mut(&id) {
                let fx = entry.room.tick(now);
                self.apply(id, fx, now);
            }
        }
    }

    /// Says goodbye on every connection, lets the writers flush, then closes
    /// the endpoint.
    async fn shutdown(&mut self) {
        let now = Instant::now();
        // Our stream first: everything a guest holds is released before the
        // connections go.
        self.end_sessions_for_shutdown(now).await;
        let ids: Vec<RoomId> = self.rooms.keys().copied().collect();
        for id in ids {
            let fx = self.rooms[&id].room.shutdown();
            self.apply(id, fx, now);
        }
        let writers: Vec<JoinHandle<()>> = self.conns.drain().map(|(_, c)| c.writer).collect();
        let _ = tokio::time::timeout(FLUSH_WAIT + Duration::from_millis(500), async {
            for writer in writers {
                let _ = writer.await;
            }
        })
        .await;
        self.endpoint.close().await;
    }
}

/// Dials a member and introduces us; returns its snapshot once it accepts.
async fn dial_member(
    endpoint: &Endpoint,
    target: couch_net::EndpointAddr,
    room: RoomId,
    policy: Signed<RoomPolicy>,
) -> Result<(Connection, ControlSender, ControlReceiver, RoomSnapshot), AdmissionError> {
    let (conn, mut tx, mut rx) = admission::connect(endpoint, target, CONNECT_TIMEOUT).await?;
    let hello = admission::hello_member(&conn, &mut tx, &mut rx, room, policy);
    match tokio::time::timeout(admission::TIMEOUT, hello).await {
        Ok(Ok(snapshot)) => Ok((conn, tx, rx, snapshot)),
        Ok(Err(e)) => Err(e),
        Err(_) => {
            admission::close(&conn, close_code::NORMAL);
            Err(AdmissionError::Timeout)
        }
    }
}

/// One join: the password check with the room's owner, the only member who
/// admits newcomers.
struct JoinJob {
    endpoint: Endpoint,
    invite: Invite,
    room: RoomId,
    password: PasswordKey,
    me: MemberId,
}

impl JoinJob {
    async fn run(self) -> Result<Box<JoinOutcome>, EngineError> {
        let owner = self.invite.owner;
        let Some(target) = addr::endpoint_addr(owner, &self.invite.addr) else {
            return Err(EngineError::Refused(
                "this invite is damaged; ask for a new one",
            ));
        };
        let attempt = async {
            let (conn, mut tx, mut rx) =
                admission::connect(&self.endpoint, target, CONNECT_TIMEOUT).await?;
            let checked = tokio::time::timeout(admission::TIMEOUT, async {
                let terms = JoinTerms {
                    room: self.room,
                    token: &self.invite.token,
                    invite: invite_tag(&self.invite.token),
                    password: &self.password,
                    me: self.me,
                    owner,
                };
                admission::join(&conn, &mut tx, &mut rx, terms).await?;
                // Admitted: the owner sends the room as it now stands.
                loop {
                    match rx.recv().await {
                        Ok(Some(ControlMessage::RoomSnapshot(snapshot))) => return Ok(*snapshot),
                        Ok(Some(_)) => continue,
                        _ => return Err(admission::closed(&conn)),
                    }
                }
            })
            .await;
            match checked {
                Ok(Ok(snapshot)) => Ok((conn, tx, rx, snapshot)),
                Ok(Err(e)) => Err(e),
                Err(_) => {
                    admission::close(&conn, close_code::NORMAL);
                    Err(AdmissionError::Timeout)
                }
            }
        };
        match attempt.await {
            Ok((conn, tx, rx, snapshot)) => Ok(Box::new(JoinOutcome {
                admitted: Admitted {
                    room: self.room,
                    peer: owner,
                    conn,
                    tx,
                    rx,
                    dialed_by_me: true,
                    newcomer: None,
                    snapshot: None,
                },
                invite: self.invite,
                snapshot,
            })),
            Err(e) => Err(self.join_error(e)),
        }
    }

    fn join_error(&self, error: AdmissionError) -> EngineError {
        match error {
            AdmissionError::WrongPassword
            | AdmissionError::Refused(close_code::ADMISSION_FAILED) => EngineError::WrongPassword,
            AdmissionError::Shown(policy)
                if proves_ban(&policy, self.room, self.invite.owner, self.me) =>
            {
                EngineError::Refused("you are banned from this room")
            }
            AdmissionError::Refused(close_code::BANNED) => {
                EngineError::Refused("you are banned from this room")
            }
            AdmissionError::Refused(close_code::INVITE_EXPIRED) => EngineError::Refused(
                "this invite has expired or was revoked; ask the room's owner for a new one",
            ),
            AdmissionError::Refused(close_code::ROOM_FULL) => {
                EngineError::Refused("this room is full")
            }
            AdmissionError::Refused(close_code::RATE_LIMITED) => {
                EngineError::Refused("too many attempts; try again in a few minutes")
            }
            AdmissionError::Refused(close_code::UNKNOWN_ROOM) => {
                EngineError::Refused("the owner has closed this room")
            }
            AdmissionError::Incompatible { minor, app_version } => EngineError::OtherVersion {
                app_version,
                newer: minor > couch_proto::PROTOCOL_MINOR,
            },
            // Closed for its version before we read the owner's `Hello`
            // (a slow path): which side is newer is unknown.
            AdmissionError::Refused(close_code::VERSION) => EngineError::Refused(
                "the room's owner runs another version of Couch Co-App; one of you needs to install the newer one",
            ),
            _ => EngineError::OwnerOffline,
        }
    }
}

async fn accept_loop(endpoint: Endpoint, inbox: mpsc::Sender<Input>, me: MemberId) {
    let permits = Arc::new(Semaphore::new(MAX_PENDING_ADMISSIONS));
    while let Some(incoming) = endpoint.accept().await {
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            incoming.refuse();
            continue;
        };
        let inbox = inbox.clone();
        tokio::spawn(async move {
            accept_one(incoming, inbox, me).await;
            drop(permit);
        });
    }
}

async fn accept_one(incoming: Incoming, inbox: mpsc::Sender<Input>, me: MemberId) {
    let Ok(accepting) = incoming.accept() else {
        return;
    };
    // Connections the gate refused end here.
    let Ok(conn) = accepting.await else {
        return;
    };
    let dialer = addr::member_id(conn.remote_id());
    // What the dialer wants, within the admission timeout. The actor is
    // asked outside it, so an answer is never lost to the timeout.
    let request = tokio::time::timeout(admission::TIMEOUT, async {
        let (tx, mut rx, _hello) = control::accept(&conn).await.map_err(|e| match e {
            NetError::Incompatible { minor, app_version } => {
                admission::close(&conn, close_code::VERSION);
                AdmissionError::Incompatible { minor, app_version }
            }
            other => AdmissionError::Connection(other.to_string()),
        })?;
        let request = admission::read_request(&conn, &mut rx).await?;
        Ok::<_, AdmissionError>((tx, rx, request))
    })
    .await;
    let (mut tx, mut rx, request) = match request {
        Ok(Ok(request)) => request,
        // Before anything about any room: only who called, and their version.
        Ok(Err(AdmissionError::Incompatible { minor, app_version })) => {
            // Debug: anyone can dial; a member is logged when remembered.
            tracing::debug!(dialer = ?dialer, "refused: Couch Co-App {app_version}, wire revision {minor}");
            let _ = inbox
                .send(Input::OtherVersion {
                    member: dialer,
                    minor,
                    app_version,
                })
                .await;
            return;
        }
        Ok(Err(e)) => {
            tracing::debug!(dialer = ?dialer, "not admitted: {e}");
            return;
        }
        Err(_) => {
            admission::close(&conn, close_code::ADMISSION_FAILED);
            return;
        }
    };
    let admitted = match request {
        Request::Member(hello) => answer_member(&conn, &mut tx, &inbox, dialer, *hello).await,
        Request::Join(start) => {
            answer_join(&conn, &mut tx, &mut rx, &inbox, me, dialer, start).await
        }
    };
    if let Some((room, newcomer)) = admitted {
        let admitted = Admitted {
            room,
            peer: dialer,
            conn,
            tx,
            rx,
            dialed_by_me: false,
            newcomer,
            snapshot: None,
        };
        let _ = inbox.send(Input::Admitted(Box::new(admitted))).await;
    }
}

/// A member's `MemberHello`: the room's answer. Returns the room if it lets
/// the member in.
async fn answer_member(
    conn: &Connection,
    tx: &mut ControlSender,
    inbox: &mpsc::Sender<Input>,
    dialer: MemberId,
    hello: MemberHello,
) -> Option<(RoomId, Option<InviteTag>)> {
    let room = hello.room;
    let (reply, answer) = oneshot::channel();
    let query = Input::MemberQuery {
        room,
        dialer,
        policy: Box::new(hello.policy),
        reply,
    };
    inbox.send(query).await.ok()?;
    match answer.await.ok()? {
        MemberDecision::Accept => Some((room, None)),
        MemberDecision::Refuse { code, proof } => {
            match proof {
                Some(proof) => admission::refuse_with(conn, tx, *proof, code).await,
                None => admission::close(conn, code),
            }
            None
        }
    }
}

/// A newcomer's `JoinStart`: the owner's decision, then the password check
/// (with its own timeout). Once the check starts, anything but admission is
/// reported as a failure, so the rate limits and the invite count it.
async fn answer_join(
    conn: &Connection,
    tx: &mut ControlSender,
    rx: &mut ControlReceiver,
    inbox: &mpsc::Sender<Input>,
    me: MemberId,
    dialer: MemberId,
    start: JoinStart,
) -> Option<(RoomId, Option<InviteTag>)> {
    let (room, invite) = (start.room, start.invite);
    let (reply, answer) = oneshot::channel();
    let query = Input::JoinQuery {
        room,
        dialer,
        invite,
        reply,
    };
    inbox.send(query).await.ok()?;
    match answer.await.ok()? {
        JoinAnswer::Refuse(code) => {
            admission::close(conn, code);
            None
        }
        JoinAnswer::Banned(proof) => {
            admission::refuse_with(conn, tx, *proof, close_code::BANNED).await;
            None
        }
        JoinAnswer::Proceed { token, password } => {
            let terms = AcceptTerms {
                token: &token,
                password: &password,
                me,
            };
            let check = admission::accept_join(conn, tx, rx, &start, terms);
            match tokio::time::timeout(admission::TIMEOUT, check).await {
                Ok(Ok(())) => return Some((room, Some(invite))),
                Ok(Err(e)) => tracing::debug!(dialer = ?dialer, "password check failed: {e}"),
                Err(_) => admission::close(conn, close_code::ADMISSION_FAILED),
            }
            let failed = Input::JoinFailed {
                room,
                dialer,
                invite,
            };
            let _ = inbox.send(failed).await;
            None
        }
    }
}

async fn reader(
    id: ConnId,
    conn: WeakConnectionHandle,
    mut rx: ControlReceiver,
    inbox: mpsc::Sender<Input>,
) {
    loop {
        let code = match rx.recv().await {
            Ok(Some(message)) => {
                let received = Input::Received { conn: id, message };
                if inbox.send(received).await.is_err() {
                    return;
                }
                continue;
            }
            Ok(None) | Err(NetError::Connection(_) | NetError::Bind(_)) => None,
            // The peer sends `Alive` and has said nothing for 5 s: a killed
            // or hung app, or a dead path. Its sessions end at once.
            Err(NetError::TimedOut) => {
                tracing::info!(conn = ?id, "the peer went silent; closing");
                Some(close_code::TIMED_OUT)
            }
            // A message we must understand and don't: the peer is newer.
            Err(NetError::Version(_) | NetError::Incompatible { .. }) => Some(close_code::VERSION),
            Err(NetError::Protocol(_)) => Some(close_code::PROTOCOL),
        };
        if let (Some(code), Some(conn)) = (code, conn.upgrade()) {
            admission::close(&conn, code);
        }
        break;
    }
    let _ = inbox.send(Input::Closed { conn: id }).await;
}

async fn writer(conn: Connection, mut tx: ControlSender, mut out: mpsc::Receiver<Outgoing>) {
    let mut alive = tokio::time::interval(ALIVE_EVERY);
    alive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = alive.tick() => {
                if tx.send(&ControlMessage::Alive(Alive {})).await.is_err() {
                    return;
                }
            }
            item = out.recv() => match item {
                Some(Outgoing::Message(message)) => {
                    if tx.send(&message).await.is_err() {
                        return;
                    }
                }
                Some(Outgoing::Close(code)) => {
                    tx.finish(FLUSH_WAIT).await;
                    admission::close(&conn, code);
                    return;
                }
                None => return,
            },
            _ = conn.closed() => return,
        }
    }
}
