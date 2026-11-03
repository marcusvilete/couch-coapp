//! The desktop's portals (`org.freedesktop.portal.*`, through `ashpd`): one
//! runtime for the whole process.
//!
//! `ashpd` keeps one D-Bus connection per process, made on the runtime that
//! first asks for it, whose tasks then read its socket. A runtime that ends
//! (a thread's own, gone with its session) would leave every later request
//! on a dead connection, so every portal request runs here, on a runtime
//! that lives as long as the process.

use std::sync::OnceLock;

use tokio::runtime::Runtime;

/// The portals' runtime, made the first time it's asked for.
pub(crate) fn runtime() -> Result<&'static Runtime, String> {
    static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("portal")
                .enable_all()
                .build()
                .map_err(|e| format!("no runtime for the desktop's portals: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}
