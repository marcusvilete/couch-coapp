//! The host's own controller, read on the host so it can play through a seat
//! like a guest's (docs/protocol.md, Seats).
//!
//! Its input still reaches a virtual pad only through the seat router, as the
//! host's holder: the reader only reports what the controller does.

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
    /// be told: such a pad is never read as the host's, nor hidden.
    pub fn could_be_ours(self) -> bool {
        let ids = (self.vendor, self.product);
        ids == XBOX_360_IDS || ids == DEV_PAD_IDS
    }
}

/// A physical controller plugged into the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalController {
    pub ids: ControllerIds,
    pub name: String,
}

/// Reads the host's physical controllers, never our own virtual pads.
pub trait LocalPads: Send + Sync {
    /// The controllers plugged in now.
    fn list(&self) -> Result<Vec<LocalController>, InputError>;

    /// Follows the first controller with `ids` until the returned handle
    /// drops: `on_state` gets its state on every change and every
    /// [`LOCAL_PAD_EVERY`], neutral while none is plugged in. It runs on the
    /// reader's thread and must return at once.
    fn follow(
        &self,
        ids: ControllerIds,
        on_state: Box<dyn Fn(PadState) + Send>,
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
