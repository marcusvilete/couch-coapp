//! The host's seat router: decides which remote input reaches the game.
//!
//! Sans-IO: every call takes the time and returns [`Effect`]s; the caller
//! injects them ([`apply`]) and sends the seat notices. Input reaches the game
//! only if all of these hold (docs/architecture.md, "Safety invariants"):
//! - it comes from the seat's holder, tagged with the seat's current epoch;
//! - the holder has the permission for that kind of input;
//! - for keyboard and mouse, the game window is in the foreground, and for a
//!   mouse button press, the pointer is over it too;
//! - it isn't a blocked key ([`crate::filter`]) or the Guide button. Blocked
//!   combinations count the modifiers injected for every seat: two holders
//!   could otherwise build Alt+Tab between them.
//!
//! The caller refreshes the foreground and pointer state right before every
//! `handle`, so a window that just took the foreground never gets input.
//!
//! Every change of holder, suspension, loss of focus and revocation releases
//! what was held first. Keys already down when control is granted stay
//! ignored until released, because only keys the router pressed are ever
//! released.

use std::{
    collections::{BTreeSet, VecDeque},
    time::{Duration, Instant},
};

use couch_proto::input::{
    HeldInput, KeyEvent, MAX_KEY_USAGE, MIN_KEY_USAGE, MouseButtonEvent, MouseDatagram,
    MouseTotals, PadDatagram,
};

use crate::{
    HidUsage, InputError, KbmInjector, MemberId, MouseButton, PadButtons, PadState, Permissions,
    SeatEpoch, SeatId, VirtualPad, filter,
};

/// A holder heard from nothing for this long is suspended: everything they
/// held is released. Viewers send a pad datagram every 100 ms.
pub const DEAD_MAN: Duration = Duration::from_millis(300);
/// A seat suspended this long, or granted and never heard from, returns to
/// the host.
pub const SUSPEND_LIMIT: Duration = Duration::from_secs(10);
/// This many suspensions within [`FLAP_WINDOW`] return the seat to the host;
/// the member needs a fresh grant.
pub const MAX_FLAPS: usize = 3;
pub const FLAP_WINDOW: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Key(HidUsage, bool),
    MouseMove(i32, i32),
    /// In 1/120 notches.
    Wheel(i32, i32),
    MouseButton(MouseButton, bool),
    Pad(SeatId, PadState),
    /// Tell `member` they hold `seat` from `epoch` on.
    Granted {
        seat: SeatId,
        epoch: SeatEpoch,
        member: MemberId,
        permissions: Permissions,
    },
    /// Tell `member` their hold on `seat`, which had `epoch`, ended.
    Revoked {
        seat: SeatId,
        epoch: SeatEpoch,
        member: MemberId,
    },
}

/// Input as it arrives from a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Key(KeyEvent),
    MouseButton(MouseButtonEvent),
    Held(HeldInput),
    Pad(PadDatagram),
    Mouse(MouseDatagram),
}

impl Input {
    fn target(&self) -> (SeatId, SeatEpoch) {
        match self {
            Self::Key(m) => (m.seat, m.seat_epoch),
            Self::MouseButton(m) => (m.seat, m.seat_epoch),
            Self::Held(m) => (m.seat, m.seat_epoch),
            Self::Pad(m) => (m.seat, m.seat_epoch),
            Self::Mouse(m) => (m.seat, m.seat_epoch),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatStatus {
    /// The host's own devices drive the game.
    Host,
    /// Granted, waiting for the holder's first input.
    Granted,
    Active,
    /// The holder went quiet; everything is released until they're back.
    Suspended,
}

struct Holder {
    member: MemberId,
    permissions: Permissions,
    granted_at: Instant,
    last_heard: Option<Instant>,
    suspended_since: Option<Instant>,
    flaps: VecDeque<Instant>,
    pad_seq: Option<u32>,
    pad: PadState,
    mouse: MouseTotals,
    /// What the router pressed for this holder and hasn't released.
    keys: BTreeSet<HidUsage>,
    buttons: BTreeSet<MouseButton>,
}

impl Holder {
    fn new(member: MemberId, permissions: Permissions, now: Instant) -> Self {
        Self {
            member,
            permissions,
            granted_at: now,
            last_heard: None,
            suspended_since: None,
            flaps: VecDeque::new(),
            pad_seq: None,
            pad: PadState::NEUTRAL,
            mouse: MouseTotals::default(),
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
        }
    }

    fn may(&self, permission: Permissions) -> bool {
        self.permissions.contains(permission)
    }

    /// Releases keys and mouse buttons.
    fn release_kbm(&mut self, effects: &mut Vec<Effect>) {
        for key in std::mem::take(&mut self.keys) {
            effects.push(Effect::Key(key, false));
        }
        for button in std::mem::take(&mut self.buttons) {
            effects.push(Effect::MouseButton(button, false));
        }
    }

    /// Releases everything, the pad included (always, to be safe).
    fn release_all(&mut self, seat: SeatId, effects: &mut Vec<Effect>) {
        self.release_kbm(effects);
        self.pad = PadState::NEUTRAL;
        effects.push(Effect::Pad(seat, PadState::NEUTRAL));
    }

    /// Applies newer motion totals; moves the pointer only if `allowed`.
    fn motion(&mut self, totals: MouseTotals, allowed: bool, effects: &mut Vec<Effect>) {
        // Versions wrap; newer means ahead by less than half the range.
        if (totals.version.wrapping_sub(self.mouse.version) as i32) <= 0 {
            return;
        }
        let old = std::mem::replace(&mut self.mouse, totals);
        if !allowed {
            return;
        }
        let (dx, dy) = (totals.x.wrapping_sub(old.x), totals.y.wrapping_sub(old.y));
        if (dx, dy) != (0, 0) {
            effects.push(Effect::MouseMove(dx, dy));
        }
        let (wx, wy) = (
            totals.wheel_x.wrapping_sub(old.wheel_x),
            totals.wheel_y.wrapping_sub(old.wheel_y),
        );
        if (wx, wy) != (0, 0) {
            effects.push(Effect::Wheel(wx, wy));
        }
    }
}

struct Seat {
    epoch: SeatEpoch,
    holder: Option<Holder>,
}

pub struct SeatRouter {
    seats: Vec<Seat>,
    /// Whether the game window is in the foreground. Keyboard and mouse are
    /// injected only then.
    foreground: bool,
    /// Whether the pointer is over the game window. A click anywhere else
    /// would reach another window (the taskbar, another app).
    pointer_inside: bool,
}

impl SeatRouter {
    /// `seats` seats, all held by the host, with the game not in front.
    pub fn new(seats: u8) -> Self {
        Self {
            seats: (0..seats)
                .map(|_| Seat {
                    epoch: SeatEpoch(0),
                    holder: None,
                })
                .collect(),
            foreground: false,
            pointer_inside: false,
        }
    }

    /// Like [`Self::new`], with each seat's epochs counting on from `epoch`.
    /// A session starts at a random one, so input left over from an earlier
    /// session on the same connection can't carry a current epoch.
    pub fn starting_at(seats: u8, epoch: SeatEpoch) -> Self {
        let mut router = Self::new(seats);
        for seat in &mut router.seats {
            seat.epoch = epoch;
        }
        router
    }

    /// Whether the pointer is over the game window. Mouse button presses go
    /// in only then; motion and releases always do (while in front), so the
    /// pointer can come back.
    pub fn set_pointer_inside(&mut self, inside: bool) {
        self.pointer_inside = inside;
    }

    /// The seat's epoch, holder and status.
    pub fn seat(&self, seat: SeatId) -> Option<(SeatEpoch, Option<MemberId>, SeatStatus)> {
        let s = self.seats.get(usize::from(seat.0))?;
        let status = match &s.holder {
            None => SeatStatus::Host,
            Some(h) if h.suspended_since.is_some() => SeatStatus::Suspended,
            Some(h) if h.last_heard.is_none() => SeatStatus::Granted,
            Some(_) => SeatStatus::Active,
        };
        Some((s.epoch, s.holder.as_ref().map(|h| h.member), status))
    }

    /// Hands `seat` to `member`, ending any current hold first.
    pub fn grant(
        &mut self,
        seat: SeatId,
        member: MemberId,
        permissions: Permissions,
        now: Instant,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        let Some(s) = self.seats.get_mut(usize::from(seat.0)) else {
            return effects;
        };
        end_hold(seat, s, &mut effects);
        s.epoch = s.epoch.next();
        s.holder = Some(Holder::new(member, permissions, now));
        effects.push(Effect::Granted {
            seat,
            epoch: s.epoch,
            member,
            permissions,
        });
        effects
    }

    /// Gives `seat` back to the host.
    pub fn revoke(&mut self, seat: SeatId) -> Vec<Effect> {
        let mut effects = Vec::new();
        if let Some(s) = self.seats.get_mut(usize::from(seat.0)) {
            end_hold(seat, s, &mut effects);
        }
        effects
    }

    /// Gives every seat back to the host (the panic key, teardown).
    pub fn revoke_all(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        for (i, s) in self.seats.iter_mut().enumerate() {
            end_hold(SeatId(i as u8), s, &mut effects);
        }
        effects
    }

    /// `member` disconnected: their seats return to the host.
    pub fn disconnected(&mut self, member: MemberId) -> Vec<Effect> {
        let mut effects = Vec::new();
        for (i, s) in self.seats.iter_mut().enumerate() {
            if s.holder.as_ref().is_some_and(|h| h.member == member) {
                end_hold(SeatId(i as u8), s, &mut effects);
            }
        }
        effects
    }

    /// The game window gained or lost the foreground. Losing it releases
    /// every key and mouse button; the pads are unaffected.
    pub fn set_foreground(&mut self, foreground: bool) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self.foreground && !foreground {
            for h in self.seats.iter_mut().filter_map(|s| s.holder.as_mut()) {
                h.release_kbm(&mut effects);
            }
        }
        self.foreground = foreground;
        effects
    }

    pub fn handle(&mut self, from: MemberId, input: Input, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        let (seat, epoch) = input.target();
        // Modifiers held for any seat count toward blocked combinations.
        let all_keys: BTreeSet<HidUsage> = match input {
            Input::Key(_) => self
                .seats
                .iter()
                .filter_map(|s| s.holder.as_ref())
                .flat_map(|h| h.keys.iter().copied())
                .collect(),
            _ => BTreeSet::new(),
        };
        let pointer_inside = self.pointer_inside;
        let Some(s) = self.seats.get_mut(usize::from(seat.0)) else {
            return effects;
        };
        let Some(h) = s.holder.as_mut() else {
            return effects;
        };
        if h.member != from || epoch != s.epoch {
            return effects;
        }
        h.last_heard = Some(now);
        h.suspended_since = None;
        let kbm = self.foreground;

        match input {
            Input::Pad(d) => {
                if h.pad_seq.is_some_and(|last| d.seq <= last) {
                    return effects;
                }
                h.pad_seq = Some(d.seq);
                if h.may(Permissions::GAMEPAD) {
                    let mut state = PadState::from(d.pad);
                    state.buttons.remove(PadButtons::GUIDE);
                    if state != h.pad {
                        h.pad = state;
                        effects.push(Effect::Pad(seat, state));
                    }
                }
            }
            Input::Mouse(d) => {
                let allowed = kbm && h.may(Permissions::MOUSE);
                h.motion(d.totals, allowed, &mut effects);
            }
            Input::MouseButton(e) => {
                let allowed = kbm && h.may(Permissions::MOUSE);
                h.motion(e.totals, allowed, &mut effects);
                let Some(button) = MouseButton::from_code(e.button) else {
                    return effects;
                };
                // A release only ever lets go of what the router pressed.
                if !e.down {
                    if h.buttons.remove(&button) {
                        effects.push(Effect::MouseButton(button, false));
                    }
                } else if allowed && pointer_inside && h.buttons.insert(button) {
                    effects.push(Effect::MouseButton(button, true));
                }
            }
            Input::Key(e) => {
                if !(MIN_KEY_USAGE..=MAX_KEY_USAGE).contains(&e.usage) {
                    return effects;
                }
                let key = HidUsage(e.usage);
                if !e.down {
                    if h.keys.remove(&key) {
                        effects.push(Effect::Key(key, false));
                    }
                } else if kbm
                    && h.may(Permissions::KEYBOARD)
                    && !h.keys.contains(&key)
                    && filter::key_allowed(key, &all_keys)
                {
                    h.keys.insert(key);
                    effects.push(Effect::Key(key, true));
                }
            }
            Input::Held(held) => {
                let stale: Vec<HidUsage> = h
                    .keys
                    .iter()
                    .copied()
                    .filter(|k| !held.keys.contains(k.0))
                    .collect();
                for key in stale {
                    h.keys.remove(&key);
                    effects.push(Effect::Key(key, false));
                }
                let stale: Vec<MouseButton> = h
                    .buttons
                    .iter()
                    .copied()
                    .filter(|b| held.buttons & b.bit() == 0)
                    .collect();
                for button in stale {
                    h.buttons.remove(&button);
                    effects.push(Effect::MouseButton(button, false));
                }
            }
        }
        effects
    }

    /// Runs the dead-man timer and suspension limits. Call it often (every
    /// 20 ms or so); releases happen at most that late.
    pub fn tick(&mut self, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        for (i, s) in self.seats.iter_mut().enumerate() {
            let seat = SeatId(i as u8);
            let Some(h) = s.holder.as_mut() else {
                continue;
            };
            let end = match (h.last_heard, h.suspended_since) {
                (None, _) => now.duration_since(h.granted_at) >= SUSPEND_LIMIT,
                (Some(_), Some(since)) => now.duration_since(since) >= SUSPEND_LIMIT,
                (Some(heard), None) if now.duration_since(heard) >= DEAD_MAN => {
                    h.release_all(seat, &mut effects);
                    h.suspended_since = Some(now);
                    while h
                        .flaps
                        .front()
                        .is_some_and(|&t| now.duration_since(t) >= FLAP_WINDOW)
                    {
                        h.flaps.pop_front();
                    }
                    h.flaps.push_back(now);
                    h.flaps.len() >= MAX_FLAPS
                }
                (Some(_), None) => false,
            };
            if end {
                end_hold(seat, s, &mut effects);
            }
        }
        effects
    }
}

/// Releases everything the holder had and tells them the hold ended.
fn end_hold(seat: SeatId, s: &mut Seat, effects: &mut Vec<Effect>) {
    if let Some(mut h) = s.holder.take() {
        h.release_all(seat, effects);
        effects.push(Effect::Revoked {
            seat,
            epoch: s.epoch,
            member: h.member,
        });
    }
}

/// Carries out the injection effects; seat notices are the caller's. Keeps
/// going after a failure, so one failed key-up can't leave the rest held, and
/// returns the first error. Pad effects for seats without a pad are dropped.
pub fn apply(
    effects: &[Effect],
    kbm: &mut dyn KbmInjector,
    pads: &mut [Box<dyn VirtualPad>],
) -> Result<(), InputError> {
    let mut first_error = None;
    for effect in effects {
        let result = match *effect {
            Effect::Key(key, down) => kbm.key(key, down),
            Effect::MouseMove(dx, dy) => kbm.mouse_move(dx, dy),
            Effect::Wheel(dx, dy) => kbm.wheel(dx, dy),
            Effect::MouseButton(button, down) => kbm.mouse_button(button, down),
            Effect::Pad(seat, state) => match pads.get_mut(usize::from(seat.0)) {
                Some(pad) => pad.submit(&state),
                None => Ok(()),
            },
            Effect::Granted { .. } | Effect::Revoked { .. } => Ok(()),
        };
        if let Err(e) = result {
            first_error.get_or_insert(e);
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use couch_proto::input::{KeyBitmap, PadSnapshot, mouse_button};

    use super::*;
    use crate::filter::{LEFT_ALT, LEFT_GUI, TAB};

    const SEAT: SeatId = SeatId(0);
    const GUEST: MemberId = MemberId([1; 32]);
    const OTHER: MemberId = MemberId([2; 32]);
    const ALL: Permissions = Permissions::all();
    const W: u16 = 0x1A;

    struct Fixture {
        router: SeatRouter,
        t0: Instant,
        epoch: SeatEpoch,
    }

    impl Fixture {
        /// The guest holds seat 0 with `permissions`, and the game is in front.
        fn new(permissions: Permissions) -> Self {
            let t0 = Instant::now();
            let mut router = SeatRouter::new(1);
            router.set_foreground(true);
            router.set_pointer_inside(true);
            router.grant(SEAT, GUEST, permissions, t0);
            let epoch = router.seat(SEAT).unwrap().0;
            Self { router, t0, epoch }
        }

        fn at(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }

        fn send(&mut self, input: Input, ms: u64) -> Vec<Effect> {
            let now = self.at(ms);
            self.router.handle(GUEST, input, now)
        }

        fn key(&self, usage: u16, down: bool) -> Input {
            Input::Key(KeyEvent {
                seat: SEAT,
                seat_epoch: self.epoch,
                usage,
                down,
            })
        }

        fn pad(&self, seq: u32, buttons: PadButtons) -> Input {
            Input::Pad(PadDatagram {
                seat: SEAT,
                seat_epoch: self.epoch,
                seq,
                pad: PadSnapshot {
                    buttons: buttons.bits(),
                    ..Default::default()
                },
            })
        }

        fn motion(&self, version: u32, x: i32, y: i32) -> MouseTotals {
            MouseTotals {
                version,
                x,
                y,
                ..Default::default()
            }
        }
    }

    fn pressed(state: PadButtons) -> PadState {
        PadState {
            buttons: state,
            ..PadState::NEUTRAL
        }
    }

    #[test]
    fn keys_from_the_holder_go_through() {
        let mut f = Fixture::new(ALL);
        assert_eq!(
            f.send(f.key(W, true), 10),
            vec![Effect::Key(HidUsage(W), true)]
        );
        // A repeated down presses nothing more; the up releases.
        assert_eq!(f.send(f.key(W, true), 20), vec![]);
        assert_eq!(
            f.send(f.key(W, false), 30),
            vec![Effect::Key(HidUsage(W), false)]
        );
    }

    #[test]
    fn guests_by_default_drive_only_the_pad() {
        let mut f = Fixture::new(Permissions::GUEST_DEFAULT);
        assert_eq!(f.send(f.key(W, true), 10), vec![]);
        let click = Input::MouseButton(MouseButtonEvent {
            seat: SEAT,
            seat_epoch: f.epoch,
            button: mouse_button::LEFT,
            down: true,
            totals: f.motion(1, 5, 5),
        });
        assert_eq!(f.send(click, 20), vec![]);
        assert_eq!(
            f.send(f.pad(1, PadButtons::A), 30),
            vec![Effect::Pad(SEAT, pressed(PadButtons::A))]
        );
    }

    #[test]
    fn input_from_anyone_else_or_an_old_epoch_is_dropped() {
        let mut f = Fixture::new(ALL);
        let now = f.at(10);
        assert_eq!(f.router.handle(OTHER, f.key(W, true), now), vec![]);
        let stale = Input::Key(KeyEvent {
            seat: SEAT,
            seat_epoch: SeatEpoch(f.epoch.0 - 1),
            usage: W,
            down: true,
        });
        assert_eq!(f.send(stale, 20), vec![]);
        // A seat that doesn't exist.
        let elsewhere = Input::Key(KeyEvent {
            seat: SeatId(3),
            seat_epoch: f.epoch,
            usage: W,
            down: true,
        });
        assert_eq!(f.send(elsewhere, 30), vec![]);
    }

    #[test]
    fn a_session_counts_epochs_on_from_its_own_start() {
        let now = Instant::now();
        let mut router = SeatRouter::starting_at(1, SeatEpoch(41));
        router.set_foreground(true);
        let effects = router.grant(SEAT, GUEST, ALL, now);
        assert!(effects.contains(&Effect::Granted {
            seat: SEAT,
            epoch: SeatEpoch(42),
            member: GUEST,
            permissions: ALL,
        }));
        // Input tagged with a fresh router's first epoch, as an earlier
        // session would have used, is dropped.
        let earlier = Input::Key(KeyEvent {
            seat: SEAT,
            seat_epoch: SeatEpoch(1),
            usage: W,
            down: true,
        });
        assert_eq!(router.handle(GUEST, earlier, now), vec![]);
    }

    #[test]
    fn blocked_keys_and_the_guide_button_never_reach_the_game() {
        let mut f = Fixture::new(ALL);
        assert_eq!(f.send(f.key(LEFT_GUI.0, true), 10), vec![]);
        assert_eq!(
            f.send(f.key(LEFT_ALT.0, true), 20),
            vec![Effect::Key(LEFT_ALT, true)]
        );
        assert_eq!(f.send(f.key(TAB.0, true), 30), vec![]);
        // Its release is swallowed too: Tab was never pressed.
        assert_eq!(f.send(f.key(TAB.0, false), 40), vec![]);
        assert_eq!(
            f.send(f.pad(1, PadButtons::GUIDE | PadButtons::B), 50),
            vec![Effect::Pad(SEAT, pressed(PadButtons::B))]
        );
        // Out-of-range usages are ignored.
        assert_eq!(f.send(f.key(0x01, true), 60), vec![]);
        assert_eq!(f.send(f.key(0xE8, true), 70), vec![]);
    }

    #[test]
    fn two_holders_cant_build_a_blocked_combination() {
        let t0 = Instant::now();
        let mut router = SeatRouter::new(2);
        router.set_foreground(true);
        router.grant(SEAT, GUEST, ALL, t0);
        router.grant(SeatId(1), OTHER, ALL, t0);
        let key = |seat: SeatId, usage: u16| {
            Input::Key(KeyEvent {
                seat,
                seat_epoch: SeatEpoch(1),
                usage,
                down: true,
            })
        };
        assert_eq!(
            router.handle(GUEST, key(SEAT, LEFT_ALT.0), t0),
            vec![Effect::Key(LEFT_ALT, true)]
        );
        assert_eq!(router.handle(OTHER, key(SeatId(1), TAB.0), t0), vec![]);
    }

    #[test]
    fn clicks_need_the_pointer_over_the_game() {
        let mut f = Fixture::new(ALL);
        let epoch = f.epoch;
        let click = |down, version| {
            Input::MouseButton(MouseButtonEvent {
                seat: SEAT,
                seat_epoch: epoch,
                button: mouse_button::LEFT,
                down,
                totals: MouseTotals {
                    version,
                    x: version as i32,
                    ..Default::default()
                },
            })
        };
        f.router.set_pointer_inside(false);
        // The pointer still moves (it can come back), but the press is dropped.
        assert_eq!(f.send(click(true, 1), 10), vec![Effect::MouseMove(1, 0)]);
        assert_eq!(f.send(click(false, 1), 20), vec![]);
        f.router.set_pointer_inside(true);
        assert_eq!(
            f.send(click(true, 1), 30),
            vec![Effect::MouseButton(MouseButton::Left, true)]
        );
        // Once pressed, the release goes through wherever the pointer is.
        f.router.set_pointer_inside(false);
        assert_eq!(
            f.send(click(false, 1), 40),
            vec![Effect::MouseButton(MouseButton::Left, false)]
        );
    }

    #[test]
    fn keys_down_before_the_grant_stay_ignored_until_released() {
        let mut f = Fixture::new(ALL);
        // The holder was already holding W when the seat arrived: only its
        // release comes through, and it releases nothing.
        assert_eq!(f.send(f.key(W, false), 10), vec![]);
        assert_eq!(
            f.send(f.key(W, true), 20),
            vec![Effect::Key(HidUsage(W), true)]
        );
    }

    #[test]
    fn keyboard_and_mouse_need_the_game_in_front() {
        let mut f = Fixture::new(ALL);
        f.send(f.key(W, true), 10);
        assert_eq!(
            f.router.set_foreground(false),
            vec![Effect::Key(HidUsage(W), false)]
        );
        assert_eq!(f.send(f.key(0x04, true), 20), vec![]);
        let moved = Input::Mouse(MouseDatagram {
            seat: SEAT,
            seat_epoch: f.epoch,
            totals: f.motion(1, 100, 0),
        });
        assert_eq!(f.send(moved, 30), vec![]);
        // The pad still works.
        assert_eq!(
            f.send(f.pad(1, PadButtons::X), 40),
            vec![Effect::Pad(SEAT, pressed(PadButtons::X))]
        );
        // Back in front: motion from the new baseline only, no jump.
        f.router.set_foreground(true);
        let moved = Input::Mouse(MouseDatagram {
            seat: SEAT,
            seat_epoch: f.epoch,
            totals: f.motion(2, 103, -1),
        });
        assert_eq!(f.send(moved, 50), vec![Effect::MouseMove(3, -1)]);
    }

    #[test]
    fn mouse_totals_apply_once_and_in_order() {
        let mut f = Fixture::new(ALL);
        let epoch = f.epoch;
        let datagram = |v, x, y| {
            Input::Mouse(MouseDatagram {
                seat: SEAT,
                seat_epoch: epoch,
                totals: MouseTotals {
                    version: v,
                    x,
                    y,
                    wheel_x: 0,
                    wheel_y: if v == 3 { -120 } else { 0 },
                },
            })
        };
        assert_eq!(
            f.send(datagram(1, 10, 5), 10),
            vec![Effect::MouseMove(10, 5)]
        );
        // A lost datagram's motion arrives with the next one.
        assert_eq!(
            f.send(datagram(3, 25, 5), 20),
            vec![Effect::MouseMove(15, 0), Effect::Wheel(0, -120)]
        );
        // Late or repeated totals do nothing.
        assert_eq!(f.send(datagram(2, 18, 5), 30), vec![]);
        assert_eq!(f.send(datagram(3, 25, 5), 40), vec![]);
        // A click carries newer totals: the pointer moves before the press.
        let click = Input::MouseButton(MouseButtonEvent {
            seat: SEAT,
            seat_epoch: f.epoch,
            button: mouse_button::RIGHT,
            down: true,
            totals: MouseTotals {
                version: 4,
                x: 20,
                y: 5,
                wheel_x: 0,
                wheel_y: -120,
            },
        });
        assert_eq!(
            f.send(click, 50),
            vec![
                Effect::MouseMove(-5, 0),
                Effect::MouseButton(MouseButton::Right, true)
            ]
        );
    }

    #[test]
    fn totals_and_versions_wrap_around() {
        let mut f = Fixture::new(ALL);
        let epoch = f.epoch;
        let at = |version, x| {
            Input::Mouse(MouseDatagram {
                seat: SEAT,
                seat_epoch: epoch,
                totals: MouseTotals {
                    version,
                    x,
                    ..Default::default()
                },
            })
        };
        f.send(at(0x7FFF_FFFF, i32::MAX - 1), 10);
        assert_eq!(
            f.send(at(0x8000_0000, i32::MIN + 2), 20),
            vec![Effect::MouseMove(4, 0)]
        );
        f.send(at(u32::MAX, 0), 30);
        assert_eq!(f.send(at(0, 7), 40), vec![Effect::MouseMove(7, 0)]);
    }

    #[test]
    fn old_pad_datagrams_are_dropped() {
        let mut f = Fixture::new(ALL);
        f.send(f.pad(5, PadButtons::A), 10);
        assert_eq!(f.send(f.pad(4, PadButtons::empty()), 20), vec![]);
        assert_eq!(
            f.send(f.pad(6, PadButtons::empty()), 30),
            vec![Effect::Pad(SEAT, PadState::NEUTRAL)]
        );
    }

    #[test]
    fn the_held_sync_releases_but_never_presses() {
        let mut f = Fixture::new(ALL);
        f.send(f.key(W, true), 10);
        f.send(f.key(0x04, true), 20);
        let mut keys = KeyBitmap::default();
        keys.set(W, true);
        keys.set(0x05, true);
        let sync = Input::Held(HeldInput {
            seat: SEAT,
            seat_epoch: f.epoch,
            keys,
            buttons: 0,
        });
        assert_eq!(f.send(sync, 30), vec![Effect::Key(HidUsage(0x04), false)]);
    }

    #[test]
    fn a_quiet_holder_is_released_within_the_dead_man_time() {
        let mut f = Fixture::new(ALL);
        f.send(f.pad(1, PadButtons::A), 0);
        f.send(f.key(W, true), 0);
        assert_eq!(f.router.tick(f.at(299)), vec![]);
        assert_eq!(
            f.router.tick(f.at(300)),
            vec![
                Effect::Key(HidUsage(W), false),
                Effect::Pad(SEAT, PadState::NEUTRAL)
            ]
        );
        assert_eq!(f.router.seat(SEAT).unwrap().2, SeatStatus::Suspended);
        // Back: the seat is active again, and the pad state reapplies.
        assert_eq!(
            f.send(f.pad(2, PadButtons::A), 400),
            vec![Effect::Pad(SEAT, pressed(PadButtons::A))]
        );
        assert_eq!(f.router.seat(SEAT).unwrap().2, SeatStatus::Active);
    }

    #[test]
    fn a_long_suspension_returns_the_seat_to_the_host() {
        let mut f = Fixture::new(ALL);
        f.send(f.pad(1, PadButtons::empty()), 0);
        f.router.tick(f.at(300));
        assert_eq!(f.router.tick(f.at(10_299)), vec![]);
        assert_eq!(
            f.router.tick(f.at(10_300)),
            vec![
                Effect::Pad(SEAT, PadState::NEUTRAL),
                Effect::Revoked {
                    seat: SEAT,
                    epoch: f.epoch,
                    member: GUEST
                }
            ]
        );
        assert_eq!(f.router.seat(SEAT).unwrap().2, SeatStatus::Host);
        assert_eq!(f.send(f.pad(2, PadButtons::A), 10_400), vec![]);
    }

    #[test]
    fn a_grant_never_heard_from_lapses() {
        let f = Fixture::new(ALL);
        let mut router = f.router;
        // No dead-man before the first input: the grant is still on its way.
        assert_eq!(router.tick(f.t0 + Duration::from_secs(5)), vec![]);
        let effects = router.tick(f.t0 + SUSPEND_LIMIT);
        assert!(matches!(effects.last(), Some(Effect::Revoked { .. })));
    }

    #[test]
    fn the_third_flap_needs_a_fresh_grant() {
        let mut f = Fixture::new(ALL);
        let mut t = 0;
        for flap in 1..=MAX_FLAPS {
            f.send(f.pad(flap as u32, PadButtons::empty()), t);
            t += 300;
            let effects = f.router.tick(f.at(t));
            let revoked = matches!(effects.last(), Some(Effect::Revoked { .. }));
            assert_eq!(revoked, flap == MAX_FLAPS, "flap {flap}");
            t += 100;
        }
    }

    #[test]
    fn flaps_older_than_the_window_are_forgotten() {
        let mut f = Fixture::new(ALL);
        let mut seq = 0;
        for start in [0, 1_000, 400_000] {
            seq += 1;
            f.send(f.pad(seq, PadButtons::empty()), start);
            f.router.tick(f.at(start + 300));
        }
        assert_eq!(f.router.seat(SEAT).unwrap().2, SeatStatus::Suspended);
    }

    #[test]
    fn a_new_holder_starts_clean_and_old_input_is_dropped() {
        let mut f = Fixture::new(ALL);
        f.send(f.key(W, true), 10);
        f.send(f.pad(1, PadButtons::A), 10);
        let old_epoch = f.epoch;
        let effects = f.router.grant(SEAT, OTHER, Permissions::GAMEPAD, f.at(20));
        let new_epoch = old_epoch.next();
        assert_eq!(
            effects,
            vec![
                Effect::Key(HidUsage(W), false),
                Effect::Pad(SEAT, PadState::NEUTRAL),
                Effect::Revoked {
                    seat: SEAT,
                    epoch: old_epoch,
                    member: GUEST
                },
                Effect::Granted {
                    seat: SEAT,
                    epoch: new_epoch,
                    member: OTHER,
                    permissions: Permissions::GAMEPAD
                },
            ]
        );
        assert_eq!(f.send(f.key(W, true), 30), vec![]);
    }

    #[test]
    fn disconnect_and_revoke_all_release_everything() {
        let mut f = Fixture::new(ALL);
        f.send(f.key(W, true), 10);
        let effects = f.router.disconnected(GUEST);
        assert_eq!(effects[0], Effect::Key(HidUsage(W), false));
        assert_eq!(f.router.seat(SEAT).unwrap().2, SeatStatus::Host);
        assert_eq!(f.router.revoke_all(), vec![]);
    }

    #[derive(Default)]
    struct Log(Vec<String>);

    impl KbmInjector for Log {
        fn key(&mut self, key: HidUsage, down: bool) -> Result<(), InputError> {
            if key.0 == 0x04 {
                return Err(InputError::Injection("refused".into()));
            }
            self.0.push(format!("key {:#x} {down}", key.0));
            Ok(())
        }
        fn mouse_move(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
            self.0.push(format!("move {dx} {dy}"));
            Ok(())
        }
        fn mouse_button(&mut self, button: MouseButton, down: bool) -> Result<(), InputError> {
            self.0.push(format!("{button:?} {down}"));
            Ok(())
        }
        fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
            self.0.push(format!("wheel {dx} {dy}"));
            Ok(())
        }
    }

    #[test]
    fn apply_keeps_releasing_after_a_failure() {
        let mut log = Log::default();
        let effects = [
            Effect::Key(HidUsage(0x04), false),
            Effect::Key(HidUsage(W), false),
            Effect::Pad(SEAT, PadState::NEUTRAL),
            Effect::MouseButton(MouseButton::Left, false),
        ];
        assert!(apply(&effects, &mut log, &mut []).is_err());
        assert_eq!(log.0, vec!["key 0x1a false", "Left false"]);
    }
}
