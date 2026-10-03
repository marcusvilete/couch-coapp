//! USB HID keyboard usages to PC scancodes (set 1), which `SendInput` takes.
//! Games read scancodes, so a key keeps its position whatever the host's
//! layout: WASD stays WASD.
//!
//! From Microsoft's "Keyboard Scan Code Specification" HID-to-PS/2 table.
//! Media and power keys are left out on purpose: a guest has no business
//! changing the host's volume or putting it to sleep.

/// How to inject one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// A scancode, with the E0 prefix ("extended") or not.
    Scan { code: u16, extended: bool },
    /// Keys whose scancode sequences don't fit `SendInput` (Pause's is E1 1D
    /// 45; Num Lock shares its 45). Injected by virtual-key code instead.
    Vk(u16),
}

const VK_PAUSE: u16 = 0x13;
const VK_NUMLOCK: u16 = 0x90;

/// The injection for a keyboard usage, or `None` for keys we don't send.
pub fn key_code(usage: u16) -> Option<KeyCode> {
    let plain = |code| KeyCode::Scan {
        code,
        extended: false,
    };
    let ext = |code| KeyCode::Scan {
        code,
        extended: true,
    };
    Some(match usage {
        // Letters A to Z.
        0x04..=0x1D => plain(
            [
                0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31,
                0x18, 0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
            ][usize::from(usage - 0x04)],
        ),
        // 1 to 9, then 0.
        0x1E..=0x27 => plain(usage - 0x1E + 0x02),
        0x28 => plain(0x1C), // Enter
        0x29 => plain(0x01), // Escape
        0x2A => plain(0x0E), // Backspace
        0x2B => plain(0x0F), // Tab
        0x2C => plain(0x39), // Space
        0x2D => plain(0x0C), // - _
        0x2E => plain(0x0D), // = +
        0x2F => plain(0x1A), // [ {
        0x30 => plain(0x1B), // ] }
        0x31 => plain(0x2B), // \ |
        0x32 => plain(0x2B), // non-US # ~ (same position as \ on ISO boards)
        0x33 => plain(0x27), // ; :
        0x34 => plain(0x28), // ' "
        0x35 => plain(0x29), // ` ~
        0x36 => plain(0x33), // , <
        0x37 => plain(0x34), // . >
        0x38 => plain(0x35), // / ?
        0x39 => plain(0x3A), // Caps Lock
        // F1 to F10, then F11 and F12.
        0x3A..=0x43 => plain(usage - 0x3A + 0x3B),
        0x44 => plain(0x57),
        0x45 => plain(0x58),
        0x46 => ext(0x37),   // Print Screen
        0x47 => plain(0x46), // Scroll Lock
        0x48 => KeyCode::Vk(VK_PAUSE),
        0x49 => ext(0x52), // Insert
        0x4A => ext(0x47), // Home
        0x4B => ext(0x49), // Page Up
        0x4C => ext(0x53), // Delete
        0x4D => ext(0x4F), // End
        0x4E => ext(0x51), // Page Down
        0x4F => ext(0x4D), // Right
        0x50 => ext(0x4B), // Left
        0x51 => ext(0x50), // Down
        0x52 => ext(0x48), // Up
        0x53 => KeyCode::Vk(VK_NUMLOCK),
        0x54 => ext(0x35),   // keypad /
        0x55 => plain(0x37), // keypad *
        0x56 => plain(0x4A), // keypad -
        0x57 => plain(0x4E), // keypad +
        0x58 => ext(0x1C),   // keypad Enter
        // Keypad 1 to 9, 0 and the decimal point.
        0x59..=0x63 => plain(
            [
                0x4F, 0x50, 0x51, 0x4B, 0x4C, 0x4D, 0x47, 0x48, 0x49, 0x52, 0x53,
            ][usize::from(usage - 0x59)],
        ),
        0x64 => plain(0x56), // non-US \ | (the extra key by left Shift on ISO boards)
        0x65 => ext(0x5D),   // Application (menu)
        0x67 => plain(0x59), // keypad =
        // F13 to F23, then F24.
        0x68..=0x72 => plain(usage - 0x68 + 0x64),
        0x73 => plain(0x76),
        0x85 => plain(0x7E), // keypad , (ABNT2 keypad .)
        0x87 => plain(0x73), // International1: Ro, and ABNT2's / ? key
        0x88 => plain(0x70), // International2: Katakana/Hiragana
        0x89 => plain(0x7D), // International3: Yen
        0x8A => plain(0x79), // International4: Henkan
        0x8B => plain(0x7B), // International5: Muhenkan
        0xE0 => plain(0x1D), // Left Ctrl
        0xE1 => plain(0x2A), // Left Shift
        0xE2 => plain(0x38), // Left Alt
        0xE3 => ext(0x5B),   // Left Windows
        0xE4 => ext(0x1D),   // Right Ctrl
        0xE5 => plain(0x36), // Right Shift
        0xE6 => ext(0x38),   // Right Alt (AltGr)
        0xE7 => ext(0x5C),   // Right Windows
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(usage: u16) -> (u16, bool) {
        match key_code(usage) {
            Some(KeyCode::Scan { code, extended }) => (code, extended),
            other => panic!("{usage:#x}: {other:?}"),
        }
    }

    #[test]
    fn known_positions() {
        assert_eq!(scan(0x1A), (0x11, false)); // W
        assert_eq!(scan(0x04), (0x1E, false)); // A
        assert_eq!(scan(0x16), (0x1F, false)); // S
        assert_eq!(scan(0x07), (0x20, false)); // D
        assert_eq!(scan(0x1D), (0x2C, false)); // Z
        assert_eq!(scan(0x1E), (0x02, false)); // 1
        assert_eq!(scan(0x27), (0x0B, false)); // 0
        assert_eq!(scan(0x43), (0x44, false)); // F10
        assert_eq!(scan(0x52), (0x48, true)); // Up
        assert_eq!(scan(0x62), (0x52, false)); // keypad 0
        assert_eq!(scan(0x63), (0x53, false)); // keypad .
        assert_eq!(scan(0x72), (0x6E, false)); // F23
        assert_eq!(scan(0xE6), (0x38, true)); // AltGr
    }

    #[test]
    fn scancodes_are_unique_except_the_iso_hash_key() {
        let mut seen = std::collections::HashMap::new();
        for usage in 0..=0xFF {
            if let Some(code) = key_code(usage)
                && let Some(previous) = seen.insert(code, usage)
            {
                assert_eq!((previous, usage), (0x31, 0x32), "{code:?}");
            }
        }
    }

    #[test]
    fn media_keys_are_not_sent() {
        for usage in [0x66, 0x7F, 0x80, 0x81] {
            assert_eq!(key_code(usage), None, "{usage:#x}");
        }
    }
}
