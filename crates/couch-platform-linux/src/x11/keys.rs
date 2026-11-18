//! The host's keys on an X11 desktop whose global-shortcuts portal can't
//! take them (M28-2, ADR 0015: Linux Mint, Xfce, MATE; KDE's portal works
//! on X11 and is tried first): a passive grab of each key with Ctrl, Alt
//! and Shift on the root window (`GrabKey`), so the X server hands us its
//! presses whatever window is in front.
//!
//! - **Each key four times,** with Caps Lock and Num Lock on or off: a
//!   grab names its exact modifiers.
//! - **A key another program holds** (the server refuses the grab) is left
//!   out of the keys in force, and said so; the console shows which are.
//! - **It hears injected keys,** as the portal does (`uinput`'s reach the X
//!   server as real ones): the engine's guards for the portal apply as they
//!   are (`Backend::host_keys_hear_injected`).
//! - **One press per press:** a held key's repeats are dropped (the server's
//!   autorepeat sends a release and a press at the same time), and so is
//!   any event another client sent.
//! - **Ungrabbed when dropped,** and by the server if this process goes.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

use x11rb::{
    connection::Connection as _,
    errors::ReplyError,
    protocol::{
        Event,
        xproto::{ConnectionExt as _, GrabMode, ModMask},
    },
};

use crate::global_keys::Event as KeyEvent;

/// How often the thread looks for presses and whether it should stop.
const WAIT: Duration = Duration::from_millis(15);
/// An event's code with this bit set came from `SendEvent`.
const SENT: u8 = 0x80;
/// The X server's keycode for an evdev key code (Xorg's evdev and libinput
/// drivers both add 8).
const EVDEV_OFFSET: u16 = 8;

/// One key to grab.
pub struct Key {
    /// Its id, as reported in [`KeyEvent::Pressed`].
    pub id: &'static str,
    /// The key, as an evdev key code (`kbm::key_code`).
    pub code: u16,
    /// How it is shown: "Ctrl+Alt+Shift+Backspace".
    pub described: String,
}

/// The keys, grabbed until this drops.
pub struct Grabbed {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Grabbed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Grabs `keys` and reports on them through `on_event`: `Bound` with those
/// in force, then each press; `Failed` when the X server goes. Blocks
/// briefly (the grabs); fails when none could be grabbed.
pub fn grab(keys: Vec<Key>, on_event: Box<dyn Fn(KeyEvent<'_>) + Send>) -> Result<Grabbed, String> {
    let (conn, root) = super::connect()?;
    let chord = ModMask::CONTROL | ModMask::M1 | ModMask::SHIFT;
    let locks = [
        ModMask::from(0u16),
        ModMask::LOCK,
        ModMask::M2,
        ModMask::LOCK | ModMask::M2,
    ];
    let mut ours: HashMap<u8, &'static str> = HashMap::new();
    let mut bound = Vec::new();
    for key in &keys {
        let Ok(keycode) = u8::try_from(key.code + EVDEV_OFFSET) else {
            continue;
        };
        let mut held = true;
        for &lock in &locks {
            let grabbed = conn
                .grab_key(
                    false,
                    root,
                    chord | lock,
                    keycode,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )
                .map_err(super::failed)?
                .check();
            match grabbed {
                Ok(()) => {}
                Err(ReplyError::X11Error(_)) => held = false,
                Err(ReplyError::ConnectionError(e)) => return Err(super::failed(e)),
            }
        }
        if held {
            ours.insert(keycode, key.id);
            bound.push((key.id.to_string(), key.described.clone()));
        } else {
            tracing::warn!(key = %key.described, "another program holds this host key");
            for &lock in &locks {
                if let Ok(cookie) = conn.ungrab_key(keycode, root, chord | lock) {
                    cookie.ignore_error();
                }
            }
        }
    }
    if bound.is_empty() {
        return Err("another program holds every one of the host's keys".into());
    }
    conn.flush().map_err(super::failed)?;
    tracing::info!(
        keys = bound.len(),
        "the host's keys grabbed from the X server"
    );
    on_event(KeyEvent::Bound(bound));
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let thread = std::thread::Builder::new()
        .name("x11-host-keys".into())
        .spawn(move || {
            let mut presses = Presses::default();
            while !stopped.load(Ordering::Relaxed) {
                loop {
                    match conn.poll_for_event() {
                        Ok(Some(event)) => {
                            if let Some(id) = presses.event(&event, &ours) {
                                on_event(KeyEvent::Pressed(id));
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            on_event(KeyEvent::Failed(format!("the X server went away: {e}")));
                            return;
                        }
                    }
                }
                std::thread::sleep(WAIT);
            }
        })
        .map_err(|e| format!("the host's keys: {e}"))?;
    Ok(Grabbed {
        stop,
        thread: Some(thread),
    })
}

/// Tells presses from repeats.
#[derive(Default)]
struct Presses {
    down: HashSet<u8>,
    /// The last release: a press of the same key at the same time is the
    /// server's autorepeat.
    released: Option<(u8, u32)>,
}

impl Presses {
    /// The id of a key of `ours` newly pressed by `event`, if it is one.
    fn event(&mut self, event: &Event, ours: &HashMap<u8, &'static str>) -> Option<&'static str> {
        match event {
            Event::KeyPress(e) if e.response_type & SENT == 0 => {
                let id = ours.get(&e.detail).copied()?;
                let repeat = self.released == Some((e.detail, e.time));
                self.released = None;
                (self.down.insert(e.detail) && !repeat).then_some(id)
            }
            Event::KeyRelease(e) if e.response_type & SENT == 0 => {
                self.down.remove(&e.detail);
                self.released = Some((e.detail, e.time));
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x11rb::protocol::xproto::{KEY_PRESS_EVENT, KEY_RELEASE_EVENT, KeyPressEvent};

    fn key(code: u8, keycode: u8, time: u32) -> Event {
        let event = KeyPressEvent {
            response_type: code,
            detail: keycode,
            sequence: 0,
            time,
            root: 1,
            event: 1,
            child: 0,
            root_x: 0,
            root_y: 0,
            event_x: 0,
            event_y: 0,
            state: Default::default(),
            same_screen: true,
        };
        if code & !SENT == KEY_PRESS_EVENT {
            Event::KeyPress(event)
        } else {
            Event::KeyRelease(event)
        }
    }

    #[test]
    fn one_report_per_press_none_for_repeats_or_sent_keys() {
        let ours = HashMap::from([(22, "panic")]);
        let mut p = Presses::default();
        assert_eq!(
            p.event(&key(KEY_PRESS_EVENT, 22, 100), &ours),
            Some("panic")
        );
        // Held: the server's autorepeat, a release and a press at once.
        assert_eq!(p.event(&key(KEY_RELEASE_EVENT, 22, 600), &ours), None);
        assert_eq!(p.event(&key(KEY_PRESS_EVENT, 22, 600), &ours), None);
        // Detectable autorepeat: presses with no release between.
        assert_eq!(p.event(&key(KEY_PRESS_EVENT, 22, 633), &ours), None);
        // Let go, pressed again.
        assert_eq!(p.event(&key(KEY_RELEASE_EVENT, 22, 700), &ours), None);
        assert_eq!(
            p.event(&key(KEY_PRESS_EVENT, 22, 900), &ours),
            Some("panic")
        );
        assert_eq!(p.event(&key(KEY_RELEASE_EVENT, 22, 950), &ours), None);
        // Another client's sent press, and a key not ours.
        assert_eq!(p.event(&key(KEY_PRESS_EVENT | SENT, 22, 1000), &ours), None);
        assert_eq!(p.event(&key(KEY_PRESS_EVENT, 23, 1100), &ours), None);
    }

    /// Needs an X server and `xdotool` (XTEST, as a `uinput` key reaches the
    /// server: real). Run with `--ignored x11::`.
    #[test]
    #[ignore = "needs an X server and xdotool"]
    fn a_typed_chord_is_heard_once_and_a_key_another_program_holds_is_left_out() {
        use std::sync::mpsc;

        // Another program holds Ctrl+Alt+Shift+Y (evdev 21, X keycode 29).
        let (other, root) = super::super::connect().expect("the X server");
        other
            .grab_key(
                false,
                root,
                ModMask::CONTROL | ModMask::M1 | ModMask::SHIFT,
                29u8,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .expect("sent")
            .check()
            .expect("held");
        let (tx, rx) = mpsc::channel::<String>();
        let keys = vec![
            Key {
                id: "panic",
                code: 14,
                described: "Ctrl+Alt+Shift+Backspace".into(),
            },
            Key {
                id: "accept",
                code: 21,
                described: "Ctrl+Alt+Shift+Y".into(),
            },
        ];
        let grabbed = grab(
            keys,
            Box::new(move |e| {
                let _ = tx.send(format!("{e:?}"));
            }),
        )
        .expect("grabbed");
        let next = || rx.recv_timeout(Duration::from_secs(3)).expect("an event");
        assert_eq!(
            next(),
            r#"Bound([("panic", "Ctrl+Alt+Shift+Backspace")])"#,
            "only the key nobody else holds"
        );
        let typed = std::process::Command::new("xdotool")
            .args(["key", "--delay", "300", "ctrl+alt+shift+BackSpace"])
            .status()
            .expect("xdotool runs");
        assert!(typed.success());
        assert_eq!(next(), r#"Pressed("panic")"#);
        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "one press, one report"
        );
        drop(grabbed);
    }
}
