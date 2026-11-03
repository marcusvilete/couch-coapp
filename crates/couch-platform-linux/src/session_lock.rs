//! Whether the desktop shows its lock screen (M16c, ADR 0012: M25c's
//! "Windows is locked", the Linux way): logind's `LockedHint` for the
//! user's graphical session, which GNOME's and KDE's lock screens set.
//!
//! The session is the user's `Display` (an app a desktop starts runs under
//! the user's service manager, in no session of its own), else the
//! caller's own. Read on the portals' runtime: [`locked`] returns the last
//! answer at once and asks for a fresh one, so a caller looking every few
//! seconds is one look behind at most, and never waits on the bus.

use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use ashpd::{
    zbus,
    zvariant::{OwnedObjectPath, Value},
};
use tokio::sync::Notify;

const LOGIN1: &str = "org.freedesktop.login1";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
/// The longest one look may take.
const LOOK_WAIT: Duration = Duration::from_secs(2);

struct Watcher {
    locked: AtomicBool,
    look: Notify,
}

/// Whether the lock screen was up when last looked at (unlocked until
/// known), and looks again.
pub fn locked() -> bool {
    static WATCHER: OnceLock<Option<Arc<Watcher>>> = OnceLock::new();
    let watcher = WATCHER.get_or_init(|| {
        let runtime = match crate::portal::runtime() {
            Ok(runtime) => runtime,
            Err(e) => {
                tracing::warn!("the lock screen can't be read: {e}");
                return None;
            }
        };
        let watcher = Arc::new(Watcher {
            locked: AtomicBool::new(false),
            look: Notify::new(),
        });
        runtime.spawn(watch(watcher.clone()));
        Some(watcher)
    });
    let Some(watcher) = watcher else {
        return false;
    };
    // Looks pile into one.
    watcher.look.notify_one();
    watcher.locked.load(Ordering::Relaxed)
}

async fn watch(watcher: Arc<Watcher>) {
    let mut bus: Option<zbus::Connection> = None;
    let mut warned = false;
    loop {
        watcher.look.notified().await;
        let looked = tokio::time::timeout(LOOK_WAIT, async {
            if bus.is_none() {
                bus = Some(zbus::Connection::system().await?);
            }
            let bus = bus.as_ref().expect("connected above");
            locked_hint(bus).await
        })
        .await;
        match looked {
            Ok(Ok(locked)) => {
                watcher.locked.store(locked, Ordering::Relaxed);
                warned = false;
            }
            failed => {
                if !warned {
                    match failed {
                        Ok(Err(e)) => tracing::warn!("the lock screen can't be read: {e}"),
                        _ => tracing::warn!("the lock screen can't be read: logind didn't answer"),
                    }
                    warned = true;
                }
                // The next look starts on a new connection.
                bus = None;
            }
        }
    }
}

async fn locked_hint(bus: &zbus::Connection) -> zbus::Result<bool> {
    let session = match display_session(bus).await {
        Ok(Some(session)) => session,
        // No graphical session of the user's: this process's own.
        Ok(None) | Err(_) => manager(bus, "GetSessionByPID", std::process::id()).await?,
    };
    match property(
        bus,
        &session,
        "org.freedesktop.login1.Session",
        "LockedHint",
    )
    .await?
    {
        Value::Bool(locked) => Ok(locked),
        other => Err(zbus::Error::Failure(format!(
            "LockedHint isn't a boolean: {other:?}"
        ))),
    }
}

/// The object logind's manager names for an id (a user's, a process's).
async fn manager(bus: &zbus::Connection, method: &str, id: u32) -> zbus::Result<OwnedObjectPath> {
    let reply = bus
        .call_method(
            Some(LOGIN1),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            method,
            &(id,),
        )
        .await?;
    reply.body().deserialize()
}

/// The user's graphical session, if any (`/` when none). The user by id:
/// `user/self` names the caller's, which logind finds only for a process in
/// one of its sessions.
async fn display_session(bus: &zbus::Connection) -> zbus::Result<Option<OwnedObjectPath>> {
    let user = manager(bus, "GetUser", rustix::process::getuid().as_raw()).await?;
    let Value::Structure(display) =
        property(bus, &user, "org.freedesktop.login1.User", "Display").await?
    else {
        return Ok(None);
    };
    Ok(match display.fields() {
        [_, Value::ObjectPath(path)] if path.as_str() != "/" => {
            Some(OwnedObjectPath::from(path.to_owned()))
        }
        _ => None,
    })
}

async fn property(
    bus: &zbus::Connection,
    path: &OwnedObjectPath,
    interface: &str,
    name: &str,
) -> zbus::Result<Value<'static>> {
    let reply = bus
        .call_method(
            Some(LOGIN1),
            path,
            Some(PROPERTIES),
            "Get",
            &(interface, name),
        )
        .await?;
    let body = reply.body();
    let value: Value<'_> = body.deserialize()?;
    // A variant's own value.
    let value = match value {
        Value::Value(inner) => *inner,
        value => value,
    };
    value
        .try_to_owned()
        .map(Into::into)
        .map_err(|e| zbus::Error::Failure(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The user's session's `LockedHint` reads (`--ignored`: needs logind
    /// and a session of the user's).
    #[test]
    #[ignore = "needs logind"]
    fn the_lock_screen_reads() {
        let runtime = crate::portal::runtime().unwrap();
        let locked = runtime.block_on(async {
            let bus = zbus::Connection::system().await?;
            locked_hint(&bus).await
        });
        assert!(locked.is_ok(), "{locked:?}");
    }
}
