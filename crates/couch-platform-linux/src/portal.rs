//! The desktop's portals (`org.freedesktop.portal.*`, through `ashpd`): one
//! runtime for the whole process.
//!
//! `ashpd` keeps one D-Bus connection per process, made on the runtime that
//! first asks for it, whose tasks then read its socket. A runtime that ends
//! (a thread's own, gone with its session) would leave every later request
//! on a dead connection, so every portal request runs here, on a runtime
//! that lives as long as the process.
//!
//! An app outside Flatpak and Snap has no app id the portals can see, and
//! since xdg-desktop-portal 1.19 some refuse it ("An app id is required":
//! the global shortcuts, on GNOME 48 and KDE 6 alike). So the connection's
//! first request registers ours ([`APP_ID`]): the portal takes a
//! registration only first, so every task awaits [`ready`] before its own
//! first request (those using `ashpd`: `notices` and `session_lock` have buses of their own); and only for an app id with an installed desktop entry, so
//! the AppImage's menu entry is installed just before (`launcher`). A
//! development build has none, and goes unregistered.

use std::{sync::OnceLock, time::Duration};

use tokio::runtime::Runtime;

/// The app id the portals know us by, and our menu entry's name. Not the
/// app's identifier (`tauri.conf.json`'s `com.couchcoapp.desktop`): KDE
/// keeps a portal session's shortcuts in a KGlobalAccel component named by
/// the app id, and takes a component name ending in `.desktop` for a desktop
/// file's, which it then doesn't find, so the keys were never kept (seen on
/// Plasma 6.6). Any id ending so would do the same.
pub const APP_ID: &str = "com.couchcoapp.CouchCoApp";

/// How long registering may take: a session bus that doesn't answer
/// mustn't hold up the first portal request for long.
const REGISTER_WITHIN: Duration = Duration::from_secs(3);
/// Tries, and the wait between them, while the portal doesn't yet know an
/// entry just written: it reads entries through GIO, which notices a new
/// file by watching the folder.
const REGISTER_TRIES: u32 = 5;
const REGISTER_AGAIN_AFTER: Duration = Duration::from_millis(400);

/// The portals' runtime, made the first time it's asked for. Returns at once
/// (its callers mustn't wait, the room actor among them): registering
/// starts as its first task, and every task awaits [`ready`] before its
/// first portal request.
pub(crate) fn runtime() -> Result<&'static Runtime, String> {
    static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("portal")
                .enable_all()
                .build()
                .map_err(|e| format!("no runtime for the desktop's portals: {e}"))?;
            runtime.spawn(ready());
            Ok(runtime)
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Done once for the process, whichever task comes first (the others wait
/// for it): our menu entry installed, then [`APP_ID`] registered, or given
/// up. Every portal task awaits this before its first request, the portal
/// taking a registration only as the connection's first.
pub(crate) async fn ready() {
    static REGISTERED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    REGISTERED
        .get_or_init(|| async {
            match crate::launcher::ensure() {
                Ok(true) => tracing::info!("installed the menu entry for this AppImage"),
                Ok(false) => {}
                Err(e) => tracing::warn!("the menu entry couldn't be installed: {e}"),
            }
            register().await;
        })
        .await;
}

/// Registers [`APP_ID`] for this process's connection. A portal too old to
/// know registering (before 1.19) needs none: logged, and the requests go on
/// without it, as before. Tried again for a moment only while an entry of
/// ours is installed (the portal not having read it yet): a development
/// build has none, and gets one answer.
async fn register() {
    let tries = if crate::launcher::installed() {
        REGISTER_TRIES
    } else {
        1
    };
    for attempt in 1..=tries {
        let app_id = ashpd::AppID::try_from(APP_ID).expect("a valid app id");
        match tokio::time::timeout(REGISTER_WITHIN, ashpd::register_host_app(app_id)).await {
            Ok(Ok(())) => {
                tracing::info!("the desktop's portals know this app as {APP_ID}");
                return;
            }
            // Its entry not seen yet (or none: a development build).
            Ok(Err(e)) if e.to_string().contains("App info not found") && attempt < tries => {
                tokio::time::sleep(REGISTER_AGAIN_AFTER).await;
            }
            Ok(Err(e)) => {
                tracing::info!("not registered with the desktop's portals: {e}");
                return;
            }
            Err(_) => {
                tracing::warn!("the desktop's portals didn't answer registering in time");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_id_is_one_the_portals_take() {
        assert!(ashpd::AppID::try_from(APP_ID).is_ok());
    }

    #[test]
    fn kde_keeps_shortcuts_under_the_app_id() {
        assert!(
            !APP_ID.to_ascii_lowercase().ends_with(".desktop"),
            "KGlobalAccel takes a component ending in .desktop for a desktop file's"
        );
    }
}
