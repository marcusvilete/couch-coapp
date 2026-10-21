//! The host's own controller, read on the host so it can play through a seat
//! like a guest's (docs/protocol.md, Seats).
//!
//! Its input still reaches a virtual pad only through the seat router, as the
//! host's holder: the reader only reports what the controller does. Since
//! M22 it may be another program's virtual pad (a remapper's), read like a
//! physical controller.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{DEV_PAD_IDS, InputError, PadState, Rumble, XBOX_360_IDS};

/// A followed controller's state is reported this often even when nothing
/// changes, like a viewer's pad: the seat router's dead-man timer then never
/// takes a quiet host for a gone one.
pub const LOCAL_PAD_EVERY: Duration = Duration::from_millis(100);

/// Which controller, by its USB ids. They survive reconnecting it, which the
/// host is asked to do once it is hidden from games.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ControllerIds {
    pub vendor: u16,
    pub product: u16,
}

impl ControllerIds {
    /// Our virtual pads report a real Xbox 360 controller's ids (or the
    /// development ids), so by its ids alone a pad with them can't be told
    /// from ours. Since P9 where it comes from decides
    /// (`couch_platform_win::pad_origin`); this is the rule where that can't
    /// be told: such a pad is never read as the host's, nor hidden, unless
    /// it is proven another program's (M22).
    pub fn could_be_ours(self) -> bool {
        let ids = (self.vendor, self.product);
        ids == XBOX_360_IDS || ids == DEV_PAD_IDS
    }
}

/// What a host's controller is (M22).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ControllerKind {
    /// A controller plugged into the PC, or one whose origin can't be told
    /// that hasn't our pads' ids (P9's rule).
    #[default]
    Physical,
    /// Another program's virtual pad (a remapper's, DS4Windows's say),
    /// proven not ours: a pad of ours, or one in doubt, is never listed.
    OtherVirtual,
}

/// Which of the host's controllers: its ids and kind. A remapper's pad has
/// our pads' ids, so the ids alone can't say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ControllerChoice {
    pub ids: ControllerIds,
    pub kind: ControllerKind,
}

/// A controller plugged into the host that can play a seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalController {
    pub ids: ControllerIds,
    pub name: String,
    pub kind: ControllerKind,
    /// Another program's virtual pad's device (its instance id), which
    /// hiding takes; `None` for a physical controller, hidden by its ids.
    pub device: Option<String>,
}

/// Reads the host's controllers, never our own virtual pads.
pub trait LocalPads: Send + Sync {
    /// The controllers plugged in now, in the order follows take them.
    fn list(&self) -> Result<Vec<LocalController>, InputError>;

    /// Follows the first controller of `choice` that no other follow reads
    /// (M23: two of one model go to two follows, in the order found), until
    /// the returned handle drops: `on_state` gets its state on every change
    /// and every [`LOCAL_PAD_EVERY`], neutral while none is plugged in. For
    /// another program's virtual pad, `on_device` hears of each device it
    /// reads (M22: hiding follows it), `None` when it goes. Both run on the
    /// reader's thread and must return at once.
    fn follow(
        &self,
        choice: ControllerChoice,
        on_state: Box<dyn Fn(PadState) + Send>,
        on_device: Box<dyn Fn(Option<String>) + Send>,
    ) -> Result<Box<dyn LocalPad>, InputError>;
}

/// A followed controller; dropping it stops following.
pub trait LocalPad: Send {
    /// Shakes the controller, for about a second unless renewed.
    fn rumble(&self, rumble: Rumble);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_ids_are_never_the_hosts() {
        let ids = |(vendor, product)| ControllerIds { vendor, product };
        assert!(ids(XBOX_360_IDS).could_be_ours());
        assert!(ids(DEV_PAD_IDS).could_be_ours());
        // A DualSense.
        assert!(!ids((0x054C, 0x0CE6)).could_be_ours());
    }
}
