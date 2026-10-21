//! One room as one member sees it: who is in it, and the connections to the
//! other members.
//!
//! Sans-IO. The engine owns the connections and runs the password check; it
//! reports what happened (`connected`, `received`, `disconnected`,
//! `dial_failed`, `tick`) and carries out the returned [`Effect`]s.
//!
//! **The owner decides who is in.** The owner (the creator, bound into the
//! room id) signs the policy: the roster of members, and the banned. Only
//! the owner admits newcomers, with an invite it made and the password, and
//! puts them on the roster. Members accept connections from ids on the
//! roster and nobody else; iroh has already authenticated the ids. Removing
//! someone (a ban, a departure, the owner closing the room) is a new roster:
//! every connection to someone no longer on it is closed, with the policy as
//! proof.
//!
//! **Who writes what else.** Each member signs its own record: name,
//! addresses, whether it is leaving. A record belongs to one stay (the
//! roster entry that admitted the member), so records from before a member
//! left and came back count for nothing. Any member relays records and the
//! policy, and everyone keeps the highest version that verifies.
//!
//! **Leaving takes delivery.** A member that leaves while nobody is connected
//! keeps trying until one member has its departure; an owner that closes the
//! room keeps it, hidden, until every former member has been shown that.
//!
//! **Connections.** One per pair of members in a room. Both may dial at once;
//! when two connections between the same pair are both accepted, both sides
//! keep the one dialed by the lower id. A second connection from the same
//! dialer replaces the first, since the dialer only redials when it thinks
//! the old one is dead.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use couch_proto::{
    control::{ControlMessage, close_code},
    ids::{InviteTag, InviteToken, MemberId, RoomId, RoomNonce},
    invite::Invite,
    room::{
        Goodbye, GoodbyeReason, MAX_BANNED, MAX_IPS, MAX_RELAY_URL_BYTES, MemberRecord, PeerAddr,
        Presence, RoomPolicy, RoomSnapshot, RosterEntry, Signed, valid_name,
    },
};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use crate::signing::{
    invite_tag, member_id, policy_is_authentic, record_is_authentic, room_id, sign_policy,
    sign_record,
};

/// The owner admits nobody new once the roster has this many members (the
/// UI stops at 8).
pub const MAX_MEMBERS: usize = 12;
/// Wrong passwords an invite takes before it stops working.
pub const MAX_INVITE_FAILURES: u32 = 5;
/// Invites the owner keeps at once; making another drops the one closest
/// to expiring.
pub const MAX_INVITES: usize = 32;

/// The engine's handle for one accepted connection.
pub type ConnId = u64;

#[derive(Debug, Clone)]
pub struct Timings {
    /// Every connected peer gets a full snapshot this often.
    pub repair_every: Duration,
    /// Redial delay after a connection drops or the first dial fails;
    /// doubles with each failure.
    pub first_redial: Duration,
    pub max_redial: Duration,
    /// A member that said it is shutting down is dialed again after this;
    /// normally it dials us first when it starts.
    pub after_shutdown: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            repair_every: Duration::from_secs(30),
            first_redial: Duration::from_secs(1),
            max_redial: Duration::from_secs(60),
            after_shutdown: Duration::from_secs(60),
        }
    }
}

/// What the engine must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Connect to `peer` and introduce ourselves with `MemberHello`, then
    /// report `connected` or `dial_failed`.
    Dial { peer: MemberId, addr: PeerAddr },
    Send {
        conn: ConnId,
        message: ControlMessage,
    },
    /// Close after the messages already sent on it have gone out.
    Close { conn: ConnId, code: u32 },
    /// The saved state changed; persist [`Room::saved`] (before sending).
    Save,
    /// Something the UI shows changed.
    Changed,
    /// The owner's policy no longer lists us: banned, or removed (the owner
    /// closed the room). The room is over for us; forget it.
    Removed { banned: bool },
    /// We left (or closed the room) and the word got out: forget the room.
    Departed,
    /// A room restored from a backup has caught up with its members (or the
    /// owner stopped waiting): it may sign again ([`Room::catch_up_first`]).
    CaughtUp,
}

/// Who we are in a room: our key, and what our record says about us.
#[derive(Clone)]
pub struct Identity {
    pub key: SigningKey,
    pub name: String,
    pub addr: PeerAddr,
}

/// The owner's answer to a newcomer's `JoinStart`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinDecision {
    /// Run the password check with this invite's token. The invite is held
    /// for the check: end it with [`Room::admit`], [`Room::join_failed`] or
    /// [`Room::join_released`].
    Proceed { token: InviteToken },
    /// Close with this code.
    Refuse(u32),
    /// Refuse, showing the newcomer the owner's policy that bans it.
    Banned(Box<Signed<RoomPolicy>>),
}

/// A member's answer to another's `MemberHello`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberDecision {
    Accept,
    /// Close with `code`, after sending `proof` (the owner's policy showing
    /// why) if there is one. Only a former member or a banned one gets it:
    /// a stranger learns nothing about who is in the room.
    Refuse {
        code: u32,
        proof: Option<Box<Signed<RoomPolicy>>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoomError {
    #[error("only the room's owner can do that")]
    NotOwner,
    #[error("you were banned from this room")]
    Banned,
    #[error("you are not a member of this room")]
    NotMember,
    #[error("the room's policy doesn't check out")]
    BadPolicy,
    #[error("the owner can't ban themselves")]
    CantBanSelf,
    #[error("this room has too many bans")]
    TooManyBans,
    #[error("this room can't change any more")]
    Exhausted,
    #[error("names must be 1 to 96 bytes, with nothing invisible in them")]
    InvalidName,
    #[error(
        "this room was restored from a backup and is catching up with its members: try again once they have been online, or stop waiting"
    )]
    CatchingUp,
}

/// An invite the owner keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedInvite {
    pub token: InviteToken,
    /// Seconds since 1970 (UTC).
    pub expires: u64,
    pub failures: u32,
}

/// Who still has to hear that we left, or that we closed the room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leaving {
    /// The owner closing the room (everyone must be told), rather than a
    /// member leaving (one is enough: the rest hear it from them).
    pub closing: bool,
    pub untold: Vec<(MemberId, PeerAddr)>,
}

/// What a member remembers about a room between runs. The engine stores it
/// encrypted (DPAPI).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedRoom {
    pub owner: MemberId,
    pub nonce: RoomNonce,
    pub policy: Signed<RoomPolicy>,
    pub records: Vec<Signed<MemberRecord>>,
    /// The owner's outstanding invites; empty for everyone else.
    pub invites: Vec<SavedInvite>,
    pub leaving: Option<Leaving>,
}

/// How a member is connected, as the UI shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    Me,
    Connected,
    /// Not connected to us, but connected to someone we are connected to.
    Online,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberStatus {
    pub id: MemberId,
    pub name: String,
    pub is_owner: bool,
    pub link: LinkState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomStatus {
    pub id: RoomId,
    pub name: String,
    pub owner: MemberId,
    /// Members on the roster we have a record of, this member first.
    pub members: Vec<MemberStatus>,
}

#[derive(Debug, Default)]
struct Link {
    /// The connection in use and whether we dialed it.
    current: Option<(ConnId, bool)>,
    dialing: bool,
    failures: u32,
    next_dial: Option<Instant>,
    /// Who this peer last said it is connected to.
    reports: BTreeSet<MemberId>,
}

pub struct Room {
    key: SigningKey,
    me: MemberId,
    id: RoomId,
    owner: MemberId,
    nonce: RoomNonce,
    policy: Signed<RoomPolicy>,
    /// The latest record of each member on the roster, for its current stay;
    /// ours included.
    records: BTreeMap<MemberId, Signed<MemberRecord>>,
    links: BTreeMap<MemberId, Link>,
    /// Every accepted connection still open, current or not.
    conns: BTreeMap<ConnId, MemberId>,
    /// The owner's outstanding invites, by tag.
    invites: BTreeMap<InviteTag, SavedInvite>,
    /// Password checks running per invite; they count against its failures.
    checking: BTreeMap<InviteTag, u32>,
    leaving: Option<Leaving>,
    timings: Timings,
    next_repair: Instant,
    /// Over: removed, or left with the word out. The engine forgets it.
    gone: bool,
    /// Restored from a backup (M13): what we hold may be older than the
    /// room, so the owner admits, bans and removes nobody until it has
    /// heard every member's state (`heard`, see [`Room::catching_up_with`]).
    /// Signing from older state forks the room: a ban made after the
    /// backup, held by one absent member, could be outvoted by what the
    /// owner signs next (the review's finding). A member signs nothing that
    /// matters (its own records, which the room's newer ones supersede), so
    /// any snapshot catches it up.
    catching_up: bool,
    /// While catching up: members whose snapshot arrived this run.
    heard: BTreeSet<MemberId>,
    /// Restored from a backup: the next policy we sign skips ahead
    /// ([`RESTORE_SKIP`]).
    skip_next: bool,
}

/// How far a restored room's first policy skips ahead (M13): past whatever
/// the lost PC signed after the backup, which a member we never heard from
/// may hold (one admitted after it, say). That member then finds the room's
/// newer state instead of a second, different policy under the same
/// version: a change only it held is lost, but the room never forks. Covers
/// 65,536 changes after a backup, and as many restores.
pub const RESTORE_SKIP: u32 = 1 << 16;
/// The same for the record a member leaves with while catching up: its own
/// records on the lost PC may have gone on after the backup.
const RESTORE_RECORD_SKIP: u64 = 1 << 32;

impl std::fmt::Debug for Room {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Room")
            .field("id", &self.id)
            .field("me", &self.me)
            .field("version", &self.policy.body.version)
            .field("roster", &self.policy.body.roster.len())
            .field("conns", &self.conns.len())
            .field("leaving", &self.leaving.is_some())
            .finish_non_exhaustive()
    }
}

impl Room {
    /// A new room owned by `me`.
    pub fn create(
        me: Identity,
        nonce: RoomNonce,
        room_name: &str,
        timings: Timings,
        now: Instant,
    ) -> Result<Self, RoomError> {
        let Identity {
            key,
            name: my_name,
            addr: my_addr,
        } = me;
        if !valid_name(room_name) || !valid_name(&my_name) {
            return Err(RoomError::InvalidName);
        }
        let me = member_id(&key);
        let id = room_id(me, nonce);
        let policy = sign_policy(
            &key,
            RoomPolicy {
                room: id,
                version: 0,
                name: room_name.to_string(),
                roster: vec![RosterEntry { id: me, since: 0 }],
                banned: Vec::new(),
            },
        );
        let mut room = Self::empty(key, me, me, nonce, policy, timings, now);
        room.publish_mine(1, my_name, my_addr, false);
        Ok(room)
    }

    /// Joins with the snapshot the owner sent once it admitted us. `owner`
    /// and `nonce` come from the invite.
    pub fn join(
        me: Identity,
        owner: MemberId,
        nonce: RoomNonce,
        snapshot: RoomSnapshot,
        timings: Timings,
        now: Instant,
    ) -> Result<Self, RoomError> {
        let Identity {
            key,
            name: my_name,
            addr: my_addr,
        } = me;
        if !valid_name(&my_name) {
            return Err(RoomError::InvalidName);
        }
        let me = member_id(&key);
        let id = room_id(owner, nonce);
        check_policy(&snapshot.policy, id, owner)?;
        if snapshot.policy.body.banned.contains(&me) {
            return Err(RoomError::Banned);
        }
        if !snapshot.policy.body.lists(me) {
            return Err(RoomError::NotMember);
        }
        let mut room = Self::empty(key, me, owner, nonce, snapshot.policy, timings, now);
        room.adopt_records(snapshot.members, now);
        // Above any record of ours this stay already has.
        let version = room.records.get(&me).map_or(1, |r| r.body.version + 1);
        room.publish_mine(version, my_name, my_addr, false);
        Ok(room)
    }

    /// A remembered room, checked again as if it came from a peer.
    pub fn restore(
        key: SigningKey,
        saved: SavedRoom,
        timings: Timings,
        now: Instant,
    ) -> Result<Self, RoomError> {
        let me = member_id(&key);
        let id = room_id(saved.owner, saved.nonce);
        check_policy(&saved.policy, id, saved.owner)?;
        if saved.policy.body.banned.contains(&me) {
            return Err(RoomError::Banned);
        }
        // An owner closing the room is no longer on its (empty) roster.
        if !saved.policy.body.lists(me) && saved.leaving.is_none() {
            return Err(RoomError::NotMember);
        }
        let mut room = Self::empty(
            key,
            me,
            saved.owner,
            saved.nonce,
            saved.policy,
            timings,
            now,
        );
        room.adopt_records(saved.records, now);
        if saved.leaving.is_none() && !room.records.contains_key(&me) {
            return Err(RoomError::BadPolicy);
        }
        if room.is_owner() {
            room.invites = saved
                .invites
                .into_iter()
                .map(|invite| (invite_tag(&invite.token), invite))
                .collect();
        }
        if let Some(leaving) = saved.leaving {
            for (peer, _) in &leaving.untold {
                room.links.entry(*peer).or_default().next_dial = Some(now);
            }
            room.leaving = Some(leaving);
        }
        Ok(room)
    }

    fn empty(
        key: SigningKey,
        me: MemberId,
        owner: MemberId,
        nonce: RoomNonce,
        policy: Signed<RoomPolicy>,
        timings: Timings,
        now: Instant,
    ) -> Self {
        Self {
            key,
            me,
            id: policy.body.room,
            owner,
            nonce,
            policy,
            records: BTreeMap::new(),
            links: BTreeMap::new(),
            conns: BTreeMap::new(),
            invites: BTreeMap::new(),
            checking: BTreeMap::new(),
            leaving: None,
            next_repair: now + timings.repair_every,
            timings,
            gone: false,
            catching_up: false,
            heard: BTreeSet::new(),
            skip_next: false,
        }
    }

    /// The room was restored from a backup (M13): catch up with the members
    /// before signing, then sign ahead of anything since the backup
    /// ([`RESTORE_SKIP`]). A room with nobody else on its roster has no one
    /// to catch up with: it is caught up at once, its owner signing the room
    /// again ahead (whoever the lost PC let in after the backup finds it,
    /// and is no longer listed). The effects are applied as usual
    /// ([`Effect::CaughtUp`] among them, if so).
    pub fn catch_up_first(&mut self) -> Vec<Effect> {
        self.catching_up = true;
        self.skip_next = true;
        self.heard.clear();
        let mut fx = Vec::new();
        if self.catching_up_with().is_empty() {
            self.caught_up(&mut fx);
        }
        fx
    }

    pub fn is_catching_up(&self) -> bool {
        self.catching_up
    }

    /// While catching up, the members not heard from yet: everyone on the
    /// roster as we know it now (a newer policy may have dropped someone
    /// banned after the backup), but us and whoever left. A member catches
    /// up from anyone, so for one this is empty or just the first.
    pub fn catching_up_with(&self) -> Vec<MemberId> {
        if !self.catching_up {
            return Vec::new();
        }
        self.policy
            .body
            .roster
            .iter()
            .map(|e| e.id)
            .filter(|id| {
                *id != self.me
                    && !self.heard.contains(id)
                    && !self.records.get(id).is_some_and(|r| r.body.left)
            })
            .collect()
    }

    /// Owner only, the user's choice: stop waiting for members who may
    /// never come back, and sign from what we hold. A change made after the
    /// backup that only they hold can then be undone (a ban among them).
    pub fn stop_catching_up(&mut self) -> Result<Vec<Effect>, RoomError> {
        if !self.is_owner() {
            return Err(RoomError::NotOwner);
        }
        let mut fx = Vec::new();
        if self.catching_up {
            self.caught_up(&mut fx);
        }
        Ok(fx)
    }

    /// Verified records of the current stays from a snapshot or saved state;
    /// everyone else gets dialed straight away.
    fn adopt_records(&mut self, records: Vec<Signed<MemberRecord>>, now: Instant) {
        for record in records {
            let body = &record.body;
            if body.room != self.id
                || !self.current_stay(body)
                || !body.is_valid()
                || !record_is_authentic(&record)
            {
                continue;
            }
            if self
                .records
                .get(&body.id)
                .is_none_or(|old| old.body.version < body.version)
            {
                self.records.insert(body.id, record);
            }
        }
        let peers: Vec<MemberId> = self.active_peers().collect();
        for peer in peers {
            self.links.entry(peer).or_default().next_dial = Some(now);
        }
    }

    // ---- Queries ----

    pub fn id(&self) -> RoomId {
        self.id
    }

    pub fn me(&self) -> MemberId {
        self.me
    }

    pub fn owner(&self) -> MemberId {
        self.owner
    }

    pub fn is_owner(&self) -> bool {
        self.me == self.owner
    }

    pub fn name(&self) -> &str {
        &self.policy.body.name
    }

    pub fn version(&self) -> u32 {
        self.policy.body.version
    }

    /// Leaving (or closing), still getting the word out. The UI no longer
    /// shows the room.
    pub fn is_leaving(&self) -> bool {
        self.leaving.is_some()
    }

    /// The owner's current policy: sent with `MemberHello`.
    pub fn policy(&self) -> &Signed<RoomPolicy> {
        &self.policy
    }

    pub fn saved(&self) -> SavedRoom {
        SavedRoom {
            owner: self.owner,
            nonce: self.nonce,
            policy: self.policy.clone(),
            records: self.records.values().cloned().collect(),
            invites: self.invites.values().cloned().collect(),
            leaving: self.leaving.clone(),
        }
    }

    pub fn is_banned(&self, member: MemberId) -> bool {
        self.policy.body.banned.contains(&member)
    }

    fn in_roster(&self, member: MemberId) -> bool {
        self.policy.body.lists(member)
    }

    /// Whether `record` belongs to its member's current stay.
    fn current_stay(&self, record: &MemberRecord) -> bool {
        self.policy.body.since(record.id) == Some(record.since)
    }

    /// On the roster, not banned, and not on the way out.
    pub fn is_member(&self, member: MemberId) -> bool {
        self.in_roster(member)
            && !self.is_banned(member)
            && self.records.get(&member).is_none_or(|r| !r.body.left)
    }

    fn active_peers(&self) -> impl Iterator<Item = MemberId> + '_ {
        self.records
            .keys()
            .copied()
            .filter(|id| *id != self.me && self.is_member(*id))
    }

    pub fn status(&self) -> RoomStatus {
        let online: BTreeSet<MemberId> = self
            .links
            .values()
            .filter(|l| l.current.is_some())
            .flat_map(|l| l.reports.iter().copied())
            .collect();
        let mut members: Vec<MemberStatus> = self
            .records
            .values()
            .filter(|r| r.body.id == self.me || self.is_member(r.body.id))
            .map(|r| {
                let id = r.body.id;
                let link = if id == self.me {
                    LinkState::Me
                } else if self.links.get(&id).is_some_and(|l| l.current.is_some()) {
                    LinkState::Connected
                } else if online.contains(&id) {
                    LinkState::Online
                } else {
                    LinkState::Offline
                };
                MemberStatus {
                    id,
                    name: r.body.name.clone(),
                    is_owner: id == self.owner,
                    link,
                }
            })
            .collect();
        members.sort_by_key(|m| (m.link != LinkState::Me, m.name.to_lowercase(), m.id));
        RoomStatus {
            id: self.id,
            name: self.policy.body.name.clone(),
            owner: self.owner,
            members,
        }
    }

    /// The connection in use per peer, and whether we dialed it.
    pub fn connections(&self) -> Vec<(MemberId, ConnId, bool)> {
        self.links
            .iter()
            .filter_map(|(peer, l)| l.current.map(|(conn, mine)| (*peer, conn, mine)))
            .collect()
    }

    // ---- Invites and joining (the owner's side) ----

    /// Owner only: a new invite with this token, good until `now_unix +
    /// valid_for` seconds. Without `allow_ips` (relays are configured) it
    /// names our relay only, never an IP address.
    pub fn create_invite(
        &mut self,
        token: InviteToken,
        now_unix: u64,
        valid_for: u64,
        allow_ips: bool,
    ) -> Result<(Invite, Vec<Effect>), RoomError> {
        if !self.is_owner() || self.leaving.is_some() || self.gone {
            return Err(RoomError::NotOwner);
        }
        self.invites
            .retain(|_, i| i.expires > now_unix && i.failures < MAX_INVITE_FAILURES);
        while self.invites.len() >= MAX_INVITES {
            let soonest = self
                .invites
                .iter()
                .min_by_key(|(_, i)| i.expires)
                .map(|(tag, _)| *tag)
                .expect("not empty");
            self.invites.remove(&soonest);
        }
        let expires = now_unix.saturating_add(valid_for);
        self.invites.insert(
            invite_tag(&token),
            SavedInvite {
                token,
                expires,
                failures: 0,
            },
        );
        let addr = &self.records[&self.me].body.addr;
        let addr = match (&addr.relay, allow_ips) {
            (Some(relay), _) => PeerAddr {
                relay: Some(relay.clone()),
                ips: Vec::new(),
            },
            (None, true) => addr.clone(),
            (None, false) => PeerAddr::default(),
        };
        let invite = Invite {
            owner: self.owner,
            nonce: self.nonce,
            name: self.policy.body.name.clone(),
            token,
            expires,
            addr,
        };
        Ok((invite, vec![Effect::Save]))
    }

    /// Owner only: every outstanding invite stops working.
    pub fn revoke_invites(&mut self) -> Result<Vec<Effect>, RoomError> {
        if !self.is_owner() {
            return Err(RoomError::NotOwner);
        }
        self.invites.clear();
        Ok(vec![Effect::Save])
    }

    /// A usable invite: known, not expired, and not worn out counting the
    /// checks still running on it.
    fn usable_invite(&self, invite: InviteTag, now_unix: u64) -> Option<&SavedInvite> {
        let running = self.checking.get(&invite).copied().unwrap_or(0);
        self.invites
            .get(&invite)
            .filter(|i| i.expires > now_unix && i.failures + running < MAX_INVITE_FAILURES)
    }

    /// How the owner answers a newcomer's `JoinStart`. Rate limits are
    /// checked by the engine before this. On `Proceed` the invite is held
    /// for the check, so parallel guesses can't outnumber its allowance.
    pub fn join_request(
        &mut self,
        dialer: MemberId,
        invite: InviteTag,
        now_unix: u64,
    ) -> JoinDecision {
        if self.gone || self.leaving.is_some() {
            return JoinDecision::Refuse(close_code::UNKNOWN_ROOM);
        }
        if !self.is_owner() || dialer == self.me {
            return JoinDecision::Refuse(close_code::PROTOCOL);
        }
        if self.is_banned(dialer) {
            return JoinDecision::Banned(Box::new(self.policy.clone()));
        }
        let Some(token) = self.usable_invite(invite, now_unix).map(|i| i.token) else {
            return JoinDecision::Refuse(close_code::INVITE_EXPIRED);
        };
        if self.room_full(dialer) {
            return JoinDecision::Refuse(close_code::ROOM_FULL);
        }
        *self.checking.entry(invite).or_default() += 1;
        JoinDecision::Proceed { token }
    }

    fn room_full(&self, newcomer: MemberId) -> bool {
        !self.in_roster(newcomer) && self.policy.body.roster.len() >= MAX_MEMBERS
    }

    fn release(&mut self, invite: InviteTag) {
        if let Some(running) = self.checking.get_mut(&invite) {
            *running = running.saturating_sub(1);
            if *running == 0 {
                self.checking.remove(&invite);
            }
        }
    }

    /// A newcomer's password check with this invite failed or was abandoned.
    /// After [`MAX_INVITE_FAILURES`] the invite stops working.
    pub fn join_failed(&mut self, invite: InviteTag) -> Vec<Effect> {
        self.release(invite);
        let Some(state) = self.invites.get_mut(&invite) else {
            return Vec::new();
        };
        state.failures += 1;
        if state.failures >= MAX_INVITE_FAILURES {
            self.invites.remove(&invite);
        }
        vec![Effect::Save]
    }

    /// A check [`Room::join_request`] allowed never ran (whoever asked is
    /// gone): the invite is no longer held for it.
    pub fn join_released(&mut self, invite: InviteTag) {
        self.release(invite);
    }

    /// Owner only: `member` passed the password check with `invite`. Checked
    /// again, since things may have changed during the check (a ban, the
    /// invite revoked or expired, the room filling up); on success the member
    /// goes on the roster. Report the connection with `connected` afterwards.
    /// `Err` is the close code.
    pub fn admit(
        &mut self,
        member: MemberId,
        invite: InviteTag,
        now_unix: u64,
    ) -> Result<Vec<Effect>, u32> {
        self.release(invite);
        if !self.is_owner() {
            return Err(close_code::PROTOCOL);
        }
        if self.gone || self.leaving.is_some() {
            return Err(close_code::UNKNOWN_ROOM);
        }
        if self.is_banned(member) {
            return Err(close_code::BANNED);
        }
        // Restored from a backup and not caught up: signing now could reuse
        // a version the members already hold (M13).
        if self.catching_up {
            return Err(close_code::UNKNOWN_ROOM);
        }
        if self.usable_invite(invite, now_unix).is_none() {
            return Err(close_code::INVITE_EXPIRED);
        }
        if self.room_full(member) {
            return Err(close_code::ROOM_FULL);
        }
        if self.in_roster(member) {
            return Ok(Vec::new());
        }
        let Some(version) = self.policy.body.version.checked_add(1) else {
            return Err(close_code::ROOM_FULL);
        };
        let mut roster = self.policy.body.roster.clone();
        roster.push(RosterEntry {
            id: member,
            since: version,
        });
        let banned = self.policy.body.banned.clone();
        self.issue_policy(roster, banned)
            .map_err(|_| close_code::ROOM_FULL)
    }

    // ---- Members meeting each other ----

    /// How to answer a member's `MemberHello`, which carries the newest
    /// policy the dialer has. A newer one is adopted first, so a member who
    /// missed a change (someone joined meanwhile) can still decide.
    pub fn member_hello(
        &mut self,
        dialer: MemberId,
        policy: Signed<RoomPolicy>,
    ) -> (MemberDecision, Vec<Effect>) {
        let mut fx = Vec::new();
        // A former member, by the owner's own word, may see why it's refused;
        // a stranger may not even see who is in the room.
        let former =
            policy.body.lists(dialer) && check_policy(&policy, self.id, self.owner).is_ok();
        if self.leaving.is_none() && !self.gone {
            self.merge_policy(policy, None, &mut fx);
        }
        let refuse = |code, proof: Option<&Signed<RoomPolicy>>| MemberDecision::Refuse {
            code,
            proof: proof.map(|p| Box::new(p.clone())),
        };
        let decision = if self.gone {
            refuse(close_code::UNKNOWN_ROOM, None)
        } else if dialer == self.me {
            refuse(close_code::PROTOCOL, None)
        } else if let Some(leaving) = &self.leaving {
            if leaving.closing {
                // The room is closed: tell a former member, once.
                if former {
                    let proof = refuse(close_code::NOT_MEMBER, Some(&self.policy));
                    self.mark_told(dialer, true, &mut fx);
                    proof
                } else {
                    refuse(close_code::UNKNOWN_ROOM, None)
                }
            } else if self.in_roster(dialer) {
                // Let it in, so it hears that we are leaving.
                MemberDecision::Accept
            } else {
                refuse(close_code::UNKNOWN_ROOM, None)
            }
        } else if self.is_banned(dialer) {
            refuse(close_code::BANNED, former.then_some(&self.policy))
        } else if !self.is_member(dialer) {
            refuse(close_code::NOT_MEMBER, former.then_some(&self.policy))
        } else {
            MemberDecision::Accept
        };
        (decision, fx)
    }

    /// A member we dialed refused us with `code`, perhaps showing the owner's
    /// policy. If that no longer lists us, the room is over for us.
    pub fn refused(
        &mut self,
        peer: MemberId,
        code: u32,
        proof: Option<Signed<RoomPolicy>>,
        now: Instant,
    ) -> Vec<Effect> {
        let mut fx = Vec::new();
        if self.gone {
            return fx;
        }
        let Some(leaving) = &self.leaving else {
            if let Some(policy) = proof {
                self.merge_policy(policy, None, &mut fx);
            }
            if !self.gone {
                self.dial_failed(peer, now);
            }
            return fx;
        };
        // Answers that come from the peer's room, after it read our hello
        // (unlike a busy or rate-limited refusal, which come before).
        let answered = proof.is_some()
            || matches!(
                code,
                close_code::UNKNOWN_ROOM | close_code::NOT_MEMBER | close_code::BANNED
            );
        let proven = proof.is_some_and(|p| check_policy(&p, self.id, self.owner).is_ok());
        if leaving.closing && answered {
            // Our hello carried the closing policy: it has seen it (or has no
            // such room).
            self.mark_told(peer, true, &mut fx);
        } else if leaving.closing {
            self.dial_failed(peer, now);
        } else if proven {
            // It already knows we are out.
            self.mark_told(peer, true, &mut fx);
        } else if code == close_code::UNKNOWN_ROOM {
            // It isn't in the room any more: nobody to tell there.
            self.mark_told(peer, false, &mut fx);
        } else {
            self.dial_failed(peer, now);
        }
        fx
    }

    // ---- Events ----

    /// A connection with `peer` was accepted, by us or by them.
    pub fn connected(&mut self, peer: MemberId, conn: ConnId, dialed_by_me: bool) -> Vec<Effect> {
        let mut fx = Vec::new();
        if dialed_by_me && let Some(link) = self.links.get_mut(&peer) {
            link.dialing = false;
        }
        if self.gone || peer == self.me {
            let code = if self.gone {
                close_code::LEFT_ROOM
            } else {
                close_code::PROTOCOL
            };
            fx.push(Effect::Close { conn, code });
            return fx;
        }
        if self.leaving.is_some() {
            // Our last word, then goodbye.
            fx.extend(self.final_word(conn));
            fx.push(Effect::Close {
                conn,
                code: close_code::LEFT_ROOM,
            });
            self.mark_told(peer, true, &mut fx);
            return fx;
        }
        if !self.is_member(peer) {
            // Removed while the connection was being made.
            fx.push(Effect::Send {
                conn,
                message: self.policy_message(),
            });
            fx.push(Effect::Close {
                conn,
                code: self.removal_code(peer),
            });
            return fx;
        }
        self.conns.insert(conn, peer);
        let lower_is_me = self.me < peer;
        let link = self.links.entry(peer).or_default();
        link.failures = 0;
        link.next_dial = None;
        let keep_new = match link.current {
            None => true,
            // Crossed dials: both sides keep the one the lower id dialed.
            Some((_, old_mine)) if old_mine != dialed_by_me => dialed_by_me == lower_is_me,
            // The same side dialed again: it gave up on the old one.
            Some(_) => true,
        };
        if keep_new {
            if let Some((old, _)) = link.current.replace((conn, dialed_by_me)) {
                fx.push(Effect::Close {
                    conn: old,
                    code: close_code::DUPLICATE,
                });
            }
            fx.push(Effect::Send {
                conn,
                message: ControlMessage::RoomSnapshot(Box::new(self.snapshot())),
            });
            self.presence_changed(&mut fx);
        } else {
            fx.push(Effect::Close {
                conn,
                code: close_code::DUPLICATE,
            });
        }
        fx
    }

    /// An accepted connection closed, for whatever reason.
    pub fn disconnected(&mut self, conn: ConnId, now: Instant) -> Vec<Effect> {
        let mut fx = Vec::new();
        let Some(peer) = self.conns.remove(&conn) else {
            return fx;
        };
        let redial = self.should_dial(peer);
        let first_redial = self.timings.first_redial;
        let Some(link) = self.links.get_mut(&peer) else {
            return fx;
        };
        if link.current.is_some_and(|(c, _)| c == conn) {
            link.current = None;
            link.reports.clear();
            if redial && link.next_dial.is_none() {
                link.next_dial = Some(now + first_redial);
            }
            self.presence_changed(&mut fx);
        }
        fx
    }

    /// A dial didn't produce an accepted connection.
    pub fn dial_failed(&mut self, peer: MemberId, now: Instant) {
        let redial = self.should_dial(peer);
        let timings = self.timings.clone();
        let link = self.links.entry(peer).or_default();
        link.dialing = false;
        link.failures = link.failures.saturating_add(1);
        if redial && link.current.is_none() {
            let doublings = (link.failures - 1).min(16);
            let delay = timings
                .first_redial
                .saturating_mul(1 << doublings)
                .min(timings.max_redial);
            link.next_dial = Some(now + delay);
        }
    }

    /// A control message on an accepted connection. Messages that aren't
    /// about the room are ignored here, and so is anything from a peer that
    /// is being cut off.
    pub fn received(&mut self, conn: ConnId, message: ControlMessage, now: Instant) -> Vec<Effect> {
        let mut fx = Vec::new();
        let Some(&from) = self.conns.get(&conn) else {
            return fx;
        };
        if self.gone || self.leaving.is_some() || !self.is_member(from) {
            return fx;
        }
        match message {
            ControlMessage::RoomSnapshot(snapshot) => {
                let RoomSnapshot { policy, members } = *snapshot;
                self.merge_policy(policy, Some(from), &mut fx);
                if self.gone {
                    return fx;
                }
                for record in members {
                    self.merge_record(record, Some(from), now, &mut fx);
                }
                // A member's whole view. An owner caught up with everyone
                // holds anything newer it signed before the backup was
                // restored; a member, from anyone.
                if self.catching_up {
                    self.heard.insert(from);
                    if !self.is_owner() || self.catching_up_with().is_empty() {
                        self.caught_up(&mut fx);
                    }
                }
            }
            ControlMessage::MemberUpdate(record) => {
                self.merge_record(*record, Some(from), now, &mut fx);
                // Someone we waited for may have left.
                if self.catching_up && self.is_owner() && self.catching_up_with().is_empty() {
                    self.caught_up(&mut fx);
                }
            }
            ControlMessage::PolicyUpdate(policy) => {
                self.merge_policy(*policy, Some(from), &mut fx);
                // A newer roster may have dropped someone we waited for.
                if self.catching_up && self.is_owner() && self.catching_up_with().is_empty() {
                    self.caught_up(&mut fx);
                }
            }
            ControlMessage::Presence(presence) => {
                // Kept even for ids whose record hasn't arrived yet: a
                // presence can overtake the record it mentions. Only
                // members are ever shown or dialed.
                let reports: BTreeSet<MemberId> = presence
                    .connected
                    .into_iter()
                    .take(couch_proto::room::MAX_ROSTER)
                    .collect();
                let first_redial = self.timings.first_redial;
                if let Some(link) = self.links.get_mut(&from)
                    && link.reports != reports
                {
                    link.reports = reports;
                    fx.push(Effect::Changed);
                }
                // Someone reachable that we aren't connected to: try now
                // rather than at the end of a long backoff.
                let missing: Vec<MemberId> = self
                    .links
                    .get(&from)
                    .map(|l| l.reports.iter().copied().collect())
                    .unwrap_or_default();
                for id in missing {
                    if !self.should_dial(id) {
                        continue;
                    }
                    let link = self.links.entry(id).or_default();
                    if link.current.is_none()
                        && !link.dialing
                        && link.next_dial.is_some_and(|t| t > now + first_redial)
                    {
                        link.next_dial = Some(now);
                    }
                }
            }
            ControlMessage::Goodbye(goodbye) => {
                let after_shutdown = self.timings.after_shutdown;
                if let Some(link) = self.links.get_mut(&from) {
                    link.next_dial = match goodbye.reason {
                        GoodbyeReason::LeftRoom => None,
                        GoodbyeReason::Shutdown => Some(now + after_shutdown),
                    };
                }
            }
            _ => {}
        }
        fx
    }

    /// Dials whoever is due, and sends the periodic repair snapshot.
    pub fn tick(&mut self, now: Instant) -> Vec<Effect> {
        let mut fx = Vec::new();
        if self.gone {
            return fx;
        }
        let targets: Vec<(MemberId, PeerAddr)> = match &self.leaving {
            Some(leaving) => leaving.untold.clone(),
            None => self
                .active_peers()
                .map(|peer| (peer, self.records[&peer].body.addr.clone()))
                .collect(),
        };
        for (peer, addr) in targets {
            let link = self.links.entry(peer).or_default();
            if link.current.is_none() && !link.dialing && link.next_dial.is_some_and(|t| t <= now) {
                link.dialing = true;
                link.next_dial = None;
                fx.push(Effect::Dial { peer, addr });
            }
        }
        if now >= self.next_repair && self.leaving.is_none() {
            self.next_repair = now + self.timings.repair_every;
            let snapshot = ControlMessage::RoomSnapshot(Box::new(self.snapshot()));
            let presence = self.presence();
            for conn in self.current_conns() {
                fx.push(Effect::Send {
                    conn,
                    message: snapshot.clone(),
                });
                fx.push(Effect::Send {
                    conn,
                    message: presence.clone(),
                });
            }
        }
        fx
    }

    // ---- Our own changes ----

    /// Our display name or addresses changed.
    pub fn set_profile(&mut self, name: &str, addr: PeerAddr) -> Result<Vec<Effect>, RoomError> {
        if !valid_name(name) {
            return Err(RoomError::InvalidName);
        }
        let addr = trim_addr(addr);
        if self.gone || self.leaving.is_some() {
            return Ok(Vec::new());
        }
        let mine = &self.records[&self.me].body;
        if mine.name == name && mine.addr == addr {
            return Ok(Vec::new());
        }
        let version = mine.version + 1;
        let record = self.publish_mine(version, name.to_string(), addr, false);
        let mut fx = vec![Effect::Save];
        fx.extend(self.broadcast(ControlMessage::MemberUpdate(Box::new(record)), None));
        fx.push(Effect::Changed);
        Ok(fx)
    }

    /// Owner only: takes `member` out of the room for good, whether it is in
    /// it now or not. Every invite is revoked too, since the banned member
    /// may hold one.
    pub fn ban(&mut self, member: MemberId) -> Result<Vec<Effect>, RoomError> {
        if !self.is_owner() || self.gone || self.leaving.is_some() {
            return Err(RoomError::NotOwner);
        }
        if member == self.me {
            return Err(RoomError::CantBanSelf);
        }
        if self.catching_up {
            return Err(RoomError::CatchingUp);
        }
        let roster: Vec<RosterEntry> = self
            .policy
            .body
            .roster
            .iter()
            .copied()
            .filter(|e| e.id != member)
            .collect();
        let mut banned = self.policy.body.banned.clone();
        if !banned.contains(&member) {
            if banned.len() >= MAX_BANNED {
                return Err(RoomError::TooManyBans);
            }
            banned.push(member);
        }
        self.invites.clear();
        self.issue_policy(roster, banned)
    }

    /// We leave for good. A member tells whoever it is connected to, or keeps
    /// trying until one member has heard (the rest hear it from them); the
    /// owner then takes it off the roster. The owner leaving closes the room:
    /// the roster empties, and every former member is told, now or when it
    /// next reaches us. [`Effect::Departed`] says when the room can go.
    pub fn leave(&mut self, now: Instant) -> Result<Vec<Effect>, RoomError> {
        if self.gone || self.leaving.is_some() {
            return Ok(Vec::new());
        }
        // Allowed while catching up (M13): a room whose members never come
        // back must still be leavable. The closing policy, or our last
        // record, skips ahead of whatever the members may hold that is
        // newer than ours, so it isn't ignored.
        let others: Vec<(MemberId, PeerAddr)> = self
            .records
            .values()
            .filter(|r| r.body.id != self.me && self.is_member(r.body.id))
            .map(|r| (r.body.id, r.body.addr.clone()))
            .collect();
        let closing = self.is_owner();
        if closing {
            let version = self.next_policy_version()?;
            self.policy = sign_policy(
                &self.key,
                RoomPolicy {
                    room: self.id,
                    version,
                    name: self.policy.body.name.clone(),
                    roster: Vec::new(),
                    banned: self.policy.body.banned.clone(),
                },
            );
            self.invites.clear();
        } else {
            let mine = self.records[&self.me].body.clone();
            let step = if self.catching_up {
                RESTORE_RECORD_SKIP
            } else {
                1
            };
            let version = mine.version.checked_add(step).ok_or(RoomError::Exhausted)?;
            self.publish_mine(version, mine.name, mine.addr, true);
        }
        self.leaving = Some(Leaving {
            closing,
            untold: others,
        });
        let mut fx = vec![Effect::Save];
        let current: Vec<(MemberId, ConnId)> = self
            .connections()
            .into_iter()
            .map(|(peer, conn, _)| (peer, conn))
            .collect();
        for (_, conn) in &current {
            fx.extend(self.final_word(*conn));
        }
        for conn in std::mem::take(&mut self.conns).into_keys() {
            fx.push(Effect::Close {
                conn,
                code: close_code::LEFT_ROOM,
            });
        }
        for link in self.links.values_mut() {
            link.current = None;
            link.reports.clear();
            link.dialing = false;
            link.failures = 0;
            link.next_dial = None;
        }
        for (peer, _) in current {
            self.mark_told(peer, true, &mut fx);
        }
        if self.gone {
            return Ok(fx);
        }
        // Whoever we didn't reach, we keep dialing until they hear it.
        if let Some(leaving) = &self.leaving {
            if leaving.untold.is_empty() {
                self.gone = true;
                fx.push(Effect::Departed);
                return Ok(fx);
            }
            for (peer, _) in &leaving.untold {
                self.links.entry(*peer).or_default().next_dial = Some(now);
            }
        }
        Ok(fx)
    }

    /// Our app is closing: say so on every connection.
    pub fn shutdown(&self) -> Vec<Effect> {
        let mut fx = Vec::new();
        for conn in self.current_conns() {
            fx.push(Effect::Send {
                conn,
                message: ControlMessage::Goodbye(Goodbye {
                    reason: GoodbyeReason::Shutdown,
                }),
            });
        }
        for conn in self.conns.keys() {
            fx.push(Effect::Close {
                conn: *conn,
                code: close_code::NORMAL,
            });
        }
        fx
    }

    // ---- Internals ----

    fn should_dial(&self, peer: MemberId) -> bool {
        if self.gone || peer == self.me {
            return false;
        }
        match &self.leaving {
            Some(leaving) => leaving.untold.iter().any(|(id, _)| *id == peer),
            None => self.is_member(peer),
        }
    }

    /// What a leaving member or closing owner says on `conn`.
    fn final_word(&self, conn: ConnId) -> Vec<Effect> {
        let closing = self.leaving.as_ref().is_some_and(|l| l.closing);
        if closing {
            return vec![Effect::Send {
                conn,
                message: self.policy_message(),
            }];
        }
        // A snapshot, which carries our record saying we left: a member who
        // dialed us expects one first.
        vec![
            Effect::Send {
                conn,
                message: ControlMessage::RoomSnapshot(Box::new(self.snapshot())),
            },
            Effect::Send {
                conn,
                message: ControlMessage::Goodbye(Goodbye {
                    reason: GoodbyeReason::LeftRoom,
                }),
            },
        ]
    }

    /// `peer` has heard that we are leaving (`heard`), or has no such room
    /// to hear it in. A leaving member needs only one member to have heard,
    /// since the rest hear it from them; a closing owner needs everyone.
    fn mark_told(&mut self, peer: MemberId, heard: bool, fx: &mut Vec<Effect>) {
        if self.gone {
            return;
        }
        let Some(leaving) = &mut self.leaving else {
            return;
        };
        if heard && !leaving.closing {
            leaving.untold.clear();
        } else {
            leaving.untold.retain(|(id, _)| *id != peer);
        }
        if leaving.untold.is_empty() {
            self.gone = true;
            fx.push(Effect::Departed);
        } else {
            fx.push(Effect::Save);
        }
    }

    fn removal_code(&self, peer: MemberId) -> u32 {
        if self.is_banned(peer) {
            close_code::BANNED
        } else {
            close_code::NOT_MEMBER
        }
    }

    fn snapshot(&self) -> RoomSnapshot {
        RoomSnapshot {
            policy: self.policy.clone(),
            members: self.records.values().cloned().collect(),
        }
    }

    fn policy_message(&self) -> ControlMessage {
        ControlMessage::PolicyUpdate(Box::new(self.policy.clone()))
    }

    fn presence(&self) -> ControlMessage {
        ControlMessage::Presence(Presence {
            connected: self.connections().into_iter().map(|(p, _, _)| p).collect(),
        })
    }

    fn current_conns(&self) -> Vec<ConnId> {
        self.links
            .values()
            .filter_map(|l| l.current.map(|(conn, _)| conn))
            .collect()
    }

    fn broadcast(&self, message: ControlMessage, except: Option<MemberId>) -> Vec<Effect> {
        self.links
            .iter()
            .filter(|(peer, _)| Some(**peer) != except)
            .filter_map(|(_, l)| l.current.map(|(conn, _)| conn))
            .map(|conn| Effect::Send {
                conn,
                message: message.clone(),
            })
            .collect()
    }

    fn presence_changed(&self, fx: &mut Vec<Effect>) {
        fx.extend(self.broadcast(self.presence(), None));
        fx.push(Effect::Changed);
    }

    /// Signs and stores a new record of ours, for our current stay; returns
    /// it to send.
    fn publish_mine(
        &mut self,
        version: u64,
        name: String,
        addr: PeerAddr,
        left: bool,
    ) -> Signed<MemberRecord> {
        let since = self.policy.body.since(self.me).unwrap_or_default();
        let record = sign_record(
            &self.key,
            MemberRecord {
                room: self.id,
                id: self.me,
                since,
                version,
                name,
                addr: trim_addr(addr),
                left,
            },
        );
        self.records.insert(self.me, record.clone());
        record
    }

    fn merge_record(
        &mut self,
        record: Signed<MemberRecord>,
        from: Option<MemberId>,
        now: Instant,
        fx: &mut Vec<Effect>,
    ) {
        // Cheap checks first: signatures are only verified for news about
        // members' current stays.
        let body = &record.body;
        if body.room != self.id || !self.current_stay(body) {
            return;
        }
        let peer = body.id;
        if peer == self.me {
            // The room holds a record of ours at least as new as our own
            // (from before a crash): supersede it.
            let mine = &self.records[&self.me];
            if body.version >= mine.body.version
                && *body != mine.body
                && body.is_valid()
                && record_is_authentic(&record)
            {
                let (name, addr) = (mine.body.name.clone(), mine.body.addr.clone());
                let republished = self.publish_mine(body.version + 1, name, addr, false);
                fx.push(Effect::Save);
                fx.extend(
                    self.broadcast(ControlMessage::MemberUpdate(Box::new(republished)), None),
                );
                fx.push(Effect::Changed);
            }
            return;
        }
        let old = self.records.get(&peer);
        if old.is_some_and(|old| old.body.version >= body.version)
            || !body.is_valid()
            || !record_is_authentic(&record)
        {
            return;
        }
        let was_member = self.is_member(peer);
        let moved = old.is_none_or(|old| old.body.addr != body.addr);
        let left = body.left;
        self.records.insert(peer, record.clone());
        fx.push(Effect::Save);
        fx.extend(self.broadcast(ControlMessage::MemberUpdate(Box::new(record)), from));
        if left {
            if let Some(link) = self.links.get_mut(&peer) {
                link.next_dial = None;
            }
            // The owner takes a departing member off the roster; catching
            // up, once caught up (`caught_up`).
            if self.is_owner() && !self.catching_up {
                let roster: Vec<RosterEntry> = self
                    .policy
                    .body
                    .roster
                    .iter()
                    .copied()
                    .filter(|e| e.id != peer)
                    .collect();
                let banned = self.policy.body.banned.clone();
                if let Ok(more) = self.issue_policy(roster, banned) {
                    fx.extend(more);
                }
            }
        } else if self.should_dial(peer) {
            let link = self.links.entry(peer).or_default();
            if link.current.is_none() && !link.dialing && (moved || !was_member) {
                link.failures = 0;
                link.next_dial = Some(now);
            }
        }
        fx.push(Effect::Changed);
    }

    /// Caught up after a restore (M13): signing may resume. The owner signs
    /// the room again at once, [`RESTORE_SKIP`] ahead, taking off the roster
    /// whoever left meanwhile: saved before [`Effect::CaughtUp`] clears the
    /// mark, so a restart never loses the skip, and from then on whatever
    /// the lost PC signed after the backup is superseded wherever it is
    /// held, never forked.
    fn caught_up(&mut self, fx: &mut Vec<Effect>) {
        self.catching_up = false;
        self.heard.clear();
        if !self.is_owner() {
            self.skip_next = false;
            fx.push(Effect::CaughtUp);
            fx.push(Effect::Changed);
            return;
        }
        let departed: Vec<MemberId> = self
            .records
            .values()
            .filter(|r| r.body.left && r.body.id != self.me && self.in_roster(r.body.id))
            .map(|r| r.body.id)
            .collect();
        let roster: Vec<RosterEntry> = self
            .policy
            .body
            .roster
            .iter()
            .copied()
            .filter(|e| !departed.contains(&e.id))
            .collect();
        let banned = self.policy.body.banned.clone();
        if let Ok(more) = self.issue_policy(roster, banned) {
            fx.extend(more);
        }
        fx.push(Effect::CaughtUp);
        fx.push(Effect::Changed);
    }

    fn merge_policy(
        &mut self,
        policy: Signed<RoomPolicy>,
        from: Option<MemberId>,
        fx: &mut Vec<Effect>,
    ) {
        if policy.body.version <= self.policy.body.version
            || check_policy(&policy, self.id, self.owner).is_err()
        {
            return;
        }
        self.install_policy(policy, from, fx);
    }

    /// The version our next policy takes: the next one, or for the first
    /// after a restore, [`RESTORE_SKIP`] ahead.
    fn next_policy_version(&mut self) -> Result<u32, RoomError> {
        let step = if std::mem::take(&mut self.skip_next) {
            RESTORE_SKIP
        } else {
            1
        };
        self.policy
            .body
            .version
            .checked_add(step)
            .ok_or(RoomError::Exhausted)
    }

    fn issue_policy(
        &mut self,
        roster: Vec<RosterEntry>,
        banned: Vec<MemberId>,
    ) -> Result<Vec<Effect>, RoomError> {
        let version = self.next_policy_version()?;
        let policy = sign_policy(
            &self.key,
            RoomPolicy {
                room: self.id,
                version,
                name: self.policy.body.name.clone(),
                roster,
                banned,
            },
        );
        let mut fx = Vec::new();
        self.install_policy(policy, None, &mut fx);
        Ok(fx)
    }

    /// Adopts a newer policy and settles every connection under it: anyone
    /// no longer a member gets the policy as proof and is closed; everyone
    /// else gets the policy (except `from`, who sent it). Saved first, so a
    /// version is never sent before it is on disk.
    fn install_policy(
        &mut self,
        policy: Signed<RoomPolicy>,
        from: Option<MemberId>,
        fx: &mut Vec<Effect>,
    ) {
        self.policy = policy;
        let message = self.policy_message();
        if !self.in_roster(self.me) {
            // Banned, or the owner closed the room.
            let banned = self.is_banned(self.me);
            for conn in self.conns.keys() {
                fx.push(Effect::Close {
                    conn: *conn,
                    code: close_code::LEFT_ROOM,
                });
            }
            self.gone = true;
            fx.push(Effect::Removed { banned });
            return;
        }
        // Records belong to stays: drop those the roster no longer has.
        let mine = self.records[&self.me].body.clone();
        let policy = &self.policy.body;
        self.records
            .retain(|id, r| policy.since(*id) == Some(r.body.since));
        self.links.retain(|id, _| policy.lists(*id));
        if !self.records.contains_key(&self.me) {
            // The owner moved us to a new stay (it never should): carry on
            // under it.
            let record = self.publish_mine(1, mine.name, mine.addr, mine.left);
            fx.extend(self.broadcast(ControlMessage::MemberUpdate(Box::new(record)), None));
        }
        fx.push(Effect::Save);
        let current: BTreeSet<ConnId> = self.current_conns().into_iter().collect();
        let conns: Vec<(ConnId, MemberId)> = self.conns.iter().map(|(c, p)| (*c, *p)).collect();
        for (conn, peer) in conns {
            if !self.is_member(peer) {
                fx.push(Effect::Send {
                    conn,
                    message: message.clone(),
                });
                fx.push(Effect::Close {
                    conn,
                    code: self.removal_code(peer),
                });
            } else if current.contains(&conn) && Some(peer) != from {
                fx.push(Effect::Send {
                    conn,
                    message: message.clone(),
                });
            }
        }
        fx.push(Effect::Changed);
    }
}

/// Whether `policy` is the owner's signed word that `member` is banned from
/// the room with this id. Joiners use it before they have a [`Room`].
pub fn proves_ban(
    policy: &Signed<RoomPolicy>,
    room: RoomId,
    owner: MemberId,
    member: MemberId,
) -> bool {
    policy.body.banned.contains(&member) && check_policy(policy, room, owner).is_ok()
}

fn check_policy(
    policy: &Signed<RoomPolicy>,
    room: RoomId,
    owner: MemberId,
) -> Result<(), RoomError> {
    if policy.body.room == room && policy.body.is_valid() && policy_is_authentic(policy, owner) {
        Ok(())
    } else {
        Err(RoomError::BadPolicy)
    }
}

/// Keeps an address within what a record may carry.
fn trim_addr(mut addr: PeerAddr) -> PeerAddr {
    addr.ips.truncate(MAX_IPS);
    if addr
        .relay
        .as_ref()
        .is_some_and(|url| url.len() > MAX_RELAY_URL_BYTES)
    {
        addr.relay = None;
    }
    addr
}

#[cfg(test)]
mod tests;
