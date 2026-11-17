//! Relay codes passed on by themselves (M26b, [ADR 0014] "Relay codes passed
//! on by themselves"): a member asks the members of a room it is connected to
//! for the code of a relay it needs and lacks (another member's, by its
//! signed record) or of one of ours refusing the code we hold (a rotation);
//! they answer only with a relay the room really uses, and the code is
//! checked against the relay before it is kept.
//!
//! The rules, each with its test:
//! - **pulled, never pushed:** an answer counts only on a connection we
//!   asked on, for the relay we asked about;
//! - **an answer proves what the room uses:** we answer with relay R's code
//!   only to a member of the room, on that room's current connection, only
//!   if R is ours to pass on there ([`crate::network::Passable`]: the user's
//!   own, not personal, its switch on; a received one only in the rooms it
//!   came for), and R is our own home relay (our signed record names it) or
//!   one of our connections in that room runs through it now, to someone
//!   else than the asker;
//! - **a personal code is never asked for again** once refused: taken back
//!   on purpose (the author's choice);
//! - **at most [`ANSWERS_PER_MINUTE`] answers** a minute to one member,
//!   whatever it asks, and **one ask a minute** per room and relay.
//!
//! [ADR 0014]: ../../../../docs/adr/0014-public-builds-and-registered-relays.md

use std::collections::VecDeque;

use couch_net::endpoint::{Relay, RelayCheck, RelayToken, check_relay};
use couch_proto::room::{NoRelayCode, RelayAnswer, RelayAnswerKind, RelayAsk, RelaySecret};

use super::*;
use crate::reasons::{CodePassing, MemberReason};

/// Answers to one member in a minute, at most.
pub(crate) const ANSWERS_PER_MINUTE: usize = 6;
/// Codes remembered as refused, at most.
const MAX_REFUSED: usize = 256;
/// How often the relays received for rooms are looked at: still needed,
/// or forgotten after 30 days.
const NEEDED_EVERY: Duration = Duration::from_secs(60 * 60);

/// Why we ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    /// A member of the room is on it, and we lack it.
    Missing,
    /// One of ours refuses the code we hold.
    Renew,
}

#[derive(Debug)]
struct Ask {
    why: Why,
    sent: Instant,
    /// How long until it is asked again: doubling while nobody can answer,
    /// up to the regular checks' interval.
    wait: Duration,
    /// The connections asked that haven't answered yet.
    waiting: BTreeSet<ConnId>,
    /// Some answered with a code, being checked now.
    checking: usize,
    /// Everyone asked answered without one (or nobody could be asked).
    nobody: bool,
}

#[derive(Default)]
pub(super) struct Passing {
    asks: BTreeMap<(RoomId, String), Ask>,
    /// When we answered each member lately.
    answered: HashMap<MemberId, VecDeque<Instant>>,
    /// Codes checked and refused, by relay and a hash of the code: never
    /// checked again.
    refused: BTreeSet<(String, [u8; 32])>,
    next_needed: Option<Instant>,
}

/// What the check of a code passed on found.
#[derive(Debug)]
pub(crate) enum Checked {
    /// Kept (it let us in, or didn't answer and is kept unchecked).
    Kept,
    /// The relay refused it.
    Refused,
    /// Not kept: the store said no (full, or the user's own relay didn't
    /// answer), or the relays are fixed.
    NotKept,
}

/// The answer to give `asker`'s ask for `url` in `room`, the pure rule:
/// `token` is the code if it is ours to pass on in that room; `home` that
/// our own signed record names `url`; `carried` that one of our connections
/// in that room, to someone other than the asker, runs through it now.
pub(crate) fn answer_for(token: Option<&RelayToken>, home: bool, carried: bool) -> RelayAnswerKind {
    match token {
        None => RelayAnswerKind::None(NoRelayCode::Unavailable),
        Some(_) if !(home || carried) => RelayAnswerKind::None(NoRelayCode::Unavailable),
        Some(token) => {
            let secret = RelaySecret(token.reveal().to_string());
            // One the asker would refuse whole (too long, or saved before
            // codes were visible ASCII alone) is no answer.
            if secret.is_valid() {
                RelayAnswerKind::Code(secret)
            } else {
                RelayAnswerKind::None(NoRelayCode::Unavailable)
            }
        }
    }
}

fn digest(token: &str) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(token.as_bytes()).into()
}

impl Actor {
    // ---- Answering ----

    pub(super) fn relay_received(
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
            ControlMessage::RelayAsk(ask) if ask.is_valid() => {
                self.answer_ask(room, conn, peer, ask, now);
            }
            ControlMessage::RelayAnswer(answer) if answer.is_valid() => {
                self.take_answer(room, conn, peer, answer, now);
            }
            _ => {}
        }
    }

    fn answer_ask(
        &mut self,
        room: RoomId,
        conn: ConnId,
        peer: MemberId,
        ask: RelayAsk,
        now: Instant,
    ) {
        let recent = self.passing.answered.entry(peer).or_default();
        while recent
            .front()
            .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(60))
        {
            recent.pop_front();
        }
        let answer = if recent.len() >= ANSWERS_PER_MINUTE {
            tracing::debug!(room = ?room, member = ?peer, "too many asks for relay codes");
            RelayAnswerKind::None(NoRelayCode::Busy)
        } else {
            recent.push_back(now);
            let url = crate::reasons::normalized(&ask.url).map(|u| u.to_string());
            match url {
                None => RelayAnswerKind::None(NoRelayCode::Unavailable),
                Some(url) => {
                    let token =
                        crate::network::lock(&self.network).passable_token(&url, &room.to_hex());
                    let home = self
                        .addr
                        .relay
                        .as_deref()
                        .and_then(crate::reasons::normalized)
                        .is_some_and(|ours| ours.as_str() == url);
                    let carried = token.is_some() && !home && self.carried_in(room, peer, &url);
                    answer_for(token.as_ref(), home, carried)
                }
            }
        };
        if !matches!(answer, RelayAnswerKind::None(NoRelayCode::Busy)) {
            tracing::info!(
                room = ?room,
                member = ?peer,
                url = ?ask.url,
                given = matches!(answer, RelayAnswerKind::Code(_)),
                "asked for a relay's code"
            );
        }
        self.send(
            conn,
            Outgoing::Message(ControlMessage::RelayAnswer(RelayAnswer {
                url: ask.url,
                answer,
            })),
        );
    }

    /// Whether one of our connections in `room`, to a member other than
    /// `asker`, runs through relay `url` now.
    fn carried_in(&self, room: RoomId, asker: MemberId, url: &str) -> bool {
        let Some(entry) = self.rooms.get(&room) else {
            return false;
        };
        entry
            .room
            .connections()
            .into_iter()
            .filter(|(peer, _, _)| *peer != asker)
            .filter_map(|(_, conn, _)| self.conns.get(&conn))
            .any(|c| {
                couch_net::health::relay_paths(&c.conn)
                    .iter()
                    .any(|r| r.as_str() == url)
            })
    }

    // ---- Asking ----

    fn take_answer(
        &mut self,
        room: RoomId,
        conn: ConnId,
        peer: MemberId,
        answer: RelayAnswer,
        _now: Instant,
    ) {
        let key = (room, answer.url.clone());
        let Some(ask) = self.passing.asks.get_mut(&key) else {
            return;
        };
        // Only an answer we asked for, once.
        if !ask.waiting.remove(&conn) {
            return;
        }
        let why = ask.why;
        match answer.answer {
            RelayAnswerKind::None(why_not) => {
                tracing::debug!(room = ?room, member = ?peer, url = ?answer.url, ?why_not, "no code for a relay");
            }
            RelayAnswerKind::Code(secret) => {
                let hash = digest(&secret.0);
                let tried = self.passing.refused.contains(&(answer.url.clone(), hash));
                match (
                    RelayToken::new(secret.0),
                    crate::reasons::normalized(&answer.url),
                ) {
                    (Some(token), Some(url)) if !tried => {
                        ask.checking += 1;
                        let from_name = self.member_name(room, peer);
                        self.check_passed_code(room, peer, from_name, url, token, why);
                    }
                    _ => {}
                }
            }
        }
        let Some(ask) = self.passing.asks.get_mut(&key) else {
            return;
        };
        if ask.waiting.is_empty() && ask.checking == 0 && !ask.nobody {
            ask.nobody = true;
            self.reasons_dirty = true;
        }
    }

    /// Checks a code passed on against its relay, off the actor, then keeps
    /// it if it let us in (or, for a relay new to us, didn't answer).
    fn check_passed_code(
        &self,
        room: RoomId,
        from: MemberId,
        from_name: String,
        url: couch_net::RelayUrl,
        token: RelayToken,
        why: Why,
    ) {
        let hub = self.hub.clone();
        let inbox = self.inbox.clone();
        let timeout = self.relay_check;
        tokio::spawn(async move {
            let hash = digest(token.reveal());
            let outcome = check_relay(&Relay::new(url.clone(), Some(token.clone())), timeout).await;
            let checked = match outcome {
                Ok(RelayCheck::Refused(_)) => Checked::Refused,
                Ok(RelayCheck::Accepted) => {
                    keep(&hub, &url, &token, room, from, &from_name, true).await
                }
                // A relay of ours wanting a new code takes it only once it
                // lets us in; one new to us is kept unchecked, tried later.
                Ok(RelayCheck::Unreachable(_)) | Err(_) if why == Why::Missing => {
                    keep(&hub, &url, &token, room, from, &from_name, false).await
                }
                Ok(RelayCheck::Unreachable(_)) | Err(_) => Checked::NotKept,
            };
            tracing::info!(room = ?room, %url, ?checked, "a relay's code passed on, checked");
            let _ = inbox
                .send(Input::RelayCodeChecked {
                    room,
                    url: url.to_string(),
                    hash,
                    checked,
                })
                .await;
        });
    }

    pub(super) fn relay_code_checked(
        &mut self,
        room: RoomId,
        url: String,
        hash: [u8; 32],
        checked: Checked,
    ) {
        if matches!(checked, Checked::Refused) {
            // Bounded: a member could send a new bogus code each minute.
            if self.passing.refused.len() >= MAX_REFUSED {
                self.passing.refused.clear();
            }
            self.passing.refused.insert((url.clone(), hash));
        }
        if let Some(ask) = self.passing.asks.get_mut(&(room, url)) {
            ask.checking = ask.checking.saturating_sub(1);
            if !matches!(checked, Checked::Kept) && ask.waiting.is_empty() && ask.checking == 0 {
                ask.nobody = true;
            }
        }
        self.reasons_dirty = true;
    }

    /// On the tick: asks for the codes our rooms need, and looks after the
    /// relays received for them.
    pub(super) fn passing_tick(&mut self, now: Instant) {
        // Answers older than a minute count no more.
        self.passing.answered.retain(|_, q| {
            q.back()
                .is_some_and(|at| now.duration_since(*at) < Duration::from_secs(60))
        });
        let (may_ask, renew) = {
            let network = crate::network::lock(&self.network);
            // Relays fixed at start: nothing to ask for, nowhere to keep it.
            if !network.changeable {
                return;
            }
            // Ours, refusing our code: a new one may come from a room. Never
            // one from a personal code; a received one only from its rooms
            // (a code never moves from one room to another by itself).
            let renew: Vec<(String, Option<Vec<String>>)> = network
                .view()
                .relays
                .into_iter()
                .filter(|r| {
                    !r.personal
                        && r.has_code
                        && matches!(r.state, crate::network::RelayStateView::Refused { .. })
                })
                .map(|r| {
                    let rooms = r
                        .received
                        .map(|x| x.rooms.into_iter().map(|f| f.room).collect());
                    (r.url, rooms)
                })
                .collect();
            let may_ask: BTreeSet<String> = self
                .reasons
                .values()
                .flat_map(|m| m.values())
                .filter_map(|u| match &u.reason {
                    MemberReason::NeedsCode { url, .. } => Some(url.clone()),
                    _ => None,
                })
                .chain(renew.iter().map(|(u, _)| u.clone()))
                .filter(|url| network.may_ask(url))
                .collect();
            (may_ask, renew)
        };
        let mut wanted: BTreeMap<(RoomId, String), Why> = BTreeMap::new();
        for (room, members) in &self.reasons {
            for reason in members.values() {
                if let MemberReason::NeedsCode { url, .. } = &reason.reason
                    && may_ask.contains(url)
                {
                    wanted.insert((*room, url.clone()), Why::Missing);
                }
            }
        }
        for (url, only) in renew.iter().filter(|(u, _)| may_ask.contains(u)) {
            for (room, entry) in &self.rooms {
                let its = only
                    .as_ref()
                    .is_none_or(|rooms| rooms.contains(&room.to_hex()));
                if its && !entry.room.is_leaving() && !entry.room.connections().is_empty() {
                    wanted.insert((*room, url.clone()), Why::Renew);
                }
            }
        }
        // Asks nobody needs any more go.
        let before = self.passing.asks.len();
        self.passing.asks.retain(|key, _| wanted.contains_key(key));
        if self.passing.asks.len() != before {
            self.reasons_dirty = true;
        }
        for ((room, url), why) in wanted {
            let due = self
                .passing
                .asks
                .get(&(room, url.clone()))
                .is_none_or(|a| a.checking == 0 && now.duration_since(a.sent) >= a.wait);
            if due {
                self.ask_room(room, url, why, now);
            }
        }
        if self.passing.next_needed.is_none_or(|at| now >= at) {
            self.passing.next_needed = Some(now + NEEDED_EVERY);
            self.relays_needed();
        }
    }

    /// Asks every member of `room` we are connected to for `url`'s code.
    fn ask_room(&mut self, room: RoomId, url: String, why: Why, now: Instant) {
        let conns: BTreeSet<ConnId> = self
            .rooms
            .get(&room)
            .map(|e| {
                e.room
                    .connections()
                    .into_iter()
                    .map(|(_, c, _)| c)
                    .collect()
            })
            .unwrap_or_default();
        for conn in &conns {
            self.send(
                *conn,
                Outgoing::Message(ControlMessage::RelayAsk(RelayAsk { url: url.clone() })),
            );
        }
        let before = self.passing.asks.get(&(room, url.clone()));
        let wait = match before {
            Some(a) if a.nobody => (a.wait * 2).min(self.check_every.max(self.ask_every)),
            Some(a) => a.wait,
            None => self.ask_every,
        };
        if before.is_none() {
            tracing::info!(room = ?room, %url, ?why, asked = conns.len(), "asking the room for a relay's code");
        } else {
            tracing::debug!(room = ?room, %url, ?why, asked = conns.len(), "asking the room again for a relay's code");
        }
        let nobody = conns.is_empty();
        let was = self.passing.asks.insert(
            (room, url),
            Ask {
                why,
                sent: now,
                wait,
                waiting: conns,
                checking: 0,
                nobody,
            },
        );
        if was.is_none_or(|w| w.nobody != nobody) {
            self.reasons_dirty = true;
        }
    }

    /// Where asking for `url`'s code in `room` stands, for the member's
    /// reason.
    pub(super) fn code_passing(&self, room: RoomId, url: &str) -> CodePassing {
        if !crate::network::lock(&self.network).may_ask(url) {
            return CodePassing::Off;
        }
        match self.passing.asks.get(&(room, url.to_string())) {
            Some(ask) if ask.nobody => CodePassing::Nobody,
            _ => CodePassing::Asking,
        }
    }

    /// Tells the store which received relays our rooms' members still use;
    /// those unused for 30 days go.
    fn relays_needed(&self) {
        // Every room we hold, those being left included: a relay received for
        // one we no longer hold goes.
        let live: Vec<String> = self.rooms.keys().map(RoomId::to_hex).collect();
        // Taken with the snapshot: whatever is kept after it is newer.
        let now = crate::remote::unix_now();
        let needed: Vec<(String, String)> = self
            .rooms
            .iter()
            .flat_map(|(id, entry)| {
                entry
                    .room
                    .status()
                    .members
                    .into_iter()
                    .filter_map(|m| entry.room.member_addr(m.id)?.relay)
                    .filter_map(|r| crate::reasons::normalized(&r))
                    .map(|r| (id.to_hex(), r.to_string()))
                    .collect::<Vec<_>>()
            })
            .collect();
        let hub = self.hub.clone();
        tokio::spawn(async move {
            let _ = hub
                .change(|state| {
                    state.saved.needed(&needed, &live, now);
                    Ok(())
                })
                .await;
        });
    }

    /// What a new invite of `room` can carry of the relay it names: what the
    /// invite dialog says of it, and the code when we may pass it on there.
    /// `None` where the relay wants no code (iroh's public ones, one open to
    /// all, or one we don't hold).
    pub(super) fn invite_code(
        &self,
        room: RoomId,
        invite: &couch_proto::invite::Invite,
    ) -> Option<(crate::InviteCodeView, Option<RelaySecret>)> {
        let url = crate::reasons::normalized(invite.addr.relay.as_deref()?)?;
        let network = crate::network::lock(&self.network);
        network.token(url.as_str())?;
        let secret = network
            .passable_token(url.as_str(), &room.to_hex())
            .map(|t| RelaySecret(t.reveal().to_string()))
            // Only what a relay can take (a header's characters).
            .filter(RelaySecret::is_valid);
        Some((
            crate::InviteCodeView {
                host: url.host_str().unwrap_or_default().to_string(),
                passable: secret.is_some(),
            },
            secret,
        ))
    }

    /// The relay an invite names, with the code it carries, to lend the
    /// endpoint for the join (M26b): only a relay we don't hold, that the
    /// dial rules allow, and codes from rooms are taken.
    pub(super) fn lend_for_join(&self, invite: &couch_proto::invite::Invite) -> Option<Relay> {
        let secret = invite.relay_code.as_ref()?;
        let url = crate::reasons::normalized(invite.addr.relay.as_deref()?)?;
        if !self.dial_rules().allows(&url) {
            return None;
        }
        let network = crate::network::lock(&self.network);
        if network.held(url.as_str()).is_some() || !network.may_ask(url.as_str()) {
            return None;
        }
        Some(Relay::new(url, RelayToken::new(secret.0.clone())))
    }

    /// A join with a link's code ended (M26b). Admitted, the code is kept
    /// for the room, from its owner: the link's, if this join lent it, or
    /// ours, if the relay was received for another room (the room joins
    /// it). `lent`: the relay this join lent the endpoint, taken back here.
    pub(super) fn link_code_done(
        &self,
        room: RoomId,
        invite: &couch_proto::invite::Invite,
        admitted: bool,
        lent: Option<Relay>,
    ) {
        let received = || {
            let url = crate::reasons::normalized(invite.addr.relay.as_deref()?)?;
            let network = crate::network::lock(&self.network);
            let listed = network.listed.iter().find(|l| l.view.url == url.as_str())?;
            listed.view.received.as_ref()?;
            let ours = listed.token.clone()?;
            // Only the code we hold: a link merely claiming one must not
            // carry another room's code into this one (the second review).
            (invite.relay_code.as_ref()?.0 == ours.reveal()).then_some((url, ours))
        };
        let kept = match (&lent, admitted && invite.relay_code.is_some()) {
            (_, false) => None,
            (
                Some(Relay {
                    url,
                    token: Some(token),
                    ..
                }),
                true,
            ) => Some((url.clone(), token.clone())),
            (_, true) => received(),
        };
        if kept.is_none() && lent.is_none() {
            return;
        }
        let owner = invite.owner;
        let owner_name = self.member_name(room, owner);
        let hub = self.hub.clone();
        tokio::spawn(async move {
            if let Some((url, token)) = &kept {
                // It let us in just now: kept as checked.
                keep(&hub, url, token, room, owner, &owner_name, true).await;
            }
            if let Some(relay) = lent {
                hub.unlend(relay.url.as_str()).await;
            }
        });
    }

    /// We left or forgot `room`: the relays received for it alone go.
    pub(super) fn drop_received_for(&self, room: RoomId) {
        let hub = self.hub.clone();
        tokio::spawn(async move {
            let _ = hub
                .change(|state| {
                    state.saved.drop_room(&room.to_hex());
                    Ok(())
                })
                .await;
        });
    }
}

#[allow(clippy::too_many_arguments)]
async fn keep(
    hub: &crate::RelayHub,
    url: &couch_net::RelayUrl,
    token: &RelayToken,
    room: RoomId,
    from: MemberId,
    from_name: &str,
    checked: bool,
) -> Checked {
    let now = crate::remote::unix_now();
    let kept = hub
        .change(|state| {
            state
                .saved
                .receive(
                    url,
                    token,
                    &room.to_hex(),
                    &from.to_hex(),
                    from_name,
                    checked,
                    now,
                )
                .map(|_| ())
        })
        .await;
    match kept {
        Ok(()) => Checked::Kept,
        Err(e) => {
            tracing::info!(%url, "a relay's code passed on wasn't kept: {e}");
            Checked::NotKept
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_relay_the_room_uses_is_answered() {
        let token = RelayToken::new("s3cret").unwrap();
        // Not ours to pass on (personal, kept, received for another room,
        // or not held): nothing, whatever we are on.
        assert_eq!(
            answer_for(None, true, true),
            RelayAnswerKind::None(NoRelayCode::Unavailable)
        );
        // Ours, but nobody in the room is on it as far as we can tell: a
        // member claiming a home relay it isn't on gets nothing.
        assert_eq!(
            answer_for(Some(&token), false, false),
            RelayAnswerKind::None(NoRelayCode::Unavailable)
        );
        // Our home relay, or carrying a connection in the room.
        for (home, carried) in [(true, false), (false, true), (true, true)] {
            assert_eq!(
                answer_for(Some(&token), home, carried),
                RelayAnswerKind::Code(RelaySecret("s3cret".into()))
            );
        }
    }
}
