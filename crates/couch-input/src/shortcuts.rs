//! Every keyboard shortcut the app reacts to, in one table. The viewer and
//! the host's panic-key hook read their keys from here, and the app lists
//! them, so a shortcut is never described in one place and bound in another.
//!
//! Shortcuts are physical keys (HID usages), like forwarded keys. The chord
//! ones hold Ctrl, Alt and Shift (either side of each); those are never
//! forwarded to the host. The table is fixed for now; editing it is planned.

use crate::HidUsage;

/// Something a shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shortcut {
    /// Viewer: show or hide the overlay.
    ToggleHud,
    /// Viewer: free the mouse, or capture it again.
    FreeMouse,
    /// Viewer: ask for the controller, withdraw the request, or give it back.
    ToggleControl,
    /// Viewer: full screen, or back to a window.
    Fullscreen,
    /// Host: take the controller back from everyone and pause control.
    Panic,
}

/// Where a shortcut works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// In the stream window, while it has the focus.
    Viewer,
    /// Anywhere on the host's PC while streaming, even inside the game.
    Host,
}

/// One row of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub shortcut: Shortcut,
    /// Ctrl, Alt and Shift must be held.
    pub chord: bool,
    pub key: HidUsage,
    /// The key as printed on it.
    pub key_name: &'static str,
    pub action: &'static str,
    pub scope: Scope,
    /// When it applies, if not always; empty otherwise.
    pub condition: &'static str,
}

impl Binding {
    /// The keys to press, such as "Ctrl+Alt+Shift+H".
    pub fn keys(&self) -> String {
        if self.chord {
            format!("Ctrl+Alt+Shift+{}", self.key_name)
        } else {
            self.key_name.to_string()
        }
    }
}

const KEY_C: HidUsage = HidUsage(0x06);
const KEY_F: HidUsage = HidUsage(0x09);
const KEY_H: HidUsage = HidUsage(0x0B);
const KEY_M: HidUsage = HidUsage(0x10);
const KEY_BACKSPACE: HidUsage = HidUsage(0x2A);
const KEY_F3: HidUsage = HidUsage(0x3C);

/// The table, in the order the app lists it.
pub const SHORTCUTS: &[Binding] = &[
    Binding {
        shortcut: Shortcut::Panic,
        chord: true,
        key: KEY_BACKSPACE,
        key_name: "Backspace",
        action: "Take the controller back from everyone and pause control",
        scope: Scope::Host,
        condition: "while you stream; the game never sees it",
    },
    Binding {
        shortcut: Shortcut::ToggleControl,
        chord: true,
        key: KEY_C,
        key_name: "C",
        action: "Ask for the controller, withdraw the request, or give it back",
        scope: Scope::Viewer,
        condition: "",
    },
    Binding {
        shortcut: Shortcut::FreeMouse,
        chord: true,
        key: KEY_M,
        key_name: "M",
        action: "Free the mouse, or capture it again",
        scope: Scope::Viewer,
        condition: "while you may use the mouse",
    },
    Binding {
        shortcut: Shortcut::Fullscreen,
        chord: true,
        key: KEY_F,
        key_name: "F",
        action: "Full screen, or back to a window",
        scope: Scope::Viewer,
        condition: "",
    },
    Binding {
        shortcut: Shortcut::ToggleHud,
        chord: true,
        key: KEY_H,
        key_name: "H",
        action: "Show or hide the overlay",
        scope: Scope::Viewer,
        condition: "",
    },
    Binding {
        shortcut: Shortcut::ToggleHud,
        chord: false,
        key: KEY_F3,
        key_name: "F3",
        action: "Show or hide the overlay",
        scope: Scope::Viewer,
        condition: "while you aren't sending keys to the game",
    },
];

/// The viewer's shortcut for `key`, pressed with the chord held or not.
/// Conditions are the caller's to check.
pub fn viewer_shortcut(key: HidUsage, chord: bool) -> Option<Shortcut> {
    SHORTCUTS
        .iter()
        .find(|b| b.scope == Scope::Viewer && b.key == key && b.chord == chord)
        .map(|b| b.shortcut)
}

/// The (first) binding for `shortcut`.
pub fn binding(shortcut: Shortcut) -> &'static Binding {
    SHORTCUTS
        .iter()
        .find(|b| b.shortcut == shortcut)
        .expect("every shortcut is in the table")
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn no_two_rows_share_keys() {
        let mut seen = HashSet::new();
        for b in SHORTCUTS {
            assert!(seen.insert((b.chord, b.key)), "{} is bound twice", b.keys());
        }
    }

    #[test]
    fn every_shortcut_has_a_row() {
        for s in [
            Shortcut::ToggleHud,
            Shortcut::FreeMouse,
            Shortcut::ToggleControl,
            Shortcut::Fullscreen,
            Shortcut::Panic,
        ] {
            assert_eq!(binding(s).shortcut, s);
        }
    }

    #[test]
    fn unchorded_keys_are_never_ones_a_game_needs_held() {
        // Only F3 goes without the chord, and only while keys aren't sent.
        for b in SHORTCUTS.iter().filter(|b| !b.chord) {
            assert_eq!(b.scope, Scope::Viewer);
            assert!(!b.condition.is_empty());
        }
    }

    #[test]
    fn the_panic_key_is_the_hosts() {
        let panic = binding(Shortcut::Panic);
        assert_eq!(panic.keys(), "Ctrl+Alt+Shift+Backspace");
        assert_eq!(panic.scope, Scope::Host);
        assert_eq!(viewer_shortcut(panic.key, true), None);
    }

    #[test]
    fn viewer_lookup() {
        assert_eq!(viewer_shortcut(KEY_H, true), Some(Shortcut::ToggleHud));
        assert_eq!(viewer_shortcut(KEY_F3, false), Some(Shortcut::ToggleHud));
        assert_eq!(viewer_shortcut(KEY_H, false), None);
        assert_eq!(viewer_shortcut(KEY_C, true), Some(Shortcut::ToggleControl));
    }
}
