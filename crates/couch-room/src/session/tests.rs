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

// ---- The host on a seat ----

const HOST: MemberId = MemberId([9; 32]);

fn hosted(seats: &[SeatKind]) -> HostSession {
    HostSession::new(
        SessionId(1),
        SessionConfig {
            seats: seats.to_vec(),
            host: Some(HOST),
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
        vec![Effect::Claim {
            seat: S1,
            member: HOST
        }]
    );

    let mut s = hosted(&[SeatKind::Controller, SeatKind::Controller]);
    assert_eq!(claims(&s.start()), vec![(S0, HOST)]);
    let fx = s.seat_changed(S0, Some(HOST));
    assert!(broadcasts_status(&fx));
    assert_eq!(s.holders(), &[Some(HOST), None]);
    assert_eq!(s.host_seat(), Some(S0));
    // On the wire, the host's seat is apart from the viewers'.
    let status = status_of(&s);
    assert_eq!(status.holders, vec![None, None]);
    assert_eq!(status.host_seat, Some(S0));
    assert!(status.is_valid());

    // Nothing to play with a keyboard seat alone, and nothing without a host.
    let s = hosted(&[SeatKind::KeyboardMouse]);
    assert!(s.start().is_empty());
    assert_eq!(s.host(), None);
    let s = with_seats(&[SeatKind::Controller]);
    assert!(s.start().is_empty());
    assert_eq!(status_of(&s).host_seat, None);
}

#[test]
fn the_hosts_seat_comes_back_when_a_viewer_lets_go() {
    let now = Instant::now();
    let mut s = hosted(&[SeatKind::Both]);
    s.start();
    s.seat_changed(S0, Some(HOST));
    s.join(ANA);
    s.request(ANA, S0, now);
    let fx = s.answer(ANA, true, now).unwrap();
    assert_eq!(grant_of(&fx), Some((S0, ANA, SeatKind::Both.permissions())));
    // The router ends the host's hold first. The claim this causes reaches
    // it after the grant, so it finds the seat taken and does nothing.
    let fx = s.seat_changed(S0, None);
    assert_eq!(claims(&fx), vec![(S0, HOST)]);
    s.seat_changed(S0, Some(ANA));
    assert_eq!(s.host_seat(), None);
    assert_eq!(status_of(&s).host_seat, None);

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
    s.play(S0).unwrap();
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
    assert_eq!(status.host_seat, None);
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
        grant_of(&s.play(S0).unwrap()),
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

    assert_eq!(s.play(SeatId(2)).unwrap_err(), SessionError::NoPad);
    assert_eq!(s.play(SeatId(3)).unwrap_err(), SessionError::NoSuchSeat);
    assert!(s.play(S0).unwrap().is_empty(), "already there");
    let fx = s.play(S1).unwrap();
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
    assert_eq!(s.play(S0).unwrap_err(), SessionError::HostNotOnASeat);
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
    assert!(s.play(S1).is_ok());
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
    TakeBack(Option<u8>),
    /// The host moves to a seat.
    Play(u8),
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
        5 => (who, seat.clone()).prop_map(|(w, s)| Op::Pass(w, s)),
        1 => proptest::option::of(seat.clone()).prop_map(Op::TakeBack),
        1 => seat.clone().prop_map(Op::Play),
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

    /// At any moment (M18): the host holds one seat at most; a viewer holds
    /// at most one seat with a pad per controller it said it had (one with
    /// none: a move), and at most the one keyboard-and-mouse seat beside.
    fn within_controllers(&self, controllers: &BTreeMap<MemberId, u8>) -> bool {
        let mut members: Vec<MemberId> = self.holders.iter().flatten().copied().collect();
        members.sort();
        members.dedup();
        members.into_iter().all(|m| {
            let seats: Vec<usize> = (0..self.holders.len())
                .filter(|i| self.holders[*i] == Some(m))
                .collect();
            if m == HOST {
                return seats.len() <= 1;
            }
            let pads = seats
                .iter()
                .filter(|i| SEATS[**i].permissions().contains(Permissions::GAMEPAD))
                .count();
            let allowed = usize::from(controllers.get(&m).copied().unwrap_or(0).max(1));
            pads <= allowed && seats.len() - pads <= 1
        })
    }
}

fn member(i: u8) -> MemberId {
    MemberId([i + 1; 32])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn only_viewers_and_the_host_play_within_their_controllers(
        host_plays in any::<bool>(),
        ops in proptest::collection::vec(op(), 1..100),
    ) {
        let t0 = Instant::now();
        let mut now = t0;
        let mut s = HostSession::new(
            SessionId(9),
            SessionConfig {
                seats: SEATS.to_vec(),
                host: host_plays.then_some(HOST),
                ..config(3)
            },
        )
        .unwrap();
        let mut router = Router::new();
        let mut declined_at: BTreeMap<MemberId, Instant> = BTreeMap::new();
        // The most controllers each viewer said it has, this time it watched.
        let mut most_controllers: BTreeMap<MemberId, u8> = BTreeMap::new();
        let fx = s.start();
        prop_assert_eq!(fx.len(), usize::from(host_plays));
        router.apply(&fx);
        // What the host did with their seat, from the ops alone: the seat
        // they last played, whether they passed it on, and whether they
        // took it back to leave it empty.
        let mut home = host_plays.then_some(S0);
        let mut lent = false;
        let mut left_empty = false;

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
                Op::TakeBack(seat) => s.take_back(seat.map(SeatId)),
                Op::Play(seat) => match s.play(SeatId(seat)) {
                    // Empty when the mirror shows the host on it already.
                    Ok(fx) => {
                        if !fx.is_empty() || home == Some(SeatId(seat)) {
                            (home, lent, left_empty) = (Some(SeatId(seat)), false, false);
                        }
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
                    Op::TakeBack(Some(seat)) if home == Some(SeatId(seat)) => {
                        (lent, left_empty) = (false, true);
                    }
                    Op::TakeBack(None) | Op::Panic if lent => (lent, left_empty) = (false, true),
                    _ => {}
                }
            }
            let passed_home = fx.iter().any(|e| {
                matches!(e, Effect::Grant { seat, member, .. } if *member != HOST && Some(*seat) == home)
            });
            if passed_home {
                (lent, left_empty) = (true, false);
            }

            for effect in &fx {
                if let Effect::Grant { seat, member, permissions } = effect {
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
                if let Effect::Claim { member, .. } = effect {
                    prop_assert!(host_plays && *member == HOST, "only the host claims");
                    prop_assert!(!ended_before);
                    prop_assert!(!s.control_paused(), "a claim while paused");
                    prop_assert!(!left_empty, "a claim for a seat the host left empty");
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
                router.within_controllers(&most_controllers),
                "a member holds more seats than it may: {:?}, controllers {:?}",
                router.holders,
                most_controllers
            );

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
        // session not over, hold seats; and the host is back on their seat
        // unless a viewer has it, they took it back to leave it empty, or
        // control is paused.
        for _ in 0..256 {
            let Some((seat, holder)) = router.reports.pop_front() else { break };
            let fx = s.seat_changed(seat, holder);
            prop_assert!(
                !left_empty || claims(&fx).is_empty(),
                "a late report refills a seat the host left empty"
            );
            router.apply(&fx);
            prop_assert!(router.within_controllers(&most_controllers));
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
        prop_assert_eq!(home, s.host_home);
        if host_plays && !s.is_ended() && s.host_seat().is_none() && !left_empty && !s.control_paused() {
            let home = usize::from(home.unwrap().0);
            prop_assert!(
                router.holders[home].is_some(),
                "the host's seat is free but not theirs: {:?}",
                router.holders
            );
        }
    }
}
