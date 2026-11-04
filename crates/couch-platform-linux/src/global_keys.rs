//! The host's keys on Linux (M16c, ADR 0012): the panic key, Accept and
//! Decline, through the desktop's global-shortcuts portal
//! (`org.freedesktop.portal.GlobalShortcuts`: KDE, GNOME since 48).
//!
//! The desktop owns the keys: we offer each shortcut with the user's key as
//! its preferred trigger, the desktop may let the user change it, and it
//! tells us when one is pressed, whatever window is in front. Unlike
//! Windows' hook, the desktop can't tell a key a program injects from a real
//! one; a Linux host offers no keyboard seat yet (M16c-1), so no guest has
//! keys to inject.

use std::{sync::mpsc, time::Duration};

use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut, Shortcut};
use futures_util::StreamExt;

/// How long the portal may take to open a session before the keys are
/// given up.
const OPEN_WAIT: Duration = Duration::from_secs(5);
/// How long dropping the keys waits for the desktop to close their session.
const CLOSE_WAIT: Duration = Duration::from_millis(500);

/// One shortcut to offer: its stable id, what it does, and its preferred
/// trigger in the XDG shortcuts form ("CTRL+ALT+SHIFT+BackSpace").
pub struct Key {
    pub id: &'static str,
    pub description: &'static str,
    pub trigger: String,
}

/// What the keys report, by their ids.
#[derive(Debug)]
pub enum Event<'a> {
    /// One was pressed.
    Pressed(&'a str),
    /// The desktop bound them, each with its trigger as the desktop
    /// describes it; again whenever the user changes them. One missing
    /// isn't bound.
    Bound(Vec<(String, String)>),
    /// The desktop has no such portal, didn't answer, refused them, or went
    /// away: none of them work.
    Failed(String),
}

/// The keys, bound until this drops.
pub struct GlobalKeys {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    /// Closed when the task ends, its session closed.
    done: mpsc::Receiver<()>,
}

impl Drop for GlobalKeys {
    fn drop(&mut self) {
        // The task closes the session, which unbinds the keys; the next
        // session's keys shouldn't meet them still bound.
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = self.done.recv_timeout(CLOSE_WAIT);
    }
}

/// Offers `keys` to the desktop and reports on them through `on_event`, on
/// the portals' runtime (it must return at once). Returns at once: opening
/// the portal's session may take a moment, and binding may wait on a dialog
/// of the desktop's, so whether the keys work comes later, as `Bound` or
/// `Failed`. With `answer_within` (nobody may be at the PC: a session
/// started from afar, M16c), a dialog not answered by then is closed and the
/// keys fail.
pub fn bind(
    keys: Vec<Key>,
    answer_within: Option<Duration>,
    on_event: Box<dyn Fn(Event<'_>) + Send>,
) -> Result<GlobalKeys, String> {
    let runtime = crate::portal::runtime()?;
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    runtime.spawn(async move {
        let _done = done_tx;
        run(keys, answer_within, on_event, stop_rx).await;
    });
    Ok(GlobalKeys {
        stop: Some(stop_tx),
        done: done_rx,
    })
}

async fn run(
    keys: Vec<Key>,
    answer_within: Option<Duration>,
    on_event: Box<dyn Fn(Event<'_>) + Send>,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) {
    crate::portal::ready().await;
    let opened = tokio::time::timeout(OPEN_WAIT, async {
        let portal = GlobalShortcuts::new().await?;
        let session = portal.create_session().await?;
        let activated = portal.receive_activated().await?;
        let changed = portal.receive_shortcuts_changed().await?;
        Ok::<_, ashpd::Error>((portal, session, activated, changed))
    });
    let (portal, session, mut activated, mut changed) = tokio::select! {
        _ = &mut stop => return,
        opened = opened => match opened {
            Ok(Ok(opened)) => opened,
            Ok(Err(e)) => {
                on_event(Event::Failed(format!("no global shortcuts here: {e}")));
                return;
            }
            Err(_) => {
                on_event(Event::Failed(
                    "the desktop's global-shortcuts portal didn't answer".into(),
                ));
                return;
            }
        },
    };
    let offered: Vec<NewShortcut> = keys
        .iter()
        .map(|k| NewShortcut::new(k.id, k.description).preferred_trigger(k.trigger.as_str()))
        .collect();
    let bound = async {
        portal
            .bind_shortcuts(&session, &offered, None)
            .await?
            .response()
    };
    let unanswered = async {
        match answer_within {
            Some(wait) => tokio::time::sleep(wait).await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        biased;
        _ = &mut stop => {
            let _ = session.close().await;
            return;
        }
        bound = bound => match bound {
            Ok(bound) => on_event(Event::Bound(triggers(bound.shortcuts()))),
            Err(e) => {
                on_event(Event::Failed(format!("the desktop didn't bind them: {e}")));
                let _ = session.close().await;
                return;
            }
        },
        // Closing the session closes its dialog.
        () = unanswered => {
            on_event(Event::Failed(
                "nobody answered the desktop's shortcuts dialog at this PC".into(),
            ));
            let _ = session.close().await;
            return;
        }
    }
    // The portal sends a session's signals to its owner alone, and only one
    // task of this process reads them at a time: each stops reading when its
    // keys drop, before the next session's are bound. So the ids are ours.
    loop {
        tokio::select! {
            _ = &mut stop => break,
            pressed = activated.next() => match pressed {
                Some(pressed) => on_event(Event::Pressed(pressed.shortcut_id())),
                None => {
                    on_event(Event::Failed("the desktop's portal went away".into()));
                    break;
                }
            },
            Some(changed) = changed.next() => on_event(Event::Bound(triggers(changed.shortcuts()))),
        }
    }
    let _ = session.close().await;
}

/// Each bound shortcut's id and trigger, as the desktop describes it.
fn triggers(shortcuts: &[Shortcut]) -> Vec<(String, String)> {
    let bound: Vec<(String, String)> = shortcuts
        .iter()
        .map(|s| (s.id().to_owned(), readable(s.trigger_description())))
        .collect();
    for (id, trigger) in &bound {
        tracing::info!("the host's key {id}: {trigger}");
    }
    bound
}

/// A trigger in GTK's accelerator syntax, as GNOME 50 described them after
/// binding without its dialog ("Press <Shift><Control><Alt>y"), the way
/// people write it ("Ctrl+Alt+Shift+Y"); any other description as it is.
fn readable(description: &str) -> String {
    let text = description.trim();
    let mut rest = text.strip_prefix("Press ").unwrap_or(text);
    let (mut ctrl, mut alt, mut shift, mut sup) = (false, false, false, false);
    while let Some(after) = rest.strip_prefix('<') {
        let Some((name, after)) = after.split_once('>') else {
            return description.to_owned();
        };
        match name.to_ascii_lowercase().as_str() {
            "control" | "ctrl" | "primary" => ctrl = true,
            "alt" | "mod1" => alt = true,
            "shift" => shift = true,
            "super" | "meta" | "mod4" => sup = true,
            _ => return description.to_owned(),
        }
        rest = after;
    }
    let none = !(ctrl || alt || shift || sup);
    if none || rest.is_empty() || rest.contains(char::is_whitespace) {
        return description.to_owned();
    }
    let key = match rest {
        "BackSpace" => "Backspace".to_owned(),
        "minus" => "-".to_owned(),
        "equal" => "=".to_owned(),
        "bracketleft" => "[".to_owned(),
        "bracketright" => "]".to_owned(),
        "backslash" => "\\".to_owned(),
        "semicolon" => ";".to_owned(),
        "apostrophe" => "'".to_owned(),
        "grave" => "`".to_owned(),
        "comma" => ",".to_owned(),
        "period" => ".".to_owned(),
        "slash" => "/".to_owned(),
        key if key.chars().count() == 1 => key.to_uppercase(),
        key => key.to_owned(),
    };
    let mut parts: Vec<&str> = Vec::new();
    for (on, name) in [
        (ctrl, "Ctrl"),
        (alt, "Alt"),
        (shift, "Shift"),
        (sup, "Super"),
    ] {
        if on {
            parts.push(name);
        }
    }
    parts.push(&key);
    parts.join("+")
}

/// The XDG shortcuts name of the key with this HID usage, among those a
/// shortcut may use (`couch_input::shortcuts::KEYS`): xkbcommon's keysym
/// names.
pub fn key_name(usage: u16) -> Option<&'static str> {
    Some(match usage {
        0x04..=0x1D => {
            const LETTERS: [&str; 26] = [
                "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p",
                "q", "r", "s", "t", "u", "v", "w", "x", "y", "z",
            ];
            LETTERS[usize::from(usage - 0x04)]
        }
        0x1E..=0x26 => {
            const DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];
            DIGITS[usize::from(usage - 0x1E)]
        }
        0x27 => "0",
        0x2A => "BackSpace",
        0x2D => "minus",
        0x2E => "equal",
        0x2F => "bracketleft",
        0x30 => "bracketright",
        0x31 => "backslash",
        0x33 => "semicolon",
        0x34 => "apostrophe",
        0x35 => "grave",
        0x36 => "comma",
        0x37 => "period",
        0x38 => "slash",
        0x3A..=0x45 => {
            const F: [&str; 12] = [
                "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
            ];
            F[usize::from(usage - 0x3A)]
        }
        _ => return None,
    })
}

/// "CTRL+ALT+SHIFT+<key>", the host's chord, in the XDG shortcuts form.
pub fn chord(usage: u16) -> Option<String> {
    key_name(usage).map(|key| format!("CTRL+ALT+SHIFT+{key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gtk_accelerators_read_as_people_write_them() {
        assert_eq!(readable("Press <Shift><Control><Alt>y"), "Ctrl+Alt+Shift+Y");
        assert_eq!(
            readable("Press <Shift><Control><Alt>BackSpace"),
            "Ctrl+Alt+Shift+Backspace"
        );
        assert_eq!(readable("<Primary><Alt>F12"), "Ctrl+Alt+F12");
        assert_eq!(
            readable("<Control><Alt><Shift>bracketleft"),
            "Ctrl+Alt+Shift+["
        );
        // Anything else is the desktop's own words.
        assert_eq!(readable("Ctrl+Alt+Shift+Y"), "Ctrl+Alt+Shift+Y");
        assert_eq!(readable("Press y"), "Press y");
        assert_eq!(readable("Press <Hyper>y"), "Press <Hyper>y");
        assert_eq!(
            readable("Press <Control>y or <Alt>y"),
            "Press <Control>y or <Alt>y"
        );
        assert_eq!(readable(""), "");
    }

    #[test]
    fn every_shortcut_key_has_a_name() {
        for key in couch_input::shortcuts::KEYS {
            assert!(key_name(key.usage.0).is_some(), "{}", key.name);
        }
        assert_eq!(chord(0x2A).as_deref(), Some("CTRL+ALT+SHIFT+BackSpace"));
        assert_eq!(chord(0x1C).as_deref(), Some("CTRL+ALT+SHIFT+y"));
        assert_eq!(chord(0x45).as_deref(), Some("CTRL+ALT+SHIFT+F12"));
        assert_eq!(key_name(0x29), None, "Esc is never a shortcut's");
    }

    /// Where the desktop has no portal (WSL, CI): binding returns at once
    /// and the keys report they failed, without hanging; and a second
    /// binding after the first dropped does the same, on the same runtime.
    /// Skipped on a desktop, where it would bind real keys.
    #[test]
    fn without_a_portal_the_keys_fail_soon_and_again() {
        if std::env::var_os("XDG_CURRENT_DESKTOP").is_some() {
            return;
        }
        for _ in 0..2 {
            let (tx, rx) = mpsc::channel::<String>();
            let keys = bind(
                vec![Key {
                    id: "panic",
                    description: "test",
                    trigger: "CTRL+ALT+SHIFT+BackSpace".into(),
                }],
                None,
                Box::new(move |event| {
                    if let Event::Failed(e) = event {
                        let _ = tx.send(e);
                    }
                }),
            )
            .expect("bind returns at once");
            let failed = rx.recv_timeout(OPEN_WAIT + Duration::from_secs(2));
            drop(keys);
            assert!(failed.is_ok(), "no portal, yet no failure reported");
        }
    }
}
