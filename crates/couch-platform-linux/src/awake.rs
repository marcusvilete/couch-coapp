//! Keeping a home PC awake while remote play is on (M16c, ADR 0012: M25c's
//! unattended PC, the Linux way), and its screen on too while a device is
//! unlocked: a screen that blanks stops the capture, and the desktop may
//! lock then.
//!
//! Two requests, each enough where it is honoured:
//! - **logind's inhibitor** (`sleep`, and `idle` with the screen), held as
//!   the file descriptor logind gives back: the system's own sleep, and
//!   what reads logind's list;
//! - **the desktop's inhibit portal** (`org.freedesktop.portal.Inhibit`,
//!   suspend, and idle with the screen): GNOME's and KDE's own power
//!   settings, which suspend and blank the screen themselves.
//!
//! Taken and let go on the portals' runtime, never blocking the caller: a
//! request fails only in the log.

use std::{sync::OnceLock, time::Duration};

use ashpd::{
    desktop::inhibit::{InhibitFlags, InhibitProxy},
    enumflags2::BitFlags,
    zbus,
    zvariant::OwnedFd,
};
use tokio::sync::oneshot;

const LOGIN1: &str = "org.freedesktop.login1";

/// Held while the PC stays awake; dropped, it may sleep again.
pub struct Awake {
    _release: oneshot::Sender<()>,
}

impl std::fmt::Debug for Awake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Awake")
    }
}

/// Keeps the PC awake, and with `display` its screen on too, until the
/// returned guard drops; `why` is what the desktop shows for it. Returns at
/// once.
pub fn keep(why: &str, display: bool) -> Result<Awake, String> {
    let runtime = crate::portal::runtime()?;
    let (release, released) = oneshot::channel();
    let why = why.to_string();
    runtime.spawn(hold(why, display, released));
    Ok(Awake { _release: release })
}

async fn hold(why: String, display: bool, released: oneshot::Receiver<()>) {
    // Both asked at once: neither waits on the other's bus. Each within
    // its time, so a bus that never answers can't keep the other's request
    // past the guard.
    let (logind, portal) =
        tokio::join!(within(logind(&why, display)), within(portal(&why, display)));
    match (&logind, &portal) {
        (Err(l), Err(p)) => once(&BOTH_SAID, || {
            tracing::warn!("this PC can't be kept awake: logind: {l}; the desktop: {p}")
        }),
        (Err(e), Ok(_)) => once(&LOGIND_SAID, || {
            tracing::info!("logind keeps nothing awake ({e}); the desktop does")
        }),
        (Ok(_), Err(e)) => once(&DESKTOP_SAID, || {
            tracing::warn!(
                "the desktop's inhibit portal: {e}; only logind keeps this PC awake, and \
                 the desktop's own power settings may still suspend it or blank the screen"
            )
        }),
        (Ok(_), Ok(_)) => {}
    }
    // Dropped guard or not, the requests go when this ends.
    let _ = released.await;
    drop(logind);
    if let Ok(request) = portal {
        let _ = request.close().await;
    }
}

/// How long either request may take.
const REQUEST_WAIT: Duration = Duration::from_secs(10);

/// `request`, or an error once [`REQUEST_WAIT`] has passed.
async fn within<T, E: std::fmt::Display>(
    request: impl Future<Output = Result<T, E>>,
) -> Result<T, String> {
    match tokio::time::timeout(REQUEST_WAIT, request).await {
        Ok(Ok(held)) => Ok(held),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("no answer".into()),
    }
}

// A system that can't do one stays so: each said once, not at every unlock.
static BOTH_SAID: OnceLock<()> = OnceLock::new();
static LOGIND_SAID: OnceLock<()> = OnceLock::new();
static DESKTOP_SAID: OnceLock<()> = OnceLock::new();

fn once(said: &OnceLock<()>, say: impl FnOnce()) {
    if said.set(()).is_ok() {
        say();
    }
}

/// logind's block inhibitor: held as long as its descriptor is open.
async fn logind(why: &str, display: bool) -> zbus::Result<OwnedFd> {
    let bus = zbus::Connection::system().await?;
    let what = if display { "sleep:idle" } else { "sleep" };
    let reply = bus
        .call_method(
            Some(LOGIN1),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "Inhibit",
            &(what, "Couch Co-App", why, "block"),
        )
        .await?;
    reply.body().deserialize::<OwnedFd>()
}

/// The desktop's inhibit portal: held until its request is closed.
async fn portal(why: &str, display: bool) -> Result<ashpd::desktop::Request<()>, ashpd::Error> {
    let mut flags = BitFlags::from(InhibitFlags::Suspend);
    if display {
        flags |= InhibitFlags::Idle;
    }
    InhibitProxy::new().await?.inhibit(None, flags, why).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// logind lists our inhibitor while the guard lives, and not after
    /// (`--ignored`: needs logind on the system bus).
    #[test]
    #[ignore = "needs logind"]
    fn logind_lists_us_while_held() {
        let listed = || {
            let out = std::process::Command::new("systemd-inhibit")
                .arg("--list")
                .output()
                .expect("systemd-inhibit");
            String::from_utf8_lossy(&out.stdout).contains("Couch Co-App test")
        };
        let awake = keep("Couch Co-App test", true).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !listed() {
            assert!(std::time::Instant::now() < deadline, "never listed");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        drop(awake);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while listed() {
            assert!(std::time::Instant::now() < deadline, "never let go");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
