//! Admission rate limits (docs/protocol.md, Admission).
//!
//! Every failed password check costs the guesser, the same way whether the
//! password was wrong or they walked away after seeing our confirmation (that
//! confirmation lets them test one guess offline):
//! - **Per remote id:** exponential backoff, checked as soon as the TLS
//!   handshake names the peer.
//! - **Per room:** too many failures from ids that aren't members pause new
//!   joiners for a while. Known members are exempt, so a stranger with the
//!   invite can't lock the room's own members out.
//!
//! Checks still running count against the room's threshold as if they had
//! failed, so parallel attempts can't get past it before the first result
//! lands. A guesser can mint ids for free, so the room limit is the real
//! bound; the per-id backoff mainly slows a single misbehaving install.

use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use couch_proto::ids::{MemberId, RoomId};

#[derive(Debug, Clone)]
pub struct LimitConfig {
    /// Backoff after the first failure; doubles with each further one.
    pub first_backoff: Duration,
    pub max_backoff: Duration,
    /// A remote id's failures are forgotten this long after its last one.
    pub forget_after: Duration,
    /// This many failures by non-members within `room_window` pause the room.
    pub room_failures: usize,
    pub room_window: Duration,
    pub room_pause: Duration,
    /// Remote ids tracked at once; the stalest is dropped beyond this.
    pub max_tracked_ids: usize,
    /// A check never reported as ended stops counting as running after this
    /// (it must be longer than the check's own timeout).
    pub in_flight_expiry: Duration,
}

impl Default for LimitConfig {
    fn default() -> Self {
        Self {
            first_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(600),
            forget_after: Duration::from_secs(3600),
            room_failures: 10,
            room_window: Duration::from_secs(3600),
            room_pause: Duration::from_secs(600),
            max_tracked_ids: 4096,
            in_flight_expiry: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct IdFailures {
    count: u32,
    last: Instant,
}

#[derive(Debug, Default)]
struct RoomFailures {
    recent: VecDeque<Instant>,
    paused_until: Option<Instant>,
}

#[derive(Debug, Clone, Copy)]
struct InFlight {
    id: MemberId,
    room: RoomId,
    known: bool,
    started: Instant,
}

/// A room stopped admitting newcomers until `until`; the UI warns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomPaused {
    pub room: RoomId,
    pub until: Instant,
}

#[derive(Debug)]
pub struct AdmissionLimits {
    config: LimitConfig,
    ids: HashMap<MemberId, IdFailures>,
    rooms: HashMap<RoomId, RoomFailures>,
    in_flight: Vec<InFlight>,
}

impl AdmissionLimits {
    pub fn new(config: LimitConfig) -> Self {
        Self {
            config,
            ids: HashMap::new(),
            rooms: HashMap::new(),
            in_flight: Vec::new(),
        }
    }

    /// How long `id` must still wait before another attempt, if at all.
    pub fn id_blocked_for(&self, id: MemberId, now: Instant) -> Option<Duration> {
        let failures = self.ids.get(&id)?;
        let until = failures.last + self.backoff(failures.count);
        until.checked_duration_since(now).filter(|d| !d.is_zero())
    }

    /// Whether a password check from `id` into `room` may start now; if so,
    /// it counts as running until [`AdmissionLimits::end`]. Refused when the
    /// id is in backoff or already has a check running for the room, and,
    /// for non-members, while the room is paused or when the checks running
    /// would take it past the threshold. After a pause, with the window still
    /// full, strangers get one check at a time; a failure pauses again.
    pub fn begin(&mut self, id: MemberId, room: RoomId, known_member: bool, now: Instant) -> bool {
        self.expire_in_flight(now);
        if self.id_blocked_for(id, now).is_some()
            || self.in_flight.iter().any(|f| f.id == id && f.room == room)
        {
            return false;
        }
        if !known_member {
            if self.room_paused_until(room, now).is_some() {
                return false;
            }
            let window = self.config.room_window;
            let recent = self.rooms.get(&room).map_or(0, |r| {
                r.recent
                    .iter()
                    .filter(|t| now.saturating_duration_since(**t) <= window)
                    .count()
            });
            let running = self
                .in_flight
                .iter()
                .filter(|f| f.room == room && !f.known)
                .count();
            let allowed = self.config.room_failures.saturating_sub(recent).max(1);
            if running >= allowed {
                return false;
            }
        }
        self.in_flight.push(InFlight {
            id,
            room,
            known: known_member,
            started: now,
        });
        true
    }

    /// A check [`AdmissionLimits::begin`] allowed has ended. Anything but
    /// admission is a failure. Returns the pause if this failure started one.
    pub fn end(
        &mut self,
        id: MemberId,
        room: RoomId,
        admitted: bool,
        now: Instant,
    ) -> Option<RoomPaused> {
        let known = match self
            .in_flight
            .iter()
            .position(|f| f.id == id && f.room == room)
        {
            Some(i) => self.in_flight.swap_remove(i).known,
            // Expired: count it as a stranger's, the cautious choice.
            None => false,
        };
        if admitted {
            self.ids.remove(&id);
            None
        } else {
            self.failed(id, Some(room), known, now)
        }
    }

    /// A check [`AdmissionLimits::begin`] allowed never started (the task that
    /// asked is gone): release its slot without counting anything.
    pub fn cancel(&mut self, id: MemberId, room: RoomId) {
        if let Some(i) = self
            .in_flight
            .iter()
            .position(|f| f.id == id && f.room == room)
        {
            self.in_flight.swap_remove(i);
        }
    }

    /// When `room`'s pause ends, while it is paused.
    pub fn room_paused_until(&self, room: RoomId, now: Instant) -> Option<Instant> {
        self.rooms
            .get(&room)
            .and_then(|r| r.paused_until)
            .filter(|until| now < *until)
    }

    fn failed(
        &mut self,
        id: MemberId,
        room: Option<RoomId>,
        known_member: bool,
        now: Instant,
    ) -> Option<RoomPaused> {
        self.forget_stale(now);
        let entry = self.ids.entry(id).or_insert(IdFailures {
            count: 0,
            last: now,
        });
        entry.count = entry.count.saturating_add(1);
        entry.last = now;
        if self.ids.len() > self.config.max_tracked_ids
            && let Some(stalest) = self
                .ids
                .iter()
                .filter(|(other, _)| **other != id)
                .min_by_key(|(_, f)| f.last)
                .map(|(other, _)| *other)
        {
            self.ids.remove(&stalest);
        }

        let room = room.filter(|_| !known_member)?;
        let failures = self.rooms.entry(room).or_default();
        failures.recent.push_back(now);
        while failures
            .recent
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) > self.config.room_window)
        {
            failures.recent.pop_front();
        }
        // Keeping more than the threshold tells us nothing more.
        while failures.recent.len() > self.config.room_failures {
            failures.recent.pop_front();
        }
        let already_paused = failures.paused_until.is_some_and(|until| now < until);
        if failures.recent.len() >= self.config.room_failures && !already_paused {
            let until = now + self.config.room_pause;
            failures.paused_until = Some(until);
            return Some(RoomPaused { room, until });
        }
        None
    }

    fn backoff(&self, failures: u32) -> Duration {
        let doublings = failures.saturating_sub(1).min(20);
        self.config
            .first_backoff
            .saturating_mul(1 << doublings)
            .min(self.config.max_backoff)
    }

    fn forget_stale(&mut self, now: Instant) {
        let forget_after = self.config.forget_after;
        self.ids
            .retain(|_, f| now.saturating_duration_since(f.last) < forget_after);
    }

    fn expire_in_flight(&mut self, now: Instant) {
        let expiry = self.config.in_flight_expiry;
        self.in_flight
            .retain(|f| now.saturating_duration_since(f.started) < expiry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM: RoomId = RoomId([1; 16]);

    fn id(n: u32) -> MemberId {
        let mut bytes = [0; 32];
        bytes[..4].copy_from_slice(&n.to_be_bytes());
        MemberId(bytes)
    }

    fn limits() -> AdmissionLimits {
        AdmissionLimits::new(LimitConfig::default())
    }

    /// One whole check by a stranger that fails.
    fn guess(limits: &mut AdmissionLimits, n: u32, now: Instant) -> Option<RoomPaused> {
        assert!(limits.begin(id(n), ROOM, false, now), "guess {n} refused");
        limits.end(id(n), ROOM, false, now)
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let mut limits = limits();
        let t0 = Instant::now();
        assert_eq!(limits.id_blocked_for(id(1), t0), None);
        guess(&mut limits, 1, t0);
        assert_eq!(
            limits.id_blocked_for(id(1), t0),
            Some(Duration::from_secs(1))
        );
        assert!(!limits.begin(id(1), ROOM, false, t0), "in backoff");
        limits.failed(id(1), Some(ROOM), false, t0);
        assert_eq!(
            limits.id_blocked_for(id(1), t0),
            Some(Duration::from_secs(2))
        );
        for _ in 0..30 {
            limits.failed(id(1), None, false, t0);
        }
        assert_eq!(
            limits.id_blocked_for(id(1), t0),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            limits.id_blocked_for(id(1), t0 + Duration::from_secs(600)),
            None
        );
        // Other ids are unaffected.
        assert_eq!(limits.id_blocked_for(id(2), t0), None);
    }

    #[test]
    fn admission_clears_and_an_hour_forgets() {
        let mut limits = limits();
        let t0 = Instant::now();
        guess(&mut limits, 1, t0);
        let later = t0 + Duration::from_secs(2);
        assert!(limits.begin(id(1), ROOM, false, later));
        limits.end(id(1), ROOM, true, later);
        assert_eq!(limits.id_blocked_for(id(1), later), None);

        limits.failed(id(2), None, false, t0);
        limits.failed(id(2), None, false, t0);
        // The next failure after the hour starts the count again.
        let later = t0 + Duration::from_secs(3601);
        limits.failed(id(2), None, false, later);
        assert_eq!(
            limits.id_blocked_for(id(2), later),
            Some(Duration::from_secs(1))
        );
    }

    #[test]
    fn ten_strangers_pause_the_room_for_newcomers_only() {
        let mut limits = limits();
        let t0 = Instant::now();
        for n in 0..9 {
            assert_eq!(guess(&mut limits, n, t0), None);
        }
        let paused = guess(&mut limits, 9, t0);
        assert_eq!(
            paused,
            Some(RoomPaused {
                room: ROOM,
                until: t0 + Duration::from_secs(600)
            })
        );
        assert!(
            !limits.begin(id(100), ROOM, false, t0),
            "a fresh id is refused"
        );
        assert!(
            limits.begin(id(101), ROOM, true, t0),
            "members still get in"
        );
        assert!(
            limits.begin(id(102), RoomId([2; 16]), false, t0),
            "other rooms unaffected"
        );

        // More failures during the pause don't announce it again.
        assert_eq!(limits.failed(id(10), Some(ROOM), false, t0), None);
        // After the pause, with ten still in the window: one stranger at a
        // time, and a failure pauses again.
        let after = t0 + Duration::from_secs(600);
        assert!(limits.begin(id(103), ROOM, false, after));
        assert!(!limits.begin(id(104), ROOM, false, after));
        assert!(limits.end(id(103), ROOM, false, after).is_some());
        assert!(!limits.begin(id(104), ROOM, false, after));
    }

    /// Checks running at once count against the threshold: a burst can't get
    /// more guesses in than a sequence could.
    #[test]
    fn parallel_checks_cannot_overshoot_the_threshold() {
        let mut limits = limits();
        let t0 = Instant::now();
        for n in 0..10 {
            assert!(limits.begin(id(n), ROOM, false, t0), "check {n}");
        }
        assert!(
            !limits.begin(id(10), ROOM, false, t0),
            "an 11th has to wait"
        );
        // A member is never held up by strangers.
        assert!(limits.begin(id(50), ROOM, true, t0));
        // One stranger gets in: room for one more.
        assert_eq!(limits.end(id(0), ROOM, true, t0), None);
        assert!(limits.begin(id(11), ROOM, false, t0));
        // The rest fail: the room pauses at ten failures, not before.
        let mut paused = None;
        for n in (1..10).chain([11]) {
            paused = paused.or(limits.end(id(n), ROOM, false, t0));
        }
        assert!(paused.is_some());
        assert!(!limits.begin(id(12), ROOM, false, t0));
    }

    #[test]
    fn one_check_at_a_time_per_id_and_room() {
        let mut limits = limits();
        let t0 = Instant::now();
        assert!(limits.begin(id(1), ROOM, true, t0));
        assert!(!limits.begin(id(1), ROOM, true, t0));
        assert!(
            limits.begin(id(1), RoomId([2; 16]), true, t0),
            "another room is fine"
        );
        limits.end(id(1), ROOM, true, t0);
        assert!(limits.begin(id(1), ROOM, true, t0));
    }

    /// A check whose end was never reported (its task was cancelled) stops
    /// holding a slot, uncounted.
    #[test]
    fn unreported_checks_expire() {
        let mut limits = limits();
        let t0 = Instant::now();
        for n in 0..10 {
            assert!(limits.begin(id(n), ROOM, false, t0));
        }
        let later = t0 + Duration::from_secs(31);
        assert!(limits.begin(id(10), ROOM, false, later));
        assert_eq!(limits.id_blocked_for(id(0), later), None);
    }

    #[test]
    fn members_failing_never_pause_the_room() {
        let mut limits = limits();
        let t0 = Instant::now();
        for n in 0..50 {
            assert!(limits.begin(id(n), ROOM, true, t0));
            assert_eq!(limits.end(id(n), ROOM, false, t0), None);
        }
        assert!(limits.begin(id(100), ROOM, false, t0));
    }

    #[test]
    fn failures_outside_the_window_age_out() {
        let mut limits = limits();
        let t0 = Instant::now();
        for n in 0..9 {
            guess(&mut limits, n, t0);
        }
        let later = t0 + Duration::from_secs(3601);
        assert_eq!(guess(&mut limits, 100, later), None);
    }

    #[test]
    fn tracked_ids_are_bounded() {
        let mut limits = AdmissionLimits::new(LimitConfig {
            max_tracked_ids: 100,
            ..LimitConfig::default()
        });
        let t0 = Instant::now();
        for n in 0..1000 {
            limits.failed(id(n), None, false, t0 + Duration::from_millis(n as u64));
        }
        assert!(limits.ids.len() <= 100);
        // The newest is still tracked.
        assert!(
            limits
                .id_blocked_for(id(999), t0 + Duration::from_millis(999))
                .is_some()
        );
    }
}
