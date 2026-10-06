//! The host's panic key: a low-level keyboard hook (`WH_KEYBOARD_LL`) on a
//! thread of its own, installed while the host streams.
//!
//! - **Only physical keys count.** Injected events (`LLKHF_INJECTED`, which
//!   is what `SendInput` produces, including a guest's keys) are passed on
//!   untouched and never change what the hook believes is held. So a guest
//!   can neither press the panic key nor hold a modifier that completes it.
//!   Nor can a guest block it: Windows keeps one down bit per key for
//!   physical and injected input alike, so once a guest's event touched a
//!   modifier, the hook trusts only its own physical tracking for it.
//! - **The game never sees the key.** The chord's key-down, its repeats and
//!   its key-up are swallowed. The modifiers themselves pass through.
//! - **The hook returns quickly.** Windows drops a low-level hook that keeps
//!   it waiting, so the callback only flips a flag and sends a message.
//! - **Nothing in this process may register for raw keyboard input.** While
//!   a process that did is in front, Windows skips that process's own
//!   low-level keyboard hooks (measured in M5b). Tauri registers by default,
//!   so the desktop app turns it off (`device_event_filter`); otherwise the
//!   panic key went blind exactly while the app had the focus.

use std::{
    cell::RefCell,
    panic::AssertUnwindSafe,
    sync::mpsc,
    thread::{self, JoinHandle},
};

use windows::{
    Win32::{
        Foundation::{E_INVALIDARG, LPARAM, LRESULT, WPARAM},
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{
            Input::KeyboardAndMouse::GetAsyncKeyState,
            WindowsAndMessaging::{
                CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED,
                LLKHF_INJECTED, LLKHF_UP, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
                SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_QUIT,
            },
        },
    },
    core::Error,
};

use crate::keymap::{KeyCode, key_code};

const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;

/// The installed hook. Dropping it removes the hook and ends its thread.
pub struct PanicKey {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl PanicKey {
    /// Hooks Ctrl+Alt+Shift+`key` (a HID usage). `on_press` runs on the
    /// hook's thread, once per press; it must return at once.
    pub fn install(key: u16, on_press: impl Fn() + Send + 'static) -> windows::core::Result<Self> {
        let Some(KeyCode::Scan { code, extended }) = key_code(key) else {
            return Err(Error::new(E_INVALIDARG, "the panic key has no scancode"));
        };
        let chord = Chord::new(code, extended);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("panic-key".into())
            .spawn(move || hook_thread(chord, Box::new(on_press), &ready_tx))
            .map_err(|e| Error::new(E_INVALIDARG, e.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(Error::new(E_INVALIDARG, "the panic-key thread died"))
            }
        }
    }
}

impl Drop for PanicKey {
    fn drop(&mut self) {
        // SAFETY: posting a message to a thread id has no memory effects;
        // the thread made its queue before reporting its id.
        if let Err(e) = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }
        {
            tracing::warn!("stopping the panic-key hook: {e}");
            return;
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct HookState {
    chord: Chord,
    on_press: Box<dyn Fn() + Send>,
}

thread_local! {
    /// The hook's state. A low-level hook is called on the thread that
    /// installed it, from that thread's message loop, so this is it.
    static HOOK: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

fn hook_thread(
    chord: Chord,
    on_press: Box<dyn Fn() + Send>,
    ready: &mpsc::SyncSender<windows::core::Result<u32>>,
) {
    let mut msg = MSG::default();
    // SAFETY: `msg` is a valid MSG to write to. This call only makes the
    // thread's message queue, so `Drop` can post WM_QUIT to it.
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
    HOOK.with(|h| *h.borrow_mut() = Some(HookState { chord, on_press }));
    // SAFETY: `hook_proc` matches HOOKPROC and lives for the program; the
    // module handle is our own exe's, which contains it.
    let hook = unsafe { GetModuleHandleW(None) }.and_then(|module| unsafe {
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), Some(module.into()), 0)
    });
    let hook = match hook {
        Ok(hook) => hook,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    // SAFETY: no preconditions.
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
    // The hook is called from inside GetMessageW. No windows live on this
    // thread, so there is nothing to dispatch; WM_QUIT ends the loop (0),
    // as does an error (-1).
    // SAFETY: `msg` is a valid MSG to write to.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {}
    // SAFETY: `hook` was installed above and not yet removed.
    if let Err(e) = unsafe { UnhookWindowsHookEx(hook) } {
        tracing::warn!("removing the panic-key hook: {e}");
    }
    HOOK.with(|h| h.borrow_mut().take());
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for WH_KEYBOARD_LL and HC_ACTION, `lparam` points at a
        // KBDLLHOOKSTRUCT valid for the duration of this call.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let event = KeyEvent {
            vk: info.vkCode,
            scan: info.scanCode,
            extended: info.flags.contains(LLKHF_EXTENDED),
            down: !info.flags.contains(LLKHF_UP),
            injected: info.flags.contains(LLKHF_INJECTED),
            time_ms: info.time,
        };
        // Never panics out of the callback: a busy RefCell (it can't be, on
        // one thread without reentry) or a panic in `on_press` passes the key
        // on. Nothing here logs on the way: a log write can block (a paused
        // console), and every key on the PC waits for this function.
        let verdict = HOOK.with(|h| {
            let Ok(mut state) = h.try_borrow_mut() else {
                return Verdict::Pass;
            };
            let Some(state) = state.as_mut() else {
                return Verdict::Pass;
            };
            let verdict = state.chord.key(event, os_down);
            if verdict == Verdict::Pressed {
                let on_press = &state.on_press;
                if std::panic::catch_unwind(AssertUnwindSafe(on_press)).is_err() {
                    tracing::error!("the panic key's handler panicked");
                }
            }
            verdict
        });
        if verdict != Verdict::Pass {
            return LRESULT(1);
        }
    }
    // SAFETY: passing the hook's own arguments on, as every hook must.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Windows' own view of a key, physical or injected.
fn os_down(vk: u32) -> bool {
    // SAFETY: no preconditions; any virtual-key code is accepted.
    (unsafe { GetAsyncKeyState(vk as i32) } as u16) & 0x8000 != 0
}

#[derive(Debug, Clone, Copy)]
struct KeyEvent {
    vk: u32,
    scan: u32,
    extended: bool,
    down: bool,
    injected: bool,
    /// The event's time stamp (`KBDLLHOOKSTRUCT::time`), milliseconds.
    time_ms: u32,
}

/// A held key's auto-repeat follows within Windows' longest repeat delay
/// (1 s); a down after a longer gap is a new press whose key-up was lost.
const REPEAT_GAP_MS: u32 = 1_100;

/// Generic (sideless) modifier codes. Injected input may carry these.
const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pass,
    /// The chord's key, repeating or going up: the game mustn't see it.
    Swallow,
    /// The panic key was pressed: act, and swallow it.
    Pressed,
}

/// One modifier key (one side), as the hook sees it.
#[derive(Debug, Default, Clone, Copy)]
struct Modifier {
    /// Physically held, from physical events only.
    held: bool,
    /// An injected event touched this key since its physical press. Windows
    /// keeps one down bit per key for physical and injected input alike, so
    /// a guest's injected key-up clears it while the host still holds the
    /// key: Windows' view can't be trusted for it until the next physical
    /// press.
    injected_since: bool,
}

/// What the chord's own key is doing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum KeyState {
    #[default]
    Up,
    /// It went down as the panic key: its repeats and release are ours.
    Ours,
    /// It went down without the chord: the game has it, repeats and release
    /// included, even if the chord is completed meanwhile.
    Passed,
}

/// Which modifiers are physically held, and what the chord's key is doing.
#[derive(Debug, Default)]
struct Chord {
    scan: u32,
    extended: bool,
    /// Indexed by `vk - VK_LSHIFT`: left and right Shift, Ctrl, Alt.
    modifiers: [Modifier; 6],
    key: KeyState,
    /// When the chord's key last did anything.
    key_time_ms: u32,
}

impl Chord {
    fn new(scan: u16, extended: bool) -> Self {
        Self {
            scan: u32::from(scan),
            extended,
            ..Self::default()
        }
    }

    /// `os_down` is Windows' view of a key, physical and injected alike.
    fn key(&mut self, e: KeyEvent, os_down: impl Fn(u32) -> bool) -> Verdict {
        if let Some(sides) = modifier_sides(e.vk) {
            for i in sides {
                let m = &mut self.modifiers[i];
                if e.injected {
                    m.injected_since = true;
                } else {
                    m.held = e.down;
                    if e.down {
                        m.injected_since = false;
                    }
                }
            }
            return Verdict::Pass;
        }
        if e.injected || e.scan != self.scan || e.extended != self.extended {
            return Verdict::Pass;
        }
        // A down right after the last event is an auto-repeat; after a
        // longer gap it is a new press (the key-up before it got lost).
        let repeat = e.time_ms.wrapping_sub(self.key_time_ms) <= REPEAT_GAP_MS;
        self.key_time_ms = e.time_ms;
        match (e.down, self.key) {
            (true, KeyState::Ours) if repeat => Verdict::Swallow,
            (true, KeyState::Passed) if repeat => Verdict::Pass,
            (true, _) if self.held(&os_down) => {
                self.key = KeyState::Ours;
                Verdict::Pressed
            }
            (true, _) => {
                self.key = KeyState::Passed;
                Verdict::Pass
            }
            (false, KeyState::Ours) => {
                self.key = KeyState::Up;
                Verdict::Swallow
            }
            (false, _) => {
                self.key = KeyState::Up;
                Verdict::Pass
            }
        }
    }

    /// Ctrl, Alt and Shift each held on at least one side. A side counts if
    /// a physical event pressed it and Windows still has it down, so a
    /// key-up that never reached the hook (the secure desktop of
    /// Ctrl+Alt+Del eats them) doesn't leave it held forever; but once a
    /// guest's injected event touched it, only the physical state counts,
    /// so a guest can't release the host's modifier from under the chord.
    fn held(&self, os_down: &impl Fn(u32) -> bool) -> bool {
        let side = |vk: u32| {
            let m = self.modifiers[(vk - VK_LSHIFT) as usize];
            m.held && (m.injected_since || os_down(vk))
        };
        let either = |left: u32| side(left) || side(left + 1);
        either(VK_LCONTROL) && either(VK_LMENU) && either(VK_LSHIFT)
    }
}

/// The `modifiers` slots a key code stands for: one side, or both for the
/// generic codes.
fn modifier_sides(vk: u32) -> Option<std::ops::RangeInclusive<usize>> {
    let left = |left: u32| (left - VK_LSHIFT) as usize;
    Some(match vk {
        VK_LSHIFT..=VK_RMENU => left(vk)..=left(vk),
        VK_SHIFT => left(VK_LSHIFT)..=left(VK_RSHIFT),
        VK_CONTROL => left(VK_LCONTROL)..=left(VK_RCONTROL),
        VK_MENU => left(VK_LMENU)..=left(VK_RMENU),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BACKSPACE: u32 = 0x0E;

    fn key(vk: u32, scan: u32, down: bool) -> KeyEvent {
        KeyEvent {
            vk,
            scan,
            extended: false,
            down,
            injected: false,
            time_ms: 0,
        }
    }

    fn injected(mut e: KeyEvent) -> KeyEvent {
        e.injected = true;
        e
    }

    fn at(mut e: KeyEvent, time_ms: u32) -> KeyEvent {
        e.time_ms = time_ms;
        e
    }

    const BS_DOWN: KeyEvent = KeyEvent {
        vk: 0x08,
        scan: BACKSPACE,
        extended: false,
        down: true,
        injected: false,
        time_ms: 0,
    };
    const BS_UP: KeyEvent = KeyEvent {
        down: false,
        ..BS_DOWN
    };

    fn hold_chord(c: &mut Chord, right: bool) {
        let r = u32::from(right);
        for vk in [VK_LCONTROL + r, VK_LMENU + r, VK_LSHIFT + r] {
            assert_eq!(c.key(key(vk, 0, true), |_| true), Verdict::Pass);
        }
    }

    #[test]
    fn the_chord_presses_once_and_hides_the_key() {
        for right in [false, true] {
            let mut c = Chord::new(BACKSPACE as u16, false);
            hold_chord(&mut c, right);
            assert_eq!(c.key(BS_DOWN, |_| true), Verdict::Pressed);
            // Auto-repeat, then the release: swallowed, not pressed again.
            assert_eq!(c.key(BS_DOWN, |_| true), Verdict::Swallow);
            assert_eq!(c.key(BS_UP, |_| true), Verdict::Swallow);
            assert_eq!(c.key(BS_DOWN, |_| true), Verdict::Pressed);
        }
    }

    #[test]
    fn backspace_alone_or_with_part_of_the_chord_is_the_games() {
        let mut c = Chord::new(BACKSPACE as u16, false);
        assert_eq!(c.key(BS_DOWN, |_| true), Verdict::Pass);
        assert_eq!(c.key(BS_UP, |_| true), Verdict::Pass);
        c.key(key(VK_LCONTROL, 0, true), |_| true);
        c.key(key(VK_LSHIFT, 0, true), |_| true);
        assert_eq!(c.key(BS_DOWN, |_| true), Verdict::Pass);
        assert_eq!(c.key(BS_UP, |_| true), Verdict::Pass);
    }

    #[test]
    fn injected_keys_never_count() {
        let mut c = Chord::new(BACKSPACE as u16, false);
        // A guest's injected chord does nothing...
        for vk in [VK_LCONTROL, VK_LMENU, VK_LSHIFT] {
            assert_eq!(c.key(injected(key(vk, 0, true)), |_| true), Verdict::Pass);
        }
        assert_eq!(c.key(injected(BS_DOWN), |_| true), Verdict::Pass);
        assert_eq!(c.key(BS_DOWN, |_| true), Verdict::Pass);
        assert_eq!(c.key(BS_UP, |_| true), Verdict::Pass);
        // ...nor completes the host's half.
        c.key(key(VK_LCONTROL, 0, true), |_| true);
        c.key(key(VK_LMENU, 0, true), |_| true);
        assert_eq!(c.key(BS_DOWN, |_| true), Verdict::Pass);
        assert_eq!(c.key(BS_UP, |_| true), Verdict::Pass);
        c.key(key(VK_LSHIFT, 0, true), |_| true);
        assert_eq!(c.key(injected(BS_DOWN), |_| true), Verdict::Pass);
        assert_eq!(c.key(BS_DOWN, |_| true), Verdict::Pressed);
        // Nor can an injected key-up release a physical modifier.
        assert_eq!(
            c.key(injected(key(VK_LSHIFT, 0, false)), |_| true),
            Verdict::Pass
        );
        assert!(c.modifiers[0].held);
    }

    /// A guest's injected key-ups clear Windows' down bit for that key while
    /// the host still holds it: the chord must still go through.
    #[test]
    fn a_guests_modifier_key_ups_cannot_block_it() {
        for vk in [VK_LSHIFT, VK_LCONTROL, VK_LMENU, VK_SHIFT, VK_CONTROL] {
            let mut c = Chord::new(BACKSPACE as u16, false);
            hold_chord(&mut c, false);
            c.key(injected(key(vk, 0, true)), |_| true);
            c.key(injected(key(vk, 0, false)), |_| true);
            // Windows now has that modifier up, on both sides.
            let cleared = |k: u32| match vk {
                VK_SHIFT => !(VK_LSHIFT..=VK_RSHIFT).contains(&k),
                VK_CONTROL => !(VK_LCONTROL..=VK_RCONTROL).contains(&k),
                _ => k != vk,
            };
            assert_eq!(c.key(BS_DOWN, cleared), Verdict::Pressed, "vk {vk:#x}");
        }
    }

    /// Backspace already held when the chord is completed belongs to the
    /// game: its repeats and its release pass, so it never sticks down.
    #[test]
    fn a_backspace_held_before_the_chord_stays_the_games() {
        let mut c = Chord::new(BACKSPACE as u16, false);
        assert_eq!(c.key(at(BS_DOWN, 0), |_| true), Verdict::Pass);
        hold_chord(&mut c, false);
        assert_eq!(c.key(at(BS_DOWN, 500), |_| true), Verdict::Pass);
        assert_eq!(c.key(at(BS_DOWN, 533), |_| true), Verdict::Pass);
        assert_eq!(c.key(at(BS_UP, 560), |_| true), Verdict::Pass);
        // A fresh press with the chord held is the panic key.
        assert_eq!(c.key(at(BS_DOWN, 900), |_| true), Verdict::Pressed);
    }

    /// If the panic key's key-up never arrived, the next press long after
    /// is a new press, not a repeat.
    #[test]
    fn a_lost_key_up_costs_no_press() {
        let mut c = Chord::new(BACKSPACE as u16, false);
        hold_chord(&mut c, false);
        assert_eq!(c.key(at(BS_DOWN, 1_000), |_| true), Verdict::Pressed);
        assert_eq!(c.key(at(BS_DOWN, 1_500), |_| true), Verdict::Swallow);
        assert_eq!(c.key(at(BS_DOWN, 5_000), |_| true), Verdict::Pressed);
    }

    #[test]
    fn a_lost_key_up_does_not_hold_a_modifier_forever() {
        let mut c = Chord::new(BACKSPACE as u16, false);
        hold_chord(&mut c, false);
        // Ctrl and Alt came up on the secure desktop; Windows knows.
        let only_shift = |vk| vk == VK_LSHIFT;
        assert_eq!(c.key(BS_DOWN, only_shift), Verdict::Pass);
    }

    #[test]
    fn releasing_a_modifier_ends_the_chord() {
        let mut c = Chord::new(BACKSPACE as u16, false);
        hold_chord(&mut c, false);
        c.key(key(VK_LMENU, 0, false), |_| true);
        assert_eq!(c.key(BS_DOWN, |_| true), Verdict::Pass);
    }

    /// The real hook goes in and comes out promptly. (Pressing it needs a
    /// person: it ignores injected keys by design.)
    #[test]
    fn installs_and_removes() {
        let started = std::time::Instant::now();
        let hook = PanicKey::install(0x2A, || {}).expect("installing the hook");
        drop(hook);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert!(
            PanicKey::install(0x48, || {}).is_err(),
            "Pause has no plain scancode"
        );
    }

    #[test]
    fn the_extended_twin_is_another_key() {
        // Numpad Enter is Enter's scancode with E0: not the same key.
        let mut c = Chord::new(0x1C, false);
        hold_chord(&mut c, false);
        let mut numpad_enter = key(0x0D, 0x1C, true);
        numpad_enter.extended = true;
        assert_eq!(c.key(numpad_enter, |_| true), Verdict::Pass);
    }
}
