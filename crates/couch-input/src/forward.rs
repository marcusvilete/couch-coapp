//! A viewer's side of input: turns local events from `couch-viewer` into
//! what goes to the host, while this member holds a seat.
//!
//! Sans-IO, like the router. It keeps the running mouse totals, numbers the
//! pad datagrams, and on [`Forwarder::tick`] repeats the pad state every
//! [`PAD_REFRESH`] (the holder's heartbeat) and sends what is held every
//! [`HELD_SYNC`].
//!
//! The heartbeat stands for the viewer, not this process: it only goes out
//! while the viewer has been heard from within [`VIEWER_QUIET`]
//! ([`LocalInput::Alive`] comes every 100 ms). A hung viewer stops it, and the
//! host's dead-man timer releases whatever was held.

use std::time::{Duration, Instant};

use couch_proto::{
    input::{
        HeldInput, InputMessage, KeyBitmap, KeyEvent, MAX_KEY_USAGE, MIN_KEY_USAGE,
        MouseButtonEvent, MouseDatagram, MouseTotals, PadDatagram, PadSnapshot,
    },
    ipc::LocalInput,
};

use crate::{MouseButton, Permissions, SeatEpoch, SeatId};

/// How often the pad state goes out unchanged. Well inside the router's
/// 300 ms dead-man time, so two lost datagrams in a row are harmless.
pub const PAD_REFRESH: Duration = Duration::from_millis(100);
pub const HELD_SYNC: Duration = Duration::from_secs(1);
/// A viewer silent this long counts as hung: nothing more goes out for it.
pub const VIEWER_QUIET: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outgoing {
    /// On the input stream.
    Stream(InputMessage),
    Pad(PadDatagram),
    Mouse(MouseDatagram),
}

#[derive(Debug, Clone, Copy)]
struct Hold {
    seat: SeatId,
    epoch: SeatEpoch,
    permissions: Permissions,
}

#[derive(Debug, Default)]
pub struct Forwarder {
    hold: Option<Hold>,
    pad_seq: u32,
    pad: PadSnapshot,
    totals: MouseTotals,
    /// Local state, tracked whether or not this member holds a seat.
    keys: KeyBitmap,
    buttons: u8,
    next_refresh: Option<Instant>,
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

    /// The seat this member holds and what it may send.
    pub fn holding(&self) -> Option<(SeatId, Permissions)> {
        self.hold.map(|h| (h.seat, h.permissions))
    }

    /// The host granted a seat. Sends the pad state at once if the viewer is
    /// up, so the host hears from the new holder without waiting for a
    /// refresh. Otherwise the first refresh after the viewer speaks does.
    pub fn seat_assigned(
        &mut self,
        seat: SeatId,
        epoch: SeatEpoch,
        permissions: Permissions,
        now: Instant,
    ) -> Vec<Outgoing> {
        self.hold = Some(Hold {
            seat,
            epoch,
            permissions,
        });
        self.pad_seq = 0;
        self.totals = MouseTotals::default();
        self.next_sync = Some(now + HELD_SYNC);
        if self.viewer_alive(now) {
            vec![self.pad_datagram(now)]
        } else {
            self.next_refresh = None;
            Vec::new()
        }
    }

    pub fn seat_released(&mut self, seat: SeatId, epoch: SeatEpoch) {
        if self
            .hold
            .is_some_and(|h| h.seat == seat && h.epoch == epoch)
        {
            self.hold = None;
        }
    }

    pub fn local(&mut self, input: LocalInput, now: Instant) -> Vec<Outgoing> {
        let mut out = Vec::new();
        self.viewer_heard = Some(now);
        let hold = self.hold;
        let may = |p: Permissions| hold.filter(|h| h.permissions.contains(p));
        match input {
            LocalInput::Key { usage, down } => {
                if !(MIN_KEY_USAGE..=MAX_KEY_USAGE).contains(&usage)
                    || self.keys.contains(usage) == down
                {
                    return out;
                }
                self.keys.set(usage, down);
                if let Some(h) = may(Permissions::KEYBOARD) {
                    out.push(Outgoing::Stream(InputMessage::Key(KeyEvent {
                        seat: h.seat,
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
                if let Some(h) = may(Permissions::MOUSE) {
                    out.push(Outgoing::Stream(InputMessage::MouseButton(
                        MouseButtonEvent {
                            seat: h.seat,
                            seat_epoch: h.epoch,
                            button,
                            down,
                            totals: self.totals,
                        },
                    )));
                }
            }
            LocalInput::MouseMotion { dx, dy } => {
                if let Some(h) = may(Permissions::MOUSE)
                    && (dx, dy) != (0, 0)
                {
                    self.totals.x = self.totals.x.wrapping_add(dx);
                    self.totals.y = self.totals.y.wrapping_add(dy);
                    self.totals.version = self.totals.version.wrapping_add(1);
                    out.push(self.mouse_datagram(h));
                }
            }
            LocalInput::Wheel { dx, dy } => {
                if let Some(h) = may(Permissions::MOUSE)
                    && (dx, dy) != (0, 0)
                {
                    self.totals.wheel_x = self.totals.wheel_x.wrapping_add(dx);
                    self.totals.wheel_y = self.totals.wheel_y.wrapping_add(dy);
                    self.totals.version = self.totals.version.wrapping_add(1);
                    out.push(self.mouse_datagram(h));
                }
            }
            LocalInput::Pad(pad) => {
                if pad != self.pad {
                    self.pad = pad;
                    if self.hold.is_some() {
                        out.push(self.pad_datagram(now));
                    }
                }
            }
            LocalInput::Alive => {}
        }
        out
    }

    /// Refreshes and syncs that are due. Call it every few milliseconds.
    /// Nothing goes out while the viewer is quiet.
    pub fn tick(&mut self, now: Instant) -> Vec<Outgoing> {
        let mut out = Vec::new();
        let Some(h) = self.hold else {
            return out;
        };
        if !self.viewer_alive(now) {
            return out;
        }
        if self.next_refresh.is_none_or(|t| now >= t) {
            out.push(self.pad_datagram(now));
            // Motion datagrams can be lost too; the latest totals ride along.
            if h.permissions.contains(Permissions::MOUSE) && self.totals.version != 0 {
                out.push(self.mouse_datagram(h));
            }
        }
        if h.permissions
            .intersects(Permissions::KEYBOARD | Permissions::MOUSE)
            && self.next_sync.is_none_or(|t| now >= t)
        {
            self.next_sync = Some(now + HELD_SYNC);
            out.push(Outgoing::Stream(InputMessage::Held(HeldInput {
                seat: h.seat,
                seat_epoch: h.epoch,
                keys: self.keys,
                buttons: self.buttons,
            })));
        }
        out
    }

    fn pad_datagram(&mut self, now: Instant) -> Outgoing {
        let h = self.hold.expect("only called while holding a seat");
        self.pad_seq = self.pad_seq.wrapping_add(1);
        self.next_refresh = Some(now + PAD_REFRESH);
        Outgoing::Pad(PadDatagram {
            seat: h.seat,
            seat_epoch: h.epoch,
            seq: self.pad_seq,
            pad: self.pad,
        })
    }

    fn mouse_datagram(&self, h: Hold) -> Outgoing {
        Outgoing::Mouse(MouseDatagram {
            seat: h.seat,
            seat_epoch: h.epoch,
            totals: self.totals,
        })
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

    fn as_input(out: Outgoing) -> Input {
        match out {
            Outgoing::Stream(InputMessage::Key(m)) => Input::Key(m),
            Outgoing::Stream(InputMessage::MouseButton(m)) => Input::MouseButton(m),
            Outgoing::Stream(InputMessage::Held(m)) => Input::Held(m),
            Outgoing::Pad(m) => Input::Pad(m),
            Outgoing::Mouse(m) => Input::Mouse(m),
        }
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
        let pad = PadSnapshot {
            buttons: 0x1000,
            ..Default::default()
        };
        assert!(matches!(
            f.local(LocalInput::Pad(pad), now)[..],
            [Outgoing::Pad(PadDatagram { seq: 2, .. })]
        ));
    }

    #[test]
    fn repeats_are_not_resent() {
        let now = Instant::now();
        let mut f = holding(Permissions::all(), now);
        assert_eq!(f.local(key(0x04, true), now).len(), 1);
        assert!(f.local(key(0x04, true), now).is_empty());
        assert!(
            f.local(LocalInput::Pad(PadSnapshot::default()), now)
                .is_empty()
        );
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
        assert!(f.holding().is_some());
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
        assert!(f.holding().is_none());
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
}
