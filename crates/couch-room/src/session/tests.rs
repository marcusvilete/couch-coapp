use std::collections::VecDeque;

use couch_proto::session::{ControlDeclined, SessionJoinReply};
use proptest::prelude::*;

use super::*;

const ANA: MemberId = MemberId([1; 32]);
const BIA: MemberId = MemberId([2; 32]);
const CAIO: MemberId = MemberId([3; 32]);

fn config(max_viewers: u8) -> SessionConfig {
    SessionConfig {
        title: "Celeste".into(),
        mode: SessionMode::SinglePlayer,
        keyboard_mouse: false,
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
    assert!(s.request(BIA, now).is_empty());

    let fx = s.request(ANA, now);
    assert!(fx.contains(&Effect::Requested(ANA)));
    assert_eq!(s.requests(), &[ANA]);
    assert!(s.request(ANA, now).is_empty(), "one request at a time");

    let fx = s.answer(ANA, true, now).unwrap();
    assert!(fx.contains(&Effect::Grant {
        member: ANA,
        permissions: Permissions::GUEST_DEFAULT,
    }));
    assert!(s.requests().is_empty());
    // The router's report makes it so.
    assert_eq!(s.controller(), None);
    let fx = s.seat_changed(Some(ANA));
    assert!(broadcasts_status(&fx));
    assert_eq!(s.controller(), Some(ANA));
    assert!(s.request(ANA, now).is_empty(), "already playing");

    assert_eq!(
        s.answer(BIA, true, now).unwrap_err(),
        SessionError::NoRequest
    );
}

#[test]
fn keyboard_and_mouse_come_only_with_the_hosts_switch() {
    let mut s = HostSession::new(
        SessionId(1),
        SessionConfig {
            keyboard_mouse: true,
            ..config(4)
        },
    )
    .unwrap();
    s.join(ANA);
    assert_eq!(
        s.pass(ANA).unwrap()[0],
        Effect::Grant {
            member: ANA,
            permissions: Permissions::GAMEPAD | Permissions::KEYBOARD | Permissions::MOUSE,
        }
    );
}

#[test]
fn a_declined_viewer_waits_before_asking_again() {
    let now = Instant::now();
    let mut s = session(4);
    s.join(ANA);
    s.request(ANA, now);
    let fx = s.answer(ANA, false, now).unwrap();
    assert!(declined(&fx, ANA));
    assert!(!fx.iter().any(|e| matches!(e, Effect::Grant { .. })));

    let soon = now + DECLINE_COOLDOWN - Duration::from_millis(1);
    let fx = s.request(ANA, soon);
    assert!(declined(&fx, ANA), "declined again during the cooldown");
    assert!(s.requests().is_empty());

    let fx = s.request(ANA, now + DECLINE_COOLDOWN);
    assert!(fx.contains(&Effect::Requested(ANA)));
}

#[test]
fn the_host_passes_and_takes_back() {
    let mut s = session(4);
    s.join(ANA);
    assert_eq!(s.pass(BIA).unwrap_err(), SessionError::NotViewer);
    assert!(matches!(s.pass(ANA).unwrap()[0], Effect::Grant { member, .. } if member == ANA));
    s.seat_changed(Some(ANA));
    assert!(s.pass(ANA).unwrap().is_empty(), "already holds it");
    assert_eq!(s.take_back(), vec![Effect::Revoke { holder: None }]);
    s.seat_changed(None);
    assert_eq!(s.controller(), None);
}

#[test]
fn a_viewer_withdraws_or_gives_back() {
    let now = Instant::now();
    let mut s = session(4);
    s.join(ANA);
    s.request(ANA, now);
    assert_eq!(s.cancel(ANA), vec![Effect::Changed]);
    assert!(s.requests().is_empty());

    s.pass(ANA).unwrap();
    s.seat_changed(Some(ANA));
    assert_eq!(s.cancel(ANA), vec![Effect::Revoke { holder: Some(ANA) }]);
    assert!(s.cancel(BIA).is_empty());
}

#[test]
fn the_panic_key_takes_everything_back_until_allowed() {
    let now = Instant::now();
    let mut s = session(4);
    s.join(ANA);
    s.join(BIA);
    s.pass(ANA).unwrap();
    s.seat_changed(Some(ANA));
    s.request(BIA, now);

    let fx = s.panic();
    assert_eq!(fx[0], Effect::RevokeAll, "released first");
    assert!(declined(&fx, BIA), "the pending request is declined");
    assert!(s.requests().is_empty());
    assert!(s.control_paused());
    assert!(broadcasts_status(&fx));

    // Nothing gets control while paused.
    assert!(declined(&s.request(BIA, now), BIA));
    assert_eq!(s.pass(BIA).unwrap_err(), SessionError::ControlPaused);
    // A grant that crossed the panic key goes straight back.
    assert_eq!(
        s.seat_changed(Some(BIA)),
        vec![Effect::Revoke { holder: Some(BIA) }]
    );
    assert_eq!(
        s.panic()[0],
        Effect::RevokeAll,
        "pressing again releases again"
    );

    let fx = s.allow_control();
    assert!(broadcasts_status(&fx));
    assert!(!s.control_paused());
    assert!(s.request(BIA, now).contains(&Effect::Requested(BIA)));
}

#[test]
fn accepting_while_paused_is_refused() {
    let now = Instant::now();
    let mut s = session(4);
    s.join(ANA);
    s.request(ANA, now);
    s.paused = true;
    assert_eq!(
        s.answer(ANA, true, now).unwrap_err(),
        SessionError::ControlPaused
    );
    assert_eq!(s.requests(), &[ANA], "still waiting");
}

#[test]
fn a_controller_who_leaves_loses_the_seat_at_once() {
    let mut s = session(4);
    s.join(ANA);
    s.pass(ANA).unwrap();
    s.seat_changed(Some(ANA));
    let fx = s.leave(ANA);
    assert_eq!(
        fx[0],
        Effect::StopViewer(ANA),
        "stopping releases its input"
    );
    assert_eq!(s.controller(), None);
    assert!(broadcasts_status(&fx));
    // The router's late report of the old grant is sent straight back.
    assert_eq!(
        s.seat_changed(Some(ANA)),
        vec![Effect::Revoke { holder: Some(ANA) }]
    );
    assert!(s.seat_changed(None).is_empty());
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
    assert!(s.request(ANA, now).is_empty());
    assert_eq!(s.pass(ANA).unwrap_err(), SessionError::Ended);
    assert!(s.take_back().is_empty());
    assert!(s.connected(BIA).is_empty());
    assert_eq!(
        s.seat_changed(Some(ANA)),
        vec![Effect::Revoke { holder: Some(ANA) }]
    );
}

#[test]
fn declines_name_the_session() {
    let now = Instant::now();
    let mut s = session(4);
    s.join(ANA);
    s.request(ANA, now);
    let fx = s.answer(ANA, false, now).unwrap();
    assert!(fx.contains(&Effect::Send {
        to: ANA,
        message: ControlMessage::ControlDeclined(ControlDeclined {
            session: SessionId(9)
        }),
    }));
}

// ---- Random sequences against a seat router whose reports arrive late ----

#[derive(Debug, Clone)]
enum Op {
    Join(u8),
    Leave(u8),
    Request(u8),
    Cancel(u8),
    Answer(u8, bool),
    Pass(u8),
    TakeBack,
    Panic,
    Allow,
    /// The router hands one report to the session.
    Deliver,
    /// The router gives the seat back on its own (a holder gone quiet).
    Lapse,
    Wait(u64),
    End,
}

fn op() -> impl Strategy<Value = Op> {
    let who = 0u8..4;
    prop_oneof![
        who.clone().prop_map(Op::Join),
        who.clone().prop_map(Op::Leave),
        who.clone().prop_map(Op::Request),
        who.clone().prop_map(Op::Cancel),
        (who.clone(), any::<bool>()).prop_map(|(w, a)| Op::Answer(w, a)),
        who.prop_map(Op::Pass),
        Just(Op::TakeBack),
        Just(Op::Panic),
        Just(Op::Allow),
        Just(Op::Deliver),
        Just(Op::Deliver),
        Just(Op::Lapse),
        (0u64..4000).prop_map(Op::Wait),
        Just(Op::End),
    ]
}

/// Stands in for `couch_input::router::SeatRouter`: who holds the seat, and
/// its reports still on their way to the session.
#[derive(Default)]
struct Router {
    holder: Option<MemberId>,
    reports: VecDeque<Option<MemberId>>,
}

impl Router {
    fn release(&mut self) {
        if self.holder.take().is_some() {
            self.reports.push_back(None);
        }
    }

    fn apply(&mut self, fx: &[Effect]) {
        for effect in fx {
            match effect {
                Effect::Grant { member, .. } => {
                    // Ending the old hold first, as the router does.
                    self.release();
                    self.holder = Some(*member);
                    self.reports.push_back(Some(*member));
                }
                Effect::Revoke { holder } if holder.is_none_or(|h| self.holder == Some(h)) => {
                    self.release();
                }
                Effect::RevokeAll => self.release(),
                Effect::StopViewer(member) if self.holder == Some(*member) => self.release(),
                _ => {}
            }
        }
    }
}

fn member(i: u8) -> MemberId {
    MemberId([i + 1; 32])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn only_viewers_ever_play(ops in proptest::collection::vec(op(), 1..80)) {
        let t0 = Instant::now();
        let mut now = t0;
        let mut s = session(2);
        let mut router = Router::default();
        let mut declined_at: BTreeMap<MemberId, Instant> = BTreeMap::new();

        for op in ops {
            let paused_before = s.control_paused();
            let ended_before = s.is_ended();
            let fx = match op {
                Op::Join(i) => s.join(member(i)),
                Op::Leave(i) => s.leave(member(i)),
                Op::Request(i) => s.request(member(i), now),
                Op::Cancel(i) => s.cancel(member(i)),
                Op::Answer(i, accept) => {
                    let fx = s.answer(member(i), accept, now).unwrap_or_default();
                    if !accept && declined(&fx, member(i)) {
                        declined_at.insert(member(i), now);
                    }
                    fx
                }
                Op::Pass(i) => s.pass(member(i)).unwrap_or_default(),
                Op::TakeBack => s.take_back(),
                Op::Panic => s.panic(),
                Op::Allow => s.allow_control(),
                Op::Deliver => match router.reports.pop_front() {
                    Some(report) => s.seat_changed(report),
                    None => Vec::new(),
                },
                Op::Lapse => {
                    router.release();
                    Vec::new()
                }
                Op::Wait(ms) => {
                    now += Duration::from_millis(ms);
                    Vec::new()
                }
                Op::End => {
                    // The engine stops the stream, which releases everything.
                    let fx = s.end(EndReason::HostStopped);
                    router.release();
                    fx
                }
            };

            for effect in &fx {
                if let Effect::Grant { member, .. } = effect {
                    prop_assert!(!paused_before && !ended_before, "a grant while paused or ended");
                    prop_assert!(s.is_viewer(*member), "a grant to a non-viewer");
                }
                if let Effect::Requested(member) = effect {
                    let cooling = declined_at
                        .get(member)
                        .is_some_and(|t| now < *t + DECLINE_COOLDOWN);
                    prop_assert!(!cooling, "a request inside the cooldown");
                }
            }
            router.apply(&fx);

            let viewers = s.viewers();
            prop_assert!(viewers.len() <= usize::from(s.config().max_viewers));
            let mut unique = viewers.to_vec();
            unique.sort();
            unique.dedup();
            prop_assert_eq!(unique.len(), viewers.len());
            if let Some(c) = s.controller() {
                prop_assert!(s.is_viewer(c), "the controller is a viewer");
                prop_assert!(!s.requests().contains(&c));
            }
            prop_assert!(s.requests().iter().all(|r| s.is_viewer(*r)));
            if s.is_ended() {
                prop_assert!(s.requests().is_empty());
            }
        }

        // Once every report has arrived, the mirror matches the router, and
        // only a viewer, in a session neither paused nor over, holds the seat.
        for _ in 0..64 {
            let Some(report) = router.reports.pop_front() else { break };
            let fx = s.seat_changed(report);
            router.apply(&fx);
        }
        prop_assert!(router.reports.is_empty());
        prop_assert_eq!(router.holder, s.controller());
        if let Some(holder) = router.holder {
            prop_assert!(s.is_viewer(holder) && !s.control_paused() && !s.is_ended());
        }
    }
}
