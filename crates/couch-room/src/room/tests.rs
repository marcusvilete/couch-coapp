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
        let Some(policy) = self.members[&dialer]
            .room
            .as_ref()
            .map(|r| r.policy().clone())
        else {
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
                Effect::Changed => {}
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
    assert_eq!(
        sim.room_mut(b).join_request(stranger, tag, unix),
        JoinDecision::Refuse(close_code::PROTOCOL)
    );
    assert_eq!(sim.room_mut(b).ban(c), Err(RoomError::NotOwner));
    assert_eq!(
        sim.room_mut(b).admit(stranger, tag, unix),
        Err(close_code::PROTOCOL)
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
    let policy_c_had = sim.room(c).policy().clone();
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
    let fx = sim
        .room_mut(c)
        .refused(b, close_code::BANNED, Some(forged), now);
    assert!(fx.is_empty());
    // The owner's real policy, which lists C.
    let real = sim.room(a).policy().clone();
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
        ControlMessage::PolicyUpdate(Box::new(forged)),
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
    let real = sim.room(a).policy().clone();
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
    let (decision, _) = sim.room_mut(b).member_hello(id, forged);
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
    let policy_c_had = sim.room(c).policy().clone();
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Whatever order messages interleave in, and whoever crashes and comes
    /// back (the owner included), the room settles into one mesh with the
    /// same view everywhere.
    #[test]
    fn rooms_converge(
        seed in any::<u64>(),
        crashes in proptest::collection::vec((0usize..4, 0u64..30), 0..6),
        late_join in proptest::option::of(0usize..6),
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
            if late_join == Some(i) && sim.members[&ids[0]].online {
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
            prop_assert_eq!(&saved.records, &reference.records);
        }
    }
}
