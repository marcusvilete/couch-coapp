//! What a remote member may never press, whatever their permissions
//! (docs/architecture.md, "Safety invariants"): the Windows keys, and the
//! combinations that leave the game: Alt+Tab, Alt+Esc, Ctrl+Esc and
//! Ctrl+Shift+Esc. The gamepad's Guide button is cleared in the router.

use std::collections::BTreeSet;

use crate::HidUsage;

pub const ESCAPE: HidUsage = HidUsage(0x29);
pub const TAB: HidUsage = HidUsage(0x2B);
pub const LEFT_CTRL: HidUsage = HidUsage(0xE0);
pub const LEFT_SHIFT: HidUsage = HidUsage(0xE1);
pub const LEFT_ALT: HidUsage = HidUsage(0xE2);
pub const LEFT_GUI: HidUsage = HidUsage(0xE3);
pub const RIGHT_CTRL: HidUsage = HidUsage(0xE4);
pub const RIGHT_SHIFT: HidUsage = HidUsage(0xE5);
pub const RIGHT_ALT: HidUsage = HidUsage(0xE6);
pub const RIGHT_GUI: HidUsage = HidUsage(0xE7);

/// Whether a remote member may press `key` while `held` (the keys injected
/// for them that are still down) are down.
pub fn key_allowed(key: HidUsage, held: &BTreeSet<HidUsage>) -> bool {
    let any = |keys: [HidUsage; 2]| keys.iter().any(|k| held.contains(k));
    let alt = any([LEFT_ALT, RIGHT_ALT]);
    let ctrl = any([LEFT_CTRL, RIGHT_CTRL]);
    match key {
        LEFT_GUI | RIGHT_GUI => false,
        TAB => !alt,
        ESCAPE => !alt && !ctrl,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(keys: &[HidUsage]) -> BTreeSet<HidUsage> {
        keys.iter().copied().collect()
    }

    #[test]
    fn windows_keys_are_never_allowed() {
        assert!(!key_allowed(LEFT_GUI, &held(&[])));
        assert!(!key_allowed(RIGHT_GUI, &held(&[LEFT_SHIFT])));
    }

    #[test]
    fn switching_combinations_are_blocked() {
        assert!(!key_allowed(TAB, &held(&[LEFT_ALT])));
        assert!(!key_allowed(TAB, &held(&[RIGHT_ALT])));
        assert!(!key_allowed(ESCAPE, &held(&[LEFT_ALT])));
        assert!(!key_allowed(ESCAPE, &held(&[RIGHT_CTRL])));
        assert!(!key_allowed(ESCAPE, &held(&[LEFT_CTRL, LEFT_SHIFT])));
    }

    #[test]
    fn the_same_keys_alone_are_fine() {
        assert!(key_allowed(TAB, &held(&[])));
        assert!(key_allowed(TAB, &held(&[LEFT_SHIFT, LEFT_CTRL])));
        assert!(key_allowed(ESCAPE, &held(&[LEFT_SHIFT])));
        assert!(key_allowed(LEFT_ALT, &held(&[TAB])));
        assert!(key_allowed(HidUsage(0x1A), &held(&[LEFT_ALT, LEFT_CTRL])));
    }
}
