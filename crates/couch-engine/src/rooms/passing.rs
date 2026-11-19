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
//! A rotation's overlap (the M26b follow-up): while a relay takes both its
//! old and its new code, nobody's is refused, so nothing above moves. So:
//! - **a hint, never a code:** a member whose code for a relay changed (the
//!   user pasted a new one, or it kept a new standby, below) sends
//!   [`RelayHint`] to the rooms where it would pass that relay on, at most
//!   once a minute per room and relay;
//! - **the hinter alone is asked,** on the connection the hint came on, at
//!   most once a minute per room and relay, and only for a relay we may
//!   take a code for there (ours, not personal; a received one only in its
//!   rooms) that the room uses (our home, or one of our connections in it runs
//!   through it: hints at made-up relays learn nothing from our asks);
//! - **a standby, never a replacement:** a code that lets us in and isn't
//!   ours is kept beside ours, since nobody can tell an old code from a new
//!   one while the relay takes both; ours is never replaced while it works.
//!   Once ours is refused, the standby is checked again and taken, needing
//!   nobody reachable; refused, it goes and the room is asked as before;
//! - **answers give the standby** while it is fresh and lately checked (an
//!   hourly check), in its own room for a received relay, and not for a day
//!   after the user pasted a code (that one is new), so a member spreading a
//!   hint passes on the new code; invites carry the code in use.
//!
//! [ADR 0014]: ../../../../docs/adr/0014-public-builds-and-registered-relays.md

use std::collections::VecDeque;

use couch_net::endpoint::{Relay, RelayCheck, RelayToken, check_relay};
use couch_proto::room::{
    NoRelayCode, RelayAnswer, RelayAnswerKind, RelayAsk, RelayHint, RelaySecret,
};

use super::*;
use crate::reasons::{CodePassing, MemberReason};

/// Answers to one member in a minute, at most.
pub(crate) const ANSWERS_PER_MINUTE: usize = 6;
/// Codes remembered as refused, at most.
const MAX_REFUSED: usize = 256;
/// How often the relays received for rooms are looked at: still needed,
/// or forgotten after 30 days.
const NEEDED_EVERY: Duration = Duration::from_secs(60 * 60);
/// Hints sent, and hints answered by an ask, once a minute at most per room
/// and relay; an ask after a hint is given as long for its answer.
pub(crate) const HINT_EVERY: Duration = Duration::from_secs(60);
/// A code that changed at us is hinted to our rooms again this often...
const REHINT_EVERY: Duration = Duration::from_secs(10 * 60);
/// ...for this long, as a rotation's overlap lasts a day or so.
const REHINT_FOR: Duration = Duration::from_secs(24 * 60 * 60);

/// Why we ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    /// A member of the room is on it, and we lack it.
    Missing,
    /// One of ours refuses the code we hold.
    Renew,
    /// A member hinted that its code changed: kept on standby.
    Standby,
}

/// When a relay's code changed at us, and when its rooms were last hinted.
#[derive(Debug)]
struct Changed {
    since: Instant,
    last: Instant,
    /// A digest of the code: hinted again only while it is still the one
    /// we hand out (the second review's finding).
    code: [u8; 32],
}

/// An ask sent after a hint, to the hinter alone.
#[derive(Debug)]
struct HintAsk {
    /// The connection it went on: the answer counts from there alone.
    conn: ConnId,
    sent: Instant,
    answered: bool,
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
    /// Asks sent after a hint, by room, relay and the hinter's connection,
    /// kept [`HINT_EVERY`].
    hint_asks: BTreeMap<(RoomId, String, MemberId), HintAsk>,
    /// When each relay's standby was last checked against it again.
    standby_checks: BTreeMap<String, Instant>,
    /// Relays whose code changed lately at us, hinted again for a while.
    changed: BTreeMap<String, Changed>,
    /// When we last hinted each room of each relay.
    hints_sent: BTreeMap<(RoomId, String), Instant>,
    /// Relays whose standby code is being tried (`None`), or was tried and
    /// didn't answer (when: tried again after `ask_every`).
    switching: BTreeMap<String, Option<Instant>>,
}

/// What the check of a code passed on found.
#[derive(Debug)]
pub(crate) enum Checked {
    /// Kept (it let us in, or didn't answer and is kept unchecked).
    Kept,
    /// It let us in and waits on standby; `new` if it wasn't held already.
    StandingBy { new: bool },
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
        let Some(peer) = self.member_speaking(room, conn) else {
            return;
        };
        match message {
            ControlMessage::RelayAsk(ask) if ask.is_valid() => {
                self.answer_ask(room, conn, peer, ask, now);
            }
            ControlMessage::RelayAnswer(answer) if answer.is_valid() => {
                self.take_answer(room, conn, peer, answer, now);
            }
            ControlMessage::RelayHint(hint) if hint.is_valid() => {
                self.take_hint(room, conn, peer, hint, now);
            }
            _ => {}
        }
    }

    /// A member says its code for a relay changed: if we may take a code
    /// for it in this room, and the room uses it, we ask that member alone.
    fn take_hint(
        &mut self,
        room: RoomId,
        conn: ConnId,
        peer: MemberId,
        hint: RelayHint,
        now: Instant,
    ) {
        let Some(url) = crate::reasons::normalized(&hint.url).map(|u| u.to_string()) else {
            return;
        };
        // Per hinter (the review's finding: one member hinting every minute
        // must not hold the slot an honest paster's hint needs), by member,
        // so reconnecting doesn't reset it (the second review's).
        let key = (room, url.clone(), peer);
        if self
            .passing
            .hint_asks
            .get(&key)
            .is_some_and(|a| now.duration_since(a.sent) < HINT_EVERY)
        {
            return;
        }
        if !crate::network::lock(&self.network).may_stand_by(&url, &room.to_hex()) {
            return;
        }
        // Only for a relay this room uses: a member hinting at made-up
        // relays learns nothing of the others we hold from our asks.
        if !self.home_relay(&url) && !self.carried_in(room, None, &url) {
            return;
        }
        tracing::info!(room = ?room, member = ?peer, %url, "a member's code for a relay changed: asking it");
        self.send(
            conn,
            Outgoing::Message(ControlMessage::RelayAsk(RelayAsk { url })),
        );
        self.passing.hint_asks.insert(
            key,
            HintAsk {
                conn,
                sent: now,
                answered: false,
            },
        );
    }

    /// Whether `url` is our home relay (our signed record names it).
    fn home_relay(&self, url: &str) -> bool {
        self.addr
            .relay
            .as_deref()
            .and_then(crate::reasons::normalized)
            .is_some_and(|ours| ours.as_str() == url)
    }

    /// The code we now hand out for `url` changed: the user pasted it
    /// (`None`: the code in use), or a new standby was kept (its digest).
    /// Our rooms are hinted now, and again every [`REHINT_EVERY`] for
    /// [`REHINT_FOR`], so members who connect later in the overlap hear it,
    /// for as long as that code is still the one we hand out.
    pub(super) fn code_changed(&mut self, url: &str, code: Option<[u8; 32]>, now: Instant) {
        let code = code.or_else(|| {
            crate::network::lock(&self.network)
                .token(url)
                .map(|t| digest(t.reveal()))
        });
        let Some(code) = code else {
            return;
        };
        self.passing.changed.insert(
            url.to_string(),
            Changed {
                since: now,
                last: now,
                code,
            },
        );
        self.hint_rooms(url, code, now);
    }

    /// Hints the members of each room where our answer for `url` is the
    /// code `code` (so a received relay's standby is hinted only in its own
    /// room), at most once a minute per room: each member only if the room
    /// uses the relay apart from that member (our home, or one of our
    /// connections to someone else runs through it), as an answer to it
    /// would need. Whether any room still gets `code` from us.
    fn hint_rooms(&mut self, url: &str, code: [u8; 32], now: Instant) -> bool {
        let unix = crate::remote::unix_now();
        let ours: Vec<RoomId> = {
            let network = crate::network::lock(&self.network);
            self.rooms
                .iter()
                .filter(|(id, entry)| {
                    !entry.room.is_leaving()
                        && network
                            .answer_token(url, &id.to_hex(), unix)
                            .is_some_and(|t| digest(t.reveal()) == code)
                })
                .map(|(id, _)| *id)
                .collect()
        };
        self.passing
            .hints_sent
            .retain(|_, at| now.duration_since(*at) < HINT_EVERY);
        let home = self.home_relay(url);
        for room in &ours {
            let key = (*room, url.to_string());
            if self.passing.hints_sent.contains_key(&key) {
                continue;
            }
            let conns: Vec<ConnId> = self
                .rooms
                .get(room)
                .map(|e| e.room.connections())
                .unwrap_or_default()
                .into_iter()
                .filter(|(peer, _, _)| home || self.carried_in(*room, Some(*peer), url))
                .map(|(_, c, _)| c)
                .collect();
            if conns.is_empty() {
                continue;
            }
            tracing::info!(room = ?room, %url, told = conns.len(), "hinting a room that our code for a relay changed");
            for conn in conns {
                self.send(
                    conn,
                    Outgoing::Message(ControlMessage::RelayHint(RelayHint {
                        url: url.to_string(),
                    })),
                );
            }
            self.passing.hints_sent.insert(key, now);
        }
        !ours.is_empty()
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
                    let token = crate::network::lock(&self.network).answer_token(
                        &url,
                        &room.to_hex(),
                        crate::remote::unix_now(),
                    );
                    let home = self
                        .addr
                        .relay
                        .as_deref()
                        .and_then(crate::reasons::normalized)
                        .is_some_and(|ours| ours.as_str() == url);
                    let carried =
                        token.is_some() && !home && self.carried_in(room, Some(peer), &url);
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
    /// `asker` (any, for `None`), runs through relay `url` now.
    fn carried_in(&self, room: RoomId, asker: Option<MemberId>, url: &str) -> bool {
        let Some(entry) = self.rooms.get(&room) else {
            return false;
        };
        entry
            .room
            .connections()
            .into_iter()
            .filter(|(peer, _, _)| Some(*peer) != asker)
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
        // Only an answer we asked for, once: a room's ask, or one after a
        // hint, from the hinter.
        let asked = self
            .passing
            .asks
            .get_mut(&key)
            .and_then(|ask| ask.waiting.remove(&conn).then_some(ask.why));
        let why = match asked {
            Some(why) => why,
            None => match self
                .passing
                .hint_asks
                .get_mut(&(room, answer.url.clone(), peer))
            {
                Some(hinted) if hinted.conn == conn && !hinted.answered => {
                    hinted.answered = true;
                    self.hint_answer(room, peer, answer);
                    return;
                }
                _ => return,
            },
        };
        let Some(ask) = self.passing.asks.get_mut(&key) else {
            return;
        };
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
                // After a hint: beside ours, never in its place.
                Ok(RelayCheck::Accepted) if why == Why::Standby => {
                    stand_by(&hub, &url, &token, room, from, &from_name).await
                }
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
                    hinted: why == Why::Standby,
                })
                .await;
        });
    }

    /// A code a member gave after its hint: checked (never kept unchecked:
    /// ours still works), unless it is ours or known refused.
    fn hint_answer(&mut self, room: RoomId, peer: MemberId, answer: RelayAnswer) {
        let RelayAnswerKind::Code(secret) = answer.answer else {
            return;
        };
        let (Some(token), Some(url)) = (
            RelayToken::new(secret.0),
            crate::reasons::normalized(&answer.url),
        ) else {
            return;
        };
        if self
            .passing
            .refused
            .contains(&(url.to_string(), digest(token.reveal())))
            || crate::network::lock(&self.network)
                .listed
                .iter()
                .find(|l| l.view.url == url.as_str())
                .is_some_and(|l| {
                    // Ours, or the standby we hold already.
                    l.token
                        .as_ref()
                        .is_some_and(|t| t.reveal() == token.reveal())
                        || l.standby
                            .as_ref()
                            .is_some_and(|s| s.token.reveal() == token.reveal())
                })
        {
            return;
        }
        let from_name = self.member_name(room, peer);
        self.check_passed_code(room, peer, from_name, url, token, Why::Standby);
    }

    pub(super) fn relay_code_checked(
        &mut self,
        room: RoomId,
        url: String,
        hash: [u8; 32],
        checked: Checked,
        hinted: bool,
        now: Instant,
    ) {
        if matches!(checked, Checked::Refused) {
            // Bounded: a member could send a new bogus code each minute.
            if self.passing.refused.len() >= MAX_REFUSED {
                self.passing.refused.clear();
            }
            self.passing.refused.insert((url.clone(), hash));
        }
        if hinted {
            // A new standby: our rooms hear of it in turn, so it spreads
            // past the rooms of whoever pasted it.
            if matches!(checked, Checked::StandingBy { new: true }) {
                self.code_changed(&url, Some(hash), now);
            }
            return;
        }
        if let Some(ask) = self.passing.asks.get_mut(&(room, url)) {
            ask.checking = ask.checking.saturating_sub(1);
            if !matches!(checked, Checked::Kept) && ask.waiting.is_empty() && ask.checking == 0 {
                ask.nobody = true;
            }
        }
        self.reasons_dirty = true;
    }

    /// Ours refuses our code and we hold a standby one: it is checked again
    /// and taken if it lets us in, needing nobody reachable. One try at a
    /// time per relay; one that didn't answer is tried again after
    /// `ask_every`.
    fn switch_to_standby(&mut self, url: &str, now: Instant) {
        match self.passing.switching.get(url) {
            Some(None) => return,
            Some(Some(at)) if now.duration_since(*at) < self.ask_every => return,
            _ => {}
        }
        self.passing.switching.insert(url.to_string(), None);
        let (hub, inbox, timeout, url) = (
            self.hub.clone(),
            self.inbox.clone(),
            self.relay_check,
            url.to_string(),
        );
        tokio::spawn(async move {
            let code = hub
                .relays
                .lock()
                .await
                .saved
                .standby(&url)
                .map(|s| s.token.clone());
            let taken = match (code.and_then(RelayToken::new), url.parse()) {
                (Some(token), Ok(parsed)) => {
                    match check_relay(&Relay::new(parsed, Some(token.clone())), timeout).await {
                        Ok(RelayCheck::Accepted) => {
                            let mut taken = false;
                            let _ = hub
                                .change(|state| {
                                    taken = state.saved.take_standby(&url, token.reveal());
                                    Ok(())
                                })
                                .await;
                            if taken {
                                // It let us in just now.
                                crate::network::lock(&hub.network).checked_with(
                                    &url,
                                    Some(&token),
                                    RelayCheck::Accepted,
                                    Instant::now(),
                                );
                            }
                            taken
                        }
                        Ok(RelayCheck::Refused(_)) => {
                            let _ = hub
                                .change(|state| {
                                    state.saved.drop_standby(&url, token.reveal());
                                    Ok(())
                                })
                                .await;
                            false
                        }
                        Ok(RelayCheck::Unreachable(_)) | Err(_) => false,
                    }
                }
                _ => false,
            };
            tracing::info!(%url, taken, "our code for a relay was refused: its standby code tried");
            let _ = inbox.send(Input::StandbyTried { url, taken }).await;
        });
    }

    /// Each standby held is checked against its relay again every
    /// [`crate::relays::STANDBY_RECHECK_SECS`] (the second review's finding):
    /// let in, answers may keep giving it; refused, it goes. Not answering
    /// changes nothing (answers stop giving it once its last check is old).
    fn recheck_standbys(&mut self, now: Instant) {
        let every = Duration::from_secs(crate::relays::STANDBY_RECHECK_SECS);
        let held: Vec<(String, RelayToken)> = crate::network::lock(&self.network)
            .listed
            .iter()
            .filter_map(|l| Some((l.view.url.clone(), l.standby.as_ref()?.token.clone())))
            .collect();
        let checks = &mut self.passing.standby_checks;
        checks.retain(|url, _| held.iter().any(|(u, _)| u == url));
        for (url, token) in held {
            // Checked when it was kept: the first time comes an interval on.
            let last = checks.entry(url.clone()).or_insert(now);
            if now.duration_since(*last) < every {
                continue;
            }
            *last = now;
            let Ok(parsed) = url.parse::<couch_net::RelayUrl>() else {
                continue;
            };
            let (hub, timeout) = (self.hub.clone(), self.relay_check);
            tokio::spawn(async move {
                let outcome = check_relay(&Relay::new(parsed, Some(token.clone())), timeout).await;
                let unix = crate::remote::unix_now();
                let _ = match outcome {
                    Ok(RelayCheck::Accepted) => {
                        hub.change(|state| {
                            state.saved.refresh_standby(&url, token.reveal(), unix);
                            Ok(())
                        })
                        .await
                    }
                    Ok(RelayCheck::Refused(_)) => {
                        tracing::info!(%url, "a relay's standby code is refused now: dropped");
                        hub.change(|state| {
                            state.saved.drop_standby(&url, token.reveal());
                            Ok(())
                        })
                        .await
                    }
                    Ok(RelayCheck::Unreachable(_)) | Err(_) => Ok(()),
                };
            });
        }
    }

    pub(super) fn standby_tried(&mut self, url: String, taken: bool, now: Instant) {
        if taken {
            self.passing.switching.remove(&url);
        } else {
            self.passing.switching.insert(url, Some(now));
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
        self.passing
            .hint_asks
            .retain(|_, a| now.duration_since(a.sent) < HINT_EVERY);
        self.passing
            .hints_sent
            .retain(|_, at| now.duration_since(*at) < HINT_EVERY);
        let ask_every = self.ask_every;
        self.passing
            .switching
            .retain(|_, at| at.is_none_or(|at| now.duration_since(at) < ask_every));
        // Codes that changed at us lately: their rooms hear of it again, for
        // members who connected since (the review's finding).
        self.passing
            .changed
            .retain(|_, c| now.duration_since(c.since) < REHINT_FOR);
        let again: Vec<(String, [u8; 32])> = self
            .passing
            .changed
            .iter_mut()
            .filter(|(_, c)| now.duration_since(c.last) >= REHINT_EVERY)
            .map(|(url, c)| {
                c.last = now;
                (url.clone(), c.code)
            })
            .collect();
        for (url, code) in again {
            // No longer the code we hand out anywhere (its standby went,
            // or another came): nothing more to hint.
            if !self.hint_rooms(&url, code, now) {
                self.passing.changed.remove(&url);
            }
        }
        self.recheck_standbys(now);
        let (may_ask, renew, standby) = {
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
            // Refused, with a standby code to try first: the room is asked
            // only once it has been (dropped if refused), or while its check
            // doesn't answer. Never with codes from rooms turned off.
            let standby: Vec<String> = renew
                .iter()
                .filter(|(url, _)| {
                    network.may_ask(url)
                        && network
                            .listed
                            .iter()
                            .any(|l| l.view.url == *url && l.standby.is_some())
                })
                .map(|(url, _)| url.clone())
                .collect();
            (may_ask, renew, standby)
        };
        for url in &standby {
            self.switch_to_standby(url, now);
        }
        let renew: Vec<_> = renew
            .into_iter()
            .filter(|(url, _)| {
                // Asked too once a try went unanswered (the review's
                // finding: a check that never answers mustn't block asks).
                !standby.contains(url) || matches!(self.passing.switching.get(url), Some(Some(_)))
            })
            .collect();
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

/// Keeps a code given after a hint on standby (it let us in just now).
async fn stand_by(
    hub: &crate::RelayHub,
    url: &couch_net::RelayUrl,
    token: &RelayToken,
    room: RoomId,
    from: MemberId,
    from_name: &str,
) -> Checked {
    let mut new = false;
    let now = crate::remote::unix_now();
    let kept = hub
        .change(|state| {
            new =
                state
                    .saved
                    .stand_by(url, token, &room.to_hex(), &from.to_hex(), from_name, now)?;
            Ok(())
        })
        .await;
    match kept {
        Ok(()) => Checked::StandingBy { new },
        Err(e) => {
            tracing::info!(%url, "a relay's code given after a hint wasn't kept: {e}");
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
