//! A viewer's side of input: turns local events from `couch-viewer` into
//! what goes to the host, while this member holds seats.
//!
//! Sans-IO, like the router. It keeps the running mouse totals, numbers the
//! pad datagrams per seat, and on [`Forwarder::tick`] repeats each held
//! seat's pad state every [`PAD_REFRESH`] (the holder's heartbeat) and sends
//! what is held every [`HELD_SYNC`].
//!
//! **Several seats** (M18). A member may hold several seats: one with a pad
//! for each of its controllers, and the keyboard-and-mouse seat. Keys and the
//! mouse go to the seat that allows them. Which controller drives which pad
//! seat is decided here alone, and the stream window is told
//! ([`Forwarder::seat_state`]) for its rumble and HUD:
//! - **one pad seat:** the controller touched last drives it (any of them, as
//!   before M18), and it stays when a controller goes;
//! - **several:** each seat is pinned to a controller, by its slot (a
//!   controller keeps its slot while connected). A new seat takes the lowest
//!   free controller; when a second seat comes, the first keeps the
//!   controller driving it. A controller that goes leaves its seat: neutral
//!   at once, and [`Forwarder::take_left`] names it to give back. The others
//!   stay put, and a pin stays while its controller is connected, even once
//!   one seat is left: only a seat never pinned, or whose controller went,
//!   is driven by the controller touched last.
//!
//! The heartbeat stands for the viewer, not this process: it only goes out
//! while the viewer has been heard from within [`VIEWER_QUIET`]
//! ([`LocalInput::Alive`] comes every 100 ms). A hung viewer stops it, and the
//! host's dead-man timer releases whatever was held.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use couch_proto::{
    input::{
        HeldInput, InputMessage, KeyBitmap, KeyEvent, MAX_KEY_USAGE, MIN_KEY_USAGE,
        MouseButtonEvent, MouseDatagram, MouseTotals, PadDatagram, PadSnapshot,
    },
    ipc::{HeldSeat, LocalInput, SeatState},
};

use crate::{MouseButton, Permissions, SeatEpoch, SeatId};

/// How often the pad state goes out unchanged. Well inside the router's
/// 300 ms dead-man time, so two lost datagrams in a row are harmless.
pub const PAD_REFRESH: Duration = Duration::from_millis(100);
pub const HELD_SYNC: Duration = Duration::from_secs(1);
/// A viewer silent this long counts as hung: nothing more goes out for it.
pub const VIEWER_QUIET: Duration = Duration::from_millis(250);
/// Local controller slots (the bits of [`LocalInput::ControllerSlots`]); a
/// controller beyond them is ignored.
pub const MAX_LOCAL_CONTROLLERS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outgoing {
    /// On the input stream.
    Stream(InputMessage),
    Pad(PadDatagram),
    Mouse(MouseDatagram),
}

#[derive(Debug, Clone, Copy)]
struct Hold {
    epoch: SeatEpoch,
    permissions: Permissions,
    pad_seq: u32,
    /// The pad state last sent for this seat.
    sent: PadSnapshot,
    next_refresh: Option<Instant>,
    /// With several pad seats: the controller slot pinned to drive it.
    driver: Option<u8>,
    /// Its controller went, with several pad seats: neutral, and given back;
    /// it drives nothing until the host releases it.
    leaving: bool,
}

#[derive(Debug, Default)]
pub struct Forwarder {
    holds: BTreeMap<SeatId, Hold>,
    /// Each controller slot's state; `None` while nothing is connected there.
    pads: [Option<PadSnapshot>; MAX_LOCAL_CONTROLLERS],
    last_touched: Option<u8>,
    /// Seats whose controller went, not yet taken by [`Self::take_left`].
    left: Vec<SeatId>,
    totals: MouseTotals,
    /// Local state, tracked whether or not this member holds a seat.
    keys: KeyBitmap,
    buttons: u8,
    next_sync: Option<Instant>,
    /// When the viewer last sent anything.
    viewer_heard: Option<Instant>,
}

impl Forwarder {
    pub fn new() -> Self {
        Self::default()
    }

    fn viewer_alive(&self, now: Instant) -> bool {
        self.viewer_heard
            .is_some_and(|t| now.saturating_duration_since(t) < VIEWER_QUIET)
    }

    /// The seats this member holds and what each may send, in seat order.
    pub fn holding(&self) -> Vec<(SeatId, Permissions)> {
        self.holds
            .iter()
            .map(|(seat, h)| (*seat, h.permissions))
            .collect()
    }

    /// What the stream window is told: every held seat, and the controller
    /// slot driving each.
    pub fn seat_state(&self) -> SeatState {
        SeatState {
            seats: self
                .holds
                .iter()
                .map(|(seat, h)| HeldSeat {
                    seat: seat.0,
                    permissions: h.permissions,
                    controller: self.driver(*seat),
                })
                .collect(),
        }
    }

    /// How many controllers the viewer has connected, as it last said.
    pub fn controllers(&self) -> usize {
        self.pads.iter().flatten().count()
    }

    /// Seats whose controller went since the last call (with several pad
    /// seats): give each back. They stay neutral until released.
    pub fn take_left(&mut self) -> Vec<SeatId> {
        std::mem::take(&mut self.left)
    }

    fn connected(&self, slot: u8) -> bool {
        self.pads
            .get(usize::from(slot))
            .is_some_and(Option::is_some)
    }

    fn slots(&self) -> u8 {
        (0..MAX_LOCAL_CONTROLLERS as u8)
            .filter(|c| self.connected(*c))
            .fold(0, |mask, c| mask | 1 << c)
    }

    /// The held seats with a pad that a controller may drive (not leaving),
    /// in seat order.
    fn pad_seats(&self) -> Vec<SeatId> {
        self.holds
            .iter()
            .filter(|(_, h)| h.permissions.contains(Permissions::GAMEPAD) && !h.leaving)
            .map(|(seat, _)| *seat)
            .collect()
    }

    /// The controller slot driving `seat` now, if any.
    fn driver(&self, seat: SeatId) -> Option<u8> {
        let h = self.holds.get(&seat)?;
        if !h.permissions.contains(Permissions::GAMEPAD) || h.leaving {
            return None;
        }
        let pinned = h.driver.filter(|c| self.connected(*c));
        if pinned.is_none() && self.pad_seats().len() == 1 {
            return self
                .last_touched
                .filter(|c| self.connected(*c))
                .or_else(|| (0..MAX_LOCAL_CONTROLLERS as u8).find(|c| self.connected(*c)));
        }
        pinned
    }

    /// With several pad seats, pins each seat without a connected controller
    /// to the lowest one free. A pin stays while its controller is
    /// connected, even down to one seat: a controller coming back never takes
    /// the seat another one plays.
    fn pin(&mut self) {
        let seats = self.pad_seats();
        if seats.len() <= 1 {
            return;
        }
        let mut used: Vec<u8> = seats
            .iter()
            .filter_map(|s| self.holds[s].driver)
            .filter(|c| self.connected(*c))
            .collect();
        for seat in seats {
            if self.holds[&seat].driver.is_some_and(|c| self.connected(c)) {
                continue;
            }
            let free =
                (0..MAX_LOCAL_CONTROLLERS as u8).find(|c| self.connected(*c) && !used.contains(c));
            used.extend(free);
            if let Some(h) = self.holds.get_mut(&seat) {
                h.driver = free;
            }
        }
    }

    /// The pad state `seat` should have now.
    fn pad_for(&self, seat: SeatId) -> PadSnapshot {
        self.driver(seat)
            .and_then(|c| self.pads[usize::from(c)])
            .unwrap_or_default()
    }

    /// The held seat that takes `permission` (keyboard, mouse): at most one.
    fn seat_for(&self, permission: Permissions) -> Option<(SeatId, Hold)> {
        self.holds
            .iter()
            .find(|(_, h)| h.permissions.contains(permission))
            .map(|(seat, h)| (*seat, *h))
    }

    /// The host granted a seat. Sends its pad state at once if the viewer is
    /// up, so the host hears from the new holder without waiting for a
    /// refresh, and the others' if the new seat changed which controller
    /// drives them. Otherwise the first refresh after the viewer speaks does.
    pub fn seat_assigned(
        &mut self,
        seat: SeatId,
        epoch: SeatEpoch,
        permissions: Permissions,
        now: Instant,
    ) -> Vec<Outgoing> {
        // A second pad seat: the first keeps the controller driving it.
        if permissions.contains(Permissions::GAMEPAD)
            && let [only] = self.pad_seats()[..]
            && only != seat
        {
            let driving = self.driver(only);
            if let Some(h) = self.holds.get_mut(&only) {
                h.driver = driving;
            }
        }
        self.holds.insert(
            seat,
            Hold {
                epoch,
                permissions,
                pad_seq: 0,
                sent: PadSnapshot::default(),
                next_refresh: None,
                driver: None,
                leaving: false,
            },
        );
        self.left.retain(|s| *s != seat);
        self.pin();
        if permissions.contains(Permissions::MOUSE) {
            self.totals = MouseTotals::default();
        }
        if permissions.intersects(Permissions::KEYBOARD | Permissions::MOUSE) {
            self.next_sync = Some(now + HELD_SYNC);
        }
        if !self.viewer_alive(now) {
            return Vec::new();
        }
        let mut out = vec![self.pad_datagram(seat, now)];
        out.extend(self.changed_pads(now));
        out
    }

    pub fn seat_released(&mut self, seat: SeatId, epoch: SeatEpoch) {
        if self.holds.get(&seat).is_some_and(|h| h.epoch == epoch) {
            self.holds.remove(&seat);
            self.left.retain(|s| *s != seat);
            self.pin();
        }
    }

    /// The connection to the host went: no seat survives it. What the
    /// viewer has (its controllers, keys down) stays, for the next one.
    pub fn connection_lost(&mut self) {
        self.holds.clear();
        self.left.clear();
        self.next_sync = None;
    }

    /// The viewer window was replaced (it crashed and was started again):
    /// nothing the old one had down is down any more, and its controllers
    /// come again as the new one opens them. The holds stay, and none is
    /// given back. Returns what tells the host at once: a neutral pad per
    /// seat and, with keyboard or mouse, an empty held set (the host releases
    /// what it isn't in).
    pub fn viewer_replaced(&mut self, now: Instant) -> Vec<Outgoing> {
        self.pads = Default::default();
        self.last_touched = None;
        self.keys = KeyBitmap::default();
        self.buttons = 0;
        let seats: Vec<SeatId> = self.holds.keys().copied().collect();
        let mut out: Vec<Outgoing> = seats
            .into_iter()
            .map(|seat| self.pad_datagram(seat, now))
            .collect();
        if let Some((seat, h)) = self
            .seat_for(Permissions::KEYBOARD)
            .or_else(|| self.seat_for(Permissions::MOUSE))
        {
            self.next_sync = Some(now + HELD_SYNC);
            out.push(self.held_input(seat, h));
        }
        out
    }

    pub fn local(&mut self, input: LocalInput, now: Instant) -> Vec<Outgoing> {
        let mut out = Vec::new();
        self.viewer_heard = Some(now);
        match input {
            LocalInput::Key { usage, down } => {
                if !(MIN_KEY_USAGE..=MAX_KEY_USAGE).contains(&usage)
                    || self.keys.contains(usage) == down
                {
                    return out;
                }
                self.keys.set(usage, down);
                if let Some((seat, h)) = self.seat_for(Permissions::KEYBOARD) {
                    out.push(Outgoing::Stream(InputMessage::Key(KeyEvent {
                        seat,
                        seat_epoch: h.epoch,
                        usage,
                        down,
                    })));
                }
            }
            LocalInput::MouseButton { button, down } => {
                let Some(bit) = MouseButton::from_code(button).map(MouseButton::bit) else {
                    return out;
                };
                if (self.buttons & bit != 0) == down {
                    return out;
                }
                self.buttons ^= bit;
                if let Some((seat, h)) = self.seat_for(Permissions::MOUSE) {
                    out.push(Outgoing::Stream(InputMessage::MouseButton(
                        MouseButtonEvent {
                            seat,
                            seat_epoch: h.epoch,
                            button,
                            down,
                            totals: self.totals,
                        },
                    )));
                }
            }
            LocalInput::MouseMotion { dx, dy } => {
                if let Some((seat, h)) = self.seat_for(Permissions::MOUSE)
                    && (dx, dy) != (0, 0)
                {
                    self.totals.x = self.totals.x.wrapping_add(dx);
                    self.totals.y = self.totals.y.wrapping_add(dy);
                    self.totals.version = self.totals.version.wrapping_add(1);
                    out.push(self.mouse_datagram(seat, h));
                }
            }
            LocalInput::Wheel { dx, dy } => {
                if let Some((seat, h)) = self.seat_for(Permissions::MOUSE)
                    && (dx, dy) != (0, 0)
                {
                    self.totals.wheel_x = self.totals.wheel_x.wrapping_add(dx);
                    self.totals.wheel_y = self.totals.wheel_y.wrapping_add(dy);
                    self.totals.version = self.totals.version.wrapping_add(1);
                    out.push(self.mouse_datagram(seat, h));
                }
            }
            LocalInput::Pad { controller, pad } => {
                let Some(slot) = self.pads.get_mut(usize::from(controller)) else {
                    return out;
                };
                // A pad heard from is connected, even before its slot is said.
                let new = slot.replace(pad).is_none();
                self.last_touched = Some(controller);
                if new {
                    self.pin();
                }
                out.extend(self.changed_pads(now));
            }
            LocalInput::ControllerSlots(slots) => {
                let gone = self.slots() & !slots;
                // With several pad seats, a seat whose controller went leaves
                // it: neutral now, given back by the caller.
                if self.pad_seats().len() > 1 {
                    for (seat, h) in &mut self.holds {
                        if !h.leaving && h.driver.is_some_and(|c| gone & (1 << c) != 0) {
                            h.leaving = true;
                            self.left.push(*seat);
                        }
                    }
                }
                for (c, pad) in self.pads.iter_mut().enumerate() {
                    if slots & (1 << c) == 0 {
                        *pad = None;
                    } else if pad.is_none() {
                        *pad = Some(PadSnapshot::default());
                    }
                }
                if self.last_touched.is_some_and(|c| !self.connected(c)) {
                    self.last_touched = None;
                }
                self.pin();
                out.extend(self.changed_pads(now));
            }
            LocalInput::Alive => {}
        }
        out
    }

    /// Refreshes and syncs that are due. Call it every few milliseconds.
    /// Nothing goes out while the viewer is quiet.
    pub fn tick(&mut self, now: Instant) -> Vec<Outgoing> {
        let mut out = Vec::new();
        if self.holds.is_empty() || !self.viewer_alive(now) {
            return out;
        }
        let due: Vec<SeatId> = self
            .holds
            .iter()
            .filter(|(_, h)| h.next_refresh.is_none_or(|t| now >= t))
            .map(|(seat, _)| *seat)
            .collect();
        for seat in due {
            out.push(self.pad_datagram(seat, now));
            // Motion datagrams can be lost too; the latest totals ride along.
            if let Some(h) = self.holds.get(&seat).copied()
                && h.permissions.contains(Permissions::MOUSE)
                && self.totals.version != 0
            {
                out.push(self.mouse_datagram(seat, h));
            }
        }
        if let Some((seat, h)) = self
            .seat_for(Permissions::KEYBOARD)
            .or_else(|| self.seat_for(Permissions::MOUSE))
            && self.next_sync.is_none_or(|t| now >= t)
        {
            self.next_sync = Some(now + HELD_SYNC);
            out.push(self.held_input(seat, h));
        }
        out
    }

    /// A pad datagram for every held seat whose pad state changed (a
    /// controller moved, came or went, or a seat changed which one drives
    /// it).
    fn changed_pads(&mut self, now: Instant) -> Vec<Outgoing> {
        let changed: Vec<SeatId> = self
            .holds
            .iter()
            .filter(|(seat, h)| self.pad_for(**seat) != h.sent)
            .map(|(seat, _)| *seat)
            .collect();
        changed
            .into_iter()
            .map(|seat| self.pad_datagram(seat, now))
            .collect()
    }

    fn pad_datagram(&mut self, seat: SeatId, now: Instant) -> Outgoing {
        let pad = self.pad_for(seat);
        let h = self
            .holds
            .get_mut(&seat)
            .expect("only called for a held seat");
        h.pad_seq = h.pad_seq.wrapping_add(1);
        h.sent = pad;
        h.next_refresh = Some(now + PAD_REFRESH);
        Outgoing::Pad(PadDatagram {
            seat,
            seat_epoch: h.epoch,
            seq: h.pad_seq,
            pad,
        })
    }

    fn mouse_datagram(&self, seat: SeatId, h: Hold) -> Outgoing {
        Outgoing::Mouse(MouseDatagram {
            seat,
            seat_epoch: h.epoch,
            totals: self.totals,
        })
    }

    fn held_input(&self, seat: SeatId, h: Hold) -> Outgoing {
        Outgoing::Stream(InputMessage::Held(HeldInput {
            seat,
            seat_epoch: h.epoch,
            keys: self.keys,
            buttons: self.buttons,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        HidUsage, MemberId,
        router::{Effect, Input, SeatRouter},
    };

    const SEAT: SeatId = SeatId(0);
    const ME: MemberId = MemberId([9; 32]);

    fn key(usage: u16, down: bool) -> LocalInput {
        LocalInput::Key { usage, down }
    }

    fn pad(controller: u8, buttons: u16) -> LocalInput {
        LocalInput::Pad {
            controller,
            pad: PadSnapshot {
                buttons,
                ..Default::default()
            },
        }
    }

    fn as_input(out: Outgoing) -> Input {
        match out {
            Outgoing::Stream(InputMessage::Key(m)) => Input::Key(m),
            Outgoing::Stream(InputMessage::MouseButton(m)) => Input::MouseButton(m),
            Outgoing::Stream(InputMessage::Held(m)) => Input::Held(m),
            Outgoing::Pad(m) => Input::Pad(m),
            Outgoing::Mouse(m) => Input::Mouse(m),
        }
    }

    /// The pad datagrams in `out`, as (seat, buttons).
    fn pads(out: &[Outgoing]) -> Vec<(SeatId, u16)> {
        out.iter()
            .filter_map(|o| match o {
                Outgoing::Pad(p) => Some((p.seat, p.pad.buttons)),
                _ => None,
            })
            .collect()
    }

    /// Each held seat and the controller slot driving it.
    fn drivers(f: &Forwarder) -> Vec<(u8, Option<u8>)> {
        f.seat_state()
            .seats
            .iter()
            .map(|s| (s.seat, s.controller))
            .collect()
    }

    #[test]
    fn nothing_goes_out_without_a_seat() {
        let mut f = Forwarder::new();
        let now = Instant::now();
        assert!(f.local(key(0x04, true), now).is_empty());
        assert!(
            f.local(LocalInput::MouseMotion { dx: 5, dy: 0 }, now)
                .is_empty()
        );
        assert!(f.local(pad(0, 0x1000), now).is_empty());
        assert!(f.tick(now).is_empty());
    }

    /// A forwarder whose viewer just spoke, holding seat 0 with `permissions`.
    fn holding(permissions: Permissions, now: Instant) -> Forwarder {
        let mut f = Forwarder::new();
        f.local(LocalInput::Alive, now);
        assert_eq!(
            f.seat_assigned(SEAT, SeatEpoch(1), permissions, now).len(),
            1
        );
        f
    }

    #[test]
    fn a_hung_viewer_stops_the_heartbeat() {
        let t0 = Instant::now();
        let mut f = holding(Permissions::GAMEPAD, t0);
        let at = |ms| t0 + Duration::from_millis(ms);
        assert_eq!(f.tick(at(100)).len(), 1);
        assert_eq!(f.tick(at(200)).len(), 1);
        // 250 ms without a word from the viewer: nothing more goes out.
        assert!(f.tick(at(300)).is_empty());
        assert!(f.tick(at(1_000)).is_empty());
        // It's back.
        f.local(LocalInput::Alive, at(1_050));
        assert_eq!(f.tick(at(1_050)).len(), 1);
    }

    #[test]
    fn a_replaced_viewer_holds_nothing_down() {
        let t0 = Instant::now();
        let all = Permissions::GAMEPAD | Permissions::KEYBOARD | Permissions::MOUSE;
        let mut f = holding(all, t0);
        let held = PadSnapshot {
            buttons: 0x1000,
            right_trigger: 255,
            ..Default::default()
        };
        f.local(
            LocalInput::Pad {
                controller: 0,
                pad: held,
            },
            t0,
        );
        f.local(key(0x1A, true), t0);
        f.local(
            LocalInput::MouseButton {
                button: 1,
                down: true,
            },
            t0,
        );

        let out = f.viewer_replaced(t0);
        assert!(
            matches!(
                out[..],
                [
                    Outgoing::Pad(PadDatagram { pad, .. }),
                    Outgoing::Stream(InputMessage::Held(HeldInput { keys, buttons: 0, .. })),
                ] if pad == PadSnapshot::default() && keys == KeyBitmap::default()
            ),
            "{out:?}"
        );
        // The heartbeat goes on, neutral, and the hold stays.
        f.local(LocalInput::Alive, t0);
        assert!(f.tick(t0 + PAD_REFRESH).iter().all(|o| match o {
            Outgoing::Pad(p) => p.pad == PadSnapshot::default(),
            _ => true,
        }));
        assert_eq!(f.holding().len(), 1);
        // The new window's first press of the same key goes through.
        assert_eq!(f.local(key(0x1A, true), t0).len(), 1);
    }

    #[test]
    fn a_seat_granted_before_the_viewer_speaks_waits_for_it() {
        let t0 = Instant::now();
        let mut f = Forwarder::new();
        assert!(
            f.seat_assigned(SEAT, SeatEpoch(1), Permissions::GAMEPAD, t0)
                .is_empty()
        );
        assert!(f.tick(t0).is_empty());
        f.local(LocalInput::Alive, t0);
        assert!(matches!(
            f.tick(t0)[..],
            [Outgoing::Pad(PadDatagram { seq: 1, .. })]
        ));
    }

    #[test]
    fn only_permitted_input_is_sent() {
        let now = Instant::now();
        let mut f = holding(Permissions::GAMEPAD, now);
        assert!(f.local(key(0x04, true), now).is_empty());
        assert!(
            f.local(LocalInput::MouseMotion { dx: 5, dy: 0 }, now)
                .is_empty()
        );
        assert!(matches!(
            f.local(pad(0, 0x1000), now)[..],
            [Outgoing::Pad(PadDatagram { seq: 2, .. })]
        ));
    }

    #[test]
    fn repeats_are_not_resent() {
        let now = Instant::now();
        let mut f = holding(Permissions::all(), now);
        assert_eq!(f.local(key(0x04, true), now).len(), 1);
        assert!(f.local(key(0x04, true), now).is_empty());
        assert!(f.local(pad(0, 0), now).is_empty());
    }

    #[test]
    fn the_pad_refreshes_and_held_input_syncs() {
        let t0 = Instant::now();
        let mut f = holding(Permissions::all(), t0);
        assert!(f.tick(t0 + Duration::from_millis(99)).is_empty());
        assert!(matches!(
            f.tick(t0 + PAD_REFRESH)[..],
            [Outgoing::Pad(PadDatagram { seq: 2, .. })]
        ));
        f.local(LocalInput::MouseMotion { dx: 1, dy: 1 }, t0);
        f.local(LocalInput::Alive, t0 + HELD_SYNC);
        let out = f.tick(t0 + HELD_SYNC);
        assert!(matches!(
            out[..],
            [
                Outgoing::Pad(_),
                Outgoing::Mouse(_),
                Outgoing::Stream(InputMessage::Held(_))
            ]
        ));
    }

    #[test]
    fn a_new_seat_restarts_the_totals_and_old_releases_are_ignored() {
        let mut f = Forwarder::new();
        let now = Instant::now();
        f.seat_assigned(SEAT, SeatEpoch(1), Permissions::all(), now);
        f.local(LocalInput::MouseMotion { dx: 50, dy: 0 }, now);
        f.seat_assigned(SEAT, SeatEpoch(2), Permissions::all(), now);
        f.seat_released(SEAT, SeatEpoch(1));
        assert_eq!(f.holding().len(), 1);
        let out = f.local(LocalInput::MouseMotion { dx: 1, dy: 0 }, now);
        assert!(matches!(
            out[..],
            [Outgoing::Mouse(MouseDatagram {
                totals: MouseTotals {
                    version: 1,
                    x: 1,
                    ..
                },
                ..
            })]
        ));
        f.seat_released(SEAT, SeatEpoch(2));
        assert!(f.holding().is_empty());
    }

    /// M18: one pad seat is driven by the controller touched last; a second
    /// seat pins both, the first keeping the controller driving it.
    #[test]
    fn two_seats_are_driven_by_two_controllers() {
        let t0 = Instant::now();
        let mut f = Forwarder::new();
        f.local(LocalInput::ControllerSlots(0b11), t0);
        f.seat_assigned(SeatId(2), SeatEpoch(7), Permissions::GAMEPAD, t0);
        // One seat: any controller drives it.
        assert_eq!(
            pads(&f.local(pad(1, 0x1000), t0)),
            vec![(SeatId(2), 0x1000)]
        );
        // A second seat: seat 2 keeps controller 1; seat 0 takes controller 0.
        f.seat_assigned(SeatId(0), SeatEpoch(1), Permissions::GAMEPAD, t0);
        assert_eq!(drivers(&f), vec![(0, Some(0)), (2, Some(1))]);
        assert_eq!(
            pads(&f.local(pad(0, 0x2000), t0)),
            vec![(SeatId(0), 0x2000)]
        );
        // Each seat's heartbeat carries its own controller's state.
        let mut beat = pads(&f.tick(t0 + PAD_REFRESH));
        beat.sort();
        assert_eq!(beat, vec![(SeatId(0), 0x2000), (SeatId(2), 0x1000)]);
        // Giving one back: the other keeps its controller, whichever was
        // touched last.
        f.seat_released(SeatId(0), SeatEpoch(1));
        assert_eq!(
            pads(&f.tick(t0 + PAD_REFRESH * 2)),
            vec![(SeatId(2), 0x1000)]
        );
        assert!(pads(&f.local(pad(0, 0x4000), t0)).is_empty());
        // Its controller gone, the seat stays (one seat), and any drives it.
        f.local(LocalInput::ControllerSlots(0b01), t0);
        assert!(f.take_left().is_empty());
        assert_eq!(drivers(&f), vec![(2, Some(0))]);
    }

    /// M18: with several seats, a controller that goes leaves its seat
    /// (neutral, to give back); the other controller stays on its own.
    #[test]
    fn a_controller_that_goes_leaves_its_seat_and_the_others_stay() {
        let t0 = Instant::now();
        let mut f = Forwarder::new();
        f.local(LocalInput::ControllerSlots(0b11), t0);
        f.seat_assigned(SeatId(0), SeatEpoch(1), Permissions::GAMEPAD, t0);
        f.seat_assigned(SeatId(1), SeatEpoch(1), Permissions::GAMEPAD, t0);
        f.local(pad(0, 0x1000), t0);
        f.local(pad(1, 0x2000), t0);
        // Controller 0 goes, pressed: its seat goes neutral, the other stays.
        let out = pads(&f.local(LocalInput::ControllerSlots(0b10), t0));
        assert_eq!(out, vec![(SeatId(0), 0)]);
        assert_eq!(f.take_left(), vec![SeatId(0)]);
        assert!(f.take_left().is_empty(), "named once");
        assert_eq!(drivers(&f), vec![(0, None), (1, Some(1))]);
        assert_eq!(
            pads(&f.local(pad(1, 0x4000), t0)),
            vec![(SeatId(1), 0x4000)]
        );
        // It comes back before the host released the seat: still nothing.
        assert!(pads(&f.local(LocalInput::ControllerSlots(0b11), t0)).is_empty());
        assert!(pads(&f.local(pad(0, 0x8000), t0)).is_empty());
        f.seat_released(SeatId(0), SeatEpoch(1));
        assert_eq!(f.holding(), vec![(SeatId(1), Permissions::GAMEPAD)]);
    }

    /// M18: with one pad seat, a controller going frees nothing: another (or
    /// the same, back) carries on.
    #[test]
    fn one_seat_stays_when_its_controller_goes() {
        let t0 = Instant::now();
        let mut f = Forwarder::new();
        f.local(LocalInput::ControllerSlots(0b11), t0);
        f.seat_assigned(SEAT, SeatEpoch(1), Permissions::GAMEPAD, t0);
        f.local(pad(1, 0x1000), t0);
        assert_eq!(
            pads(&f.local(LocalInput::ControllerSlots(0b01), t0)),
            vec![(SEAT, 0)]
        );
        assert!(f.take_left().is_empty());
        assert_eq!(drivers(&f), vec![(0, Some(0))]);
        assert_eq!(pads(&f.local(pad(0, 0x2000), t0)), vec![(SEAT, 0x2000)]);
    }

    /// M18: a controller connecting takes no seat that has one, and moves
    /// nobody; the next seat added goes to it.
    #[test]
    fn a_controller_connecting_moves_nobody() {
        let t0 = Instant::now();
        let mut f = Forwarder::new();
        f.local(LocalInput::ControllerSlots(0b110), t0);
        f.seat_assigned(SeatId(0), SeatEpoch(1), Permissions::GAMEPAD, t0);
        f.seat_assigned(SeatId(1), SeatEpoch(1), Permissions::GAMEPAD, t0);
        assert_eq!(drivers(&f), vec![(0, Some(1)), (1, Some(2))]);
        f.local(LocalInput::ControllerSlots(0b111), t0);
        assert_eq!(drivers(&f), vec![(0, Some(1)), (1, Some(2))]);
        f.seat_assigned(SeatId(3), SeatEpoch(1), Permissions::GAMEPAD, t0);
        assert_eq!(drivers(&f), vec![(0, Some(1)), (1, Some(2)), (3, Some(0))]);
    }

    /// M18: a restarted stream window opens its controllers again; that
    /// gives no seat back.
    #[test]
    fn a_replaced_viewer_gives_no_seat_back() {
        let t0 = Instant::now();
        let mut f = Forwarder::new();
        f.local(LocalInput::ControllerSlots(0b11), t0);
        f.seat_assigned(SeatId(0), SeatEpoch(1), Permissions::GAMEPAD, t0);
        f.seat_assigned(SeatId(1), SeatEpoch(1), Permissions::GAMEPAD, t0);
        f.viewer_replaced(t0);
        f.local(LocalInput::ControllerSlots(0b01), t0);
        f.local(LocalInput::ControllerSlots(0b11), t0);
        assert!(f.take_left().is_empty());
        assert_eq!(drivers(&f), vec![(0, Some(0)), (1, Some(1))]);
    }

    #[test]
    fn keys_go_to_the_keyboard_seat_beside_a_pad_seat() {
        let t0 = Instant::now();
        let mut f = Forwarder::new();
        f.local(LocalInput::Alive, t0);
        f.seat_assigned(SeatId(0), SeatEpoch(1), Permissions::GAMEPAD, t0);
        f.seat_assigned(
            SeatId(2),
            SeatEpoch(4),
            Permissions::KEYBOARD | Permissions::MOUSE,
            t0,
        );
        let out = f.local(key(0x04, true), t0);
        assert!(matches!(
            out[..],
            [Outgoing::Stream(InputMessage::Key(KeyEvent {
                seat: SeatId(2),
                seat_epoch: SeatEpoch(4),
                ..
            }))]
        ));
        // The pad drives the pad seat; the keyboard seat stays neutral.
        assert_eq!(
            pads(&f.local(pad(0, 0x1000), t0)),
            vec![(SeatId(0), 0x1000)]
        );
    }

    #[test]
    fn forwarder_and_router_agree_end_to_end() {
        let t0 = Instant::now();
        let mut router = SeatRouter::new(1);
        router.set_foreground(true);
        router.set_pointer_inside(true);
        let grant = router.grant(SEAT, ME, Permissions::all(), t0);
        let Some(Effect::Granted {
            epoch, permissions, ..
        }) = grant.last().copied()
        else {
            panic!("no grant");
        };
        let mut f = Forwarder::new();
        let mut effects = Vec::new();
        let mut deliver = |out: Vec<Outgoing>, router: &mut SeatRouter| {
            for o in out {
                effects.extend(router.handle(ME, as_input(o), t0));
            }
        };
        deliver(f.seat_assigned(SEAT, epoch, permissions, t0), &mut router);
        for input in [
            LocalInput::MouseMotion { dx: 3, dy: 4 },
            LocalInput::MouseButton {
                button: 1,
                down: true,
            },
            key(0x2C, true),
            LocalInput::Wheel { dx: 0, dy: 120 },
            key(0x2C, false),
            LocalInput::MouseButton {
                button: 1,
                down: false,
            },
        ] {
            deliver(f.local(input, t0), &mut router);
        }
        assert_eq!(
            effects,
            vec![
                Effect::MouseMove(3, 4),
                Effect::MouseButton(MouseButton::Left, true),
                Effect::Key(HidUsage(0x2C), true),
                Effect::Wheel(0, 120),
                Effect::Key(HidUsage(0x2C), false),
                Effect::MouseButton(MouseButton::Left, false),
            ]
        );
    }

    /// M18: two seats in the router, one member driving both pads through
    /// one forwarder.
    #[test]
    fn one_member_drives_two_pads_end_to_end() {
        let t0 = Instant::now();
        let mut router = SeatRouter::new(2);
        let mut f = Forwarder::new();
        f.local(LocalInput::ControllerSlots(0b11), t0);
        for seat in [SeatId(0), SeatId(1)] {
            let grant = router.grant(seat, ME, Permissions::GAMEPAD, t0);
            let Some(Effect::Granted { epoch, .. }) = grant.last().copied() else {
                panic!("no grant");
            };
            f.seat_assigned(seat, epoch, Permissions::GAMEPAD, t0);
        }
        let mut effects = Vec::new();
        for input in [pad(0, 0x1000), pad(1, 0x2000)] {
            for o in f.local(input, t0) {
                effects.extend(router.handle(ME, as_input(o), t0));
            }
        }
        let a = |b| crate::PadState {
            buttons: crate::PadButtons::from_bits_truncate(b),
            ..crate::PadState::NEUTRAL
        };
        assert_eq!(
            effects,
            vec![
                Effect::Pad(SeatId(0), a(0x1000)),
                Effect::Pad(SeatId(1), a(0x2000)),
            ]
        );
    }
}
