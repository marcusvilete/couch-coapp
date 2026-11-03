//! The host's notices as desktop notifications (M16c, ADR 0012): no window
//! can stay over a game on GNOME without taking the focus, so a Linux host
//! shows requests for a seat and what happens in its session through the
//! desktop's notification server (`org.freedesktop.Notifications`), which
//! never takes it.
//!
//! The app says which notices should be up, each by its id; one new is
//! shown, one changed is replaced in place, one gone is closed. They ask for
//! no sound: the notification server's would be in the stream, which leaves
//! out only this app's own sounds (the app plays its chime itself). Shown
//! from one task on the portals' runtime, in the order asked, the newest
//! list winning over any not yet shown.

use std::{collections::HashMap, sync::OnceLock, time::Duration};

use ashpd::{zbus, zvariant::Value};
use tokio::sync::mpsc;

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const APP: &str = "Couch Co-App";

/// One notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Stable while it is up: the same id with new text replaces it.
    pub id: String,
    pub title: String,
    pub body: String,
    /// Stays until closed (a request waiting for an answer); otherwise the
    /// server lets it go after its usual time, and keeps no record of it.
    pub lasting: bool,
}

/// A list to make what's up, and who waits for it to be.
type List = (Vec<Notice>, Option<std::sync::mpsc::Sender<()>>);

/// The notifications' task, once anything was shown.
static LISTS: OnceLock<Result<mpsc::UnboundedSender<List>, String>> = OnceLock::new();

/// Makes `notices` what's up: returns at once.
pub fn show(notices: Vec<Notice>) -> Result<(), String> {
    let lists = LISTS
        .get_or_init(|| {
            let runtime = crate::portal::runtime()?;
            let (tx, rx) = mpsc::unbounded_channel();
            runtime.spawn(run(rx));
            Ok(tx)
        })
        .as_ref()
        .map_err(Clone::clone)?;
    lists
        .send((notices, None))
        .map_err(|_| "the notifications' task has ended".to_string())
}

/// Closes every notice up, waiting up to `wait` for it: the app quitting
/// leaves no request on the screen that nobody can answer.
pub fn close_all(wait: Duration) {
    let Some(Ok(lists)) = LISTS.get() else { return };
    let (done, closed) = std::sync::mpsc::channel();
    if lists.send((Vec::new(), Some(done))).is_ok() {
        let _ = closed.recv_timeout(wait);
    }
}

/// A notice up, by the server's id for it.
struct Shown {
    server_id: u32,
    notice: Notice,
}

async fn run(mut lists: mpsc::UnboundedReceiver<List>) {
    // The bus, and whether the server reads markup in bodies.
    let mut connection: Option<(zbus::Connection, bool)> = None;
    let mut shown: HashMap<String, Shown> = HashMap::new();
    // Said once: a desktop without a notification server stays so.
    let mut warned = false;
    while let Some((mut list, done)) = lists.recv().await {
        let mut waiting: Vec<_> = done.into_iter().collect();
        while let Ok((newer, done)) = lists.try_recv() {
            list = newer;
            waiting.extend(done);
        }
        if connection.is_none() {
            match zbus::Connection::session().await {
                Ok(c) => {
                    let markup = body_markup(&c).await;
                    connection = Some((c, markup));
                }
                Err(e) => {
                    if !warned {
                        tracing::warn!("no notifications: the session bus: {e}");
                        warned = true;
                    }
                    continue;
                }
            }
        }
        let Some((bus, markup)) = &connection else {
            continue;
        };
        if let Err(e) = apply(bus, *markup, &mut shown, list).await {
            if !warned {
                tracing::warn!("notifications: {e}");
                warned = true;
            }
            // The next list starts on a new connection.
            connection = None;
        }
        for done in waiting {
            let _ = done.send(());
        }
    }
}

/// Whether the server reads markup in bodies (most do): then ours is
/// escaped. Unknown counts as yes.
async fn body_markup(bus: &zbus::Connection) -> bool {
    match bus
        .call_method(Some(SERVICE), PATH, Some(SERVICE), "GetCapabilities", &())
        .await
    {
        Ok(reply) => reply
            .body()
            .deserialize::<Vec<String>>()
            .map_or(true, |caps| caps.iter().any(|c| c == "body-markup")),
        Err(_) => true,
    }
}

/// `text` as markup's plain text.
fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `text` with markup's characters as look-alikes, for a summary: plain text
/// by the spec, yet some servers (dunst) read markup in it too, and an
/// escape would show as itself on the others. Names in it are peers'.
fn inert(text: &str) -> String {
    text.replace('&', "\u{FF06}")
        .replace('<', "\u{2039}")
        .replace('>', "\u{203A}")
}

/// Closes what's gone, shows what's new or changed. The body is markup
/// where `markup`; the summary, plain text with no markup's characters.
async fn apply(
    bus: &zbus::Connection,
    markup: bool,
    shown: &mut HashMap<String, Shown>,
    list: Vec<Notice>,
) -> zbus::Result<()> {
    let gone: Vec<String> = shown
        .keys()
        .filter(|id| !list.iter().any(|n| &n.id == *id))
        .cloned()
        .collect();
    for id in gone {
        if let Some(old) = shown.remove(&id) {
            // Already closed by the user, it may be unknown: no matter.
            let _ = bus
                .call_method(
                    Some(SERVICE),
                    PATH,
                    Some(SERVICE),
                    "CloseNotification",
                    &(old.server_id,),
                )
                .await;
        }
    }
    for notice in list {
        let replaces = match shown.get(&notice.id) {
            Some(old) if old.notice == notice => continue,
            Some(old) => old.server_id,
            None => 0,
        };
        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        hints.insert("suppress-sound", Value::Bool(true));
        // Critical stays until closed; normal goes by itself.
        hints.insert("urgency", Value::U8(if notice.lasting { 2 } else { 1 }));
        if !notice.lasting {
            hints.insert("transient", Value::Bool(true));
        }
        let expires: i32 = if notice.lasting { 0 } else { -1 };
        let actions: Vec<&str> = Vec::new();
        let (title, body) = if markup {
            (inert(&notice.title), escaped(&notice.body))
        } else {
            (notice.title.clone(), notice.body.clone())
        };
        let reply = bus
            .call_method(
                Some(SERVICE),
                PATH,
                Some(SERVICE),
                "Notify",
                &(
                    APP,
                    replaces,
                    "",
                    title.as_str(),
                    body.as_str(),
                    actions,
                    hints,
                    expires,
                ),
            )
            .await?;
        let server_id: u32 = reply.body().deserialize()?;
        shown.insert(notice.id.clone(), Shown { server_id, notice });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_escaped() {
        assert_eq!(
            escaped("<b>Ctrl</b> & F1"),
            "&lt;b&gt;Ctrl&lt;/b&gt; &amp; F1"
        );
        assert_eq!(
            inert("<span>Bia & co</span>"),
            "\u{2039}span\u{203A}Bia \u{FF06} co\u{2039}/span\u{203A}"
        );
    }

    fn notice(id: &str, title: &str, lasting: bool) -> Notice {
        Notice {
            id: id.into(),
            title: title.into(),
            body: "a test of Couch Co-App's notices".into(),
            lasting,
        }
    }

    /// Where a notification server runs (`linux-checks`: a desktop, or
    /// `dunst` in WSL): notices are shown, replaced in place, and closed.
    #[test]
    #[ignore = "needs a notification server on the session bus"]
    fn notices_are_shown_replaced_and_closed() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let bus = zbus::Connection::session().await.expect("a session bus");
            let mut shown = HashMap::new();
            let list = vec![
                notice("request", "Bia asks for Controller 2", true),
                notice("event", "Ana started watching", false),
            ];
            apply(&bus, true, &mut shown, list).await.expect("shown");
            let request = shown["request"].server_id;
            assert_ne!(request, 0);
            apply(
                &bus,
                true,
                &mut shown,
                vec![notice("request", "Bia asks for Controller 1", true)],
            )
            .await
            .expect("replaced");
            assert_eq!(shown.len(), 1, "the event closed");
            assert_eq!(shown["request"].server_id, request, "replaced in place");
            apply(&bus, true, &mut shown, Vec::new())
                .await
                .expect("closed");
            assert!(shown.is_empty());
        });
    }
}
