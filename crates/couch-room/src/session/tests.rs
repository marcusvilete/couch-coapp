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
        host: None,
        host_controllers: 0,
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

/// M19: a session starts with nothing streamed; streams starting and ending
/// are broadcast, and viewers and seats stay through them.
#[test]
fn streams_start_and_end_inside_the_session() {
    let mut s = with_seats(&[SeatKind::Controller]);
    assert_eq!(status_of(&s).stream, StreamState::Waiting);
    s.join(ANA);
    s.seat_changed(S0, Some(ANA));
    let fx = s.set_stream(StreamState::Live);
    assert!(broadcasts_status(&fx));
    assert_eq!(status_of(&s).stream, StreamState::Live);
    // The same again tells nobody.
    assert_eq!(s.set_stream(StreamState::Live), vec![]);
    let ended = StreamState::Stopped(EndReason::GameExited);
    assert!(broadcasts_status(&s.set_stream(ended)));
    assert_eq!(s.stream(), ended);
    assert_eq!(s.viewers(), &[ANA]);
    assert_eq!(s.seat_of(ANA), Some(S0));
    // Once the session is over, nothing more.
    s.end(EndReason::HostStopped);
    assert_eq!(s.set_stream(StreamState::Live), vec![]);
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
        controller: None,
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
            ..
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
    assert_eq!(
        new(&[SeatKind::Controller; 5]).unwrap_err(),
        SessionError::InvalidSeats
    );
    assert_eq!(
        new(&[SeatKind::KeyboardMouse, SeatKind::KeyboardMouse]).unwrap_err(),
        SessionError::InvalidSeats
    );
    assert!(new(&[]).unwrap().holders().is_empty(), "watching together");
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

/// M24: a session nobody plays. Viewers watch; nothing they ask for, and
/// nothing the host tries, hands out a seat.
#[test]
fn a_session_to_watch_together_has_nothing_to_hand_out() {
    let now = Instant::now();
    let mut s = hosted_with(&[], 2);
    assert!(s.start().is_empty(), "the host's controllers stay theirs");
    assert!(s.host_seats().is_empty());
    assert!(s.join(ANA).contains(&Effect::StartViewer(ANA)), "watches");
    assert!(s.request(ANA, S0, now).is_empty());
    assert!(s.request_with(ANA, S0, Some(0), now).is_empty());
    assert!(s.requests().is_empty());
    assert_eq!(s.pass(ANA, S0).unwrap_err(), SessionError::NoSuchSeat);
    assert_eq!(s.play(S0, 0).unwrap_err(), SessionError::HostNotOnASeat);
    assert_eq!(s.take_back(Some(S0)), vec![]);
    assert_eq!(
        s.take_back(None),
        vec![Effect::RevokeViewers],
        "frees nothing"
    );
    assert!(s.give_back(ANA, None).is_empty());
    assert!(
        s.seat_changed(S0, Some(ANA)).is_empty(),
        "a report for no seat"
    );
    let status = status_of(&s);
    assert!(status.holders.is_empty() && status.host_seats.is_empty());
    assert!(status.is_valid());
    match s.announce() {
        ControlMessage::SessionAnnounce(a) => assert!(a.seats.is_empty() && a.is_valid()),
        other => panic!("{other:?}"),
    }
}

// ---- The host on a seat ----

const HOST: MemberId = MemberId([9; 32]);

fn hosted(seats: &[SeatKind]) -> HostSession {
    hosted_with(seats, 1)
}

/// The host with `controllers` controllers on seats (M23).
fn hosted_with(seats: &[SeatKind], controllers: u8) -> HostSession {
    HostSession::new(
        SessionId(1),
        SessionConfig {
            seats: seats.to_vec(),
            host: Some(HOST),
            host_controllers: controllers,
            ..config(4)
        },
    )
    .unwrap()
}

fn claims(fx: &[Effect]) -> Vec<(SeatId, MemberId)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Claim { seat, member } => Some((*seat, *member)),
            _ => None,
        })
        .collect()
}

fn status_of(s: &HostSession) -> SessionStatus {
    match s.status() {
        ControlMessage::SessionStatus(status) => status,
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_host_starts_on_the_first_seat_with_a_pad() {
    let s = hosted(&[SeatKind::KeyboardMouse, SeatKind::Controller]);
    assert_eq!(
        s.start(),
        vec![
            Effect::HostSeats(vec![S1]),
            Effect::Claim {
                seat: S1,
                member: HOST
            }
        ]
    );

    let mut s = hosted(&[SeatKind::Controller, SeatKind::Controller]);
    assert_eq!(claims(&s.start()), vec![(S0, HOST)]);
    let fx = s.seat_changed(S0, Some(HOST));
    assert!(broadcasts_status(&fx));
    assert_eq!(s.holders(), &[Some(HOST), None]);
    assert_eq!(s.host_seats(), vec![S0]);
    // On the wire, the host's seat is apart from the viewers'.
    let status = status_of(&s);
    assert_eq!(status.holders, vec![None, None]);
    assert_eq!(status.host_seats, vec![S0]);
    assert!(status.is_valid());

    // Nothing to play with a keyboard seat alone, and nothing without a host.
    let s = hosted(&[SeatKind::KeyboardMouse]);
    assert!(s.start().is_empty());
    assert_eq!(s.host(), None);
    let s = with_seats(&[SeatKind::Controller]);
    assert!(s.start().is_empty());
    assert!(status_of(&s).host_seats.is_empty());
}

#[test]
fn the_hosts_seat_comes_back_when_a_viewer_lets_go() {
    let now = Instant::now();
    let mut s = hosted(&[SeatKind::Controller]);
    s.start();
    s.seat_changed(S0, Some(HOST));
    s.join(ANA);
    s.request(ANA, S0, now);
    let fx = s.answer(ANA, true, now).unwrap();
    assert_eq!(grant_of(&fx), Some((S0, ANA, Permissions::GAMEPAD)));
    // The router ends the host's hold first. The claim this causes reaches
    // it after the grant, so it finds the seat taken and does nothing.
    let fx = s.seat_changed(S0, None);
    assert_eq!(claims(&fx), vec![(S0, HOST)]);
    s.seat_changed(S0, Some(ANA));
    assert!(s.host_seats().is_empty());
    assert!(status_of(&s).host_seats.is_empty());

    // Given back, or the viewer leaves: it goes back to the host.
    s.give_back(ANA, None);
    assert_eq!(claims(&s.seat_changed(S0, None)), vec![(S0, HOST)]);
    s.seat_changed(S0, Some(HOST));
    s.pass(ANA, S0).unwrap();
    s.seat_changed(S0, None);
    s.seat_changed(S0, Some(ANA));
    let fx = s.leave(ANA);
    let stop = fx
        .iter()
        .position(|e| *e == Effect::StopViewer(ANA))
        .unwrap();
    let claim = fx
        .iter()
        .position(|e| matches!(e, Effect::Claim { .. }))
        .unwrap();
    assert!(stop < claim, "the viewer's seat is freed before the claim");

    // Taking every seat back leaves the host's own alone: the router tells
    // it apart, even before the mirror knows of it.
    s.seat_changed(S0, Some(HOST));
    assert_eq!(s.take_back(None), vec![Effect::RevokeViewers]);
}

/// The router's reports trail what the host did: one that arrives after a
/// take back must not undo it.
#[test]
fn late_reports_dont_refill_a_seat_taken_back() {
    let mut s = hosted(&[SeatKind::Controller, SeatKind::Controller]);
    s.start();
    s.seat_changed(S0, Some(HOST));
    s.join(ANA);
    // Passed to Ana, then taken back before the router's reports arrive.
    s.pass(ANA, S0).unwrap();
    s.take_back(Some(S0));
    assert!(claims(&s.seat_changed(S0, None)).is_empty());
    s.seat_changed(S0, Some(ANA));
    assert!(claims(&s.seat_changed(S0, None)).is_empty(), "stays empty");
    assert_eq!(s.holders(), &[None, None]);

    // The same with the panic key: the mirror still shows the host on it.
    s.play(S0, 0).unwrap();
    s.seat_changed(S0, Some(HOST));
    s.pass(ANA, S0).unwrap();
    s.panic();
    s.seat_changed(S0, None);
    s.seat_changed(S0, Some(ANA));
    s.seat_changed(S0, None);
    assert!(claims(&s.allow_control()).is_empty(), "stays empty");
}

#[test]
fn a_seat_taken_back_stays_empty() {
    let mut s = hosted(&[SeatKind::Controller, SeatKind::Controller]);
    s.start();
    s.seat_changed(S0, Some(HOST));
    s.join(ANA);
    s.pass(ANA, S0).unwrap();
    s.seat_changed(S0, None);
    s.seat_changed(S0, Some(ANA));

    // Taken back from a viewer: nobody gets it, the host included.
    assert_eq!(
        s.take_back(Some(S0)),
        vec![Effect::Revoke {
            seat: S0,
            holder: None
        }]
    );
    assert!(claims(&s.seat_changed(S0, None)).is_empty());
    assert_eq!(s.holders(), &[None, None]);
    let status = status_of(&s);
    assert!(status.host_seats.is_empty());
    assert!(
        status.host_on_seats,
        "the host still plays on seats, just none"
    );
    // Neither does the panic key give it to them, nor allowing control again.
    s.panic();
    assert!(claims(&s.seat_changed(S0, None)).is_empty());
    assert!(claims(&s.allow_control()).is_empty());

    // Held by a viewer again, it comes back to the host when let go.
    s.pass(ANA, S0).unwrap();
    s.seat_changed(S0, Some(ANA));
    s.give_back(ANA, None);
    assert_eq!(claims(&s.seat_changed(S0, None)), vec![(S0, HOST)]);
    s.seat_changed(S0, Some(HOST));

    // The host leaves their own: empty too, until they play again.
    assert_eq!(
        s.take_back(Some(S0)),
        vec![Effect::Revoke {
            seat: S0,
            holder: None
        }]
    );
    assert!(claims(&s.seat_changed(S0, None)).is_empty());
    assert_eq!(
        grant_of(&s.play(S0, 0).unwrap()),
        Some((S0, HOST, Permissions::GAMEPAD))
    );
    s.seat_changed(S0, Some(HOST));

    // Taking back another seat leaves the host where they are.
    s.pass(ANA, S1).unwrap();
    s.seat_changed(S1, Some(ANA));
    s.take_back(Some(S1));
    assert!(claims(&s.seat_changed(S1, None)).is_empty());
    assert_eq!(s.holders(), &[Some(HOST), None]);
}

#[test]
fn the_host_plays_another_seat() {
    let mut s = hosted(&[
        SeatKind::Controller,
        SeatKind::Controller,
        SeatKind::KeyboardMouse,
    ]);
    s.start();
    s.seat_changed(S0, Some(HOST));
    s.join(ANA);
    s.pass(ANA, S1).unwrap();
    s.seat_changed(S1, Some(ANA));

    assert_eq!(s.play(SeatId(2), 0).unwrap_err(), SessionError::NoPad);
    assert_eq!(s.play(SeatId(3), 0).unwrap_err(), SessionError::NoSuchSeat);
    assert_eq!(s.play(S1, 1).unwrap_err(), SessionError::NoSuchController);
    assert!(s.play(S0, 0).unwrap().is_empty(), "already there");
    let fx = s.play(S1, 0).unwrap();
    let revoke = fx
        .iter()
        .position(|e| {
            *e == Effect::Revoke {
                seat: S0,
                holder: Some(HOST),
            }
        })
        .unwrap();
    let grant = fx
        .iter()
        .position(|e| matches!(e, Effect::Grant { .. }))
        .unwrap();
    assert!(revoke < grant, "the old seat first");
    assert_eq!(grant_of(&fx), Some((S1, HOST, Permissions::GAMEPAD)));
    // The router frees the old seat: no claim for it, or the host would
    // hold two.
    assert!(claims(&s.seat_changed(S0, None)).is_empty());
    // Ana's hold ends; a claim for the host's new seat is harmless.
    s.seat_changed(S1, None);
    s.seat_changed(S1, Some(HOST));
    assert_eq!(s.holders(), &[None, Some(HOST), None]);
    // A late report of the old seat is sent back.
    assert_eq!(
        s.seat_changed(S0, Some(HOST)),
        vec![Effect::Revoke {
            seat: S0,
            holder: Some(HOST)
        }]
    );

    // Without the host on a seat, there's nothing to play.
    let mut s = with_seats(&[SeatKind::Controller]);
    assert_eq!(s.play(S0, 0).unwrap_err(), SessionError::HostNotOnASeat);
}

// ---- The host with several controllers (M23) ----

const S2: SeatId = SeatId(2);

/// The host with two controllers on Controller 1 and 2, both held.
fn two_host_controllers(seats: &[SeatKind]) -> HostSession {
    let mut s = hosted_with(seats, 2);
    s.start();
    s.seat_changed(S0, Some(HOST));
    s.seat_changed(S1, Some(HOST));
    s
}

#[test]
fn each_host_controller_starts_on_its_own_seat() {
    let s = hosted_with(
        &[
            SeatKind::Controller,
            SeatKind::KeyboardMouse,
            SeatKind::Controller,
        ],
        2,
    );
    assert_eq!(
        s.start(),
        vec![
            Effect::HostSeats(vec![S0, S2]),
            Effect::Claim {
                seat: S0,
                member: HOST
            },
            Effect::Claim {
                seat: S2,
                member: HOST
            },
        ]
    );
    // Never more controllers than seats with a pad.
    let s = hosted_with(&[SeatKind::Controller, SeatKind::KeyboardMouse], 3);
    assert_eq!(s.host_homes(), vec![S0]);
    // Nor any without a host.
    let s = HostSession::new(
        SessionId(1),
        SessionConfig {
            seats: vec![SeatKind::Controller, SeatKind::Controller],
            host_controllers: 2,
            ..config(4)
        },
    )
    .unwrap();
    assert!(s.host_homes().is_empty() && s.start().is_empty());

    let s = two_host_controllers(&[SeatKind::Controller, SeatKind::Controller]);
    assert_eq!(s.host_seats(), vec![S0, S1]);
    let status = status_of(&s);
    assert_eq!(status.host_seats, vec![S0, S1]);
    assert_eq!(status.holders, vec![None, None]);
    assert!(status.is_valid());
}

#[test]
fn a_host_controllers_seat_passed_on_comes_back_to_it_alone() {
    let mut s = two_host_controllers(&[SeatKind::Controller, SeatKind::Controller]);
    s.join(ANA);
    // Passing one of the host's seats takes none of their others.
    let fx = s.pass(ANA, S1).unwrap();
    assert_eq!(
        grant_of(&fx),
        Some((S1, ANA, SeatKind::Controller.permissions()))
    );
    assert!(
        !fx.iter()
            .any(|e| matches!(e, Effect::Revoke { holder, .. } if *holder != Some(ANA))),
        "only Ana's other seats (she has none) are revoked"
    );
    s.seat_changed(S1, None);
    s.seat_changed(S1, Some(ANA));
    assert_eq!(s.host_seats(), vec![S0]);
    // Ana gives it back: only that seat comes back, to its controller.
    s.give_back(ANA, None);
    assert_eq!(claims(&s.seat_changed(S1, None)), vec![(S1, HOST)]);
    s.seat_changed(S1, Some(HOST));
    assert_eq!(s.host_seats(), vec![S0, S1]);
    assert_eq!(s.host_controller_on(S1), Some(1));
}

#[test]
fn a_host_controller_moves_or_swaps() {
    let mut s = two_host_controllers(&[
        SeatKind::Controller,
        SeatKind::Controller,
        SeatKind::Controller,
    ]);
    // Onto another controller's seat: they swap, the router's holders stay.
    let fx = s.play(S1, 0).unwrap();
    assert_eq!(fx, vec![Effect::HostSeats(vec![S1, S0]), Effect::Changed]);
    assert_eq!(s.host_homes(), vec![S1, S0]);
    // Onto a free seat: its old seat goes first, then the new one is granted.
    let fx = s.play(S2, 1).unwrap();
    assert_eq!(fx[0], Effect::HostSeats(vec![S1, S2]));
    let revoke = fx
        .iter()
        .position(|e| {
            *e == Effect::Revoke {
                seat: S0,
                holder: Some(HOST),
            }
        })
        .unwrap();
    let grant = fx
        .iter()
        .position(|e| matches!(e, Effect::Grant { .. }))
        .unwrap();
    assert!(revoke < grant);
    assert_eq!(grant_of(&fx), Some((S2, HOST, Permissions::GAMEPAD)));
    // The freed seat is no controller's: no claim for it (one for the new
    // seat, its grant still on its way, is harmless), and a late report of
    // the host on it is sent back.
    assert!(
        !claims(&s.seat_changed(S0, None))
            .iter()
            .any(|(seat, _)| *seat == S0)
    );
    s.seat_changed(S2, Some(HOST));
    assert_eq!(
        s.seat_changed(S0, Some(HOST)),
        vec![Effect::Revoke {
            seat: S0,
            holder: Some(HOST)
        }]
    );
    assert_eq!(s.host_seats(), vec![S1, S2]);
}

/// A swap onto a seat lent to a viewer: the moving controller takes it, the
/// other gets the moving one's seat, still held.
#[test]
fn a_swap_onto_a_lent_seat_takes_it_back_for_the_mover() {
    let mut s = two_host_controllers(&[SeatKind::Controller, SeatKind::Controller]);
    s.join(ANA);
    s.pass(ANA, S1).unwrap();
    s.seat_changed(S1, None);
    s.seat_changed(S1, Some(ANA));
    let fx = s.play(S1, 0).unwrap();
    assert_eq!(fx[0], Effect::HostSeats(vec![S1, S0]));
    assert_eq!(grant_of(&fx), Some((S1, HOST, Permissions::GAMEPAD)));
    assert!(claims(&fx).is_empty(), "controller 1 drives S0, still held");
}

/// The review's case: Play here on a seat just passed to a viewer, the
/// grant not reported yet. The mirror still shows the host on it, but the
/// controller must still be granted it, or the viewer would keep it.
#[test]
fn play_here_after_a_pass_still_on_its_way_grants_the_seat() {
    let mut s = two_host_controllers(&[SeatKind::Controller, SeatKind::Controller]);
    s.join(ANA);
    s.pass(ANA, S1).unwrap();
    let fx = s.play(S1, 1).unwrap();
    assert_eq!(grant_of(&fx), Some((S1, HOST, Permissions::GAMEPAD)));
}

#[test]
fn the_panic_key_and_take_back_spare_the_hosts_other_seats() {
    let mut s = two_host_controllers(&[
        SeatKind::Controller,
        SeatKind::Controller,
        SeatKind::Controller,
    ]);
    s.join(ANA);
    s.pass(ANA, S2).unwrap();
    s.seat_changed(S2, Some(ANA));
    assert!(s.panic().contains(&Effect::RevokeViewers));
    s.seat_changed(S2, None);
    assert_eq!(s.holders(), &[Some(HOST), Some(HOST), None]);
    s.allow_control();
    // Leaving one leaves it empty; the other stays theirs.
    s.take_back(Some(S0));
    assert!(claims(&s.seat_changed(S0, None)).is_empty());
    assert_eq!(s.host_seats(), vec![S1]);
    assert!(claims(&s.allow_control()).is_empty());
    // Play here brings it back.
    assert_eq!(
        grant_of(&s.play(S0, 0).unwrap()),
        Some((S0, HOST, Permissions::GAMEPAD))
    );
}

#[test]
fn the_panic_key_leaves_the_host_their_seat() {
    let mut s = hosted(&[SeatKind::Controller, SeatKind::Controller]);
    s.start();
    s.seat_changed(S0, Some(HOST));
    s.join(ANA);
    s.pass(ANA, S1).unwrap();
    s.seat_changed(S1, Some(ANA));
    assert!(s.panic().contains(&Effect::RevokeViewers));
    // The router frees Ana's; the host's stays theirs.
    assert!(claims(&s.seat_changed(S1, None)).is_empty());
    assert_eq!(s.holders(), &[Some(HOST), None]);
    // A report of the host's seat, even while paused, stands.
    assert!(
        s.seat_changed(S0, Some(HOST))
            .iter()
            .all(|e| !matches!(e, Effect::Revoke { .. }))
    );
    // Nothing comes back to the host while paused: their seat lapsing (the
    // controller's reader stopped) leaves it free, until control is allowed.
    assert!(claims(&s.seat_changed(S0, None)).is_empty());
    assert_eq!(claims(&s.allow_control()), vec![(S0, HOST)]);
    s.seat_changed(S0, Some(HOST));
    s.panic();
    // The host may even move while paused; viewers may not be passed to.
    assert!(s.play(S1, 0).is_ok());
    assert_eq!(s.pass(ANA, S0).unwrap_err(), SessionError::ControlPaused);
}

#[test]
fn the_host_never_watches_their_own_session() {
    let mut s = hosted(&[SeatKind::Controller]);
    assert!(s.join(HOST).is_empty());
    assert!(!s.is_viewer(HOST));
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
                permissions: Permissions::GAMEPAD,
                controller: None,
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
    assert_eq!(s.take_back(None), vec![Effect::RevokeViewers]);
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
        s.give_back(ANA, None),
        vec![Effect::Revoke {
            seat: S0,
            holder: Some(ANA)
        }]
    );
    assert!(s.give_back(BIA, None).is_empty());
}

/// M18: a viewer with two controllers keeps its seat when given a second
/// one with a pad; with one controller it moves, as before. The keyboard
/// seat is always added. Giving back one seat keeps the others.
#[test]
fn seats_are_added_for_spare_controllers_and_moved_otherwise() {
    let mut s = with_seats(&[
        SeatKind::Controller,
        SeatKind::Controller,
        SeatKind::KeyboardMouse,
    ]);
    s.join(ANA);
    let revokes = |fx: &[Effect]| {
        fx.iter()
            .filter(|e| matches!(e, Effect::Revoke { .. }))
            .count()
    };
    // One controller (or none said): a second pad seat moves it.
    s.set_controllers(ANA, 1);
    s.pass(ANA, S0).unwrap();
    s.seat_changed(S0, Some(ANA));
    assert!(!s.keeps(ANA, S1));
    assert!(revokes(&s.pass(ANA, S1).unwrap()) > 0, "moves");
    s.seat_changed(S0, None);
    s.seat_changed(S1, Some(ANA));
    assert_eq!(s.seats_of(ANA), vec![S1]);
    // The keyboard seat is always added.
    assert!(s.keeps(ANA, SeatId(2)));
    assert_eq!(revokes(&s.pass(ANA, SeatId(2)).unwrap()), 0, "adds");
    s.seat_changed(SeatId(2), Some(ANA));
    // Two controllers: the other pad seat is added too.
    s.set_controllers(ANA, 2);
    assert!(s.keeps(ANA, S0));
    assert_eq!(revokes(&s.pass(ANA, S0).unwrap()), 0, "adds");
    s.seat_changed(S0, Some(ANA));
    assert_eq!(s.seats_of(ANA), vec![S0, S1, SeatId(2)]);
    assert!(status_of(&s).is_valid(), "a viewer on several seats");
    // Giving back one keeps the rest; all gives back every one.
    assert_eq!(
        s.give_back(ANA, Some(S1)),
        vec![Effect::Revoke {
            seat: S1,
            holder: Some(ANA)
        }]
    );
    assert_eq!(revokes(&s.give_back(ANA, None)), 3);
}

/// M18: two grants in quick succession, before the router answers the
/// first, never give a viewer more pad seats than its controllers.
#[test]
fn grants_on_their_way_count_toward_a_viewers_seats() {
    let mut s = with_seats(&[SeatKind::Controller, SeatKind::Controller]);
    s.join(ANA);
    s.set_controllers(ANA, 1);
    s.pass(ANA, S0).unwrap();
    // Not reported yet: the second must still move it.
    assert!(!s.keeps(ANA, S1));
}

/// M18 review: a seat granted to one viewer and then another is pending for
/// both; the router's report of the first never clears the second's, so the
/// second still can't take a pad seat beyond its controllers.
#[test]
fn a_report_for_one_viewer_leaves_anothers_grant_pending() {
    let mut s = with_seats(&[
        SeatKind::Controller,
        SeatKind::Controller,
        SeatKind::KeyboardMouse,
    ]);
    s.join(ANA);
    s.join(BIA);
    s.set_controllers(BIA, 1);
    s.pass(ANA, S0).unwrap();
    s.pass(BIA, S0).unwrap();
    // The router reports ANA's grant; BIA's is still on its way.
    s.seat_changed(S0, Some(ANA));
    assert!(!s.keeps(BIA, S1), "BIA's grant of S0 still counts");
    let fx = s.pass(BIA, S1).unwrap();
    assert!(fx.contains(&Effect::Revoke {
        seat: S0,
        holder: Some(BIA)
    }));
}

/// M18 review: a move takes only the other pad seats; the keyboard seat
/// needs no controller and stays.
/// P13: a viewer on two pad seats with two controllers, given a third, gives
/// up only the seat of the controller that moves.
#[test]
fn only_the_moving_controllers_seat_is_freed() {
    let s2 = SeatId(2);
    let pads = [SeatKind::Controller; 3];
    let two_seats = || {
        let mut s = with_seats(&pads);
        s.join(ANA);
        s.set_controllers(ANA, 2);
        s.pass(ANA, S0).unwrap();
        s.seat_changed(S0, Some(ANA));
        s.pass(ANA, S1).unwrap();
        s.seat_changed(S1, Some(ANA));
        s.set_drivers(ANA, &[(S0, 0), (S1, 3)]);
        s
    };
    let revoked = |fx: &[Effect]| -> Vec<SeatId> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::Revoke { seat, .. } => Some(*seat),
                _ => None,
            })
            .collect()
    };

    // The host passes it with controller 3: only seat 2 goes, first.
    let mut s = two_seats();
    let fx = s.pass_with(ANA, s2, Some(3)).unwrap();
    assert_eq!(revoked(&fx), vec![S1]);
    let grant = fx
        .iter()
        .position(|e| matches!(e, Effect::Grant { .. }))
        .unwrap();
    assert!(
        fx.iter()
            .position(|e| matches!(e, Effect::Revoke { .. }))
            .unwrap()
            < grant
    );
    assert!(fx.contains(&Effect::Grant {
        seat: s2,
        member: ANA,
        permissions: Permissions::GAMEPAD,
        controller: Some(3),
    }));
    s.seat_changed(S1, None);
    assert!(revoked(&s.seat_changed(s2, Some(ANA))).is_empty());
    assert_eq!(s.seats_of(ANA), vec![S0, s2]);

    // Without saying which, or naming one that plays no seat: refused.
    let mut s = two_seats();
    assert_eq!(s.pass(ANA, s2).unwrap_err(), SessionError::ChooseController);
    assert_eq!(
        s.pass_with(ANA, s2, Some(1)).unwrap_err(),
        SessionError::ChooseController
    );

    // A request names its controller, and the answer frees its seat alone.
    let mut s = two_seats();
    let now = Instant::now();
    s.request_with(ANA, s2, Some(0), now);
    assert_eq!(s.asked_with(ANA), Some(0));
    let fx = s.answer(ANA, true, now).unwrap();
    assert_eq!(revoked(&fx), vec![S0]);
    assert_eq!(s.asked_with(ANA), None);
}

/// P13 review: a grant still on its way, freed by a later one, is reported
/// anyway before its revoke: that report frees nothing more.
#[test]
fn a_freed_grant_on_its_way_frees_nothing_when_reported() {
    let (s1, s2, s3) = (SeatId(1), SeatId(2), SeatId(3));
    let mut s = with_seats(&[SeatKind::Controller; 4]);
    s.join(ANA);
    s.set_controllers(ANA, 2);
    s.pass(ANA, s2).unwrap();
    s.seat_changed(s2, Some(ANA));
    s.pass(ANA, s3).unwrap();
    let now = Instant::now();
    s.request(ANA, s1, now);
    let fx = s.answer(ANA, true, now).unwrap();
    assert!(fx.contains(&Effect::Revoke {
        seat: s3,
        holder: Some(ANA)
    }));
    // The router took the grant of seat 3, then its revoke, then seat 2's.
    let late = s.seat_changed(s3, Some(ANA));
    assert!(
        !late
            .iter()
            .any(|e| matches!(e, Effect::Revoke { seat, .. } if *seat == s2)),
        "{late:?}"
    );
    s.seat_changed(s3, None);
    s.seat_changed(s1, Some(ANA));
    assert_eq!(s.seats_of(ANA), vec![s1, s2]);
}

/// P13, the author's run of 0.19.0: two controllers, one on a seat; that
/// one moved to another seat moves, though the other is spare. The spare
/// one given a seat adds it.
#[test]
fn a_seated_controller_moves_even_with_one_to_spare() {
    let mut s = with_seats(&[SeatKind::Controller; 3]);
    s.join(ANA);
    s.set_controllers(ANA, 2);
    s.pass(ANA, S0).unwrap();
    s.seat_changed(S0, Some(ANA));
    s.set_drivers(ANA, &[(S0, 0)]);
    let fx = s.pass_with(ANA, S1, Some(0)).unwrap();
    assert!(fx.contains(&Effect::Revoke {
        seat: S0,
        holder: Some(ANA)
    }));
    s.seat_changed(S0, None);
    s.seat_changed(S1, Some(ANA));
    assert_eq!(s.seats_of(ANA), vec![S1]);

    s.set_drivers(ANA, &[(S1, 0)]);
    let fx = s.pass_with(ANA, SeatId(2), Some(1)).unwrap();
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Revoke { .. })),
        "{fx:?}"
    );
}

/// P13: with one seat to free, nothing needs choosing.
#[test]
fn one_seat_moves_without_a_choice() {
    let mut s = with_seats(&[SeatKind::Controller; 3]);
    s.join(ANA);
    s.set_controllers(ANA, 1);
    s.pass(ANA, S0).unwrap();
    s.seat_changed(S0, Some(ANA));
    let fx = s.pass(ANA, S1).unwrap();
    assert!(fx.contains(&Effect::Revoke {
        seat: S0,
        holder: Some(ANA)
    }));
}

#[test]
fn a_move_keeps_the_keyboard_seat() {
    let kbm = SeatId(2);
    let mut s = with_seats(&[
        SeatKind::Controller,
        SeatKind::Controller,
        SeatKind::KeyboardMouse,
    ]);
    s.join(ANA);
    s.set_controllers(ANA, 1);
    s.pass(ANA, S0).unwrap();
    s.seat_changed(S0, Some(ANA));
    s.pass(ANA, kbm).unwrap();
    s.seat_changed(kbm, Some(ANA));
    let fx = s.pass(ANA, S1).unwrap();
    assert!(fx.contains(&Effect::Revoke {
        seat: S0,
        holder: Some(ANA)
    }));
    assert!(
        !fx.iter()
            .any(|e| matches!(e, Effect::Revoke { seat, .. } if *seat == kbm))
    );
    s.seat_changed(S0, None);
    assert!(
        s.seat_changed(S1, Some(ANA))
            .iter()
            .all(|e| !matches!(e, Effect::Revoke { .. }))
    );
    assert_eq!(s.seats_of(ANA), vec![S1, kbm]);
}

/// M18 review: a seat added while the viewer had two controllers stays with
/// the one it held, even if it says it has one before the router answers.
#[test]
fn a_count_dropping_during_an_add_frees_nothing() {
    let mut s = with_seats(&[SeatKind::Controller, SeatKind::Controller]);
    s.join(ANA);
    s.set_controllers(ANA, 2);
    s.pass(ANA, S0).unwrap();
    s.seat_changed(S0, Some(ANA));
    s.pass(ANA, S1).unwrap();
    s.set_controllers(ANA, 1);
    let fx = s.seat_changed(S1, Some(ANA));
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Revoke { .. })),
        "{fx:?}"
    );
    assert_eq!(s.seats_of(ANA), vec![S0, S1]);
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
    assert_eq!(fx[0], Effect::RevokeViewers, "released first");
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
        Effect::RevokeViewers,
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
    /// Gives back one seat, or every seat.
    GiveBack(u8, Option<u8>),
    /// A viewer says how many controllers it has (M18).
    Controllers(u8, u8),
    Answer(u8, bool),
    Pass(u8, u8),
    /// A pass naming one of the viewer's controllers (P13).
    PassWith(u8, u8, u8),
    /// A request naming one of the viewer's controllers (P13).
    RequestWith(u8, u8, u8),
    /// A viewer says which controller drives each seat it holds: the
    /// seats in order from this controller on (P13).
    Drive(u8, u8),
    TakeBack(Option<u8>),
    /// One of the host's controllers moves to a seat (M23).
    Play(u8, u8),
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
    // Weighted so a session lives long enough for a viewer to join, say it
    // has controllers and be given several seats (M18): with every op
    // equally likely, the end came after about 19 and no viewer ever held
    // two.
    prop_oneof![
        3 => who.clone().prop_map(Op::Join),
        1 => who.clone().prop_map(Op::Leave),
        3 => (who.clone(), seat.clone()).prop_map(|(w, s)| Op::Request(w, s)),
        1 => who.clone().prop_map(Op::Withdraw),
        2 => (who.clone(), proptest::option::of(seat.clone())).prop_map(|(w, s)| Op::GiveBack(w, s)),
        3 => (who.clone(), 0u8..4).prop_map(|(w, n)| Op::Controllers(w, n)),
        3 => (who.clone(), any::<bool>()).prop_map(|(w, a)| Op::Answer(w, a)),
        5 => (who.clone(), seat.clone()).prop_map(|(w, s)| Op::Pass(w, s)),
        3 => (who.clone(), seat.clone(), 0u8..4).prop_map(|(w, s, c)| Op::PassWith(w, s, c)),
        2 => (who.clone(), seat.clone(), 0u8..4).prop_map(|(w, s, c)| Op::RequestWith(w, s, c)),
        2 => (who, 0u8..4).prop_map(|(w, c)| Op::Drive(w, c)),
        1 => proptest::option::of(seat.clone()).prop_map(Op::TakeBack),
        2 => (seat.clone(), 0u8..3).prop_map(|(s, c)| Op::Play(s, c)),
        1 => Just(Op::Panic),
        2 => Just(Op::Allow),
        8 => Just(Op::Deliver),
        1 => seat.prop_map(Op::Lapse),
        1 => (0u64..4000).prop_map(Op::Wait),
        1 => Just(Op::End),
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
                Effect::Claim { seat, member } => {
                    // Only a free seat. (The host pipeline also refuses a
                    // claim from someone already holding one; the session
                    // must not need that, so this doesn't.)
                    let i = usize::from(seat.0);
                    if self.holders[i].is_none() {
                        self.holders[i] = Some(member);
                        self.reports.push_back((seat, Some(member)));
                    }
                }
                Effect::RevokeViewers => {
                    for seat in 0..self.holders.len() {
                        if self.holders[seat].is_some_and(|h| h != HOST) {
                            self.release(seat);
                        }
                    }
                }
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

    /// At any moment (M18, M23): the host holds a seat with a pad per
    /// controller of theirs at most, and nothing else; a viewer holds at
    /// most one seat with a pad per controller it said it had (one with
    /// none: a move), and at most the one keyboard-and-mouse seat beside.
    fn within_controllers(
        &self,
        controllers: &BTreeMap<MemberId, u8>,
        host_controllers: usize,
    ) -> bool {
        let mut members: Vec<MemberId> = self.holders.iter().flatten().copied().collect();
        members.sort();
        members.dedup();
        members.into_iter().all(|m| {
            let seats: Vec<usize> = (0..self.holders.len())
                .filter(|i| self.holders[*i] == Some(m))
                .collect();
            let pads = seats
                .iter()
                .filter(|i| SEATS[**i].permissions().contains(Permissions::GAMEPAD))
                .count();
            if m == HOST {
                return seats.len() == pads && pads <= host_controllers;
            }
            let allowed = usize::from(controllers.get(&m).copied().unwrap_or(0).max(1));
            pads <= allowed && seats.len() - pads <= 1
        })
    }
}

fn member(i: u8) -> MemberId {
    MemberId([i + 1; 32])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn only_viewers_and_the_host_play_within_their_controllers(
        // 0: the host plays outside the seats; 1 or 2 controllers (M23).
        // Half without, as before M23, so viewers still reach two pad seats
        // as often (the host's controllers take seats from them).
        host_controllers in prop_oneof![2 => Just(0u8), 1 => Just(1u8), 1 => Just(2u8)],
        ops in proptest::collection::vec(op(), 1..100),
    ) {
        let host_plays = host_controllers > 0;
        let t0 = Instant::now();
        let mut now = t0;
        let mut s = HostSession::new(
            SessionId(9),
            SessionConfig {
                seats: SEATS.to_vec(),
                host: host_plays.then_some(HOST),
                host_controllers,
                ..config(3)
            },
        )
        .unwrap();
        let mut router = Router::new();
        let mut declined_at: BTreeMap<MemberId, Instant> = BTreeMap::new();
        // The most controllers each viewer said it has, this time it watched.
        let mut most_controllers: BTreeMap<MemberId, u8> = BTreeMap::new();
        let fx = s.start();
        prop_assert_eq!(claims(&fx).len(), usize::from(host_controllers));
        router.apply(&fx);
        // What the host did with each controller's seat, from the ops alone:
        // the seat it last played, and whether they passed it on or took it
        // back to leave it empty.
        let mut homes: Vec<HostHome> = (0..host_controllers)
            .map(|k| HostHome { seat: SeatId(k), home: Home::Playing })
            .collect();
        let max_host = homes.len();
        let mut host_held_two = false;
        let mut viewer_two = false;

        for op in ops {
            let paused_before = s.control_paused();
            let ended_before = s.is_ended();
            let fx = match op.clone() {
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
                Op::GiveBack(i, seat) => s.give_back(member(i), seat.map(SeatId)),
                Op::Controllers(i, n) => {
                    if s.is_viewer(member(i)) {
                        let most = most_controllers.entry(member(i)).or_insert(0);
                        *most = (*most).max(n);
                    }
                    s.set_controllers(member(i), n)
                }
                Op::Answer(i, accept) => {
                    let fx = s.answer(member(i), accept, now).unwrap_or_default();
                    if !accept && declined(&fx, member(i)) {
                        declined_at.insert(member(i), now);
                    }
                    fx
                }
                Op::Pass(i, seat) => s.pass(member(i), SeatId(seat)).unwrap_or_default(),
                Op::PassWith(i, seat, c) => {
                    s.pass_with(member(i), SeatId(seat), Some(c)).unwrap_or_default()
                }
                Op::RequestWith(i, seat, c) => s.request_with(member(i), SeatId(seat), Some(c), now),
                Op::Drive(i, c) => {
                    let drivers: Vec<(SeatId, u8)> = s
                        .seats_of(member(i))
                        .into_iter()
                        .zip(c..)
                        .map(|(seat, c)| (seat, c % 4))
                        .collect();
                    s.set_drivers(member(i), &drivers);
                    Vec::new()
                }
                Op::TakeBack(seat) => s.take_back(seat.map(SeatId)),
                Op::Play(seat, k) => match s.play(SeatId(seat), k) {
                    Ok(fx) => {
                        let (seat, k) = (SeatId(seat), usize::from(k));
                        if homes[k].seat != seat {
                            match homes.iter().position(|h| h.seat == seat) {
                                // Onto another controller's seat: they swap.
                                Some(j) => homes.swap(j, k),
                                None => homes[k].seat = seat,
                            }
                        }
                        homes[k].home = Home::Playing;
                        fx
                    }
                    Err(_) => Vec::new(),
                },
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
            if !ended_before {
                match op {
                    Op::TakeBack(Some(seat)) => {
                        for h in homes.iter_mut().filter(|h| h.seat == SeatId(seat)) {
                            h.home = Home::LeftEmpty;
                        }
                    }
                    Op::TakeBack(None) | Op::Panic => {
                        for h in homes.iter_mut().filter(|h| h.home == Home::Lent) {
                            h.home = Home::LeftEmpty;
                        }
                    }
                    _ => {}
                }
            }
            for effect in &fx {
                if let Effect::Grant { seat, member, .. } = effect
                    && *member != HOST
                {
                    for h in homes.iter_mut().filter(|h| h.seat == *seat) {
                        h.home = Home::Lent;
                    }
                }
            }

            for effect in &fx {
                if let Effect::Grant { seat, member, permissions, .. } = effect {
                    prop_assert!(!ended_before, "a grant after the end");
                    if *member == HOST {
                        prop_assert!(host_plays);
                        prop_assert_eq!(*permissions, Permissions::GAMEPAD, "the host's pad only");
                        prop_assert!(SEATS[usize::from(seat.0)].permissions().contains(Permissions::GAMEPAD));
                    } else {
                        prop_assert!(!paused_before, "a grant to a viewer while paused");
                        prop_assert!(s.is_viewer(*member), "a grant to a non-viewer");
                        prop_assert_eq!(*permissions, SEATS[usize::from(seat.0)].permissions());
                    }
                }
                if let Effect::Claim { seat, member } = effect {
                    prop_assert!(host_plays && *member == HOST, "only the host claims");
                    prop_assert!(!ended_before);
                    prop_assert!(!s.control_paused(), "a claim while paused");
                    let home = homes.iter().find(|h| h.seat == *seat);
                    prop_assert!(home.is_some(), "a claim for no controller's seat");
                    prop_assert!(
                        home.is_some_and(|h| h.home != Home::LeftEmpty),
                        "a claim for a seat the host left empty"
                    );
                }
                if let Effect::HostSeats(seats) = effect {
                    prop_assert_eq!(seats, &homes.iter().map(|h| h.seat).collect::<Vec<_>>());
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
            // A viewer that leaves forgets what it said.
            if let Op::Leave(i) = op {
                most_controllers.remove(&member(i));
            }
            prop_assert!(
                router.within_controllers(&most_controllers, max_host),
                "a member holds more seats than it may: {:?}, controllers {:?}",
                router.holders,
                most_controllers
            );
            host_held_two |= router.holders.iter().filter(|h| **h == Some(HOST)).count() >= 2;
            viewer_two |= (0..4).any(|i| {
                router.holders[..2].iter().filter(|h| **h == Some(member(i))).count() == 2
            });

            let viewers = s.viewers();
            prop_assert!(viewers.len() <= usize::from(s.config().max_viewers));
            let mut unique = viewers.to_vec();
            unique.sort();
            unique.dedup();
            prop_assert_eq!(unique.len(), viewers.len());
            for holder in s.holders().iter().flatten() {
                prop_assert!(
                    s.is_viewer(*holder) || (host_plays && *holder == HOST),
                    "a holder is a viewer or the host"
                );
            }
            if let ControlMessage::SessionStatus(status) = s.status() {
                prop_assert!(status.is_valid(), "{:?}", status);
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

        // Once every report has arrived, the mirror matches the router; only
        // viewers, in a session neither paused nor over, and the host, in a
        // session not over, hold seats; and each host controller is back on
        // its seat unless a viewer has it, the host took it back to leave it
        // empty, or control is paused.
        for _ in 0..256 {
            let Some((seat, holder)) = router.reports.pop_front() else { break };
            let fx = s.seat_changed(seat, holder);
            for (claimed, _) in claims(&fx) {
                prop_assert!(
                    homes.iter().any(|h| h.seat == claimed && h.home != Home::LeftEmpty),
                    "a late report refills a seat the host left empty, or no controller's"
                );
            }
            router.apply(&fx);
            prop_assert!(router.within_controllers(&most_controllers, max_host));
        }
        prop_assert!(router.reports.is_empty());
        prop_assert_eq!(&router.holders[..], s.holders());
        for holder in router.holders.iter().flatten() {
            if *holder == HOST {
                prop_assert!(!s.is_ended());
            } else {
                prop_assert!(s.is_viewer(*holder) && !s.control_paused() && !s.is_ended());
            }
        }
        prop_assert_eq!(&homes, &s.homes);
        if !s.is_ended() && !s.control_paused() {
            for h in homes.iter().filter(|h| h.home != Home::LeftEmpty) {
                prop_assert!(
                    router.holders[usize::from(h.seat.0)].is_some(),
                    "a host controller's seat is free but not its: {:?}",
                    router.holders
                );
            }
        }
        // To check the ops still reach the host on two seats (M23) and a
        // viewer on two pad seats (M18), make one of these fail for a
        // moment: `prop_assert!(!host_held_two)` or `!viewer_two` must fail.
        let _ = (host_held_two, viewer_two);
    }
}
