//! A small simulated network of members, standing in for the engine: joins
//! go through the owner's decision and the password check, dials through
//! `member_hello`, messages on a connection arrive in order, and
//! connections interleave in a seeded random order.

use std::collections::VecDeque;

use couch_proto::room::MemberRecord;
use proptest::prelude::*;

use super::*;

const TICK: Duration = Duration::from_millis(100);
const WEEK: u64 = 7 * 24 * 3600;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn token(n: u8) -> InviteToken {
    InviteToken([n; 32])
}

struct Member {
    key: SigningKey,
    room: Option<Room>,
    online: bool,
    saved: Option<SavedRoom>,
    removed: Option<bool>,
    departed: bool,
}

struct Sim {
    now: Instant,
    /// Wall-clock seconds, for invite expiry.
    unix: u64,
    members: BTreeMap<MemberId, Member>,
    /// conn -> (its side, the other side, the other side's conn)
    pipes: BTreeMap<ConnId, (MemberId, MemberId, ConnId)>,
    /// Messages in flight, by the sending side's conn.
    queues: BTreeMap<ConnId, VecDeque<ControlMessage>>,
    dials: Vec<(MemberId, MemberId)>,
    next_conn: ConnId,
    next_token: u8,
    rng: u64,
}

impl Sim {
    fn new(seed: u64) -> Self {
        Self {
            now: Instant::now(),
            unix: 1_000_000,
            members: BTreeMap::new(),
            pipes: BTreeMap::new(),
            queues: BTreeMap::new(),
            dials: Vec::new(),
            next_conn: 1,
            next_token: 1,
            rng: seed | 1,
        }
    }

    fn random(&mut self, below: usize) -> usize {
        // xorshift64
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng % below as u64) as usize
    }

    fn add(&mut self, seed: u8) -> MemberId {
        let key = key(seed);
        let id = member_id(&key);
        self.members.insert(
            id,
            Member {
                key,
                room: None,
                online: true,
                saved: None,
                removed: None,
                departed: false,
            },
        );
        id
    }

    fn room(&self, id: MemberId) -> &Room {
        self.members[&id]
            .room
            .as_ref()
            .expect("member has the room")
    }

    fn room_mut(&mut self, id: MemberId) -> &mut Room {
        self.members
            .get_mut(&id)
            .unwrap()
            .room
            .as_mut()
            .expect("member has the room")
    }

    fn create(&mut self, owner: MemberId) {
        let member = self.members.get_mut(&owner).unwrap();
        let room = Room::create(
            Identity {
                key: member.key.clone(),
                name: "owner".into(),
                addr: PeerAddr::default(),
            },
            RoomNonce([7; 16]),
            "Couch",
            Timings::default(),
            self.now,
        )
        .unwrap();
        member.saved = Some(room.saved());
        member.room = Some(room);
    }

    /// A fresh invite from the owner.
    fn invite(&mut self, owner: MemberId) -> Invite {
        let token = token(self.next_token);
        self.next_token += 1;
        let unix = self.unix;
        let (invite, fx) = self
            .room_mut(owner)
            .create_invite(token, unix, WEEK, true)
            .unwrap();
        self.apply(owner, fx);
        invite
    }

    /// `joiner` joins with `invite` and the right password.
    fn join(&mut self, joiner: MemberId, invite: &Invite) -> Result<(), u32> {
        self.join_with(joiner, invite, true)
    }

    /// What the engine does for a join: the owner's decision, the password
    /// check, the owner putting the joiner on the roster, then the joiner
    /// building its room from the owner's snapshot.
    fn join_with(
        &mut self,
        joiner: MemberId,
        invite: &Invite,
        password_ok: bool,
    ) -> Result<(), u32> {
        let owner = invite.owner;
        if !self
            .members
            .get(&owner)
            .is_some_and(|m| m.online && m.room.is_some())
        {
            return Err(u32::MAX);
        }
        let tag = invite_tag(&invite.token);
        let unix = self.unix;
        match self.room_mut(owner).join_request(joiner, tag, unix) {
            JoinDecision::Proceed { token } if token == invite.token => {}
            JoinDecision::Proceed { .. } => return Err(close_code::ADMISSION_FAILED),
            JoinDecision::Refuse(code) => return Err(code),
            JoinDecision::Banned(_) => return Err(close_code::BANNED),
        }
        if !password_ok {
            let fx = self.room_mut(owner).join_failed(tag);
            self.apply(owner, fx);
            return Err(close_code::ADMISSION_FAILED);
        }
        let fx = self.room_mut(owner).admit(joiner, tag, unix)?;
        self.apply(owner, fx);
        let snapshot = self.room(owner).snapshot();
        let member = self.members.get_mut(&joiner).unwrap();
        let room = Room::join(
            Identity {
                key: member.key.clone(),
                name: format!("m{}", &joiner.to_hex()[..4]),
                addr: PeerAddr::default(),
            },
            invite.founder(),
            invite.owner,
            invite.nonce,
            snapshot,
            Timings::default(),
            self.now,
        )
        .map_err(|_| close_code::PROTOCOL)?;
        member.saved = Some(room.saved());
        member.room = Some(room);
        member.removed = None;
        member.departed = false;
        self.link(joiner, owner);
        Ok(())
    }

    /// Accepts a connection between `dialer` and `acceptor` on both sides.
    fn link(&mut self, dialer: MemberId, acceptor: MemberId) {
        let (a, b) = (self.next_conn, self.next_conn + 1);
        self.next_conn += 2;
        self.pipes.insert(a, (dialer, acceptor, b));
        self.pipes.insert(b, (acceptor, dialer, a));
        // Both sides learn of the connection before either acts on it, as
        // over a network: a close can't overtake the other side's accept.
        let dialer_fx = self.room_mut(dialer).connected(acceptor, a, true);
        let acceptor_fx = self.room_mut(acceptor).connected(dialer, b, false);
        // The dialer's hello waits for a snapshot: whatever the acceptor
        // says first must be one.
        let first = acceptor_fx.iter().find_map(|e| match e {
            Effect::Send { conn, message } if *conn == b => Some(message),
            _ => None,
        });
        assert!(
            first.is_none_or(|m| matches!(m, ControlMessage::RoomSnapshot(_))),
            "an accepted dialer must get a snapshot first, got {first:?}"
        );
        self.apply(dialer, dialer_fx);
        self.apply(acceptor, acceptor_fx);
    }

    /// What the engine does with a `Dial` effect: `MemberHello` with the
    /// dialer's policy, and a refusal's proof shown to the dialer.
    fn dial(&mut self, dialer: MemberId, peer: MemberId) {
        let Some(policy) = self.members[&dialer].room.as_ref().map(Room::owned_policy) else {
            return;
        };
        let reachable = self
            .members
            .get(&peer)
            .is_some_and(|m| m.online && m.room.is_some());
        let now = self.now;
        if !reachable {
            self.room_mut(dialer).dial_failed(peer, now);
            return;
        }
        let (decision, fx) = self.room_mut(peer).member_hello(dialer, policy);
        self.apply(peer, fx);
        match decision {
            MemberDecision::Accept => {
                if self.members[&peer].room.is_some() && self.members[&dialer].room.is_some() {
                    self.link(dialer, peer);
                }
            }
            MemberDecision::Refuse { code, proof } => {
                if let Some(room) = self.members.get_mut(&dialer).unwrap().room.as_mut() {
                    let fx = room.refused(peer, code, proof.map(|p| *p), now);
                    self.apply(dialer, fx);
                }
            }
        }
    }

    fn apply(&mut self, who: MemberId, effects: Vec<Effect>) {
        let mut removed = None;
        let mut departed = false;
        for effect in effects {
            match effect {
                Effect::Dial { peer, .. } => self.dials.push((who, peer)),
                Effect::Send { conn, message } => {
                    if self.pipes.contains_key(&conn) {
                        self.queues.entry(conn).or_default().push_back(message);
                    }
                }
                Effect::Close { conn, .. } => self.close(conn),
                Effect::Save => {
                    if let Some(room) = self.members[&who].room.as_ref() {
                        let saved = room.saved();
                        self.members.get_mut(&who).unwrap().saved = Some(saved);
                    }
                }
                Effect::Changed | Effect::Restore(_) | Effect::Ownership { .. } => {}
                Effect::Removed { banned } => removed = Some(banned),
                Effect::Departed => departed = true,
            }
        }
        let member = self.members.get_mut(&who).unwrap();
        if let Some(banned) = removed {
            member.removed = Some(banned);
        }
        if removed.is_some() || departed {
            member.departed = departed;
            member.room = None;
            member.saved = None;
        }
    }

    /// Delivers what the closing side had sent, then ends both sides.
    fn close(&mut self, conn: ConnId) {
        let Some((side, other, other_conn)) = self.pipes.get(&conn).copied() else {
            return;
        };
        while let Some(message) = self.queues.get_mut(&conn).and_then(VecDeque::pop_front) {
            self.deliver(other, other_conn, message);
        }
        self.pipes.remove(&conn);
        self.pipes.remove(&other_conn);
        self.queues.remove(&conn);
        self.queues.remove(&other_conn);
        let now = self.now;
        for (member, c) in [(side, conn), (other, other_conn)] {
            if let Some(room) = self.members.get_mut(&member).and_then(|m| m.room.as_mut()) {
                let fx = room.disconnected(c, now);
                self.apply(member, fx);
            }
        }
    }

    fn deliver(&mut self, to: MemberId, conn: ConnId, message: ControlMessage) {
        let now = self.now;
        if let Some(room) = self.members.get_mut(&to).and_then(|m| m.room.as_mut()) {
            let fx = room.received(conn, message, now);
            self.apply(to, fx);
        }
    }

    /// One step: every pending dial at once (so crossed dials happen), then
    /// messages until quiet in random interleaving, then time moves on.
    fn step(&mut self) {
        let dials = std::mem::take(&mut self.dials);
        for (dialer, peer) in dials {
            self.dial(dialer, peer);
        }
        loop {
            let busy: Vec<ConnId> = self
                .queues
                .iter()
                .filter(|(_, q)| !q.is_empty())
                .map(|(c, _)| *c)
                .collect();
            if busy.is_empty() {
                break;
            }
            let conn = busy[self.random(busy.len())];
            let message = self.queues.get_mut(&conn).unwrap().pop_front().unwrap();
            if let Some((_, other, other_conn)) = self.pipes.get(&conn).copied() {
                self.deliver(other, other_conn, message);
            }
        }
        self.now += TICK;
        let now = self.now;
        let ids: Vec<MemberId> = self.members.keys().copied().collect();
        for id in ids {
            let member = self.members.get_mut(&id).unwrap();
            if !member.online {
                continue;
            }
            if let Some(room) = member.room.as_mut() {
                let fx = room.tick(now);
                self.apply(id, fx);
            }
        }
    }

    fn settle(&mut self, secs: u64) {
        for _ in 0..secs * 10 {
            self.step();
        }
    }

    /// Takes a member offline: its connections drop without a goodbye.
    fn crash(&mut self, id: MemberId) {
        let conns: Vec<ConnId> = self
            .pipes
            .iter()
            .filter(|(_, (side, _, _))| *side == id)
            .map(|(c, _)| *c)
            .collect();
        for conn in conns {
            self.close(conn);
        }
        let member = self.members.get_mut(&id).unwrap();
        member.online = false;
        member.room = None;
    }

    /// Back online from its saved state.
    fn restart(&mut self, id: MemberId) {
        let now = self.now;
        let member = self.members.get_mut(&id).unwrap();
        member.online = true;
        let saved = member.saved.clone().expect("remembered room");
        member.room =
            Some(Room::restore(member.key.clone(), saved, Timings::default(), now).unwrap());
    }

    /// Back online from a backup (M13): `saved`, maybe older than what the
    /// member had, catching up first. Returns whether it waits to.
    fn restore_from_backup(&mut self, id: MemberId, saved: SavedRoom) -> bool {
        let now = self.now;
        let member = self.members.get_mut(&id).unwrap();
        member.online = true;
        let mut room = Room::restore(member.key.clone(), saved, Timings::default(), now).unwrap();
        let fx = room.catch_up_first();
        let waits = room.is_catching_up();
        member.saved = Some(room.saved());
        member.room = Some(room);
        self.apply(id, fx);
        waits
    }

    /// Every pair of these members shares exactly one connection, and both
    /// sides agree on it.
    fn assert_full_mesh(&self, ids: &[MemberId]) {
        for &a in ids {
            let conns = self.room(a).connections();
            let peers: BTreeSet<MemberId> = conns.iter().map(|(p, _, _)| *p).collect();
            let expected: BTreeSet<MemberId> = ids.iter().copied().filter(|x| *x != a).collect();
            assert_eq!(peers, expected, "{a:?} is connected to the wrong set");
            for (peer, conn, mine) in conns {
                let (_, other, other_conn) = self.pipes[&conn];
                assert_eq!(other, peer);
                let theirs = self
                    .room(peer)
                    .connections()
                    .into_iter()
                    .find(|(p, _, _)| *p == a)
                    .expect("peer sees us");
                assert_eq!(
                    theirs.1, other_conn,
                    "{a:?} and {peer:?} kept different connections"
                );
                assert_eq!(theirs.2, !mine);
            }
        }
        // No stray connections either.
        let stray = self
            .pipes
            .values()
            .filter(|(side, _, _)| ids.contains(side))
            .count();
        assert_eq!(
            stray,
            ids.len() * (ids.len() - 1),
            "duplicate connections left open"
        );
    }

    fn member_ids(&self, id: MemberId) -> Vec<MemberId> {
        self.room(id)
            .status()
            .members
            .iter()
            .map(|m| m.id)
            .collect()
    }

    fn conn_to(&self, from: MemberId, to: MemberId) -> ConnId {
        self.room(from)
            .connections()
            .into_iter()
            .find(|(p, _, _)| *p == to)
            .unwrap()
            .1
    }

    /// M13: `owner` offers the room to `to`, which accepts; returns whether
    /// both steps went through (the messages are still to settle).
    fn offer_and_accept(&mut self, owner: MemberId, to: MemberId) -> bool {
        let id = self.rng;
        let Ok(fx) = self.room_mut(owner).offer_room(to, id) else {
            return false;
        };
        self.apply(owner, fx);
        self.step();
        if self.members[&to].room.as_ref().and_then(Room::offered) != Some(false) {
            return false;
        }
        let fx = self.room_mut(to).answer_offer(true).unwrap();
        self.apply(to, fx);
        self.step();
        true
    }
}

/// A, the owner, with B and C, both let in by A.
fn three(seed: u64) -> (Sim, MemberId, MemberId, MemberId) {
    let mut sim = Sim::new(seed);
    let (a, b, c) = (sim.add(1), sim.add(2), sim.add(3));
    sim.create(a);
    let invite = sim.invite(a);
    sim.join(b, &invite).unwrap();
    sim.settle(1);
    sim.join(c, &invite).unwrap();
    sim.settle(2);
    (sim, a, b, c)
}

#[test]
fn three_members_form_a_full_mesh() {
    let (sim, a, b, c) = three(1);
    sim.assert_full_mesh(&[a, b, c]);
    for id in [a, b, c] {
        let status = sim.room(id).status();
        assert_eq!(status.members.len(), 3);
        assert_eq!(status.members[0].link, LinkState::Me);
        assert!(
            status.members[1..]
                .iter()
                .all(|m| m.link == LinkState::Connected)
        );
        assert_eq!(status.owner, a);
        assert!(status.members.iter().any(|m| m.id == a && m.is_owner));
        assert_eq!(sim.room(id).policy().body.roster.len(), 3);
    }
}

#[test]
fn crossed_dials_keep_the_lower_ids_connection() {
    let (mut sim, a, b, c) = three(2);
    for id in [a, b, c] {
        sim.crash(id);
    }
    // Everyone restarts at once and dials everyone in the same step.
    for id in [a, b, c] {
        sim.restart(id);
    }
    sim.settle(3);
    sim.assert_full_mesh(&[a, b, c]);
    for (x, y) in [(a, b), (a, c), (b, c)] {
        let (_, _, mine) = sim
            .room(x)
            .connections()
            .into_iter()
            .find(|(p, _, _)| *p == y)
            .unwrap();
        assert_eq!(mine, x < y, "kept the connection the lower id dialed");
    }
}

#[test]
fn a_dropped_connection_is_redialed() {
    let (mut sim, a, b, c) = three(3);
    let (_, conn, _) = sim.room(a).connections()[0];
    sim.close(conn);
    sim.settle(3);
    sim.assert_full_mesh(&[a, b, c]);
}

#[test]
fn a_member_back_from_a_crash_reconnects() {
    let (mut sim, a, b, c) = three(4);
    sim.crash(b);
    sim.settle(1);
    let link = sim
        .room(a)
        .status()
        .members
        .iter()
        .find(|m| m.id == b)
        .unwrap()
        .link;
    assert_eq!(link, LinkState::Offline);
    sim.restart(b);
    sim.settle(3);
    sim.assert_full_mesh(&[a, b, c]);
}

#[test]
fn members_reconnect_without_the_owner() {
    let (mut sim, a, b, c) = three(5);
    sim.crash(a);
    sim.crash(b);
    sim.crash(c);
    sim.restart(b);
    sim.restart(c);
    sim.settle(3);
    sim.assert_full_mesh(&[b, c]);
}

#[test]
fn only_the_owner_admits_invites_and_bans() {
    let (mut sim, a, b, c) = three(6);
    let unix = sim.unix;
    assert!(matches!(
        sim.room_mut(b).create_invite(token(99), unix, WEEK, true),
        Err(RoomError::NotOwner)
    ));
    let stranger = sim.add(9);
    let tag = InviteTag([0; 16]);
    // A member answers an invite as expired: it may be one it made as an
    // owner before handing the room over (M13).
    assert_eq!(
        sim.room_mut(b).join_request(stranger, tag, unix),
        JoinDecision::Refuse(close_code::INVITE_EXPIRED)
    );
    assert_eq!(sim.room_mut(b).ban(c), Err(RoomError::NotOwner));
    assert_eq!(
        sim.room_mut(b).admit(stranger, tag, unix),
        Err(close_code::INVITE_EXPIRED)
    );
    assert_eq!(sim.room_mut(a).ban(a), Err(RoomError::CantBanSelf));
    // The owner can ban someone who was never in the room.
    let fx = sim.room_mut(a).ban(stranger).unwrap();
    sim.apply(a, fx);
    assert!(sim.room(a).is_banned(stranger));
}

#[test]
fn joining_needs_the_owner() {
    let (mut sim, a, _b, _c) = three(7);
    let invite = sim.invite(a);
    sim.crash(a);
    let d = sim.add(4);
    assert!(sim.join(d, &invite).is_err());
}

/// A member who missed a change (here, someone joining) learns it from
/// whoever dials it with the newer policy.
#[test]
fn a_member_who_missed_a_join_catches_up_from_the_newcomer() {
    let (mut sim, a, b, c) = three(8);
    sim.crash(c);
    let invite = sim.invite(a);
    let d = sim.add(4);
    sim.join(d, &invite).unwrap();
    sim.settle(1);
    // Only C and D come back; C has never heard of D.
    sim.crash(a);
    sim.crash(b);
    sim.restart(c);
    assert!(!sim.room(c).is_member(d));
    sim.settle(3);
    sim.assert_full_mesh(&[c, d]);
    assert!(sim.room(c).is_member(d));
}

#[test]
fn a_ban_removes_the_member_everywhere_and_keeps_them_out() {
    let (mut sim, a, b, c) = three(9);
    let old_invite = sim.invite(a);
    let policy_c_had = sim.room(c).owned_policy();
    let fx = sim.room_mut(a).ban(c).unwrap();
    sim.apply(a, fx);
    sim.settle(2);

    assert_eq!(sim.members[&c].removed, Some(true), "C saw the owner's ban");
    sim.assert_full_mesh(&[a, b]);
    for id in [a, b] {
        assert!(!sim.member_ids(id).contains(&c));
    }
    // C's id is refused, by members and by the owner, with any invite.
    let (decision, _) = sim.room_mut(b).member_hello(c, policy_c_had);
    assert!(matches!(
        decision,
        MemberDecision::Refuse {
            code: close_code::BANNED,
            proof: Some(_)
        }
    ));
    let fresh = sim.invite(a);
    let unix = sim.unix;
    assert!(matches!(
        sim.room_mut(a)
            .join_request(c, invite_tag(&fresh.token), unix),
        JoinDecision::Banned(_)
    ));
    // The ban revoked every invite made before it: a new identity can't
    // use the old one.
    let d = sim.add(4);
    assert_eq!(sim.join(d, &old_invite), Err(close_code::INVITE_EXPIRED));
    sim.join(d, &fresh).unwrap();
    sim.settle(2);
    sim.assert_full_mesh(&[a, b, d]);
}

/// M13: an owner restored from a backup older than the room signs nothing
/// (no ban, departure or closing, no admission) until a member's snapshot
/// brings it up to date: signing from the older state would reuse versions
/// the members hold, and they would ignore it. Caught up, it holds the
/// room's newest policy (a ban made after the backup included) and signs
/// above it.
#[test]
fn a_restored_owner_signs_nothing_until_a_member_catches_it_up() {
    let (mut sim, a, b, c) = three(30);
    let backup = sim.members[&a].saved.clone().unwrap();
    let at_backup = backup.policy.body.version;
    // After the backup, the owner bans C; B holds that policy.
    let fx = sim.room_mut(a).ban(c).unwrap();
    sim.apply(a, fx);
    sim.settle(1);
    let newest = sim.room(b).policy().body.version;
    assert!(newest > at_backup);
    // The owner's PC is lost; the backup comes back while B is away.
    sim.crash(a);
    sim.crash(b);
    assert!(sim.restore_from_backup(a, backup), "others on its roster");
    assert!(sim.room(a).is_catching_up());
    assert_eq!(sim.room_mut(a).ban(b), Err(RoomError::CatchingUp));
    let invite = sim.invite(a);
    let d = sim.add(4);
    assert_eq!(
        sim.join(d, &invite),
        Err(close_code::UNKNOWN_ROOM),
        "nobody admitted"
    );
    assert_eq!(sim.room(a).version(), at_backup, "nothing signed");
    // B comes back: its snapshot catches the owner up, and the owner signs
    // the room again, skipping ahead.
    sim.restart(b);
    sim.settle(2);
    assert!(!sim.room(a).is_catching_up());
    assert_eq!(sim.room(a).version(), newest + RESTORE_SKIP);
    assert!(
        sim.room(a).policy().body.banned.contains(&c),
        "the ban is back"
    );
    assert_eq!(sim.room(b).version(), newest + RESTORE_SKIP);
    // And it signs above what B holds, so B takes it.
    let fresh = sim.invite(a);
    sim.join(d, &fresh).unwrap();
    sim.settle(2);
    assert!(sim.room(b).version() > newest + RESTORE_SKIP);
    assert!(sim.room(b).is_member(d));
    sim.assert_full_mesh(&[a, b, d]);
}

/// The review's finding: the first member back may hold old state. A ban
/// made after the backup, held only by C, must not be outvoted by what the
/// restored owner signs after hearing from a stale B: the owner waits for
/// every member, and X, banned meanwhile, stops counting once C is heard.
#[test]
fn a_stale_first_snapshot_doesnt_end_the_catching_up() {
    let (mut sim, a, b, c) = three(33);
    let invite = sim.invite(a);
    let x = sim.add(5);
    sim.join(x, &invite).unwrap();
    sim.settle(2);
    let backup = sim.members[&a].saved.clone().unwrap();
    // B is away when the owner bans X; C holds the ban.
    sim.crash(b);
    let fx = sim.room_mut(a).ban(x).unwrap();
    sim.apply(a, fx);
    sim.settle(1);
    assert!(sim.room(c).policy().body.banned.contains(&x));
    sim.crash(a);
    sim.crash(c);
    assert!(sim.restore_from_backup(a, backup));
    // B, stale, comes back first: still catching up, waiting for C and X.
    sim.restart(b);
    sim.settle(2);
    assert!(sim.room(a).is_catching_up());
    let waiting: BTreeSet<MemberId> = sim.room(a).catching_up_with().into_iter().collect();
    assert_eq!(waiting, BTreeSet::from([c, x]));
    // C brings the ban: X is off the roster, and nobody is left to wait for.
    sim.restart(c);
    sim.settle(2);
    assert!(!sim.room(a).is_catching_up());
    assert!(sim.room(a).policy().body.banned.contains(&x));
    // What the owner signs now keeps X out everywhere.
    let fresh = sim.invite(a);
    let d = sim.add(4);
    sim.join(d, &fresh).unwrap();
    sim.settle(2);
    for id in [a, b, c] {
        assert!(sim.room(id).policy().body.banned.contains(&x), "{id:?}");
        assert!(sim.room(id).is_member(d));
    }
}

/// Members who may never come back: the owner can stop waiting, knowing a
/// change only they hold can then be undone. Only the owner.
#[test]
fn the_owner_can_stop_waiting() {
    let (mut sim, a, b, c) = three(34);
    let backup = sim.members[&a].saved.clone().unwrap();
    sim.crash(a);
    sim.crash(b);
    sim.crash(c);
    assert!(sim.restore_from_backup(a, backup));
    assert_eq!(sim.room_mut(a).ban(b), Err(RoomError::CatchingUp));
    let fx = sim.room_mut(a).stop_catching_up().unwrap();
    assert!(fx.contains(&Effect::Restore(RestoreMark::Unconfirmed)));
    assert!(!sim.room(a).is_catching_up());
    let fx = sim.room_mut(a).ban(b).unwrap();
    sim.apply(a, fx);
}

/// A restored member signs nothing that matters: any member's snapshot
/// catches it up, and its newer records supersede what it held.
#[test]
fn a_restored_member_catches_up_from_anyone() {
    let (mut sim, a, b, c) = three(35);
    let backup = sim.members[&b].saved.clone().unwrap();
    sim.crash(b);
    sim.crash(a);
    assert!(sim.restore_from_backup(b, backup));
    assert_eq!(sim.room_mut(b).stop_catching_up(), Err(RoomError::NotOwner));
    sim.settle(2);
    assert!(!sim.room(b).is_catching_up(), "C's snapshot was enough");
    sim.assert_full_mesh(&[b, c]);
}

/// A backup made while the owner was alone in the room: nobody to wait
/// for, so it is caught up at once and signs the room again, skipping ahead.
/// Someone the lost PC let in after the backup (F) finds that, and is no
/// longer listed: dropped, never left holding a second room under the same
/// version (the second review's finding).
#[test]
fn a_lone_owner_restored_has_nobody_to_catch_up_with() {
    let mut sim = Sim::new(31);
    let a = sim.add(1);
    sim.create(a);
    let backup = sim.members[&a].saved.clone().unwrap();
    let at_backup = backup.policy.body.version;
    let invite = sim.invite(a);
    let f = sim.add(6);
    sim.join(f, &invite).unwrap();
    sim.settle(1);
    sim.crash(a);
    sim.crash(f);
    assert!(!sim.restore_from_backup(a, backup));
    assert!(!sim.room(a).is_catching_up());
    assert_eq!(sim.room(a).version(), at_backup + RESTORE_SKIP);
    // Before the owner signs anything more: what it signed on restoring is
    // already ahead of F's.
    sim.restart(f);
    sim.settle(3);
    assert_eq!(sim.members[&f].removed, Some(false), "F was dropped");
    assert!(!sim.room(a).is_member(f));
    let invite = sim.invite(a);
    let g = sim.add(2);
    sim.join(g, &invite).unwrap();
    sim.settle(1);
    sim.assert_full_mesh(&[a, g]);
}

/// Someone let in after the backup while every other member was away (F)
/// is known to nobody the restored owner hears from. The owner catches up
/// with the others and signs ahead; F, back, is dropped rather than left on
/// a second policy under the version it holds (the second review's
/// finding).
#[test]
fn a_member_only_the_lost_pc_knew_is_dropped_not_forked() {
    let (mut sim, a, b, c) = three(36);
    let backup = sim.members[&a].saved.clone().unwrap();
    sim.crash(b);
    sim.crash(c);
    let invite = sim.invite(a);
    let f = sim.add(6);
    sim.join(f, &invite).unwrap();
    sim.settle(1);
    let f_holds = sim.room(f).version();
    sim.crash(a);
    sim.crash(f);
    assert!(sim.restore_from_backup(a, backup));
    sim.restart(b);
    sim.restart(c);
    sim.settle(2);
    assert!(!sim.room(a).is_catching_up());
    assert!(sim.room(a).version() > f_holds);
    let invite = sim.invite(a);
    let g = sim.add(7);
    sim.join(g, &invite).unwrap();
    sim.restart(f);
    sim.settle(3);
    assert_eq!(sim.members[&f].removed, Some(false), "F was dropped");
    for id in [a, b, c, g] {
        assert!(!sim.room(id).policy().body.lists(f), "{id:?}");
        assert!(sim.room(id).is_member(g), "{id:?}");
    }
    sim.assert_full_mesh(&[a, b, c, g]);
}

/// Closing a room while catching up: the closing policy skips ahead of
/// whatever the members hold that is newer than the backup (D let in after
/// it), so it closes the room for them too.
#[test]
fn closing_while_catching_up_closes_for_members_holding_newer() {
    let (mut sim, a, b, c) = three(37);
    let backup = sim.members[&a].saved.clone().unwrap();
    let invite = sim.invite(a);
    let d = sim.add(4);
    sim.join(d, &invite).unwrap();
    sim.settle(2);
    sim.crash(a);
    sim.crash(b);
    sim.crash(c);
    assert!(sim.restore_from_backup(a, backup));
    let now = sim.now;
    let fx = sim.room_mut(a).leave(now).unwrap();
    sim.apply(a, fx);
    sim.restart(b);
    sim.restart(c);
    sim.settle(4);
    for id in [b, c] {
        assert_eq!(sim.members[&id].removed, Some(false), "{id:?} still in it");
    }
}

/// A restored member leaving while catching up: its last record skips ahead
/// of the ones the lost PC signed after the backup, so the owner takes it.
#[test]
fn a_restored_member_leaving_while_catching_up_is_heard() {
    let (mut sim, a, b, c) = three(38);
    let backup = sim.members[&b].saved.clone().unwrap();
    // After the backup, B's lost PC signs newer records of B.
    for name in ["B two", "B three"] {
        let fx = sim
            .room_mut(b)
            .set_profile(name, PeerAddr::default())
            .unwrap();
        sim.apply(b, fx);
        sim.settle(1);
    }
    sim.crash(a);
    sim.crash(b);
    sim.crash(c);
    assert!(sim.restore_from_backup(b, backup));
    let now = sim.now;
    let fx = sim.room_mut(b).leave(now).unwrap();
    sim.apply(b, fx);
    sim.restart(a);
    sim.restart(c);
    sim.settle(4);
    for id in [a, c] {
        assert!(!sim.room(id).policy().body.lists(b), "{id:?} still lists B");
    }
}

/// M13: a member who left while the owner was away is taken off the roster
/// once the restored owner has caught up, not before.
#[test]
fn a_departure_heard_while_catching_up_is_taken_once_caught_up() {
    let (mut sim, a, b, c) = three(32);
    let backup = sim.members[&a].saved.clone().unwrap();
    sim.crash(a);
    // B leaves; only C hears it.
    let now = sim.now;
    let fx = sim.room_mut(b).leave(now).unwrap();
    sim.apply(b, fx);
    sim.settle(1);
    assert!(sim.members[&b].departed);
    assert!(sim.restore_from_backup(a, backup));
    sim.settle(2);
    assert!(!sim.room(a).is_catching_up());
    for id in [a, c] {
        assert!(!sim.room(id).policy().body.lists(b), "B is off the roster");
    }
    sim.assert_full_mesh(&[a, c]);
}

#[test]
fn a_member_banned_while_offline_learns_it_on_return() {
    let (mut sim, a, b, c) = three(10);
    sim.crash(c);
    let fx = sim.room_mut(a).ban(c).unwrap();
    sim.apply(a, fx);
    sim.settle(1);
    // C's saved state knows nothing of the ban.
    sim.restart(c);
    sim.settle(2);
    assert_eq!(
        sim.members[&c].removed,
        Some(true),
        "shown the owner's policy"
    );
    sim.assert_full_mesh(&[a, b]);
}

#[test]
fn a_proof_must_come_from_the_owner_and_drop_us() {
    let (mut sim, a, b, c) = three(11);
    let now = sim.now;
    let room = sim.room(a).id();
    let roster = sim.room(a).policy().body.roster.clone();
    // B forges a policy without C.
    let forged = sign_policy(
        &key(2),
        RoomPolicy {
            room,
            version: 50,
            name: "Couch".into(),
            roster: roster.iter().copied().filter(|e| e.id != c).collect(),
            banned: vec![c],
        },
    );
    let fx = sim.room_mut(c).refused(
        b,
        close_code::BANNED,
        Some(OwnedPolicy::founders(forged)),
        now,
    );
    assert!(fx.is_empty());
    // The owner's real policy, which lists C.
    let real = sim.room(a).owned_policy();
    let fx = sim
        .room_mut(c)
        .refused(b, close_code::NOT_MEMBER, Some(real), now);
    assert!(fx.is_empty());
    assert!(sim.room(c).is_member(b));
}

#[test]
fn leaving_takes_the_member_off_the_roster() {
    let (mut sim, a, b, c) = three(12);
    let now = sim.now;
    let fx = sim.room_mut(c).leave(now).unwrap();
    sim.apply(c, fx);
    assert!(sim.members[&c].departed, "connected members heard at once");
    sim.settle(3);
    sim.assert_full_mesh(&[a, b]);
    for id in [a, b] {
        assert!(!sim.room(id).policy().body.lists(c));
        assert!(!sim.member_ids(id).contains(&c));
    }
    // Coming back takes a new invite.
    let invite = sim.invite(a);
    sim.join(c, &invite).unwrap();
    sim.settle(2);
    sim.assert_full_mesh(&[a, b, c]);
}

#[test]
fn the_owner_leaving_closes_the_room() {
    let (mut sim, a, b, c) = three(13);
    let now = sim.now;
    let fx = sim.room_mut(a).leave(now).unwrap();
    assert!(!fx.iter().any(|e| matches!(e, Effect::Removed { .. })));
    sim.apply(a, fx);
    assert!(sim.members[&a].departed, "everyone was connected");
    sim.settle(2);
    for id in [b, c] {
        assert_eq!(sim.members[&id].removed, Some(false), "{id:?} was told");
    }
}

#[test]
fn forged_policies_and_records_are_ignored() {
    let (mut sim, a, b, c) = three(14);
    let now = sim.now;
    let room = sim.room(a).id();
    let since_c = sim.room(a).policy().body.since(c).unwrap();
    // B signs a policy dropping A, claiming the owner wrote it.
    let forged = sign_policy(
        &key(2),
        RoomPolicy {
            room,
            version: 99,
            name: "Mine now".into(),
            roster: [b, c]
                .into_iter()
                .map(|id| RosterEntry { id, since: 0 })
                .collect(),
            banned: vec![a],
        },
    );
    // B signs a record renaming C.
    let renamed = sign_record(
        &key(2),
        MemberRecord {
            room,
            id: c,
            since: since_c,
            version: 99,
            name: "Loser".into(),
            addr: PeerAddr::default(),
            left: true,
        },
    );
    // A stranger's own record, relayed by B: not on the roster, so ignored.
    let stranger = key(50);
    let outsider = sign_record(
        &stranger,
        MemberRecord {
            room,
            id: member_id(&stranger),
            since: 0,
            version: 1,
            name: "Outsider".into(),
            addr: PeerAddr::default(),
            left: false,
        },
    );
    let conn = sim.conn_to(c, b);
    let before = sim.room(c).status();
    let room_c = sim.room_mut(c);
    for message in [
        ControlMessage::PolicyUpdate(Box::new(OwnedPolicy::founders(forged))),
        ControlMessage::MemberUpdate(Box::new(renamed)),
        ControlMessage::MemberUpdate(Box::new(outsider)),
    ] {
        assert!(room_c.received(conn, message, now).is_empty());
    }
    assert_eq!(sim.room(c).status(), before);
}

#[test]
fn records_from_another_room_are_ignored() {
    let (mut sim, a, b, _c) = three(15);
    let now = sim.now;
    let elsewhere = sign_record(
        &key(2),
        MemberRecord {
            room: RoomId([0xee; 16]),
            id: b,
            since: 1,
            version: 1000,
            name: "Other room".into(),
            addr: PeerAddr::default(),
            left: true,
        },
    );
    let conn = sim.conn_to(a, b);
    let fx = sim
        .room_mut(a)
        .received(conn, ControlMessage::MemberUpdate(Box::new(elsewhere)), now);
    assert!(fx.is_empty());
    assert!(sim.room(a).is_member(b));
}

#[test]
fn a_renamed_member_is_seen_by_everyone() {
    let (mut sim, a, b, c) = three(16);
    let fx = sim
        .room_mut(b)
        .set_profile("Bia", PeerAddr::default())
        .unwrap();
    sim.apply(b, fx);
    sim.settle(1);
    for id in [a, c] {
        let status = sim.room(id).status();
        assert_eq!(
            status.members.iter().find(|m| m.id == b).unwrap().name,
            "Bia"
        );
    }
}

#[test]
fn the_room_fills_up_at_twelve() {
    let mut sim = Sim::new(17);
    let owner = sim.add(1);
    sim.create(owner);
    let invite = sim.invite(owner);
    for seed in 2..=MAX_MEMBERS as u8 {
        let id = sim.add(seed);
        sim.join(id, &invite).unwrap();
    }
    sim.settle(1);
    let late = sim.add(100);
    assert_eq!(sim.join(late, &invite), Err(close_code::ROOM_FULL));
}

#[test]
fn presence_shows_members_we_cannot_reach_directly() {
    let (mut sim, a, _b, c) = three(18);
    // Cut A-C only; both stay connected to B.
    let conn = sim.conn_to(a, c);
    sim.close(conn);
    // Before anyone redials: A sees C as online through B.
    sim.dials.clear();
    let status = sim.room(a).status();
    assert_eq!(
        status.members.iter().find(|m| m.id == c).unwrap().link,
        LinkState::Online
    );
}

#[test]
fn restore_rejects_a_tampered_save() {
    let (sim, a, ..) = three(19);
    let mut saved = sim.members[&a].saved.clone().unwrap();
    saved.policy.body.name = "Tampered".into();
    assert_eq!(
        Room::restore(key(1), saved, Timings::default(), sim.now).unwrap_err(),
        RoomError::BadPolicy
    );
}

#[test]
fn invites_expire_and_can_be_revoked() {
    let mut sim = Sim::new(20);
    let (a, b, c) = (sim.add(1), sim.add(2), sim.add(3));
    sim.create(a);
    let invite = sim.invite(a);
    // A week later, it's gone.
    sim.unix += WEEK;
    assert_eq!(sim.join(b, &invite), Err(close_code::INVITE_EXPIRED));
    let invite = sim.invite(a);
    let fx = sim.room_mut(a).revoke_invites().unwrap();
    sim.apply(a, fx);
    assert_eq!(sim.join(b, &invite), Err(close_code::INVITE_EXPIRED));
    // Outstanding invites survive a restart of the owner.
    let invite = sim.invite(a);
    sim.crash(a);
    sim.restart(a);
    sim.join(c, &invite).unwrap();
}

#[test]
fn an_invite_dies_after_five_wrong_passwords() {
    let mut sim = Sim::new(21);
    let (a, b) = (sim.add(1), sim.add(2));
    sim.create(a);
    let invite = sim.invite(a);
    for _ in 0..MAX_INVITE_FAILURES {
        assert_eq!(
            sim.join_with(b, &invite, false),
            Err(close_code::ADMISSION_FAILED)
        );
    }
    assert_eq!(sim.join(b, &invite), Err(close_code::INVITE_EXPIRED));
    let other = sim.invite(a);
    sim.join(b, &other).unwrap();
}

#[test]
fn invites_name_the_relay_and_never_an_ip_when_asked() {
    let mut sim = Sim::new(22);
    let a = sim.add(1);
    sim.create(a);
    let unix = sim.unix;
    let fx = sim
        .room_mut(a)
        .set_profile(
            "owner",
            PeerAddr {
                relay: Some("https://relay.example./".into()),
                ips: vec!["203.0.113.5:51000".parse().unwrap()],
            },
        )
        .unwrap();
    sim.apply(a, fx);
    let (invite, _) = sim
        .room_mut(a)
        .create_invite(token(1), unix, WEEK, true)
        .unwrap();
    assert!(invite.addr.ips.is_empty(), "relay alone");
    assert!(invite.addr.relay.is_some());
    let fx = sim
        .room_mut(a)
        .set_profile(
            "owner",
            PeerAddr {
                relay: None,
                ips: vec!["192.168.1.2:51000".parse().unwrap()],
            },
        )
        .unwrap();
    sim.apply(a, fx);
    let (lan, _) = sim
        .room_mut(a)
        .create_invite(token(2), unix, WEEK, true)
        .unwrap();
    assert_eq!(
        lan.addr.ips.len(),
        1,
        "on a LAN, addresses are all there is"
    );
    let (careful, _) = sim
        .room_mut(a)
        .create_invite(token(3), unix, WEEK, false)
        .unwrap();
    assert_eq!(careful.addr, PeerAddr::default());
}

#[test]
fn dial_backoff_grows_and_caps() {
    let mut sim = Sim::new(23);
    let (a, b) = (sim.add(1), sim.add(2));
    sim.create(a);
    let invite = sim.invite(a);
    sim.join(b, &invite).unwrap();
    sim.settle(1);
    sim.crash(b);
    // A keeps failing to reach B: count dials over 5 simulated minutes.
    let mut dials = 0;
    for _ in 0..3000 {
        dials += sim.dials.iter().filter(|(d, _)| *d == a).count();
        sim.step();
    }
    // 1, 2, 4, 8, 16, 32 s, then every 60 s: about 10 in 300 s.
    assert!((8..=14).contains(&dials), "{dials} dials");
}

/// A record from before C left, replayed after C came back, belongs to the
/// old stay: it neither removes C nor splits the room.
#[test]
fn a_replayed_left_record_does_not_remove_a_returning_member() {
    let (mut sim, a, b, c) = three(24);
    let now = sim.now;
    let fx = sim.room_mut(c).leave(now).unwrap();
    // What C said on the way out, signed by C.
    let old_left = sim
        .room(c)
        .saved()
        .records
        .into_iter()
        .find(|r| r.body.id == c)
        .unwrap();
    assert!(old_left.body.left);
    sim.apply(c, fx);
    sim.settle(1);
    let invite = sim.invite(a);
    sim.join(c, &invite).unwrap();
    sim.settle(2);
    sim.assert_full_mesh(&[a, b, c]);
    // Its version beats C's new record, but its stay is over.
    let current = &sim.room(a).saved().records;
    let new = current.iter().find(|r| r.body.id == c).unwrap();
    assert!(old_left.body.version > new.body.version);
    let conn = sim.conn_to(a, b);
    let now = sim.now;
    let fx = sim
        .room_mut(a)
        .received(conn, ControlMessage::MemberUpdate(Box::new(old_left)), now);
    assert!(fx.is_empty(), "an old stay's record is ignored");
    assert!(sim.room(a).is_member(c));
    assert!(sim.room(a).policy().body.lists(c));
}

#[test]
fn a_stranger_learns_nothing_from_a_member_hello() {
    let (mut sim, a, b, _c) = three(25);
    let stranger = key(60);
    let id = member_id(&stranger);
    let room = sim.room(a).id();
    let real = sim.room(a).owned_policy();
    let (decision, _) = sim.room_mut(b).member_hello(id, real);
    assert_eq!(
        decision,
        MemberDecision::Refuse {
            code: close_code::NOT_MEMBER,
            proof: None
        }
    );
    // A policy it signed itself, listing itself, convinces nobody.
    let forged = sign_policy(
        &stranger,
        RoomPolicy {
            room,
            version: 1000,
            name: "Couch".into(),
            roster: vec![RosterEntry { id, since: 0 }],
            banned: Vec::new(),
        },
    );
    let version = sim.room(b).version();
    let (decision, _) = sim
        .room_mut(b)
        .member_hello(id, OwnedPolicy::founders(forged));
    assert_eq!(
        decision,
        MemberDecision::Refuse {
            code: close_code::NOT_MEMBER,
            proof: None
        }
    );
    assert_eq!(sim.room(b).version(), version);
}

#[test]
fn a_former_member_is_shown_why() {
    let (mut sim, a, b, c) = three(26);
    let policy_c_had = sim.room(c).owned_policy();
    sim.crash(c);
    let fx = sim.room_mut(a).ban(c).unwrap();
    sim.apply(a, fx);
    sim.settle(1);
    let (decision, _) = sim.room_mut(b).member_hello(c, policy_c_had);
    assert!(matches!(
        decision,
        MemberDecision::Refuse {
            code: close_code::BANNED,
            proof: Some(_)
        }
    ));
}

/// C leaves while nobody else is online: it keeps the departure until
/// someone has it.
#[test]
fn a_member_who_leaves_alone_tells_the_room_later() {
    let (mut sim, a, b, c) = three(27);
    sim.crash(a);
    sim.crash(b);
    let now = sim.now;
    let fx = sim.room_mut(c).leave(now).unwrap();
    sim.apply(c, fx);
    assert!(!sim.members[&c].departed);
    assert!(sim.room(c).is_leaving());
    sim.settle(2);
    // C restarts in between: still leaving.
    sim.crash(c);
    sim.restart(c);
    assert!(sim.room(c).is_leaving());
    sim.restart(a);
    sim.restart(b);
    sim.settle(5);
    assert!(sim.members[&c].departed);
    sim.assert_full_mesh(&[a, b]);
    for id in [a, b] {
        assert!(!sim.room(id).policy().body.lists(c), "{id:?}");
    }
}

/// The owner closes the room while C is offline: C hears it on return.
#[test]
fn a_closed_room_reaches_members_who_were_offline() {
    let (mut sim, a, b, c) = three(28);
    sim.crash(c);
    let now = sim.now;
    let fx = sim.room_mut(a).leave(now).unwrap();
    sim.apply(a, fx);
    assert!(!sim.members[&a].departed, "C hasn't heard");
    sim.settle(1);
    assert_eq!(sim.members[&b].removed, Some(false));
    // The owner restarts meanwhile and still owes C the news.
    sim.crash(a);
    sim.restart(a);
    assert!(sim.room(a).is_leaving());
    sim.settle(2);
    sim.restart(c);
    sim.settle(5);
    assert_eq!(sim.members[&c].removed, Some(false), "C was told");
    assert!(sim.members[&a].departed);
}

#[test]
fn parallel_checks_cannot_outnumber_an_invites_allowance() {
    let mut sim = Sim::new(29);
    let a = sim.add(1);
    sim.create(a);
    let invite = sim.invite(a);
    let tag = invite_tag(&invite.token);
    let unix = sim.unix;
    let guessers: Vec<MemberId> = (40..47).map(|s| sim.add(s)).collect();
    for (i, g) in guessers.iter().enumerate() {
        let decision = sim.room_mut(a).join_request(*g, tag, unix);
        if i < MAX_INVITE_FAILURES as usize {
            assert!(matches!(decision, JoinDecision::Proceed { .. }), "{i}");
        } else {
            assert_eq!(decision, JoinDecision::Refuse(close_code::INVITE_EXPIRED));
        }
    }
    // One check is abandoned: its place is free again.
    sim.room_mut(a).join_released(tag);
    assert!(matches!(
        sim.room_mut(a).join_request(guessers[6], tag, unix),
        JoinDecision::Proceed { .. }
    ));
    // Four fail: one try is left, and it is already running.
    for _ in 0..4 {
        let fx = sim.room_mut(a).join_failed(tag);
        sim.apply(a, fx);
    }
    assert_eq!(
        sim.room_mut(a).join_request(guessers[0], tag, unix),
        JoinDecision::Refuse(close_code::INVITE_EXPIRED)
    );
}

/// Whatever changed while a password check ran is checked again at the end.
#[test]
fn admission_is_checked_again_after_the_password() {
    let mut sim = Sim::new(30);
    let a = sim.add(1);
    sim.create(a);
    let unix = sim.unix;
    let (d, e, f) = (sim.add(4), sim.add(5), sim.add(6));

    // Revoked meanwhile.
    let invite = sim.invite(a);
    let tag = invite_tag(&invite.token);
    assert!(matches!(
        sim.room_mut(a).join_request(d, tag, unix),
        JoinDecision::Proceed { .. }
    ));
    let fx = sim.room_mut(a).revoke_invites().unwrap();
    sim.apply(a, fx);
    assert_eq!(
        sim.room_mut(a).admit(d, tag, unix),
        Err(close_code::INVITE_EXPIRED)
    );

    // Expired meanwhile.
    let invite = sim.invite(a);
    let tag = invite_tag(&invite.token);
    assert!(matches!(
        sim.room_mut(a).join_request(d, tag, unix),
        JoinDecision::Proceed { .. }
    ));
    assert_eq!(
        sim.room_mut(a).admit(d, tag, unix + WEEK),
        Err(close_code::INVITE_EXPIRED)
    );

    // Banned meanwhile.
    let invite = sim.invite(a);
    let tag = invite_tag(&invite.token);
    assert!(matches!(
        sim.room_mut(a).join_request(e, tag, unix),
        JoinDecision::Proceed { .. }
    ));
    let fx = sim.room_mut(a).ban(e).unwrap();
    sim.apply(a, fx);
    assert_eq!(sim.room_mut(a).admit(e, tag, unix), Err(close_code::BANNED));

    // Two checks race for the last place.
    let invite = sim.invite(a);
    for seed in 10..(10 + MAX_MEMBERS as u8 - 2) {
        let id = sim.add(seed);
        sim.join(id, &invite).unwrap();
    }
    let tag = invite_tag(&invite.token);
    for id in [d, f] {
        assert!(matches!(
            sim.room_mut(a).join_request(id, tag, unix),
            JoinDecision::Proceed { .. }
        ));
    }
    assert!(sim.room_mut(a).admit(d, tag, unix).is_ok());
    assert_eq!(
        sim.room_mut(a).admit(f, tag, unix),
        Err(close_code::ROOM_FULL)
    );
    assert_eq!(sim.room(a).policy().body.roster.len(), MAX_MEMBERS);
}

// ---- Handing the room over (M13) ----

/// A offers B the room, B accepts: B owns it everywhere, C (away meanwhile)
/// included once back, signs its first policy at once, and lets people in;
/// A is a member like any other.
#[test]
fn a_room_handed_over_is_owned_by_its_new_owner_everywhere() {
    let (mut sim, a, b, c) = three(40);
    sim.crash(c);
    let before = sim.room(a).version();
    assert!(sim.offer_and_accept(a, b));
    sim.settle(1);
    for id in [a, b] {
        assert_eq!(sim.room(id).owner(), b, "{id:?}");
        assert_eq!(sim.room(id).founder(), a);
    }
    assert!(sim.room(b).is_owner() && !sim.room(a).is_owner());
    assert_eq!(sim.room(b).version(), before + 1, "B signed straight away");
    assert_eq!(sim.room(a).version(), before + 1);
    sim.restart(c);
    sim.settle(3);
    assert_eq!(sim.room(c).owner(), b);
    assert_eq!(sim.room(c).saved().handovers, sim.room(b).saved().handovers);
    // B's invites name the founder: a version 2 invite.
    let invite = sim.invite(b);
    assert_eq!((invite.owner, invite.founder), (b, Some(a)));
    let d = sim.add(4);
    sim.join(d, &invite).unwrap();
    sim.settle(2);
    sim.assert_full_mesh(&[a, b, c, d]);
    // And it survives a restart, from the saved state.
    sim.crash(a);
    sim.restart(a);
    sim.settle(2);
    assert_eq!(sim.room(a).owner(), b);
}

/// After handing the room over, A admits, invites, bans and revokes nothing;
/// its old invite is refused as expired, and its leaving is a member's.
#[test]
fn a_former_owner_can_no_longer_admit_invite_or_ban() {
    let (mut sim, a, b, c) = three(41);
    let old = sim.invite(a);
    assert!(sim.offer_and_accept(a, b));
    sim.settle(1);
    let unix = sim.unix;
    assert_eq!(
        sim.room_mut(a)
            .create_invite(token(90), unix, WEEK, true)
            .err(),
        Some(RoomError::NotOwner)
    );
    assert_eq!(sim.room_mut(a).ban(c), Err(RoomError::NotOwner));
    assert_eq!(sim.room_mut(a).revoke_invites(), Err(RoomError::NotOwner));
    let d = sim.add(4);
    assert_eq!(sim.join(d, &old), Err(close_code::INVITE_EXPIRED));
    // A leaves: B takes it off the roster; the room stays.
    let now = sim.now;
    let fx = sim.room_mut(a).leave(now).unwrap();
    sim.apply(a, fx);
    sim.settle(2);
    assert!(sim.members[&a].departed);
    for id in [b, c] {
        assert!(!sim.room(id).policy().body.lists(a), "{id:?}");
    }
    sim.assert_full_mesh(&[b, c]);
}

/// Whatever A signs after handing over, under the shorter chain, loses to
/// B's, however high its version (the second review's finding: a cheap
/// "no newer than ours" check on versions alone kept it).
#[test]
fn a_former_owners_later_policies_lose_everywhere() {
    let (mut sim, a, b, c) = three(42);
    assert!(sim.offer_and_accept(a, b));
    sim.settle(1);
    let room = sim.room(a).id();
    let roster = sim.room(a).policy().body.roster.clone();
    let rogue = sign_policy(
        &key(1),
        RoomPolicy {
            room,
            version: sim.room(a).version() + RESTORE_SKIP,
            name: "Couch".into(),
            roster: roster.iter().copied().filter(|e| e.id != c).collect(),
            banned: vec![c],
        },
    );
    let conn = sim.conn_to(c, b);
    let now = sim.now;
    let fx = sim.room_mut(c).received(
        conn,
        ControlMessage::PolicyUpdate(Box::new(OwnedPolicy::founders(rogue))),
        now,
    );
    sim.apply(c, fx);
    sim.settle(1);
    assert!(sim.room(c).is_member(c) && !sim.room(c).is_banned(c));
    assert_eq!(sim.room(c).owner(), b);

    // The other way round: D, away during the handover, takes A's rogue
    // policy first (its own chain is A's too). B's, at a lower version but
    // with the longer chain, still wins once D meets it.
    let (mut sim, a, b, d) = three(54);
    sim.crash(d);
    assert!(sim.offer_and_accept(a, b));
    sim.settle(1);
    let x = sim.add(9);
    let rogue = OwnedPolicy::founders(sign_policy(
        &key(1),
        RoomPolicy {
            room: sim.room(a).id(),
            version: sim.room(a).version() + RESTORE_SKIP,
            name: "Couch".into(),
            roster: sim.room(a).policy().body.roster.clone(),
            banned: vec![x],
        },
    ));
    sim.restart(d);
    let now = sim.now;
    let fx = sim
        .room_mut(d)
        .refused(b, close_code::NOT_MEMBER, Some(rogue), now);
    sim.apply(d, fx);
    assert!(sim.room(d).is_banned(x), "D took A's rogue policy");
    sim.settle(3);
    assert_eq!(sim.room(d).owner(), b);
    assert_eq!(sim.room(d).policy(), sim.room(b).policy());
}

/// A chain that parts from ours is never taken, and a stranger showing one
/// made up by a former owner (naming the stranger) learns nothing of the
/// roster.
#[test]
fn diverging_chains_are_refused() {
    let (mut sim, a, b, c) = three(43);
    let at = sim.room(a).owned_policy();
    assert!(sim.offer_and_accept(a, b));
    sim.settle(1);
    // A, handed over to B, signs a second handover after the same version,
    // to a stranger S, and the policy it handed with it, listing S.
    let s = sim.add(9);
    let room = sim.room(a).id();
    let mut roster = at.policy.body.roster.clone();
    roster.push(RosterEntry { id: s, since: 0 });
    let fork = OwnedPolicy {
        policy: sign_policy(
            &key(1),
            RoomPolicy {
                room,
                roster,
                ..at.policy.body.clone()
            },
        ),
        handovers: vec![sign_handover(
            &key(1),
            Handover {
                room,
                from: a,
                to: s,
                after: at.policy.body.version,
            },
        )],
    };
    // It checks out on its own...
    assert!(check_owned(&fork, room, a, 0).is_ok());
    // ...but C, holding A→B, never takes it, and shows S nothing.
    let (decision, _) = sim.room_mut(c).member_hello(s, fork.clone());
    assert_eq!(
        decision,
        MemberDecision::Refuse {
            code: close_code::NOT_MEMBER,
            proof: None
        }
    );
    assert_eq!(sim.room(c).owner(), b);
    let conn = sim.conn_to(c, b);
    let now = sim.now;
    let fx = sim
        .room_mut(c)
        .received(conn, ControlMessage::PolicyUpdate(Box::new(fork)), now);
    sim.apply(c, fx);
    assert_eq!(sim.room(c).owner(), b);
}

/// What a chain must be, case by case.
#[test]
fn invalid_chains_are_refused() {
    let room = RoomId([5; 16]);
    let (f, n, m) = (key(1), key(2), key(3));
    let (fid, nid, mid) = (member_id(&f), member_id(&n), member_id(&m));
    let policy = |signer: &SigningKey, version| {
        sign_policy(
            signer,
            RoomPolicy {
                room,
                version,
                name: "Couch".into(),
                roster: vec![RosterEntry { id: fid, since: 0 }],
                banned: Vec::new(),
            },
        )
    };
    let hand = |signer: &SigningKey, from, to, after| {
        sign_handover(
            signer,
            Handover {
                room,
                from,
                to,
                after,
            },
        )
    };
    let owned = |policy, handovers| OwnedPolicy { policy, handovers };
    let ok = |o: &OwnedPolicy| check_owned(o, room, fid, 0);
    // Good: F→N after 3, N signs 4; F's own policy at 3 (the one handed
    // over); F→N→M.
    assert_eq!(
        ok(&owned(policy(&n, 4), vec![hand(&f, fid, nid, 3)])),
        Ok(nid)
    );
    assert_eq!(
        ok(&owned(policy(&f, 3), vec![hand(&f, fid, nid, 3)])),
        Ok(nid)
    );
    let two = vec![hand(&f, fid, nid, 3), hand(&n, nid, mid, 5)];
    assert_eq!(ok(&owned(policy(&m, 6), two.clone())), Ok(mid));
    let bad = [
        // Not from the founder.
        owned(policy(&m, 4), vec![hand(&n, nid, mid, 3)]),
        // A broken link.
        owned(
            policy(&m, 6),
            vec![hand(&f, fid, nid, 3), hand(&m, mid, fid, 5)],
        ),
        // To itself.
        owned(policy(&f, 4), vec![hand(&f, fid, fid, 3)]),
        // `after` not increasing.
        owned(
            policy(&m, 6),
            vec![hand(&f, fid, nid, 3), hand(&n, nid, mid, 3)],
        ),
        // The new owner signing at or before the handover's version.
        owned(policy(&n, 2), vec![hand(&f, fid, nid, 3)]),
        // The one handing over signing at another version than `after`.
        owned(policy(&f, 4), vec![hand(&f, fid, nid, 3)]),
        // The wrong signer: a handover N didn't make, a policy F signs.
        owned(
            policy(&m, 6),
            vec![hand(&f, fid, nid, 3), hand(&f, nid, mid, 5)],
        ),
        owned(policy(&f, 6), two.clone()),
        // Another room's handover.
        owned(
            policy(&n, 4),
            vec![sign_handover(
                &f,
                Handover {
                    room: RoomId([6; 16]),
                    from: fid,
                    to: nid,
                    after: 3,
                },
            )],
        ),
    ];
    for (i, case) in bad.iter().enumerate() {
        assert_eq!(ok(case), Err(RoomError::BadPolicy), "case {i}");
    }
    // At most 32: F and N passing it back and forth.
    let keys = [&f, &n];
    let ids = [fid, nid];
    let chain: Vec<_> = (0..=MAX_HANDOVERS)
        .map(|i| hand(keys[i % 2], ids[i % 2], ids[(i + 1) % 2], i as u32))
        .collect();
    // After an even number of handovers F owns it again.
    let long = owned(
        policy(keys[MAX_HANDOVERS % 2], 100),
        chain[..MAX_HANDOVERS].to_vec(),
    );
    assert!(ok(&long).is_ok());
    let too_long = owned(policy(keys[(MAX_HANDOVERS + 1) % 2], 100), chain);
    assert_eq!(ok(&too_long), Err(RoomError::BadPolicy));
}

/// F→N→F: F's policies from its first time as owner don't pass under the
/// chain that made it owner again.
#[test]
fn an_earlier_tenures_policy_is_not_replayed() {
    let room = RoomId([5; 16]);
    let (f, n) = (key(1), key(2));
    let (fid, nid) = (member_id(&f), member_id(&n));
    let first = sign_policy(
        &f,
        RoomPolicy {
            room,
            version: 2,
            name: "Couch".into(),
            roster: vec![RosterEntry { id: fid, since: 0 }],
            banned: Vec::new(),
        },
    );
    let chain = vec![
        sign_handover(
            &f,
            Handover {
                room,
                from: fid,
                to: nid,
                after: 3,
            },
        ),
        sign_handover(
            &n,
            Handover {
                room,
                from: nid,
                to: fid,
                after: 5,
            },
        ),
    ];
    let replayed = OwnedPolicy {
        policy: first,
        handovers: chain,
    };
    assert_eq!(
        check_owned(&replayed, room, fid, 0),
        Err(RoomError::BadPolicy)
    );
}

/// An answer for an offer that no longer stands hands nothing over: after a
/// policy change, after a withdrawal and a new offer (the old id), or from
/// someone else. And offers that can't be made aren't.
#[test]
fn stale_answers_and_bad_offers_hand_nothing_over() {
    let (mut sim, a, b, c) = three(44);
    // After a policy change: A bans someone in between.
    let fx = sim.room_mut(a).offer_room(b, 1).unwrap();
    sim.apply(a, fx);
    sim.step();
    assert_eq!(sim.room(b).offered(), Some(false));
    let x = sim.add(9);
    let fx = sim.room_mut(a).ban(x).unwrap();
    sim.apply(a, fx);
    assert_eq!(sim.room(a).offer(), None, "the ban voided it");
    let fx = sim.room_mut(b).answer_offer(true);
    // B may still have it (the withdrawal is on its way) or not.
    if let Ok(fx) = fx {
        sim.apply(b, fx);
    }
    sim.settle(1);
    assert_eq!(sim.room(b).owner(), a);
    assert_eq!(sim.room(b).offered(), None);
    // A withdrawn offer's id, answered on: nothing.
    let fx = sim.room_mut(a).offer_room(b, 2).unwrap();
    sim.apply(a, fx);
    sim.step();
    let fx = sim.room_mut(a).withdraw_offer();
    sim.apply(a, fx);
    let fx = sim.room_mut(a).offer_room(b, 3).unwrap();
    sim.apply(a, fx);
    let conn = sim.conn_to(a, b);
    let now = sim.now;
    let fx = sim.room_mut(a).received(
        conn,
        ControlMessage::HandoverAnswer(HandoverAnswer {
            offer: 2,
            accept: true,
        }),
        now,
    );
    sim.apply(a, fx);
    assert_eq!(sim.room(a).owner(), a);
    // C answering B's offer: nothing.
    let conn = sim.conn_to(a, c);
    let fx = sim.room_mut(a).received(
        conn,
        ControlMessage::HandoverAnswer(HandoverAnswer {
            offer: 3,
            accept: true,
        }),
        now,
    );
    sim.apply(a, fx);
    assert_eq!(sim.room(a).owner(), a);
    assert_eq!(sim.room(a).offer(), Some(b));
    // Offers that can't be made.
    assert_eq!(sim.room_mut(a).offer_room(a, 4), Err(RoomError::NotMember));
    assert_eq!(sim.room_mut(a).offer_room(x, 4), Err(RoomError::NotMember));
    assert_eq!(sim.room_mut(b).offer_room(c, 4), Err(RoomError::NotOwner));
    sim.crash(c);
    assert_eq!(sim.room_mut(a).offer_room(c, 4), Err(RoomError::NotOnline));
    // Only the owner offers: B ignores an offer from C.
    sim.restart(c);
    let fx = sim.room_mut(a).withdraw_offer();
    sim.apply(a, fx);
    sim.settle(2);
    assert_eq!(sim.room(b).offered(), None);
    let conn = sim.conn_to(b, c);
    let fx = sim.room_mut(b).received(
        conn,
        ControlMessage::HandoverOffer(HandoverOffer {
            offer: 5,
            version: 0,
        }),
        now,
    );
    sim.apply(b, fx);
    assert_eq!(sim.room(b).offered(), None);
}

/// A newcomer whose password check runs while the room changes hands is
/// refused after it: the old owner signs nothing more.
#[test]
fn a_newcomer_mid_check_during_a_handover_is_refused() {
    let (mut sim, a, b, _) = three(45);
    let invite = sim.invite(a);
    let d = sim.add(4);
    let unix = sim.unix;
    let tag = invite_tag(&invite.token);
    assert!(matches!(
        sim.room_mut(a).join_request(d, tag, unix),
        JoinDecision::Proceed { .. }
    ));
    assert!(sim.offer_and_accept(a, b));
    let version = sim.room(a).version();
    assert_eq!(
        sim.room_mut(a).admit(d, tag, unix),
        Err(close_code::INVITE_EXPIRED)
    );
    assert_eq!(sim.room(a).version(), version);
}

/// The handover reaches B through the others when A never talks to it again
/// (A's messages to B lost, A gone for good).
#[test]
fn the_others_carry_the_handover_to_the_new_owner() {
    let (mut sim, a, b, c) = three(46);
    let fx = sim.room_mut(a).offer_room(b, 1).unwrap();
    sim.apply(a, fx);
    sim.step();
    let fx = sim.room_mut(b).answer_offer(true).unwrap();
    sim.apply(b, fx);
    // Deliver B's answer alone: A hands over.
    let to_a = sim.conn_to(b, a);
    let answer = sim.queues.get_mut(&to_a).unwrap().pop_front().unwrap();
    let a_conn = sim.pipes[&to_a].2;
    sim.deliver(a, a_conn, answer);
    assert_eq!(sim.room(a).owner(), b);
    // What A sends B is lost, and A is gone for good.
    let to_b = sim.conn_to(a, b);
    sim.queues.remove(&to_b);
    let crashed = sim
        .pipes
        .keys()
        .copied()
        .filter(|c| sim.pipes[c].0 == a)
        .collect::<Vec<_>>();
    for conn in crashed {
        if conn != to_b {
            while let Some(m) = sim.queues.get_mut(&conn).and_then(VecDeque::pop_front) {
                let (_, other, other_conn) = sim.pipes[&conn];
                sim.deliver(other, other_conn, m);
            }
        }
    }
    sim.crash(a);
    sim.settle(2);
    assert_eq!(sim.room(b).owner(), b);
    assert!(sim.room(b).is_owner());
    assert_eq!(sim.room(c).owner(), b);
    assert_eq!(sim.room(c).version(), sim.room(b).version());
}

/// B accepts, then crashes before the handover reaches it: back, it learns
/// it from the room and owns it, signing its first policy.
#[test]
fn a_new_owner_that_restarted_meanwhile_still_takes_over() {
    let (mut sim, a, b, c) = three(47);
    let fx = sim.room_mut(a).offer_room(b, 1).unwrap();
    sim.apply(a, fx);
    sim.step();
    let fx = sim.room_mut(b).answer_offer(true).unwrap();
    sim.apply(b, fx);
    let before = sim.room(a).version();
    sim.crash(b); // delivers its answer as it goes
    assert_eq!(sim.room(a).owner(), b);
    sim.restart(b);
    sim.settle(3);
    assert!(sim.room(b).is_owner());
    assert_eq!(sim.room(b).version(), before + 1);
    assert_eq!(sim.room(c).version(), before + 1);
}

/// An acceptance stands until the owner's word (the second review's
/// finding: one lost to a dropped connection or a second thought, then a
/// leave, left a room nobody owned). B accepts; the owner signs, but the
/// connection drops before B hears it. B can't leave or take it back, and
/// owns the room once it reconnects.
#[test]
fn an_acceptance_stands_until_the_owner_hands_over() {
    let (mut sim, a, b, c) = three(48);
    sim.crash(c);
    let fx = sim.room_mut(a).offer_room(b, 1).unwrap();
    sim.apply(a, fx);
    sim.step();
    let fx = sim.room_mut(b).answer_offer(true).unwrap();
    sim.apply(b, fx);
    // A gets the answer and signs; what it sends B is lost with the link.
    let to_a = sim.conn_to(b, a);
    let answer = sim.queues.get_mut(&to_a).unwrap().pop_front().unwrap();
    let a_conn = sim.pipes[&to_a].2;
    sim.deliver(a, a_conn, answer);
    assert_eq!(sim.room(a).owner(), b);
    sim.queues.remove(&sim.conn_to(a, b));
    sim.close(to_a);
    let now = sim.now;
    assert_eq!(sim.room(b).offered(), Some(true));
    assert_eq!(
        sim.room_mut(b).leave(now).err(),
        Some(RoomError::OfferAccepted)
    );
    assert_eq!(
        sim.room_mut(b).answer_offer(false).err(),
        Some(RoomError::OfferAccepted)
    );
    sim.settle(3);
    assert!(sim.room(b).is_owner());
    assert_eq!(sim.room(b).offered(), None);
}

/// B accepts, but the connection drops before A hears it: A never signs,
/// and tells B the offer is void when they meet again, so B may leave.
#[test]
fn an_acceptance_lost_on_the_way_is_withdrawn_on_reconnecting() {
    let (mut sim, a, b, _) = three(55);
    let fx = sim.room_mut(a).offer_room(b, 1).unwrap();
    sim.apply(a, fx);
    sim.step();
    let fx = sim.room_mut(b).answer_offer(true).unwrap();
    sim.apply(b, fx);
    let to_a = sim.conn_to(b, a);
    sim.queues.remove(&to_a);
    sim.close(to_a);
    assert_eq!(sim.room(a).offer(), None);
    assert_eq!(sim.room(b).offered(), Some(true));
    sim.settle(3);
    assert_eq!(sim.room(a).owner(), a);
    assert_eq!(sim.room(b).offered(), None, "A said it was void");
    let now = sim.now;
    assert!(sim.room_mut(b).leave(now).is_ok());
}

/// A relayed policy keeps the verified copies of the handovers we hold: a
/// member that swaps their signatures can't plant ones that fail on our
/// next start (the second review's finding).
#[test]
fn a_relayed_policy_keeps_our_verified_handovers() {
    let (mut sim, a, b, c) = three(56);
    assert!(sim.offer_and_accept(a, b));
    sim.settle(1);
    let held = sim.room(c).saved().handovers;
    // B signs a newer policy, sent with the handover's signature ruined.
    let x = sim.add(9);
    let fx = sim.room_mut(b).ban(x).unwrap();
    sim.apply(b, fx);
    let mut owned = sim.room(b).owned_policy();
    owned.handovers[0].sig = couch_proto::ids::Signature([0; 32], [0; 32]);
    sim.queues.clear();
    let conn = sim.conn_to(c, b);
    let now = sim.now;
    let fx = sim
        .room_mut(c)
        .received(conn, ControlMessage::PolicyUpdate(Box::new(owned)), now);
    sim.apply(c, fx);
    assert!(sim.room(c).is_banned(x), "the newer policy was taken");
    assert_eq!(sim.room(c).saved().handovers, held, "with our own copies");
    let saved = sim.room(c).saved();
    assert!(Room::restore(key(3), saved, Timings::default(), now).is_ok());
}

/// A former owner can't sign, under the start of our chain, a policy past
/// the handover that names a stranger, to have the stranger shown the
/// roster (the second review's finding).
#[test]
fn a_former_owners_late_policy_proves_no_stranger_a_member() {
    let (mut sim, a, b, c) = three(57);
    assert!(sim.offer_and_accept(a, b));
    sim.settle(1);
    let s = sim.add(9);
    let mut roster = sim.room(c).policy().body.roster.clone();
    roster.push(RosterEntry { id: s, since: 0 });
    let late = OwnedPolicy::founders(sign_policy(
        &key(1),
        RoomPolicy {
            room: sim.room(c).id(),
            version: sim.room(c).version() + 5,
            name: "Couch".into(),
            roster,
            banned: Vec::new(),
        },
    ));
    let (decision, _) = sim.room_mut(c).member_hello(s, late);
    assert_eq!(
        decision,
        MemberDecision::Refuse {
            code: close_code::NOT_MEMBER,
            proof: None
        }
    );
}

/// An offer to a room catching up after a restore is declined at once,
/// and an owner can't offer the room to someone who left.
#[test]
fn offers_while_catching_up_or_to_a_departed_member_go_nowhere() {
    let (mut sim, a, b, c) = three(58);
    // C leaves; A can't offer it the room.
    let now = sim.now;
    let fx = sim.room_mut(c).leave(now).unwrap();
    sim.apply(c, fx);
    sim.settle(1);
    assert_eq!(sim.room_mut(a).offer_room(c, 1), Err(RoomError::NotMember));
    // B catching up (as after a restore): A's offer is declined at once.
    let fx = sim.room_mut(b).catch_up_first();
    assert!(fx.is_empty() && sim.room(b).is_catching_up());
    let conn = sim.conn_to(b, a);
    let (now, version) = (sim.now, sim.room(a).version());
    let fx = sim.room_mut(b).received(
        conn,
        ControlMessage::HandoverOffer(HandoverOffer { offer: 2, version }),
        now,
    );
    assert_eq!(sim.room(b).offered(), None);
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::Send {
            message: ControlMessage::HandoverAnswer(HandoverAnswer { accept: false, .. }),
            ..
        }
    )));
}

/// B bans A, then B leaving closes the room for everyone, A included.
#[test]
fn the_new_owner_can_ban_the_old_one_and_close_the_room() {
    let (mut sim, a, b, c) = three(49);
    assert!(sim.offer_and_accept(a, b));
    sim.settle(1);
    let fx = sim.room_mut(b).ban(a).unwrap();
    sim.apply(b, fx);
    sim.settle(1);
    assert_eq!(sim.members[&a].removed, Some(true));
    let now = sim.now;
    let fx = sim.room_mut(b).leave(now).unwrap();
    sim.apply(b, fx);
    sim.settle(2);
    assert_eq!(sim.members[&c].removed, Some(false));
}

/// A joiner's snapshot must come from the owner its invite names, by a
/// chain from the founder.
#[test]
fn a_joiner_takes_only_the_owner_its_invite_names() {
    let (mut sim, a, b, _) = three(50);
    assert!(sim.offer_and_accept(a, b));
    sim.settle(1);
    let d = sim.add(4);
    let invite = sim.invite(b);
    let tag = invite_tag(&invite.token);
    let unix = sim.unix;
    let fx = sim.room_mut(b).admit(d, tag, unix).unwrap();
    sim.apply(b, fx);
    let snapshot = sim.room(b).snapshot();
    let me = |name: &str| Identity {
        key: key(4),
        name: name.into(),
        addr: PeerAddr::default(),
    };
    let now = sim.now;
    // Named A as the owner (an old invite, or a made-up one): refused.
    assert!(matches!(
        Room::join(
            me("Davi"),
            a,
            a,
            invite.nonce,
            snapshot.clone(),
            Timings::default(),
            now
        ),
        Err(RoomError::BadPolicy)
    ));
    // The right founder and owner: in.
    assert!(
        Room::join(
            me("Davi"),
            a,
            b,
            invite.nonce,
            snapshot,
            Timings::default(),
            now
        )
        .is_ok()
    );
}

/// Restores with handovers: A restored from a backup made before it handed
/// the room to B catches up and finds itself a member; B, restored from a
/// backup made before it took over, finds itself the owner, catches up as
/// one and signs ahead.
#[test]
fn restores_follow_the_chain() {
    let (mut sim, a, b, c) = three(51);
    let a_before = sim.members[&a].saved.clone().unwrap();
    let b_before = sim.members[&b].saved.clone().unwrap();
    assert!(sim.offer_and_accept(a, b));
    sim.settle(1);
    let handed = sim.room(b).version();
    // A's PC is lost; its backup comes back.
    sim.crash(a);
    assert!(sim.restore_from_backup(a, a_before));
    sim.settle(2);
    assert!(!sim.room(a).is_catching_up());
    assert!(!sim.room(a).is_owner());
    assert_eq!(sim.room(a).owner(), b);
    // B's PC is lost; its backup (a member's) comes back, with A and C away
    // but for C.
    sim.crash(b);
    sim.crash(a);
    assert!(sim.restore_from_backup(b, b_before));
    sim.settle(2);
    // C's snapshot shows B owns it: it waits for A too, as an owner.
    assert!(sim.room(b).is_owner());
    assert!(sim.room(b).is_catching_up());
    sim.restart(a);
    sim.settle(3);
    assert!(!sim.room(b).is_catching_up());
    assert_eq!(sim.room(b).version(), handed + RESTORE_SKIP);
    assert_eq!(sim.room(c).version(), handed + RESTORE_SKIP);
}

/// A restored member that caught up from someone who missed its newer state
/// (it became the owner after the backup) finds a policy it signed and
/// doesn't hold: it catches up again, as owner, and signs ahead (the second
/// review's finding: it would have signed at +1, over what an absent
/// member holds).
#[test]
fn finding_our_own_newer_policy_catches_up_again() {
    let (mut sim, a, b, c) = three(52);
    let b_before = sim.members[&b].saved.clone().unwrap();
    sim.crash(c);
    assert!(sim.offer_and_accept(a, b));
    sim.settle(1);
    let x = sim.add(9);
    let fx = sim.room_mut(b).ban(x).unwrap();
    sim.apply(b, fx);
    sim.settle(1);
    let newest = sim.room(b).version();
    // B's PC is lost while A is away; C, stale, comes back with B's backup.
    sim.crash(b);
    sim.crash(a);
    sim.restart(c);
    assert!(sim.restore_from_backup(b, b_before));
    sim.settle(2);
    assert!(!sim.room(b).is_catching_up(), "a member caught up from C");
    assert_eq!(sim.room(b).owner(), a);
    // A comes back with B's own newer policy.
    sim.restart(a);
    sim.settle(1);
    assert!(sim.room(b).is_owner());
    sim.settle(35);
    assert!(!sim.room(b).is_catching_up());
    assert_eq!(sim.room(b).version(), newest + RESTORE_SKIP);
}

/// Stopped waiting: the room signs, but isn't handed over until every member
/// on the roster has been heard.
#[test]
fn no_handover_after_stop_waiting_until_everyone_is_heard() {
    let (mut sim, a, b, c) = three(53);
    let backup = sim.members[&a].saved.clone().unwrap();
    sim.crash(a);
    sim.crash(b);
    sim.crash(c);
    assert!(sim.restore_from_backup(a, backup));
    let fx = sim.room_mut(a).stop_catching_up().unwrap();
    sim.apply(a, fx);
    sim.restart(b);
    sim.settle(2);
    assert_eq!(
        sim.room_mut(a).offer_room(b, 1),
        Err(RoomError::Unconfirmed)
    );
    sim.restart(c);
    sim.settle(35);
    assert!(sim.offer_and_accept(a, b));
    sim.settle(1);
    assert_eq!(sim.room(c).owner(), b);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Whatever order messages interleave in, and whoever crashes and comes
    /// back (the owner included, before or after handing the room over),
    /// the room settles into one mesh with the same view everywhere: one
    /// policy, one chain of handovers, one owner.
    #[test]
    fn rooms_converge(
        seed in any::<u64>(),
        crashes in proptest::collection::vec((0usize..4, 0u64..30), 0..6),
        late_join in proptest::option::of(0usize..6),
        handover in proptest::option::of((0usize..6, 1usize..4)),
    ) {
        let mut sim = Sim::new(seed);
        let ids: Vec<MemberId> = (1..=5).map(|s| sim.add(s)).collect();
        sim.create(ids[0]);
        let invite = sim.invite(ids[0]);
        for id in &ids[1..4] {
            sim.join(*id, &invite).unwrap();
            sim.step();
        }
        let mut joined_late = false;
        for (i, (who, after_ticks)) in crashes.iter().enumerate() {
            for _ in 0..*after_ticks {
                sim.step();
            }
            // M13: the owner, if online, hands the room to a member online.
            if let Some((at, to)) = handover
                && at == i
            {
                let owner = ids[..4]
                    .iter()
                    .copied()
                    .find(|id| sim.members[id].online && sim.room(*id).is_owner());
                if let Some(owner) = owner
                    && sim.members[&ids[to]].online
                    && owner != ids[to]
                {
                    sim.offer_and_accept(owner, ids[to]);
                }
            }
            // A newcomer joins through whoever owns the room now.
            let owner = ids[..4]
                .iter()
                .copied()
                .find(|id| sim.members[id].online && sim.room(*id).is_owner());
            if late_join == Some(i) && let Some(owner) = owner {
                let invite = sim.invite(owner);
                sim.join(ids[4], &invite).unwrap();
                joined_late = true;
            }
            let id = ids[*who];
            if sim.members[&id].online {
                sim.crash(id);
            } else {
                sim.restart(id);
            }
        }
        for id in ids.clone() {
            if !sim.members[&id].online && sim.members[&id].saved.is_some() {
                sim.restart(id);
            }
        }
        // Restarted members dial everyone at once, so this is plenty.
        sim.settle(15);
        let expected: Vec<MemberId> = if joined_late { ids.clone() } else { ids[..4].to_vec() };
        sim.assert_full_mesh(&expected);
        let reference = sim.room(expected[0]).saved();
        for id in &expected[1..] {
            let saved = sim.room(*id).saved();
            prop_assert_eq!(&saved.policy, &reference.policy);
            prop_assert_eq!(&saved.handovers, &reference.handovers);
            prop_assert_eq!(&saved.records, &reference.records);
            prop_assert_eq!(sim.room(*id).owner(), sim.room(expected[0]).owner());
        }
        // Exactly one of them owns it, and has signed as owner.
        let owners: Vec<&MemberId> =
            expected.iter().filter(|id| sim.room(**id).is_owner()).collect();
        prop_assert_eq!(owners.len(), 1);
        prop_assert!(!sim.room(*owners[0]).takeover_pending());
    }
}
