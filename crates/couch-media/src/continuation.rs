//! Continuing a viewer's stream after frames it never got (M12d-2, ADR 0016),
//! the host's side: which frames were sent and received, which are held as
//! long-term references, and what to invalidate to continue from a frame.
//! Sans-IO: the encode loop feeds it and acts on what it says.
//!
//! **Long-term frames, two slots.** One holds the newest marked frame the
//! viewer is known to have received; the next mark goes into the other, and
//! only once the previous mark was received, at most every [`MARK_EVERY`].
//! The frame the viewer is known to hold is never overwritten, so a recovery
//! always has a reference, however far behind the viewer is.
//!
//! **Continuing from frame F** invalidates every frame sent after F that the
//! encoder may still refer to: those within [`REACH`] of the newest (its
//! short-term references) and long-term ones. The encoder then predicts from
//! F, from a long-term frame the viewer holds, or codes an intra frame.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use couch_proto::media::FrameRef;

use crate::LongTerm;

/// At most one long-term mark this often.
pub const MARK_EVERY: Duration = Duration::from_millis(100);

/// Frames remembered: 5 s at 120 fps. A recovery from a frame older than
/// that sends a keyframe.
pub const KEPT: usize = 600;

/// How many of the newest frames the encoder may still predict from besides
/// its long-term ones: H.264's largest DPB, more than HEVC's.
pub const REACH: usize = 16;

/// The most frames the encoder may have encoded after the frame it continues
/// from, the continuing frame included. An HEVC decoder works out a
/// picture's order from its low 8 bits (NVENC's `log2_max_pic_order_cnt_lsb`)
/// and the last picture it decoded: one more than 128 frames on would be
/// put in the wrong place, its references taken for missing. Frames not
/// encoded don't count: none are while a viewer is paused.
pub const MAX_AHEAD: u64 = 100;

/// The oldest frame to continue from: past it, a keyframe is the better
/// picture. Measured in Witcher 3 (HEVC, 20 Mbit/s, the author's runs): a
/// continuation lost less than a keyframe over the next 10 frames after
/// 300 ms of frames left out (11.8 dB against 26.0) and 500 ms (13.0
/// against 22.6), more after 700 ms (17.0 against 13.0): the game moved too
/// far for the old frame to predict from. So catch-ups continue, and a
/// resume after an outage (at least 500 ms of silence) mostly doesn't.
pub const MAX_AGE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy)]
struct Sent {
    frame: FrameRef,
    timestamp: u64,
    received: bool,
    /// Its number among the frames this encoder encoded since the keyframe.
    index: u64,
    /// When it was sent.
    at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Slot {
    frame: FrameRef,
    received: bool,
}

/// What continuing from a frame takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The frame the new epoch continues from.
    pub from: FrameRef,
    /// The encoder's timestamps of the frames to invalidate first.
    pub invalidate: Vec<u64>,
}

/// One viewer's stream from one encoder, since its last keyframe.
#[derive(Debug, Default)]
pub struct Continuity {
    /// In the order encoded, since the last keyframe.
    sent: VecDeque<Sent>,
    slots: [Option<Slot>; 2],
    /// The last frame marked long-term, and when.
    last_mark: Option<(FrameRef, Instant)>,
    /// Frames encoded since the keyframe, those forgotten included.
    encoded: u64,
}

impl Continuity {
    pub fn new() -> Self {
        Self::default()
    }

    /// The slot to mark the next frame into, if it is to be marked.
    pub fn mark(&self, now: Instant) -> Option<u32> {
        if let Some((frame, at)) = self.last_mark {
            if now.saturating_duration_since(at) < MARK_EVERY {
                return None;
            }
            let pending = self
                .slots
                .iter()
                .flatten()
                .any(|s| s.frame == frame && !s.received);
            if pending {
                return None;
            }
        }
        Some(match self.held() {
            Some(held) => 1 - held,
            None => 0,
        })
    }

    /// The slot holding the newest long-term frame the viewer received.
    /// Received frames all lie on the viewer's chain, in order, so the
    /// newest is the largest.
    fn held(&self) -> Option<u32> {
        (0..2u32)
            .filter_map(|i| {
                self.slots[i as usize]
                    .filter(|s| s.received)
                    .map(|s| (s.frame, i))
            })
            .max()
            .map(|(_, i)| i)
    }

    /// A frame was sent, with what the encoder said of it. A keyframe
    /// empties the encoder's references: everything before it is forgotten.
    /// False when the encoder reports what can't be followed (a long-term
    /// slot beyond the two): stop continuing.
    #[must_use]
    pub fn sent(
        &mut self,
        frame: FrameRef,
        keyframe: bool,
        long_term: LongTerm,
        now: Instant,
    ) -> bool {
        if keyframe {
            *self = Self::default();
        }
        if let Some(slot) = long_term.marked {
            let Some(s) = self.slots.get_mut(slot as usize) else {
                return false;
            };
            *s = Some(Slot {
                frame,
                received: false,
            });
            self.last_mark = Some((frame, now));
        }
        self.sent.push_back(Sent {
            frame,
            timestamp: long_term.timestamp,
            received: false,
            index: self.encoded,
            at: now,
        });
        while self.sent.len() > KEPT {
            self.sent.pop_front();
        }
        self.encoded += 1;
        true
    }

    /// The viewer received every frame of `newest`'s epoch up to it.
    pub fn received(&mut self, newest: FrameRef) {
        let got = |f: FrameRef| f.epoch == newest.epoch && f.seq <= newest.seq;
        for s in self.sent.iter_mut().rev() {
            if got(s.frame) {
                if s.received {
                    break;
                }
                s.received = true;
            }
        }
        for s in self.slots.iter_mut().flatten() {
            s.received |= got(s.frame);
        }
    }

    /// The newest frame the viewer is known to have received.
    pub fn newest_received(&self) -> Option<FrameRef> {
        self.sent.iter().rev().find(|s| s.received).map(|s| s.frame)
    }

    /// Continuing from `from`, which the viewer holds: what to invalidate.
    /// `None` when it isn't among the frames remembered (sent before the
    /// last keyframe, too long ago, more than [`MAX_AHEAD`] frames back, or
    /// itself invalidated), or sent more than [`MAX_AGE`] before `now`: a
    /// keyframe then. Forgets the frames after it.
    pub fn plan(&mut self, from: FrameRef, now: Instant) -> Option<Plan> {
        let at = self.sent.iter().position(|s| s.frame == from)?;
        // The continuing frame is the next encoded.
        if self.encoded - self.sent[at].index > MAX_AHEAD
            || now.saturating_duration_since(self.sent[at].at) > MAX_AGE
        {
            return None;
        }
        self.received(from);
        let reach = self.sent.len().saturating_sub(REACH);
        let marked = |f: FrameRef| self.slots.iter().flatten().any(|s| s.frame == f);
        let invalidate = self
            .sent
            .iter()
            .enumerate()
            .skip(at + 1)
            .filter(|&(i, s)| i >= reach || marked(s.frame))
            .map(|(_, s)| s.timestamp)
            .collect();
        let gone: Vec<FrameRef> = self.sent.drain(at + 1..).map(|s| s.frame).collect();
        for slot in &mut self.slots {
            if slot.is_some_and(|s| gone.contains(&s.frame)) {
                *slot = None;
            }
        }
        if self.last_mark.is_some_and(|(f, _)| gone.contains(&f)) {
            self.last_mark = None;
        }
        Some(Plan { from, invalidate })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(epoch: u32, seq: u32) -> FrameRef {
        FrameRef { epoch, seq }
    }

    /// Sends frames `seqs` of `epoch`, one every 17 ms from `t`, marking as
    /// [`Continuity::mark`] says; timestamps are `epoch * 1000 + seq`.
    fn send(c: &mut Continuity, epoch: u32, seqs: std::ops::Range<u32>, t: &mut Instant) {
        send_every(c, epoch, seqs, t, Duration::from_millis(17));
    }

    /// As [`send`], a frame every `step`.
    fn send_every(
        c: &mut Continuity,
        epoch: u32,
        seqs: std::ops::Range<u32>,
        t: &mut Instant,
        step: Duration,
    ) {
        for seq in seqs {
            let marked = c.mark(*t);
            let long_term = LongTerm {
                timestamp: u64::from(epoch * 1000 + seq),
                marked,
            };
            assert!(c.sent(f(epoch, seq), epoch == 1 && seq == 0, long_term, *t));
            *t += step;
        }
    }

    #[test]
    fn marks_wait_for_the_last_to_be_received_and_alternate() {
        let mut c = Continuity::new();
        let mut t = Instant::now();
        assert_eq!(c.mark(t), Some(0), "the first frame is marked");
        send(&mut c, 1, 0..1, &mut t);
        // Not received yet: no new mark, however long it takes.
        t += Duration::from_secs(1);
        assert_eq!(c.mark(t), None);
        c.received(f(1, 0));
        // Slot 0 holds what the viewer has: the next goes into slot 1.
        assert_eq!(c.mark(t), Some(1));
        send(&mut c, 1, 1..2, &mut t);
        c.received(f(1, 1));
        // Too soon after the last.
        assert_eq!(c.mark(t), None);
        t += MARK_EVERY;
        assert_eq!(c.mark(t), Some(0), "slot 1 holds the newest received");
    }

    #[test]
    fn continuing_invalidates_what_the_encoder_may_still_refer_to() {
        let mut c = Continuity::new();
        let mut t = Instant::now();
        send(&mut c, 1, 0..40, &mut t);
        c.received(f(1, 20));
        // The viewer got up to 20; frames 21 to 39 went nowhere.
        let plan = c.plan(f(1, 20), t).expect("frame 20 is remembered");
        assert_eq!(plan.from, f(1, 20));
        // The newest 16 (24..40), and any long-term frame after 20; nothing
        // the viewer holds.
        let got = &plan.invalidate;
        assert!(
            got.iter().all(|t| (1021..1040).contains(t)),
            "only frames after 20: {got:?}"
        );
        assert!(
            (1024..1040).all(|t| got.contains(&t)),
            "every frame within reach: {got:?}"
        );
        // Frames after 20 are forgotten: the next frame follows 20.
        assert_eq!(c.newest_received(), Some(f(1, 20)));
        assert!(c.plan(f(1, 30), t).is_none(), "an invalidated frame");
    }

    #[test]
    fn a_long_term_frame_after_the_point_is_invalidated_and_its_slot_freed() {
        let mut c = Continuity::new();
        let mut t = Instant::now();
        send(&mut c, 1, 0..10, &mut t);
        c.received(f(1, 9));
        // Marks go on while frames are received: the next lands soon. Frames
        // come quickly, so frame 9 isn't too old to continue from.
        send_every(&mut c, 1, 10..60, &mut t, Duration::from_millis(5));
        let marked_after: Vec<FrameRef> = c
            .slots
            .iter()
            .flatten()
            .map(|s| s.frame)
            .filter(|fr| fr.seq > 9)
            .collect();
        assert!(!marked_after.is_empty(), "a mark after frame 9");
        let plan = c.plan(f(1, 9), t).unwrap();
        for fr in &marked_after {
            assert!(
                plan.invalidate.contains(&(1000 + u64::from(fr.seq))),
                "{fr:?} invalidated"
            );
        }
        assert!(
            c.slots.iter().flatten().all(|s| s.frame.seq <= 9),
            "its slot is free"
        );
        // A mark goes into the freed slot, not the one the viewer holds.
        let held = c.held();
        let next = c.mark(t + MARK_EVERY);
        assert!(next.is_some() && next != held, "{next:?} against {held:?}");
    }

    #[test]
    fn a_keyframe_forgets_everything_before_it() {
        let mut c = Continuity::new();
        let mut t = Instant::now();
        send(&mut c, 1, 0..20, &mut t);
        c.received(f(1, 19));
        // Epoch 2 starts with a keyframe (no continuation).
        let long_term = LongTerm {
            timestamp: 2000,
            marked: Some(0),
        };
        assert!(c.sent(f(2, 0), true, long_term, t));
        assert!(c.plan(f(1, 19), t).is_none());
        assert_eq!(c.newest_received(), None);
    }

    #[test]
    fn receipt_carries_across_a_continuation() {
        let mut c = Continuity::new();
        let mut t = Instant::now();
        send(&mut c, 1, 0..30, &mut t);
        c.received(f(1, 25));
        let plan = c.plan(f(1, 25), t).unwrap();
        assert!(!plan.invalidate.is_empty());
        // Epoch 2 continues from (1, 25).
        send(&mut c, 2, 0..10, &mut t);
        assert_eq!(c.newest_received(), Some(f(1, 25)));
        c.received(f(2, 4));
        assert_eq!(c.newest_received(), Some(f(2, 4)));
        // Continuing again, from the new epoch.
        let again = c.plan(f(2, 4), t).unwrap();
        assert!(again.invalidate.contains(&2009));
        assert!(!again.invalidate.contains(&1025));
    }

    #[test]
    fn frames_too_old_are_forgotten() {
        let mut c = Continuity::new();
        let mut t = Instant::now();
        send_every(
            &mut c,
            1,
            0..(KEPT as u32 + 50),
            &mut t,
            Duration::from_millis(1),
        );
        assert!(c.plan(f(1, 10), t).is_none());
        assert!(c.plan(f(1, KEPT as u32), t).is_some());
    }

    /// An HEVC decoder would misplace a continuation more than 128 frames
    /// after the last it decoded: past [`MAX_AHEAD`], a keyframe.
    #[test]
    fn a_point_too_far_back_gets_a_keyframe() {
        let mut c = Continuity::new();
        let mut t = Instant::now();
        send_every(&mut c, 1, 0..150, &mut t, Duration::from_millis(1));
        let ahead = |seq: u32| 150 - u64::from(seq);
        assert!(ahead(49) > MAX_AHEAD && c.plan(f(1, 49), t).is_none());
        assert!(ahead(50) == MAX_AHEAD && c.plan(f(1, 50), t).is_some());
        // Frames invalidated still count: the encoder numbered them.
        send_every(&mut c, 2, 0..60, &mut t, Duration::from_millis(1));
        assert!(c.plan(f(1, 50), t).is_none(), "160 frames on");
    }

    /// Past [`MAX_AGE`], the game moved too far for the old frame: a
    /// keyframe, however few frames were encoded since.
    #[test]
    fn a_point_too_old_gets_a_keyframe() {
        let mut c = Continuity::new();
        let mut t = Instant::now();
        send(&mut c, 1, 0..20, &mut t);
        let sent = t - Duration::from_millis(17);
        assert!(c.plan(f(1, 19), sent + MAX_AGE).is_some());
        // A paused viewer: nothing encoded, but time goes on.
        let later = sent + MAX_AGE + Duration::from_millis(1);
        assert!(c.plan(f(1, 19), later).is_none());
    }

    /// A long-term slot beyond the two can't be followed: stop continuing.
    #[test]
    fn an_unknown_slot_stops_continuing() {
        let mut c = Continuity::new();
        let long_term = LongTerm {
            timestamp: 1,
            marked: Some(2),
        };
        assert!(!c.sent(f(1, 0), true, long_term, Instant::now()));
    }

    /// A viewer's word about a frame it holds (`FellBehind`) counts only for
    /// a frame sent to it and still remembered.
    #[test]
    fn a_frame_never_sent_is_no_point_to_continue_from() {
        let mut c = Continuity::new();
        let mut t = Instant::now();
        send(&mut c, 1, 0..10, &mut t);
        assert!(c.plan(f(1, 10), t).is_none());
        assert!(c.plan(f(7, 3), t).is_none());
        assert_eq!(c.plan(f(1, 3), t).map(|p| p.from), Some(f(1, 3)));
    }
}
