//! What a remote member may never press, whatever their permissions
//! (docs/architecture.md, "Safety invariants"): the Windows keys, and the
//! combinations that leave the game or close it: Alt+Tab, Alt+Esc, Ctrl+Esc,
//! Ctrl+Shift+Esc and Alt+F4. A guest quits through the game's own menu; an
//! accidental Alt+F4 would end everyone's session. Alt+Space, Alt+F1 and
//! Alt+F2 (the window menu with its Close, the desktop's launcher and run
//! dialogs) too. And Linux's (M16c-2),
//! blocked everywhere: Ctrl+Alt+F1 to F12 (switching to a text console),
//! Ctrl+Alt+Backspace (ending the desktop, where it's on) and
//! Ctrl+Alt+Delete (the desktop's log-out dialog). The gamepad's Guide
//! button is cleared in the router.
//!
//! Where the host's keys can't tell injected keys from real ones (Linux),
//! the router also refuses the host's own chords ([`host_chord`]).

use std::collections::BTreeSet;

use crate::HidUsage;

pub const ESCAPE: HidUsage = HidUsage(0x29);
pub const BACKSPACE: HidUsage = HidUsage(0x2A);
pub const TAB: HidUsage = HidUsage(0x2B);
pub const SPACE: HidUsage = HidUsage(0x2C);
pub const F1: HidUsage = HidUsage(0x3A);
pub const F2: HidUsage = HidUsage(0x3B);
pub const F3: HidUsage = HidUsage(0x3C);
pub const F7: HidUsage = HidUsage(0x40);
pub const F8: HidUsage = HidUsage(0x41);
pub const F10: HidUsage = HidUsage(0x43);
pub const L: HidUsage = HidUsage(0x0F);
pub const T: HidUsage = HidUsage(0x17);
pub const F4: HidUsage = HidUsage(0x3D);
pub const F12: HidUsage = HidUsage(0x45);
pub const DELETE: HidUsage = HidUsage(0x4C);
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
        F4 => !alt,
        // The window menu (Windows, GNOME, KDE's Alt+F3; its Close), the
        // desktop's launcher or run dialog (KDE's KRunner, GNOME's Alt+F2,
        // which takes keys over the game without changing the focused
        // window).
        SPACE | F1 | F2 | F3 => !alt,
        HidUsage(code) if (F1.0..=F12.0).contains(&code) => !(ctrl && alt),
        BACKSPACE | DELETE => !(ctrl && alt),
        _ => true,
    }
}

/// Whether `key` with `held` down is one of a Linux desktop's own shortcuts
/// that would take a guest's keys off the game without another window
/// becoming active, so the helper telling what is in front couldn't see it
/// (M16c-2): Ctrl+Alt+T (a terminal, on GNOME and KDE), Ctrl+Alt+L (the
/// lock screen) and Ctrl+F7 to F12 (KDE's window overviews and the desktop);
/// and GNOME's Alt+F7 and Alt+F8 (moving and resizing the window by the
/// keyboard, which takes the keys) and Alt+F10 (maximizing it), P18's VM
/// run. Refused only on a Linux host: on Windows, Ctrl+Alt+key is
/// AltGr+key, a character on many layouts.
pub fn desktop_key(key: HidUsage, held: &BTreeSet<HidUsage>) -> bool {
    let any = |keys: [HidUsage; 2]| keys.iter().any(|k| held.contains(k));
    let ctrl = any([LEFT_CTRL, RIGHT_CTRL]);
    let alt = any([LEFT_ALT, RIGHT_ALT]);
    match key {
        T | L => ctrl && alt,
        F7 | F8 | F10 => ctrl || alt,
        HidUsage(code) if (F7.0..=F12.0).contains(&code) => ctrl,
        _ => false,
    }
}

/// Whether `key` with `held` down is one of the host's own chords:
/// Ctrl+Alt+Shift plus one of `keys` (the host's keys as offered to Linux's
/// global-shortcuts portal, `couch_input::shortcuts`). A key the user binds
/// differently in the desktop's own settings isn't known here.
pub fn host_chord(key: HidUsage, held: &BTreeSet<HidUsage>, keys: &[HidUsage]) -> bool {
    let any = |keys: [HidUsage; 2]| keys.iter().any(|k| held.contains(k));
    keys.contains(&key)
        && any([LEFT_CTRL, RIGHT_CTRL])
        && any([LEFT_ALT, RIGHT_ALT])
        && any([LEFT_SHIFT, RIGHT_SHIFT])
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
    fn alt_f4_is_blocked() {
        assert!(!key_allowed(F4, &held(&[LEFT_ALT])));
        assert!(!key_allowed(F4, &held(&[RIGHT_ALT, LEFT_SHIFT])));
        assert!(key_allowed(F4, &held(&[])));
        assert!(key_allowed(F4, &held(&[LEFT_CTRL])));
    }

    #[test]
    fn the_window_menu_and_the_desktops_launchers_are_blocked() {
        for key in [SPACE, F1, F2] {
            assert!(!key_allowed(key, &held(&[LEFT_ALT])));
            assert!(!key_allowed(key, &held(&[RIGHT_ALT, LEFT_CTRL])));
            assert!(key_allowed(key, &held(&[])));
            assert!(key_allowed(key, &held(&[LEFT_SHIFT])));
        }
        // F1 and F2 with Ctrl alone, as games use them.
        assert!(key_allowed(F1, &held(&[LEFT_CTRL])));
        // KDE's window menu.
        assert!(!key_allowed(F3, &held(&[LEFT_ALT])));
        assert!(key_allowed(F3, &held(&[])));
    }

    #[test]
    fn a_linux_desktops_own_shortcuts_are_told() {
        assert!(desktop_key(T, &held(&[LEFT_CTRL, LEFT_ALT])));
        assert!(desktop_key(L, &held(&[RIGHT_CTRL, RIGHT_ALT])));
        for f in F7.0..=F12.0 {
            assert!(desktop_key(HidUsage(f), &held(&[LEFT_CTRL])));
            assert!(!desktop_key(HidUsage(f), &held(&[])));
        }
        // The same keys otherwise, and the F-keys below F7 with Ctrl.
        assert!(!desktop_key(T, &held(&[LEFT_CTRL])));
        assert!(!desktop_key(L, &held(&[LEFT_ALT])));
        assert!(!desktop_key(T, &held(&[])));
        assert!(!desktop_key(HidUsage(F1.0 + 5), &held(&[LEFT_CTRL])));
        // GNOME's moving, resizing and maximizing by the keyboard (P18).
        for f in [F7, F8, F10] {
            assert!(desktop_key(f, &held(&[LEFT_ALT])));
            assert!(desktop_key(f, &held(&[RIGHT_ALT])));
        }
        // Alt with the other F-keys stays the game's (Alt+F4 and F1 to F3
        // are blocked everywhere, by `key_allowed`).
        for f in [
            HidUsage(0x3E),
            HidUsage(0x3F),
            HidUsage(0x42),
            HidUsage(0x44),
            F12,
        ] {
            assert!(!desktop_key(f, &held(&[LEFT_ALT])), "{f:?}");
        }
    }

    #[test]
    fn linux_console_and_session_combinations_are_blocked() {
        for f in F1.0..=F12.0 {
            assert!(!key_allowed(HidUsage(f), &held(&[LEFT_CTRL, LEFT_ALT])));
            assert!(!key_allowed(HidUsage(f), &held(&[RIGHT_CTRL, RIGHT_ALT])));
            assert!(key_allowed(HidUsage(f), &held(&[LEFT_CTRL])));
        }
        for key in [BACKSPACE, DELETE] {
            assert!(!key_allowed(key, &held(&[LEFT_CTRL, RIGHT_ALT])));
            assert!(key_allowed(key, &held(&[LEFT_CTRL])));
            assert!(key_allowed(key, &held(&[LEFT_ALT])));
            assert!(key_allowed(key, &held(&[])));
        }
        // F13 and up switch nothing.
        assert!(key_allowed(HidUsage(0x68), &held(&[LEFT_CTRL, LEFT_ALT])));
    }

    #[test]
    fn the_hosts_chords_need_all_three_modifiers() {
        let p = HidUsage(0x13);
        let keys = [p, HidUsage(0x04)];
        assert!(host_chord(
            p,
            &held(&[LEFT_CTRL, LEFT_ALT, LEFT_SHIFT]),
            &keys
        ));
        assert!(host_chord(
            p,
            &held(&[RIGHT_CTRL, RIGHT_ALT, RIGHT_SHIFT]),
            &keys
        ));
        assert!(!host_chord(p, &held(&[LEFT_CTRL, LEFT_ALT]), &keys));
        assert!(!host_chord(p, &held(&[LEFT_CTRL, LEFT_SHIFT]), &keys));
        assert!(!host_chord(
            HidUsage(0x05),
            &held(&[LEFT_CTRL, LEFT_ALT, LEFT_SHIFT]),
            &keys
        ));
        assert!(!host_chord(
            p,
            &held(&[LEFT_CTRL, LEFT_ALT, LEFT_SHIFT]),
            &[]
        ));
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
