//! What the decoder holds (M12d-2, ADR 0016): the frames it decoded since it
//! last broke. An epoch that continues from one of them decodes on; one that
//! continues from any other frame can't, and the window asks for a keyframe.
//!
//! The decoder breaks on a decode error, a reopen (another codec, D3D12VA
//! given up) and a new window. A keyframe starts the chain again, since it
//! empties the decoder's references. Frames dropped undecoded while waiting
//! for a fresh start don't break it: the decoder still holds what it decoded.

use std::collections::VecDeque;

use couch_proto::media::FrameRef;

/// Frames remembered: more than the host's (5 s at 120 fps), so a frame the
/// host may continue from is never forgotten here first.
const KEPT: usize = 1024;

#[derive(Debug, Default)]
pub(crate) struct Chain {
    decoded: VecDeque<FrameRef>,
}

impl Chain {
    /// The decoder decoded `frame` cleanly.
    pub(crate) fn decoded(&mut self, frame: FrameRef, keyframe: bool) {
        if keyframe {
            self.decoded.clear();
        }
        self.decoded.push_back(frame);
        if self.decoded.len() > KEPT {
            self.decoded.pop_front();
        }
    }

    /// The decoder broke, or was replaced: it holds nothing an epoch could
    /// continue from.
    pub(crate) fn broke(&mut self) {
        self.decoded.clear();
    }

    /// Whether an epoch may continue from `frame` here.
    pub(crate) fn holds(&self, frame: FrameRef) -> bool {
        self.decoded.contains(&frame)
    }

    /// The last frame decoded, which a window that fell behind asks to
    /// continue from.
    pub(crate) fn last(&self) -> Option<FrameRef> {
        self.decoded.back().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(epoch: u32, seq: u32) -> FrameRef {
        FrameRef { epoch, seq }
    }

    #[test]
    fn it_holds_what_it_decoded_since_the_last_keyframe() {
        let mut chain = Chain::default();
        assert_eq!(chain.last(), None);
        for seq in 0..10 {
            chain.decoded(f(1, seq), seq == 0);
        }
        assert!(chain.holds(f(1, 0)) && chain.holds(f(1, 9)));
        assert!(!chain.holds(f(1, 10)), "never decoded");
        // An epoch continuing from (1, 5), whose frames decode on.
        chain.decoded(f(2, 0), false);
        assert!(chain.holds(f(1, 5)) && chain.holds(f(2, 0)));
        assert_eq!(chain.last(), Some(f(2, 0)));
        // A keyframe empties the decoder's references.
        chain.decoded(f(3, 0), true);
        assert!(!chain.holds(f(2, 0)));
        assert_eq!(chain.last(), Some(f(3, 0)));
    }

    #[test]
    fn a_broken_decoder_holds_nothing() {
        let mut chain = Chain::default();
        chain.decoded(f(1, 0), true);
        chain.broke();
        assert!(!chain.holds(f(1, 0)));
        assert_eq!(chain.last(), None);
    }

    #[test]
    fn the_oldest_frames_are_forgotten() {
        let mut chain = Chain::default();
        for seq in 0..(KEPT as u32 + 10) {
            chain.decoded(f(1, seq), seq == 0);
        }
        assert!(!chain.holds(f(1, 5)));
        assert!(chain.holds(f(1, 20)));
    }
}
