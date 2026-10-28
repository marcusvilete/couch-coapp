//! Remote play in the room actor (M25, ADR 0010).
//!
//! **On the home PC:** a device it trusts ([`crate::remote::RemotePlay`])
//! hears an offer on every room connection; with the right PIN it becomes
//! an *operator* on that connection, whose commands run here as if from our
//! own window, limited to the host's console ([`RemoteOp`]). The PIN check
//! runs off this thread, and wrong PINs slow the next try. The panic key,
//! the PIN changing, the device going or remote play turning off locks
//! operators out at once.
//!
//! **On a device:** an offer only asks whether that member is our own PC
//! (the codes on both screens match); a PIN goes only to one we accepted,
//! so a room member offering remote play can't collect it. Unlocked, the
//! home PC's host console comes to us as [`RemoteState`] and shows as its
//! session's host fields.
//!
//! Nothing here is ever relayed, and nothing on the wire changes who is
//! trusted or the PIN.

use std::cell::RefCell;

use unicode_normalization::UnicodeNormalization;

use couch_proto::{
    ids::SeatId,
    remote::{
        LockReason, Pin, RemoteCommand, RemoteLock, RemoteOffer, RemoteOp, RemoteReply,
        RemoteResult, RemoteScreen, RemoteSession, RemoteSource, RemoteSources, RemoteState,
        RemoteUnlock, RemoteUnlockReply, RemoteWindow, UnlockResult,
    },
};

use super::*;
use crate::{
    HidingView, InputSetup, KeyboardMouseSeat, RemoteAccessView, RemoteAction, RemoteDeviceView,
    RemotePlayView, ScreenView, SourceChoice, SourceList, WindowView,
    remote::{self, Device, PinHash, RemotePlay},
};

/// How long a device waits for its home PC to answer.
const ANSWER_WITHIN: Duration = Duration::from_secs(30);
pub(super) struct Remote {
    // ---- On the home PC ----
    settings: RemotePlay,
    /// Devices unlocked, by the connection they gave the PIN on.
    operators: BTreeMap<ConnId, MemberId>,
    /// PIN checks under way, by device: one at a time, whichever of its
    /// connections asked, until the check ends (M25a review: one per
    /// connection let a device in several rooms guess in parallel). With
    /// each, the connections waiting for its answer: one that asked
    /// meanwhile (a reconnect replacing the first) hears it too.
    checking: BTreeMap<MemberId, BTreeSet<ConnId>>,
    /// Counts PINs set this run: a check started against an older one
    /// unlocks nothing (M25a review).
    pin_generation: u64,
    /// Operators with a source listing under way, and its command's id:
    /// one at a time.
    listing: BTreeMap<ConnId, u32>,
    /// The state last sent to each operator: only changes go out.
    sent: RefCell<BTreeMap<ConnId, RemoteState>>,
    /// Keeping this PC awake (M25c): whether the display too, and the
    /// backend's guard (none where it can't).
    awake: Option<(bool, Option<Box<dyn std::any::Any + Send>>)>,
    /// Windows is locked here, as last looked at while a device is
    /// unlocked, and when to look again.
    locked: bool,
    next_lock_check: Option<Instant>,
    /// This app may start itself at sign-in ([`EngineOptions::start_at_sign_in`]).
    startup_offered: bool,
    // ---- On a device ----
    /// The home PCs we accepted as our own.
    hosts: Vec<Device>,
    /// Members that offered us remote play, by room.
    links: BTreeMap<(RoomId, MemberId), Link>,
    /// The PIN given to each home PC this run, to unlock again after a
    /// reconnect. Memory only.
    pins: BTreeMap<MemberId, Pin>,
    next_id: u32,
}

/// A member that offered us remote play, in one room.
struct Link {
    conn: ConnId,
    wait_secs: Option<u32>,
    unlocked: bool,
    /// Its session as its host console shows it, while unlocked.
    session: Option<RemoteSession>,
    /// Windows is locked there, as it last said (M25c).
    locked: bool,
    /// An unlock on its way: its id (for its timeout), who waits (none for
    /// one made by itself after a reconnect), and the PIN, remembered if
    /// right.
    unlocking: Option<(u32, Option<Reply>, Pin)>,
    /// Commands on their way, by id.
    pending: BTreeMap<u32, Reply>,
}

impl Remote {
    pub(super) fn load(dir: &std::path::Path, startup_offered: bool) -> Self {
        Self {
            settings: remote::load(dir),
            operators: BTreeMap::new(),
            checking: BTreeMap::new(),
            pin_generation: 0,
            listing: BTreeMap::new(),
            sent: RefCell::default(),
            awake: None,
            locked: false,
            next_lock_check: None,
            startup_offered,
            hosts: remote::hosts(dir),
            links: BTreeMap::new(),
            pins: BTreeMap::new(),
            next_id: 1,
        }
    }

    fn next_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }
}

/// How often a home PC with a device unlocked looks whether Windows is
/// locked (M25c).
const LOCK_CHECK_EVERY: Duration = Duration::from_secs(2);

fn message(item: ControlMessage) -> Outgoing {
    Outgoing::Message(item)
}

impl Actor {
    // ---- Connections ----

    /// A connection with `peer` became current: a device we trust hears it
    /// may unlock.
    pub(super) fn remote_connected(&self, room: RoomId, peer: MemberId, conn: ConnId) {
        if self.current_conn(room, peer) == Some(conn) && self.remote.settings.trusts(peer) {
            self.offer_remote(conn);
        }
    }

    fn offer_remote(&self, conn: ConnId) {
        let wait_secs = self.remote.settings.wait_secs(remote::unix_now());
        self.send(
            conn,
            message(ControlMessage::RemoteOffer(RemoteOffer { wait_secs })),
        );
    }

    /// Offers remote play again on every current connection with a device
    /// we trust (turned on, a device added, a new PIN).
    fn offer_remote_everywhere(&self) {
        for (conn, c) in &self.conns {
            if self.remote.settings.trusts(c.peer)
                && self.current_conn(c.room, c.peer) == Some(*conn)
            {
                self.offer_remote(*conn);
            }
        }
    }

    /// A connection closed: an operator on it is locked out, and a home PC
    /// on it goes until it offers again.
    pub(super) fn remote_closed(&mut self, conn: ConnId, now: Instant) {
        if self.remote.operators.remove(&conn).is_some() {
            tracing::info!("remote play: a device's connection closed; it is locked out");
            self.announce_hosting(now);
        }
        // A PIN check under way stays counted until it ends: reconnecting
        // must not start a second one beside it.
        self.remote.listing.remove(&conn);
        self.remote.sent.borrow_mut().remove(&conn);
        let gone: Vec<(RoomId, MemberId)> = self
            .remote
            .links
            .iter()
            .filter(|(_, l)| l.conn == conn)
            .map(|(k, _)| *k)
            .collect();
        for key in gone {
            if let Some(link) = self.remote.links.remove(&key) {
                fail_waiting(link, "the connection to your PC was lost");
            }
            self.announce(key.0, now);
        }
    }

    /// After any operator goes (every path that removes one comes here):
    /// desktop control ends unless its device is still our operator in the
    /// session's room (M25b), then our session is announced.
    fn announce_hosting(&mut self, now: Instant) {
        // The display needn't stay on for nobody (M25c).
        self.sync_awake();
        if let Some(device) = self.desktop_given() {
            let room = self.session_room();
            let still = self.remote.settings.trusts(device)
                && self.remote.operators.iter().any(|(conn, m)| {
                    *m == device && self.conns.get(conn).is_some_and(|c| Some(c.room) == room)
                });
            if !still {
                tracing::info!(device = ?device, "remote play: desktop control ends with the unlock");
                if let Err(e) = self.set_desktop(None, now) {
                    tracing::warn!("desktop control didn't end: {e}");
                }
            }
        }
        if let Some(room) = self.hosting_room() {
            self.announce(room, now);
        }
    }

    /// Keeps this PC awake while remote play is on, and its display on too
    /// while a device of ours is unlocked: a display that turns off stops
    /// the capture, and Windows may lock the PC then (M25c). Call after
    /// either changes.
    pub(super) fn sync_awake(&mut self) {
        let want = self
            .remote
            .settings
            .on
            .then_some(!self.remote.operators.is_empty());
        if want == self.remote.awake.as_ref().map(|(display, _)| *display) {
            return;
        }
        // The new request is made before the old one goes: no moment
        // without one.
        self.remote.awake = want.map(|screen_on| {
            tracing::info!(screen_on, "remote play: keeping this PC awake");
            (screen_on, self.backend.keep_awake(screen_on))
        });
        if want.is_none() {
            tracing::info!("remote play: this PC may sleep again");
        }
    }

    /// While a device of ours is unlocked: whether Windows is locked here,
    /// looked at every [`LOCK_CHECK_EVERY`], and told when it changes (M25c).
    pub(super) fn remote_tick(&mut self, now: Instant) {
        if self.remote.operators.is_empty() {
            self.remote.locked = false;
            self.remote.next_lock_check = None;
            return;
        }
        if self.remote.next_lock_check.is_some_and(|at| now < at) {
            return;
        }
        self.remote.next_lock_check = Some(now + LOCK_CHECK_EVERY);
        let locked = self.backend.screen_locked();
        if locked != self.remote.locked {
            self.remote.locked = locked;
            tracing::info!(locked, "remote play: Windows' lock screen here");
            self.remote_push();
        }
    }

    /// Whether a device of ours is unlocked in `room` (M25b: a screen with
    /// the keyboard seat is then for its desktop control).
    pub(super) fn operator_in(&self, room: RoomId) -> bool {
        self.remote.operators.iter().any(|(conn, m)| {
            self.remote.settings.trusts(*m) && self.conns.get(conn).is_some_and(|c| c.room == room)
        })
    }

    // ---- Messages ----

    pub(super) fn remote_received(
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
        match message {
            // To us, the home PC.
            ControlMessage::RemoteUnlock(m) => self.remote_unlock(conn, peer, m),
            ControlMessage::RemoteCommand(m) => self.remote_command_in(room, conn, peer, m, now),
            // An operator locking itself; any other reason is a home PC
            // locking us out (two PCs may trust each other).
            ControlMessage::RemoteLock(RemoteLock {
                reason: LockReason::ByOperator,
            }) if self.remote.operators.get(&conn) == Some(&peer) => {
                self.remote.operators.remove(&conn);
                self.remote.sent.borrow_mut().remove(&conn);
                tracing::info!(device = ?peer, "remote play: a device locked itself");
                self.announce_hosting(now);
            }
            // Only devices lock themselves: from one that isn't our operator
            // (it was just locked out, say), it means nothing. Never a home
            // PC locking us out.
            ControlMessage::RemoteLock(RemoteLock {
                reason: LockReason::ByOperator,
            }) => {}
            // To us, a device.
            ControlMessage::RemoteOffer(m) => self.remote_offered(room, conn, peer, m, now),
            ControlMessage::RemoteUnlockReply(m) => {
                self.remote_unlock_reply(room, conn, peer, m.result, now);
            }
            ControlMessage::RemoteReply(m) => self.remote_reply(room, conn, peer, *m, now),
            ControlMessage::RemoteState(m) => {
                let key = (room, peer);
                let Some(link) = self
                    .remote
                    .links
                    .get_mut(&key)
                    .filter(|l| l.conn == conn && l.unlocked)
                else {
                    return;
                };
                match m.checked() {
                    Some(state) => {
                        link.session = state.session;
                        link.locked = state.locked;
                    }
                    None => tracing::warn!("remote play: a malformed state from the home PC"),
                }
                self.announce(room, now);
            }
            ControlMessage::RemoteLock(m) => self.remote_locked(room, conn, peer, m.reason, now),
            _ => {}
        }
    }

    // ---- On the home PC ----

    fn remote_unlock(&mut self, conn: ConnId, peer: MemberId, m: RemoteUnlock) {
        let answer = |actor: &Self, result| {
            actor.send(
                conn,
                message(ControlMessage::RemoteUnlockReply(RemoteUnlockReply {
                    result,
                })),
            );
        };
        if !self.remote.settings.trusts(peer) {
            answer(self, UnlockResult::Off);
            return;
        }
        if !m.is_valid() {
            return;
        }
        // One check per device at a time, whichever connection asks: one
        // asking meanwhile waits for that check's answer.
        if let Some(waiting) = self.remote.checking.get_mut(&peer) {
            waiting.insert(conn);
            return;
        }
        if self.remote.operators.get(&conn) == Some(&peer) {
            // Unlocked already (its answer may have come too late for it):
            // the console's state again.
            self.remote.sent.borrow_mut().remove(&conn);
            answer(self, UnlockResult::Unlocked);
            self.remote_push();
            return;
        }
        let unix = remote::unix_now();
        if let Some(secs) = self.remote.settings.wait_secs(unix) {
            answer(self, UnlockResult::Wait { secs });
            return;
        }
        let Some(hash) = self.remote.settings.pin.clone() else {
            answer(self, UnlockResult::Off);
            return;
        };
        // Counted before it is checked: a crash or a dropped connection
        // meanwhile never makes a try free. A right PIN clears it.
        self.remote.settings.tried(false, unix);
        if let Err(e) = remote::save(&self.data_dir, &self.remote.settings) {
            tracing::error!("remote play: the tries couldn't be saved: {e}");
        }
        self.remote.checking.insert(peer, BTreeSet::from([conn]));
        let generation = self.remote.pin_generation;
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            let pin = m.pin;
            let right = tokio::task::spawn_blocking(move || hash.matches(&pin.0))
                .await
                .unwrap_or(false);
            let _ = inbox
                .send(Input::RemotePinChecked {
                    conn,
                    peer,
                    generation,
                    right,
                })
                .await;
        });
    }

    pub(super) fn remote_pin_checked(
        &mut self,
        conn: ConnId,
        peer: MemberId,
        generation: u64,
        right: bool,
        now: Instant,
    ) {
        // Every connection of the device that asked meanwhile hears this
        // answer: the device proved (or failed) the PIN, whichever of its
        // authenticated connections carried it.
        let mut waiting = self.remote.checking.remove(&peer).unwrap_or_default();
        waiting.insert(conn);
        // Checked against a PIN replaced since: it opens nothing.
        let right = right && generation == self.remote.pin_generation;
        let unix = remote::unix_now();
        if right {
            self.remote.settings.tried(true, unix);
            if let Err(e) = remote::save(&self.data_dir, &self.remote.settings) {
                tracing::error!("remote play: the tries couldn't be saved: {e}");
            }
            tracing::info!(device = ?peer, "remote play: a device unlocked");
        } else {
            tracing::warn!(device = ?peer, failures = self.remote.settings.failures, "remote play: a wrong PIN");
        }
        if !self.remote.settings.trusts(peer) {
            return;
        }
        let mut unlocked = false;
        for conn in waiting {
            let current = self
                .conns
                .get(&conn)
                .is_some_and(|c| c.peer == peer && self.current_conn(c.room, peer) == Some(conn));
            if !current {
                continue;
            }
            let result = if right {
                self.remote.operators.insert(conn, peer);
                unlocked = true;
                UnlockResult::Unlocked
            } else {
                UnlockResult::WrongPin {
                    wait_secs: self.remote.settings.wait_secs(unix).unwrap_or(0),
                }
            };
            self.send(
                conn,
                message(ControlMessage::RemoteUnlockReply(RemoteUnlockReply {
                    result,
                })),
            );
        }
        if unlocked {
            // The display stays on now, and the device hears at once
            // whether Windows is locked here (M25c).
            self.sync_awake();
            self.remote.locked = self.backend.screen_locked();
            self.remote.next_lock_check = Some(now + LOCK_CHECK_EVERY);
            self.remote_push();
            self.announce_hosting(now);
        }
    }

    fn remote_command_in(
        &mut self,
        room: RoomId,
        conn: ConnId,
        peer: MemberId,
        m: RemoteCommand,
        now: Instant,
    ) {
        let id = m.id;
        let refuse = |actor: &Self, text: &str| {
            actor.send(
                conn,
                message(ControlMessage::RemoteReply(Box::new(RemoteReply {
                    id,
                    result: RemoteResult::Failed(text.into()),
                }))),
            );
        };
        if self.remote.operators.get(&conn) != Some(&peer) || !self.remote.settings.trusts(peer) {
            refuse(self, "locked: give the PIN first");
            return;
        }
        if !m.op.is_valid() {
            refuse(self, "that can't be done");
            return;
        }
        // Our session, started or still starting, is run from the room it
        // is in; from another, nothing of it (its window's title included).
        if self.session_room().is_some_and(|r| r != room) {
            refuse(self, "your PC's session is in another room");
            return;
        }
        let inbox = self.inbox.clone();
        if matches!(m.op, RemoteOp::ListSources) {
            // One listing at a time per operator, known by its id.
            if self.remote.listing.contains_key(&conn) {
                refuse(self, "still listing the windows");
                return;
            }
            self.remote.listing.insert(conn, id);
            let backend = self.backend.clone();
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    (backend.sources(), backend.virtual_pads())
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|(sources, pads)| {
                    sources
                        .map(|s| remote_sources(s, pads))
                        .map_err(|e| e.to_string())
                });
                let result = match result {
                    Ok(sources) => RemoteResult::Sources(sources),
                    Err(e) => RemoteResult::Failed(e),
                };
                let _ = inbox.send(Input::RemoteAnswered { conn, id, result }).await;
            });
            return;
        }
        // Desktop control (M25b): for this operator, on this connection.
        if let RemoteOp::Desktop { on } = m.op {
            let result = if !on && self.desktop_given().is_some_and(|d| d != peer) {
                Err(EngineError::Refused("another device controls the desktop"))
            } else {
                self.set_desktop(on.then_some(peer), now)
            };
            let result = match result {
                Ok(()) => RemoteResult::Done,
                Err(e) => RemoteResult::Failed(e.to_string()),
            };
            self.remote_answered(conn, id, result);
            return;
        }
        tracing::info!(device = ?peer, "remote play: a command from afar");
        let Some(command) = command_for(room, m.op) else {
            refuse(self, "that can't be done");
            return;
        };
        let (tx, rx) = oneshot::channel();
        self.command(command, tx, now);
        tokio::spawn(async move {
            let result = match rx.await {
                Ok(Ok(_)) => RemoteResult::Done,
                Ok(Err(e)) => RemoteResult::Failed(e.to_string()),
                Err(_) => RemoteResult::Failed("it was never answered".into()),
            };
            let _ = inbox.send(Input::RemoteAnswered { conn, id, result }).await;
        });
    }

    pub(super) fn remote_answered(&mut self, conn: ConnId, id: u32, mut result: RemoteResult) {
        // The listing under way ends with its own answer, whichever it is.
        if self.remote.listing.get(&conn) == Some(&id) {
            self.remote.listing.remove(&conn);
        }
        if !self.remote.operators.contains_key(&conn) {
            return;
        }
        if let RemoteResult::Failed(text) = &mut result {
            couch_proto::remote::cut(text);
        }
        self.send(
            conn,
            message(ControlMessage::RemoteReply(Box::new(RemoteReply {
                id,
                result,
            }))),
        );
        // The console's state follows each command.
        self.remote_push();
    }

    /// Sends each operator our host console's state, if it changed: only
    /// to those in our session's room (a session elsewhere is none of
    /// theirs: its guests, their controllers, its window's title).
    pub(super) fn remote_push(&self) {
        if self.remote.operators.is_empty() {
            return;
        }
        let session = self.remote_session();
        let room = self.hosting_room();
        let mut sent = self.remote.sent.borrow_mut();
        for conn in self.remote.operators.keys() {
            let here = self.conns.get(conn).is_some_and(|c| Some(c.room) == room);
            let state = RemoteState {
                session: session.clone().filter(|_| here),
                locked: self.remote.locked,
            };
            if sent.get(conn) != Some(&state) {
                self.send(
                    *conn,
                    message(ControlMessage::RemoteState(Box::new(state.clone()))),
                );
                sent.insert(*conn, state);
            }
        }
    }

    /// Locks every operator out, saying why.
    pub(super) fn remote_lock_all(&mut self, reason: LockReason, now: Instant) {
        // A PIN check still running unlocks nothing after this either.
        self.remote.pin_generation += 1;
        if self.remote.operators.is_empty() {
            return;
        }
        tracing::warn!(?reason, "remote play: every device locked out");
        for conn in std::mem::take(&mut self.remote.operators).into_keys() {
            self.send(
                conn,
                message(ControlMessage::RemoteLock(RemoteLock { reason })),
            );
        }
        self.remote.sent.borrow_mut().clear();
        self.announce_hosting(now);
    }

    /// Our devices running our session from afar, by name.
    pub(super) fn operated_by(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .remote
            .operators
            .values()
            .filter_map(|id| {
                let hex = id.to_hex();
                self.remote
                    .settings
                    .devices
                    .iter()
                    .find(|d| d.id == hex)
                    .map(|d| d.name.clone())
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }

    // ---- Settings (the home PC's own window only) ----

    pub(super) fn remote_settings_command(&mut self, command: Command, reply: Reply, now: Instant) {
        // While a device of ours controls the desktop, it could click
        // through this PC's own window: no new device or PIN then, and no
        // running another PC of ours from here, which would stretch this
        // PIN to it (M25b). Removing a device and turning remote play off
        // still work. A speed bump, not a wall: desktop control is the whole
        // PC, its files included.
        if matches!(
            command,
            Command::AddRemoteDevice { .. }
                | Command::SetRemotePin { .. }
                | Command::AcceptRemoteHost { .. }
                | Command::UnlockRemote { .. }
                | Command::Remote { .. }
                // Turned on, the PC comes back online at every sign-in (M25c).
                | Command::SetStartAtSignIn { on: true }
        ) && self.desktop_given().is_some()
        {
            let _ = reply.send(Err(EngineError::Remote(
                "a device of yours controls this PC's desktop: end that first, or press the \
                 panic key"
                    .into(),
            )));
            return;
        }
        let result = match command {
            Command::GetRemotePlay => Ok(()),
            Command::SetRemotePlay { on } => self.set_remote_play(on, now),
            Command::SetStartAtSignIn { on } => self.set_start_at_sign_in(on),
            Command::SetRemotePin { pin } => {
                // Answers later, once hashed.
                if let Err(e) = self.set_remote_pin(&pin, reply) {
                    tracing::info!("no new PIN: {e}");
                }
                return;
            }
            Command::AddRemoteDevice { member } => self.add_remote_device(&member),
            Command::RemoveRemoteDevice { member } => self.remove_remote_device(&member, now),
            Command::AcceptRemoteHost { member } => self.accept_remote_host(&member, now),
            Command::ForgetRemoteHost { member } => self.forget_remote_host(&member, now),
            Command::UnlockRemote { room, member, pin } => {
                // Answers later, once the home PC has.
                if let Err(e) = self.unlock_remote(&room, &member, pin, reply) {
                    tracing::info!("not unlocking: {e}");
                }
                return;
            }
            Command::LockRemote { room, member } => self.lock_remote(&room, &member, now),
            Command::Remote {
                room,
                member,
                action,
            } => {
                // Answers later, once the home PC has.
                if let Err(e) = self.remote_action(&room, &member, action, reply) {
                    tracing::info!("not sent to the home PC: {e}");
                }
                return;
            }
            _ => Err(EngineError::Stopped),
        };
        let _ = reply.send(result.map(|()| Response::RemotePlay(self.remote_view())));
    }

    fn save_remote(&self) -> Result<(), EngineError> {
        remote::save(&self.data_dir, &self.remote.settings).map_err(EngineError::from)
    }

    fn set_remote_play(&mut self, on: bool, now: Instant) -> Result<(), EngineError> {
        if on && self.remote.settings.pin.is_none() {
            return Err(EngineError::Remote("set a PIN first".into()));
        }
        self.remote.settings.on = on;
        // Acted on even if it can't be saved (M25c review): turned off,
        // devices are locked out and the PC may sleep at once; the failed
        // save is still reported.
        let saved = self.save_remote();
        self.sync_awake();
        if on {
            self.offer_remote_everywhere();
        } else {
            self.remote_lock_all(LockReason::Off, now);
            self.tell_devices(LockReason::Off, |_| true);
        }
        saved
    }

    /// Starts this app at sign-in, or stops (M25c): the user's own choice,
    /// independent of remote play, where it is offered.
    fn set_start_at_sign_in(&mut self, on: bool) -> Result<(), EngineError> {
        if !self.remote.startup_offered {
            return Err(EngineError::Refused(
                "only an installed Couch Co-App can start with Windows",
            ));
        }
        self.backend
            .set_start_at_sign_in(on)
            .map_err(EngineError::Remote)?;
        tracing::info!(on, "starting at sign-in");
        Ok(())
    }

    /// Sends `reason` to every current connection with one of our devices
    /// that `which` picks, unlocked or not, so its offer goes.
    fn tell_devices(&self, reason: LockReason, which: impl Fn(MemberId) -> bool) {
        let devices: Vec<String> = self
            .remote
            .settings
            .devices
            .iter()
            .map(|d| d.id.clone())
            .collect();
        for (conn, c) in &self.conns {
            if devices.contains(&c.peer.to_hex())
                && which(c.peer)
                && self.current_conn(c.room, c.peer) == Some(*conn)
            {
                self.send(
                    *conn,
                    message(ControlMessage::RemoteLock(RemoteLock { reason })),
                );
            }
        }
    }

    fn set_remote_pin(&mut self, pin: &str, reply: Reply) -> Result<(), EngineError> {
        let checked = remote::check_pin(pin).map_err(EngineError::from);
        let salt = checked.and_then(|()| random::<16>());
        let salt = match salt {
            Ok(salt) => salt,
            Err(e) => {
                let text = e.to_string();
                let _ = reply.send(Err(e));
                return Err(EngineError::Remote(text));
            }
        };
        let inbox = self.inbox.clone();
        let mut pin = pin.to_string();
        tokio::spawn(async move {
            let hash = tokio::task::spawn_blocking(move || {
                let hash = PinHash::new(&pin, salt);
                zeroize::Zeroize::zeroize(&mut pin);
                hash
            })
            .await;
            if let Ok(hash) = hash {
                let _ = inbox.send(Input::RemotePinHashed { hash, reply }).await;
            }
        });
        Ok(())
    }

    pub(super) fn remote_pin_hashed(&mut self, hash: PinHash, reply: Reply, now: Instant) {
        // A check still running against the old PIN opens nothing (the
        // lock below moves the generation on).
        self.remote.settings.pin = Some(hash);
        self.remote.settings.failures = 0;
        self.remote.settings.wait_until = 0;
        let result = self.save_remote();
        // Unlocked with the old one: the new one, please.
        self.remote_lock_all(LockReason::PinChanged, now);
        self.offer_remote_everywhere();
        let _ = reply.send(result.map(|()| Response::RemotePlay(self.remote_view())));
    }

    fn add_remote_device(&mut self, member: &str) -> Result<(), EngineError> {
        if self.remote.settings.pin.is_none() {
            return Err(EngineError::Remote("set a PIN first".into()));
        }
        let id = MemberId::from_hex(member).ok_or(EngineError::UnknownMember)?;
        if id == self.me {
            return Err(EngineError::Remote("that's this PC".into()));
        }
        let name = self
            .rooms
            .values()
            .filter(|e| !e.room.is_leaving())
            .flat_map(|e| e.room.status().members)
            .find(|m| m.id == id)
            .map(|m| m.name)
            .ok_or(EngineError::UnknownMember)?;
        let hex = id.to_hex();
        if !self.remote.settings.devices.iter().any(|d| d.id == hex) {
            self.remote.settings.devices.push(Device { id: hex, name });
            self.save_remote()?;
            tracing::info!(device = ?id, "remote play: a device added");
        }
        for (conn, c) in &self.conns {
            if c.peer == id {
                self.remote_connected(c.room, id, *conn);
            }
        }
        Ok(())
    }

    fn remove_remote_device(&mut self, member: &str, now: Instant) -> Result<(), EngineError> {
        let id = MemberId::from_hex(member).ok_or(EngineError::UnknownMember)?;
        let hex = id.to_hex();
        self.remote.settings.devices.retain(|d| d.id != hex);
        // A check of its still running unlocks nothing, even if it is added
        // back meanwhile.
        self.remote.pin_generation += 1;
        self.save_remote()?;
        tracing::info!(device = ?id, "remote play: a device removed");
        let theirs: Vec<ConnId> = self
            .remote
            .operators
            .iter()
            .filter(|(_, p)| **p == id)
            .map(|(c, _)| *c)
            .collect();
        for conn in theirs {
            self.remote.operators.remove(&conn);
            self.remote.sent.borrow_mut().remove(&conn);
        }
        // Each of its connections hears, unlocked or not.
        for (conn, c) in &self.conns {
            if c.peer == id && self.current_conn(c.room, id) == Some(*conn) {
                self.send(
                    *conn,
                    message(ControlMessage::RemoteLock(RemoteLock {
                        reason: LockReason::Removed,
                    })),
                );
            }
        }
        self.announce_hosting(now);
        Ok(())
    }

    pub(super) fn remote_view(&self) -> RemotePlayView {
        let online = |id: MemberId| self.conns.values().any(|c| c.peer == id);
        let view = |d: &Device| {
            let id = MemberId::from_hex(&d.id);
            RemoteDeviceView {
                id: d.id.clone(),
                name: d.name.clone(),
                code: id.map(remote::device_code).unwrap_or_default(),
                online: id.is_some_and(online),
            }
        };
        let devices: Vec<RemoteDeviceView> =
            self.remote.settings.devices.iter().map(view).collect();
        let mut candidates: Vec<RemoteDeviceView> = Vec::new();
        for entry in self.rooms.values().filter(|e| !e.room.is_leaving()) {
            for m in entry.room.status().members {
                let hex = m.id.to_hex();
                if m.id == self.me
                    || devices.iter().any(|d| d.id == hex)
                    || candidates.iter().any(|c| c.id == hex)
                {
                    continue;
                }
                candidates.push(RemoteDeviceView {
                    code: remote::device_code(m.id),
                    online: online(m.id),
                    id: hex,
                    name: m.name,
                });
            }
        }
        // Online first: the device being added is normally connected.
        candidates.sort_by_key(|c| (!c.online, c.name.to_lowercase()));
        RemotePlayView {
            on: self.remote.settings.on,
            pin_set: self.remote.settings.pin.is_some(),
            my_code: remote::device_code(self.me),
            devices,
            candidates,
            wait_secs: self.remote.settings.wait_secs(remote::unix_now()),
            hosts: self.remote.hosts.iter().map(view).collect(),
            desktop: self.desktop_given().and_then(|id| {
                let hex = id.to_hex();
                self.remote
                    .settings
                    .devices
                    .iter()
                    .find(|d| d.id == hex)
                    .map(|d| d.name.clone())
            }),
            start_at_sign_in: self.remote.startup_offered.then(|| {
                self.backend.start_at_sign_in().unwrap_or_else(|e| {
                    tracing::warn!("whether we start at sign-in: {e}");
                    false
                })
            }),
        }
    }

    // ---- On a device ----

    fn remote_offered(
        &mut self,
        room: RoomId,
        conn: ConnId,
        peer: MemberId,
        m: RemoteOffer,
        now: Instant,
    ) {
        let key = (room, peer);
        let link = self.remote.links.entry(key).or_insert_with(|| Link {
            conn,
            wait_secs: None,
            unlocked: false,
            session: None,
            locked: false,
            unlocking: None,
            pending: BTreeMap::new(),
        });
        if link.conn != conn {
            let old = std::mem::replace(
                link,
                Link {
                    conn,
                    wait_secs: None,
                    unlocked: false,
                    session: None,
                    locked: false,
                    unlocking: None,
                    pending: BTreeMap::new(),
                },
            );
            fail_waiting(old, "the connection to your PC was replaced");
        }
        let id = self.remote.next_id();
        let link = self.remote.links.get_mut(&key).expect("just put");
        link.wait_secs = m.wait_secs;
        // A home PC we accepted, after a reconnect: unlock again with the
        // PIN given this run, unless it says to wait.
        let accepted = self.remote.hosts.iter().any(|h| h.id == peer.to_hex());
        if accepted
            && !link.unlocked
            && link.unlocking.is_none()
            && m.wait_secs.is_none()
            && let Some(pin) = self.remote.pins.get(&peer).cloned()
        {
            link.unlocking = Some((id, None, pin.clone()));
            self.send(
                conn,
                message(ControlMessage::RemoteUnlock(RemoteUnlock { pin })),
            );
            self.remote_timeout_later(key, id);
        }
        self.announce(room, now);
    }

    fn accept_remote_host(&mut self, member: &str, now: Instant) -> Result<(), EngineError> {
        let id = MemberId::from_hex(member).ok_or(EngineError::UnknownMember)?;
        let room = self
            .remote
            .links
            .keys()
            .find(|(_, m)| *m == id)
            .map(|(room, _)| *room)
            .ok_or(EngineError::Remote(
                "that member hasn't offered remote play".into(),
            ))?;
        let hex = id.to_hex();
        if !self.remote.hosts.iter().any(|h| h.id == hex) {
            let name = self.member_name(room, id);
            self.remote.hosts.push(Device { id: hex, name });
            remote::save_hosts(&self.data_dir, &self.remote.hosts)?;
            tracing::info!(host = ?id, "remote play: a home PC accepted");
        }
        self.announce_links(id, now);
        Ok(())
    }

    fn forget_remote_host(&mut self, member: &str, now: Instant) -> Result<(), EngineError> {
        let id = MemberId::from_hex(member).ok_or(EngineError::UnknownMember)?;
        let hex = id.to_hex();
        self.remote.hosts.retain(|h| h.id != hex);
        remote::save_hosts(&self.data_dir, &self.remote.hosts)?;
        self.remote.pins.remove(&id);
        let keys: Vec<(RoomId, MemberId)> = self
            .remote
            .links
            .keys()
            .filter(|(_, m)| *m == id)
            .copied()
            .collect();
        for key in keys {
            if let Some(link) = self.remote.links.get_mut(&key)
                && link.unlocked
            {
                link.unlocked = false;
                link.session = None;
                let conn = link.conn;
                self.send(
                    conn,
                    message(ControlMessage::RemoteLock(RemoteLock {
                        reason: LockReason::ByOperator,
                    })),
                );
            }
        }
        self.announce_links(id, now);
        Ok(())
    }

    fn announce_links(&self, member: MemberId, now: Instant) {
        let rooms: BTreeSet<RoomId> = self
            .remote
            .links
            .keys()
            .filter(|(_, m)| *m == member)
            .map(|(r, _)| *r)
            .collect();
        for room in rooms {
            self.announce(room, now);
        }
    }

    fn link_of(&self, room: &str, member: &str) -> Result<(RoomId, MemberId), EngineError> {
        let room = RoomId::from_hex(room).ok_or(EngineError::UnknownRoom)?;
        let member = MemberId::from_hex(member).ok_or(EngineError::UnknownMember)?;
        if !self.remote.links.contains_key(&(room, member)) {
            return Err(EngineError::Remote(
                "that PC isn't offering remote play to this device now".into(),
            ));
        }
        Ok((room, member))
    }

    fn unlock_remote(
        &mut self,
        room: &str,
        member: &str,
        pin: String,
        reply: Reply,
    ) -> Result<(), EngineError> {
        let key = match self.link_of(room, member) {
            Ok(key) => key,
            Err(e) => {
                let text = e.to_string();
                let _ = reply.send(Err(e));
                return Err(EngineError::Remote(text));
            }
        };
        let accepted = self.remote.hosts.iter().any(|h| h.id == key.1.to_hex());
        // As the home PC reads it (trimmed, NFKC), so the length limit
        // counts the same bytes on both ends.
        let mut pin = pin;
        let normalized: String = pin.trim().nfkc().collect();
        zeroize::Zeroize::zeroize(&mut pin);
        let unlock = RemoteUnlock {
            pin: Pin(normalized),
        };
        let id = self.remote.next_id();
        let link = self.remote.links.get_mut(&key).expect("checked");
        let problem = if !accepted {
            Some("accept it as your PC first")
        } else if !unlock.is_valid() {
            Some("that PIN can't be right")
        } else if link.unlocking.is_some() {
            Some("already unlocking")
        } else {
            None
        };
        if let Some(problem) = problem {
            let _ = reply.send(Err(EngineError::Remote(problem.into())));
            return Err(EngineError::Remote(problem.into()));
        }
        let conn = link.conn;
        link.unlocking = Some((id, Some(reply), unlock.pin.clone()));
        self.send(conn, message(ControlMessage::RemoteUnlock(unlock)));
        self.remote_timeout_later(key, id);
        Ok(())
    }

    fn remote_timeout_later(&self, (room, member): (RoomId, MemberId), id: u32) {
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            tokio::time::sleep(ANSWER_WITHIN).await;
            let _ = inbox.send(Input::RemoteTimeout { room, member, id }).await;
        });
    }

    fn remote_unlock_reply(
        &mut self,
        room: RoomId,
        conn: ConnId,
        peer: MemberId,
        result: UnlockResult,
        now: Instant,
    ) {
        let Some(link) = self
            .remote
            .links
            .get_mut(&(room, peer))
            .filter(|l| l.conn == conn)
        else {
            return;
        };
        let Some((_, waiting, pin)) = link.unlocking.take() else {
            // An answer to an unlock that timed out here: unlocked there, but
            // nobody here asked any more. Undone, so the two agree.
            if result == UnlockResult::Unlocked && !link.unlocked {
                let conn = link.conn;
                self.send(
                    conn,
                    message(ControlMessage::RemoteLock(RemoteLock {
                        reason: LockReason::ByOperator,
                    })),
                );
            }
            return;
        };
        let outcome = match result {
            UnlockResult::Unlocked => {
                link.unlocked = true;
                link.wait_secs = None;
                self.remote.pins.insert(peer, pin);
                Ok(())
            }
            UnlockResult::WrongPin { wait_secs } => {
                link.wait_secs = (wait_secs > 0).then_some(wait_secs);
                self.remote.pins.remove(&peer);
                Err(if wait_secs > 0 {
                    format!(
                        "wrong PIN; too many tries, so wait {} before the next",
                        wait_text(wait_secs)
                    )
                } else {
                    "wrong PIN".into()
                })
            }
            UnlockResult::Wait { secs } => {
                link.wait_secs = Some(secs);
                Err(format!(
                    "too many wrong PINs: try again in {}",
                    wait_text(secs)
                ))
            }
            UnlockResult::Off => {
                self.remote.pins.remove(&peer);
                Err("remote play is off on that PC, or it no longer trusts this device".into())
            }
        };
        let outcome = outcome.map_err(EngineError::Remote).and_then(|()| {
            self.view(room, now)
                .map(Response::Room)
                .ok_or(EngineError::UnknownRoom)
        });
        if let Some(reply) = waiting {
            let _ = reply.send(outcome);
        }
        self.announce(room, now);
    }

    fn lock_remote(&mut self, room: &str, member: &str, now: Instant) -> Result<(), EngineError> {
        let key = self.link_of(room, member)?;
        self.remote.pins.remove(&key.1);
        let link = self.remote.links.get_mut(&key).expect("checked");
        if link.unlocked {
            link.unlocked = false;
            link.session = None;
            let conn = link.conn;
            self.send(
                conn,
                message(ControlMessage::RemoteLock(RemoteLock {
                    reason: LockReason::ByOperator,
                })),
            );
        }
        self.announce(key.0, now);
        Ok(())
    }

    fn remote_action(
        &mut self,
        room: &str,
        member: &str,
        action: RemoteAction,
        reply: Reply,
    ) -> Result<(), EngineError> {
        let checked = self.link_of(room, member).and_then(|key| {
            let unlocked = self.remote.links.get(&key).is_some_and(|l| l.unlocked);
            if !unlocked {
                return Err(EngineError::Remote("unlock your PC first".into()));
            }
            Ok((key, op_for(action)?))
        });
        let (key, op) = match checked {
            Ok(ok) => ok,
            Err(e) => {
                let text = e.to_string();
                let _ = reply.send(Err(e));
                return Err(EngineError::Remote(text));
            }
        };
        let id = self.remote.next_id();
        let link = self.remote.links.get_mut(&key).expect("checked");
        link.pending.insert(id, reply);
        let conn = link.conn;
        self.send(
            conn,
            message(ControlMessage::RemoteCommand(RemoteCommand { id, op })),
        );
        self.remote_timeout_later(key, id);
        Ok(())
    }

    fn remote_reply(
        &mut self,
        room: RoomId,
        conn: ConnId,
        peer: MemberId,
        m: RemoteReply,
        now: Instant,
    ) {
        let Some(reply) = self
            .remote
            .links
            .get_mut(&(room, peer))
            .filter(|l| l.conn == conn)
            .and_then(|l| l.pending.remove(&m.id))
        else {
            return;
        };
        let result = match m.result {
            RemoteResult::Done => self
                .view(room, now)
                .map(Response::Room)
                .ok_or(EngineError::UnknownRoom),
            RemoteResult::Sources(sources) if sources.is_valid() => {
                Ok(Response::Sources(source_list(sources)))
            }
            RemoteResult::Sources(_) => Err(EngineError::Remote(
                "your PC sent a list that can't be read".into(),
            )),
            RemoteResult::Failed(text) => Err(EngineError::Remote(text)),
        };
        let _ = reply.send(result);
    }

    fn remote_locked(
        &mut self,
        room: RoomId,
        conn: ConnId,
        peer: MemberId,
        reason: LockReason,
        now: Instant,
    ) {
        let key = (room, peer);
        if !self.remote.links.get(&key).is_some_and(|l| l.conn == conn) {
            return;
        }
        tracing::info!(?reason, "remote play: locked out by the home PC");
        if matches!(
            reason,
            LockReason::Panic | LockReason::Removed | LockReason::PinChanged | LockReason::Off
        ) {
            self.remote.pins.remove(&peer);
        }
        if matches!(reason, LockReason::Removed | LockReason::Off) {
            if let Some(link) = self.remote.links.remove(&key) {
                fail_waiting(link, "your PC turned remote play off for this device");
            }
        } else if let Some(link) = self.remote.links.get_mut(&key) {
            link.unlocked = false;
            link.session = None;
            for (_, reply) in std::mem::take(&mut link.pending) {
                let _ = reply.send(Err(EngineError::Remote(
                    "your PC locked this device out".into(),
                )));
            }
        }
        self.announce(room, now);
    }

    pub(super) fn remote_timeout(&mut self, room: RoomId, member: MemberId, id: u32, now: Instant) {
        let Some(link) = self.remote.links.get_mut(&(room, member)) else {
            return;
        };
        let late = "your PC didn't answer in time";
        // Only the unlock this timer was for: a later one has its own.
        if link.unlocking.as_ref().is_some_and(|(u, ..)| *u == id) {
            if let Some((_, Some(reply), _)) = link.unlocking.take() {
                let _ = reply.send(Err(EngineError::Remote(late.into())));
            }
        } else if let Some(reply) = link.pending.remove(&id) {
            let _ = reply.send(Err(EngineError::Remote(late.into())));
        }
        self.announce(room, now);
    }

    /// What a device shows of `member` in `room`: one of our PCs, or one
    /// asking to be.
    pub(super) fn remote_access(&self, room: RoomId, member: MemberId) -> Option<RemoteAccessView> {
        let link = self.remote.links.get(&(room, member))?;
        Some(RemoteAccessView {
            accepted: self.remote.hosts.iter().any(|h| h.id == member.to_hex()),
            unlocked: link.unlocked,
            code: remote::device_code(member),
            wait_secs: link.wait_secs,
            locked: link.unlocked && link.locked,
        })
    }

    /// The host console of `host`'s session `session`, as one of our PCs
    /// sent it, while we are unlocked.
    pub(super) fn operated_session(
        &self,
        room: RoomId,
        host: MemberId,
        session: SessionId,
    ) -> Option<&RemoteSession> {
        self.remote
            .links
            .get(&(room, host))
            .filter(|l| l.unlocked)?
            .session
            .as_ref()
            .filter(|s| s.session == session)
    }

    /// Whether we run `host`'s sessions in `room` from here.
    pub(super) fn operating(&self, room: RoomId, host: MemberId) -> bool {
        self.remote
            .links
            .get(&(room, host))
            .is_some_and(|l| l.unlocked)
    }
}

/// Answers whoever waits on `link` that it went.
fn fail_waiting(link: Link, why: &str) {
    if let Some((_, Some(reply), _)) = link.unlocking {
        let _ = reply.send(Err(EngineError::Remote(why.into())));
    }
    for (_, reply) in link.pending {
        let _ = reply.send(Err(EngineError::Remote(why.into())));
    }
}

/// "45 s", "3 min".
fn wait_text(secs: u32) -> String {
    if secs < 90 {
        format!("{secs} s")
    } else {
        format!("{} min", secs.div_ceil(60))
    }
}

/// The home PC's own command for an operator's `op`, in `room`.
fn command_for(room: RoomId, op: RemoteOp) -> Option<Command> {
    Some(match op {
        RemoteOp::ListSources => Command::ListSources,
        RemoteOp::StartSession {
            title,
            max_viewers,
            controllers,
            keyboard_mouse,
        } => Command::StartSession {
            room: room.to_hex(),
            title,
            max_viewers,
            input: InputSetup {
                controllers,
                keyboard_mouse: if keyboard_mouse {
                    KeyboardMouseSeat::OwnSeat
                } else {
                    KeyboardMouseSeat::None
                },
            },
            // Nobody is at the home PC to play.
            controllers: Vec::new(),
        },
        RemoteOp::StartStream {
            source,
            name,
            fps,
            max_mbps,
            upload_mbps,
        } => Command::StartStream {
            source: match source {
                RemoteSource::Window(id) => SourceChoice::Window { id: id.to_string() },
                RemoteSource::Screen(id) => SourceChoice::Screen { id: id.to_string() },
            },
            name,
            fps,
            max_mbps,
            upload_mbps,
        },
        RemoteOp::StopStream => Command::StopStream,
        RemoteOp::SetStreamLevel { percent } => Command::SetStreamLevel { percent },
        RemoteOp::EndSession => Command::EndSession,
        RemoteOp::AnswerControl { member, accept } => Command::AnswerControl {
            member: member.to_hex(),
            accept,
        },
        RemoteOp::PassControl {
            member,
            seat,
            controller,
        } => Command::PassControl {
            member: member.to_hex(),
            seat: seat.0,
            controller,
        },
        RemoteOp::TakeBackControl { seat } => Command::TakeBackControl {
            seat: seat.map(|s| s.0),
        },
        RemoteOp::PlaySeat { seat, controller } => Command::PlaySeat {
            seat: seat.0,
            controller,
        },
        RemoteOp::AllowControl => Command::AllowControl,
        RemoteOp::StopWaitingForPads => Command::StopWaitingForPads,
        RemoteOp::BringToFront => Command::BringToFront,
        // Our own window has no such command: `remote_command_in` handles
        // it with the operator's id.
        RemoteOp::Desktop { .. } => return None,
    })
}

/// The wire's form of a device's `action`.
fn op_for(action: RemoteAction) -> Result<RemoteOp, EngineError> {
    let member = |m: &str| MemberId::from_hex(m).ok_or(EngineError::UnknownMember);
    let source_id = |id: &str| {
        id.parse::<i64>()
            .map_err(|_| EngineError::Refused("that window or screen is gone"))
    };
    Ok(match action {
        RemoteAction::ListSources => RemoteOp::ListSources,
        RemoteAction::StartSession {
            title,
            max_viewers,
            input,
        } => RemoteOp::StartSession {
            title,
            max_viewers,
            controllers: input.controllers,
            keyboard_mouse: input.keyboard_mouse == KeyboardMouseSeat::OwnSeat,
        },
        RemoteAction::StartStream {
            source,
            name,
            fps,
            max_mbps,
            upload_mbps,
        } => RemoteOp::StartStream {
            source: match source {
                SourceChoice::Window { id } => RemoteSource::Window(source_id(&id)?),
                SourceChoice::Screen { id } => RemoteSource::Screen(source_id(&id)?),
            },
            name,
            fps,
            max_mbps,
            upload_mbps,
        },
        RemoteAction::StopStream => RemoteOp::StopStream,
        RemoteAction::SetStreamLevel { percent } => RemoteOp::SetStreamLevel { percent },
        RemoteAction::EndSession => RemoteOp::EndSession,
        RemoteAction::AnswerControl { member: m, accept } => RemoteOp::AnswerControl {
            member: member(&m)?,
            accept,
        },
        RemoteAction::PassControl {
            member: m,
            seat,
            controller,
        } => RemoteOp::PassControl {
            member: member(&m)?,
            seat: SeatId(seat),
            controller,
        },
        RemoteAction::TakeBackControl { seat } => RemoteOp::TakeBackControl {
            seat: seat.map(SeatId),
        },
        RemoteAction::PlaySeat { seat, controller } => RemoteOp::PlaySeat {
            seat: SeatId(seat),
            controller,
        },
        RemoteAction::AllowControl => RemoteOp::AllowControl,
        RemoteAction::StopWaitingForPads => RemoteOp::StopWaitingForPads,
        RemoteAction::Desktop { on } => RemoteOp::Desktop { on },
        RemoteAction::BringToFront => RemoteOp::BringToFront,
    })
}

/// What we could stream, for an operator.
fn remote_sources(sources: crate::pipeline::Sources, virtual_pads: bool) -> RemoteSources {
    RemoteSources {
        windows: sources
            .windows
            .into_iter()
            .map(|w| RemoteWindow {
                id: w.id as i64,
                title: w.title,
                app: w.app,
                elevated: w.elevated,
                refresh_hz: w.refresh_hz,
            })
            .collect(),
        screens: sources
            .screens
            .into_iter()
            .map(|s| RemoteScreen {
                id: s.id as i64,
                name: s.name,
                width: s.width,
                height: s.height,
                primary: s.primary,
                refresh_hz: s.refresh_hz,
            })
            .collect(),
        virtual_pads,
    }
    .capped()
}

/// One of our PCs' sources, as the start dialog lists them. Its own
/// controllers stay at home: none are listed.
fn source_list(sources: RemoteSources) -> SourceList {
    SourceList {
        windows: sources
            .windows
            .into_iter()
            .map(|w| WindowView {
                id: w.id.to_string(),
                title: w.title,
                app: w.app,
                elevated: w.elevated,
                refresh_hz: w.refresh_hz,
            })
            .collect(),
        screens: sources
            .screens
            .into_iter()
            .map(|s| ScreenView {
                id: s.id.to_string(),
                name: s.name,
                width: s.width,
                height: s.height,
                primary: s.primary,
                refresh_hz: s.refresh_hz,
            })
            .collect(),
        controllers: 0,
        host_controllers: Vec::new(),
        hiding: HidingView::NotInstalled,
        virtual_pads: sources.virtual_pads,
    }
}
