use std::collections::VecDeque;

use couch_proto::session::{ControlDeclined, SessionJoinReply};
use proptest::prelude::*;

use super::*;

const ANA: MemberId = MemberId([1; 32]);
const BIA: MemberId = MemberId([2; 32]);
const CAIO: MemberId = MemberId([3; 32]);
const S0: SeatId = SeatId(0);
const S1: SeatId = SeatId(1);

fn config(max_viewers: u8) -> SessionConfig {
    SessionConfig {
        title: "Celeste".into(),
        seats: vec![SeatKind::Controller],
        max_viewers,
    }
}

fn session(max_viewers: u8) -> HostSession {
    HostSession::new(SessionId(9), config(max_viewers)).unwrap()
}

fn reply(fx: &[Effect], to: MemberId) -> Option<JoinResult> {
    fx.iter().find_map(|e| match e {
        Effect::Send {
            to: t,
            message: ControlMessage::SessionJoinReply(SessionJoinReply { result, .. }),
        } if *t == to => Some(*result),
        _ => None,
    })
}

fn declined(fx: &[Effect], to: MemberId) -> bool {
    fx.iter().any(|e| {
        matches!(e, Effect::Send { to: t, message: ControlMessage::ControlDeclined(_) } if *t == to)
    })
}

fn broadcasts_status(fx: &[Effect]) -> bool {
    fx.iter()
        .any(|e| matches!(e, Effect::Broadcast(ControlMessage::SessionStatus(_))))
}

#[test]
fn titles_and_limits_are_checked() {
    let new = |title: &str, max| {
        HostSession::new(
            SessionId(1),
            SessionConfig {
                title: title.into(),
                ..config(max)
            },
        )
    };
    assert!(new("Celeste", 4).is_ok());
    assert!(new("Celeste", MAX_VIEWERS).is_ok());
    assert_eq!(new("", 4).unwrap_err(), SessionError::InvalidTitle);
    assert_eq!(
        new("a\u{202E}b", 4).unwrap_err(),
        SessionError::InvalidTitle
    );
    assert_eq!(
        new("Celeste", 0).unwrap_err(),
        SessionError::InvalidMaxViewers
    );
    assert_eq!(
        new("Celeste", MAX_VIEWERS + 1).unwrap_err(),
        SessionError::InvalidMaxViewers
    );
}

#[test]
fn a_member_who_connects_hears_of_the_session() {
    let s = session(4);
    let fx = s.connected(ANA);
    assert_eq!(fx.len(), 2);
    assert!(matches!(
        &fx[0],
        Effect::Send { to, message: ControlMessage::SessionAnnounce(a) } if *to == ANA && a.title == "Celeste"
    ));
    assert!(matches!(
        &fx[1],
        Effect::Send {
            message: ControlMessage::SessionStatus(_),
            ..
        }
    ));
}

#[test]
fn joining_starts_a_stream_up_to_the_limit() {
    let mut s = session(2);
    let fx = s.join(ANA);
    assert_eq!(fx[0], Effect::StartViewer(ANA), "streaming starts first");
    assert_eq!(reply(&fx, ANA), Some(JoinResult::Joined));
    assert!(broadcasts_status(&fx));

    // Asking again changes nothing.
    let fx = s.join(ANA);
    assert_eq!(reply(&fx, ANA), Some(JoinResult::Joined));
    assert!(!fx.contains(&Effect::StartViewer(ANA)));

    s.join(BIA);
    let fx = s.join(CAIO);
    assert_eq!(reply(&fx, CAIO), Some(JoinResult::Full));
    assert_eq!(s.viewers(), &[ANA, BIA]);

    // A place frees up when someone leaves.
    assert!(s.leave(ANA).contains(&Effect::StopViewer(ANA)));
    assert_eq!(reply(&s.join(CAIO), CAIO), Some(JoinResult::Joined));
}

#[test]
fn a_viewer_the_host_cant_stream_to_is_dropped_and_told() {
    let mut s = session(4);
    s.join(ANA);
    let fx = s.viewer_failed(ANA);
    assert!(fx.contains(&Effect::StopViewer(ANA)));
    assert_eq!(reply(&fx, ANA), Some(JoinResult::Unavailable));
    assert!(s.viewers().is_empty());
}

#[test]
fn a_request_accepted_hands_over_the_seat() {
    let now = Instant::now();
    let mut s = session(4);
    s.join(ANA);
    // Not a viewer: ignored.
    assert!(s.request(BIA, S0, now).is_empty());

    let fx = s.request(ANA, S0, now);
    assert!(fx.contains(&Effect::Requested(ANA, S0)));
    assert_eq!(s.requests(), &[(ANA, S0)]);
    assert!(s.request(ANA, S0, now).is_empty(), "one request at a time");
    // A seat that doesn't exist: ignored.
    assert!(s.request(ANA, S1, now).is_empty());

    let fx = s.answer(ANA, true, now).unwrap();
    assert!(fx.contains(&Effect::Grant {
        seat: S0,
        member: ANA,
        permissions: Permissions::GUEST_DEFAULT,
    }));
    assert!(s.requests().is_empty());
    // The router's report makes it so.
    assert_eq!(s.holders()[0], None);
    let fx = s.seat_changed(S0, Some(ANA));
    assert!(broadcasts_status(&fx));
    assert_eq!(s.holders()[0], Some(ANA));
    assert!(s.request(ANA, S0, now).is_empty(), "already playing");

    assert_eq!(
        s.answer(BIA, true, now).unwrap_err(),
        SessionError::NoRequest
    );
}

fn with_seats(seats: &[SeatKind]) -> HostSession {
    HostSession::new(
        SessionId(1),
        SessionConfig {
            seats: seats.to_vec(),
            ..config(4)
        },
    )
    .unwrap()
}

fn grant_of(fx: &[Effect]) -> Option<(SeatId, MemberId, Permissions)> {
    fx.iter().find_map(|e| match e {
        Effect::Grant {
            seat,
            member,
            permissions,
        } => Some((*seat, *member, *permissions)),
        _ => None,
    })
}

#[test]
fn the_seat_decides_what_its_holder_may_send() {
    let kbm = Permissions::KEYBOARD | Permissions::MOUSE;
    for (kind, permissions) in [
        (SeatKind::Controller, Permissions::GAMEPAD),
        (SeatKind::KeyboardMouse, kbm),
        (SeatKind::Both, Permissions::GAMEPAD | kbm),
    ] {
        let mut s = with_seats(&[kind]);
        s.join(ANA);
        assert_eq!(
            grant_of(&s.pass(ANA, S0).unwrap()),
            Some((S0, ANA, permissions))
        );
    }
    let mut s = with_seats(&[SeatKind::Controller, SeatKind::KeyboardMouse]);
    s.join(ANA);
    assert_eq!(grant_of(&s.pass(ANA, S1).unwrap()), Some((S1, ANA, kbm)));
    assert_eq!(
        s.pass(ANA, SeatId(2)).unwrap_err(),
        SessionError::NoSuchSeat
    );
}

#[test]
fn seats_are_checked_when_going_live() {
    let new = |seats: &[SeatKind]| {
        HostSession::new(
            SessionId(1),
            SessionConfig {
                seats: seats.to_vec(),
                ..config(4)
            },
        )
    };
    assert_eq!(new(&[]).unwrap_err(), SessionError::InvalidSeats);
    assert_eq!(
        new(&[SeatKind::Controller; 5]).unwrap_err(),
        SessionError::InvalidSeats
    );
    assert_eq!(
        new(&[SeatKind::Both, SeatKind::KeyboardMouse]).unwrap_err(),
        SessionError::InvalidSeats
    );
    let s = new(&[SeatKind::Controller, SeatKind::Controller]).unwrap();
    assert_eq!(s.holders(), &[None, None]);
}

#[test]
fn two_guests_play_two_seats() {
    let mut s = with_seats(&[SeatKind::Controller, SeatKind::Controller]);
    s.join(ANA);
    s.join(BIA);
    s.pass(ANA, S0).unwrap();
    s.seat_changed(S0, Some(ANA));
    s.pass(BIA, S1).unwrap();
    s.seat_changed(S1, Some(BIA));
    assert_eq!(s.holders(), &[Some(ANA), Some(BIA)]);
    assert_eq!(s.seat_of(BIA), Some(S1));
    // Taking one back leaves the other.
    assert_eq!(
        s.take_back(Some(S0)),
        vec![Effect::Revoke {
            seat: S0,
            holder: None
        }]
    );
    s.seat_changed(S0, None);
    assert_eq!(s.holders(), &[None, Some(BIA)]);
}

#[test]
fn moving_to_another_seat_leaves_the_old_one_first() {
    let mut s = with_seats(&[SeatKind::Controller, SeatKind::Controller]);
    s.join(ANA);
    s.pass(ANA, S0).unwrap();
    s.seat_changed(S0, Some(ANA));
    let fx = s.pass(ANA, S1).unwrap();
    assert_eq!(
        fx[..2],
        [
            Effect::Revoke {
                seat: S0,
                holder: Some(ANA)
            },
            Effect::Grant {
                seat: S1,
                member: ANA,
                permissions: Permissions::GAMEPAD
            },
        ]
    );
    // Even before the router reported the first grant, a second one revokes
    // it first.
    let mut s = with_seats(&[SeatKind::Controller, SeatKind::Controller]);
    s.join(ANA);
    s.pass(ANA, S0).unwrap();
    let fx = s.pass(ANA, S1).unwrap();
    assert_eq!(
        fx[0],
        Effect::Revoke {
            seat: S0,
            holder: Some(ANA)
        }
    );
}

#[test]
fn asking_for_another_seat_replaces_the_request() {
    let now = Instant::now();
    let mut s = with_seats(&[SeatKind::Controller, SeatKind::Controller]);
    s.join(ANA);
    s.join(BIA);
    s.request(ANA, S0, now);
    let fx = s.request(ANA, S1, now);
    assert!(fx.contains(&Effect::Requested(ANA, S1)));
    assert_eq!(s.requests(), &[(ANA, S1)]);
    // Asking for a seat someone else holds is fine; the host decides.
    s.pass(BIA, S0).unwrap();
    s.seat_changed(S0, Some(BIA));
    s.request(ANA, S0, now);
    let fx = s.answer(ANA, true, now).unwrap();
    assert_eq!(grant_of(&fx), Some((S0, ANA, Permissions::GAMEPAD)));
}

#[test]
fn a_declined_viewer_waits_before_asking_again() {
    let now = Instant::now();
    let mut s = session(4);
    s.join(ANA);
    s.request(ANA, S0, now);
    let fx = s.answer(ANA, false, now).unwrap();
    assert!(declined(&fx, ANA));
    assert!(!fx.iter().any(|e| matches!(e, Effect::Grant { .. })));

    let soon = now + DECLINE_COOLDOWN - Duration::from_millis(1);
    let fx = s.request(ANA, S0, soon);
    assert!(declined(&fx, ANA), "declined again during the cooldown");
    assert!(s.requests().is_empty());

    let fx = s.request(ANA, S0, now + DECLINE_COOLDOWN);
    assert!(fx.contains(&Effect::Requested(ANA, S0)));
}

#[test]
fn the_host_passes_and_takes_back() {
    let mut s = session(4);
    s.join(ANA);
    assert_eq!(s.pass(BIA, S0).unwrap_err(), SessionError::NotViewer);
    assert!(matches!(s.pass(ANA, S0).unwrap()[0], Effect::Grant { member, .. } if member == ANA));
    s.seat_changed(S0, Some(ANA));
    assert!(s.pass(ANA, S0).unwrap().is_empty(), "already holds it");
    assert_eq!(
        s.take_back(None),
        vec![Effect::Revoke {
            seat: S0,
            holder: None
        }]
    );
    s.seat_changed(S0, None);
    assert_eq!(s.holders()[0], None);
}

#[test]
fn a_viewer_withdraws_or_gives_back() {
    let now = Instant::now();
    let mut s = session(4);
    s.join(ANA);
    s.request(ANA, S0, now);
    assert_eq!(s.withdraw(ANA), vec![Effect::Changed]);
    assert!(s.requests().is_empty());

    s.pass(ANA, S0).unwrap();
    s.seat_changed(S0, Some(ANA));
    // A withdraw with nothing pending never gives the seat back.
    assert!(s.withdraw(ANA).is_empty());
    assert_eq!(
        s.give_back(ANA),
        vec![Effect::Revoke {
            seat: S0,
            holder: Some(ANA)
        }]
    );
    assert!(s.give_back(BIA).is_empty());
}

/// A guest asking to move withdraws just as the host declines: it keeps
/// the seat it plays on (the review's race).
#[test]
fn a_withdraw_crossing_a_decline_keeps_the_seat() {
    let now = Instant::now();
    let mut s = with_seats(&[SeatKind::Controller, SeatKind::Controller]);
    s.join(ANA);
    s.pass(ANA, S0).unwrap();
    s.seat_changed(S0, Some(ANA));
    s.request(ANA, S1, now);
    s.answer(ANA, false, now).unwrap();
    assert!(
        s.withdraw(ANA).is_empty(),
        "nothing to withdraw, nothing lost"
    );
    // Within the cooldown the request isn't kept, and a withdraw still
    // leaves the seat alone.
    assert!(declined(&s.request(ANA, S1, now), ANA));
    assert!(s.withdraw(ANA).is_empty());
    assert_eq!(s.holders()[0], Some(ANA));
}

/// A request for another seat that crosses a pass stays pending as a move,
/// as the guest still shows it; one for the passed seat is answered.
#[test]
fn a_grant_of_another_seat_keeps_the_request() {
    let now = Instant::now();
    let mut s = with_seats(&[SeatKind::Controller, SeatKind::Controller]);
    s.join(ANA);
    s.request(ANA, S1, now);
    s.pass(ANA, S0).unwrap();
    s.seat_changed(S0, Some(ANA));
    assert_eq!(s.requests(), &[(ANA, S1)]);
    s.request(ANA, S0, now);
    assert_eq!(
        s.requests(),
        &[(ANA, S1)],
        "asking for the seat held: ignored"
    );
    let fx = s.answer(ANA, true, now).unwrap();
    assert_eq!(grant_of(&fx), Some((S1, ANA, Permissions::GAMEPAD)));
    s.seat_changed(S1, Some(ANA));
    assert!(s.requests().is_empty(), "answered");
}

#[test]
fn the_panic_key_takes_everything_back_until_allowed() {
    let now = Instant::now();
    let mut s = session(4);
    s.join(ANA);
    s.join(BIA);
    s.pass(ANA, S0).unwrap();
    s.seat_changed(S0, Some(ANA));
    s.request(BIA, S0, now);

    let fx = s.panic();
    assert_eq!(fx[0], Effect::RevokeAll, "released first");
    assert!(declined(&fx, BIA), "the pending request is declined");
    assert!(s.requests().is_empty());
    assert!(s.control_paused());
    assert!(broadcasts_status(&fx));

    // Nothing gets control while paused.
    assert!(declined(&s.request(BIA, S0, now), BIA));
    assert_eq!(s.pass(BIA, S0).unwrap_err(), SessionError::ControlPaused);
    // A grant that crossed the panic key goes straight back.
    assert_eq!(
        s.seat_changed(S0, Some(BIA)),
        vec![Effect::Revoke {
            seat: S0,
            holder: Some(BIA)
        }]
    );
    assert_eq!(
        s.panic()[0],
        Effect::RevokeAll,
        "pressing again releases again"
    );

    let fx = s.allow_control();
    assert!(broadcasts_status(&fx));
    assert!(!s.control_paused());
    assert!(
        s.request(BIA, S0, now)
            .contains(&Effect::Requested(BIA, S0))
    );
}

#[test]
fn accepting_while_paused_is_refused() {
    let now = Instant::now();
    let mut s = session(4);
    s.join(ANA);
    s.request(ANA, S0, now);
    s.paused = true;
    assert_eq!(
        s.answer(ANA, true, now).unwrap_err(),
        SessionError::ControlPaused
    );
    assert_eq!(s.requests(), &[(ANA, S0)], "still waiting");
}

#[test]
fn a_controller_who_leaves_loses_the_seat_at_once() {
    let mut s = session(4);
    s.join(ANA);
    s.pass(ANA, S0).unwrap();
    s.seat_changed(S0, Some(ANA));
    let fx = s.leave(ANA);
    assert_eq!(
        fx[0],
        Effect::StopViewer(ANA),
        "stopping releases its input"
    );
    assert_eq!(s.holders()[0], None);
    assert!(broadcasts_status(&fx));
    // The router's late report of the old grant is sent straight back.
    assert_eq!(
        s.seat_changed(S0, Some(ANA)),
        vec![Effect::Revoke {
            seat: S0,
            holder: Some(ANA)
        }]
    );
    assert!(s.seat_changed(S0, None).is_empty());
}

#[test]
fn after_the_end_nothing_moves() {
    let now = Instant::now();
    let mut s = session(4);
    s.join(ANA);
    let fx = s.end(EndReason::HostStopped);
    assert!(matches!(
        &fx[0],
        Effect::Broadcast(ControlMessage::SessionEnded(SessionEnded {
            reason: EndReason::HostStopped,
            ..
        }))
    ));
    assert!(s.end(EndReason::Error).is_empty(), "once");
    assert_eq!(reply(&s.join(BIA), BIA), Some(JoinResult::Ended));
    assert!(s.request(ANA, S0, now).is_empty());
    assert_eq!(s.pass(ANA, S0).unwrap_err(), SessionError::Ended);
    assert!(s.take_back(None).is_empty());
    assert!(s.connected(BIA).is_empty());
    assert_eq!(
        s.seat_changed(S0, Some(ANA)),
        vec![Effect::Revoke {
            seat: S0,
            holder: Some(ANA)
        }]
    );
}

#[test]
fn declines_name_the_session() {
    let now = Instant::now();
    let mut s = session(4);
    s.join(ANA);
    s.request(ANA, S0, now);
    let fx = s.answer(ANA, false, now).unwrap();
    assert!(fx.contains(&Effect::Send {
        to: ANA,
        message: ControlMessage::ControlDeclined(ControlDeclined {
            session: SessionId(9)
        }),
    }));
}

// ---- Random sequences against a seat router whose reports arrive late ----

/// Seats in the property test: two controllers and a keyboard seat.
const SEATS: [SeatKind; 3] = [
    SeatKind::Controller,
    SeatKind::Controller,
    SeatKind::KeyboardMouse,
];

#[derive(Debug, Clone)]
enum Op {
    Join(u8),
    Leave(u8),
    Request(u8, u8),
    Withdraw(u8),
    GiveBack(u8),
    Answer(u8, bool),
    Pass(u8, u8),
    TakeBack(Option<u8>),
    Panic,
    Allow,
    /// The router hands one report to the session.
    Deliver,
    /// The router gives a seat back on its own (a holder gone quiet).
    Lapse(u8),
    Wait(u64),
    End,
}

fn op() -> impl Strategy<Value = Op> {
    let who = 0u8..4;
    // One past the last seat: requests and passes for a seat that isn't.
    let seat = 0u8..4;
    prop_oneof![
        who.clone().prop_map(Op::Join),
        who.clone().prop_map(Op::Leave),
        (who.clone(), seat.clone()).prop_map(|(w, s)| Op::Request(w, s)),
        who.clone().prop_map(Op::Withdraw),
        who.clone().prop_map(Op::GiveBack),
        (who.clone(), any::<bool>()).prop_map(|(w, a)| Op::Answer(w, a)),
        (who, seat.clone()).prop_map(|(w, s)| Op::Pass(w, s)),
        proptest::option::of(seat.clone()).prop_map(Op::TakeBack),
        Just(Op::Panic),
        Just(Op::Allow),
        Just(Op::Deliver),
        Just(Op::Deliver),
        Just(Op::Deliver),
        seat.prop_map(Op::Lapse),
        (0u64..4000).prop_map(Op::Wait),
        Just(Op::End),
    ]
}

/// Stands in for `couch_input::router::SeatRouter`: who holds each seat, and
/// its reports still on their way to the session.
struct Router {
    holders: Vec<Option<MemberId>>,
    reports: VecDeque<(SeatId, Option<MemberId>)>,
}

impl Router {
    fn new() -> Self {
        Self {
            holders: vec![None; SEATS.len()],
            reports: VecDeque::new(),
        }
    }

    fn release(&mut self, seat: usize) {
        if let Some(slot) = self.holders.get_mut(seat)
            && slot.take().is_some()
        {
            self.reports.push_back((SeatId(seat as u8), None));
        }
    }

    fn release_all(&mut self) {
        for seat in 0..self.holders.len() {
            self.release(seat);
        }
    }

    fn apply(&mut self, fx: &[Effect]) {
        for effect in fx {
            match *effect {
                Effect::Grant { seat, member, .. } => {
                    let i = usize::from(seat.0);
                    // Ending the old hold first, as the router does.
                    self.release(i);
                    self.holders[i] = Some(member);
                    self.reports.push_back((seat, Some(member)));
                }
                Effect::Revoke { seat, holder } => {
                    let i = usize::from(seat.0);
                    if holder.is_none_or(|h| self.holders[i] == Some(h)) {
                        self.release(i);
                    }
                }
                Effect::RevokeAll => self.release_all(),
                Effect::StopViewer(member) => {
                    for seat in 0..self.holders.len() {
                        if self.holders[seat] == Some(member) {
                            self.release(seat);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Nobody holds two seats, at any moment.
    fn one_seat_each(&self) -> bool {
        let held: Vec<MemberId> = self.holders.iter().flatten().copied().collect();
        held.iter().enumerate().all(|(i, h)| !held[..i].contains(h))
    }
}

fn member(i: u8) -> MemberId {
    MemberId([i + 1; 32])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn only_viewers_ever_play_and_one_seat_each(ops in proptest::collection::vec(op(), 1..100)) {
        let t0 = Instant::now();
        let mut now = t0;
        let mut s = HostSession::new(
            SessionId(9),
            SessionConfig { seats: SEATS.to_vec(), ..config(3) },
        )
        .unwrap();
        let mut router = Router::new();
        let mut declined_at: BTreeMap<MemberId, Instant> = BTreeMap::new();

        for op in ops {
            let paused_before = s.control_paused();
            let ended_before = s.is_ended();
            let fx = match op {
                Op::Join(i) => s.join(member(i)),
                Op::Leave(i) => s.leave(member(i)),
                Op::Request(i, seat) => s.request(member(i), SeatId(seat), now),
                Op::Withdraw(i) => {
                    let fx = s.withdraw(member(i));
                    prop_assert!(
                        !fx.iter().any(|e| matches!(e, Effect::Revoke { .. })),
                        "a withdraw never gives a seat back"
                    );
                    fx
                }
                Op::GiveBack(i) => s.give_back(member(i)),
                Op::Answer(i, accept) => {
                    let fx = s.answer(member(i), accept, now).unwrap_or_default();
                    if !accept && declined(&fx, member(i)) {
                        declined_at.insert(member(i), now);
                    }
                    fx
                }
                Op::Pass(i, seat) => s.pass(member(i), SeatId(seat)).unwrap_or_default(),
                Op::TakeBack(seat) => s.take_back(seat.map(SeatId)),
                Op::Panic => s.panic(),
                Op::Allow => s.allow_control(),
                Op::Deliver => match router.reports.pop_front() {
                    Some((seat, holder)) => s.seat_changed(seat, holder),
                    None => Vec::new(),
                },
                Op::Lapse(seat) => {
                    router.release(usize::from(seat));
                    Vec::new()
                }
                Op::Wait(ms) => {
                    now += Duration::from_millis(ms);
                    Vec::new()
                }
                Op::End => {
                    // The engine stops the stream, which releases everything.
                    let fx = s.end(EndReason::HostStopped);
                    router.release_all();
                    fx
                }
            };

            for effect in &fx {
                if let Effect::Grant { seat, member, permissions } = effect {
                    prop_assert!(!paused_before && !ended_before, "a grant while paused or ended");
                    prop_assert!(s.is_viewer(*member), "a grant to a non-viewer");
                    prop_assert_eq!(*permissions, SEATS[usize::from(seat.0)].permissions());
                }
                if let Effect::Requested(member, seat) = effect {
                    let cooling = declined_at
                        .get(member)
                        .is_some_and(|t| now < *t + DECLINE_COOLDOWN);
                    prop_assert!(!cooling, "a request inside the cooldown");
                    prop_assert!(usize::from(seat.0) < SEATS.len());
                }
            }
            router.apply(&fx);
            prop_assert!(router.one_seat_each(), "a member holds two seats");

            let viewers = s.viewers();
            prop_assert!(viewers.len() <= usize::from(s.config().max_viewers));
            let mut unique = viewers.to_vec();
            unique.sort();
            unique.dedup();
            prop_assert_eq!(unique.len(), viewers.len());
            for holder in s.holders().iter().flatten() {
                prop_assert!(s.is_viewer(*holder), "a holder is a viewer");
            }
            prop_assert!(s.requests().iter().all(|(r, _)| s.is_viewer(*r)));
            let mut askers: Vec<MemberId> = s.requests().iter().map(|(r, _)| *r).collect();
            askers.sort();
            askers.dedup();
            prop_assert_eq!(askers.len(), s.requests().len(), "one request per viewer");
            if s.is_ended() {
                prop_assert!(s.requests().is_empty());
            }
        }

        // Once every report has arrived, the mirror matches the router, and
        // only viewers, in a session neither paused nor over, hold seats.
        for _ in 0..256 {
            let Some((seat, holder)) = router.reports.pop_front() else { break };
            let fx = s.seat_changed(seat, holder);
            router.apply(&fx);
            prop_assert!(router.one_seat_each());
        }
        prop_assert!(router.reports.is_empty());
        prop_assert_eq!(&router.holders[..], s.holders());
        for holder in router.holders.iter().flatten() {
            prop_assert!(s.is_viewer(*holder) && !s.control_paused() && !s.is_ended());
        }
    }
}
