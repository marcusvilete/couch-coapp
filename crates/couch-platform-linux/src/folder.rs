//! Opening a folder in the desktop's file manager (M26a: Settings → Network's
//! "Open the log folder"), through the OpenURI portal: the file manager
//! starts outside our process, so an AppImage's environment (its library
//! paths) never reaches it, as it would through `xdg-open`.

use std::{
    os::fd::AsFd,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use ashpd::desktop::open_uri::OpenDirectoryRequest;

/// How long the portal may take to answer.
const WAIT: Duration = Duration::from_secs(10);

/// Opens `dir` in the file manager, with `file` in it selected. The portal
/// opens the folder holding what it's given, so a file inside: `file` if
/// it's there, else any file in `dir`, else `dir` itself (its parent opens,
/// with it selected). Waits for the portal's answer.
pub fn open(dir: &Path, file: &str) -> Result<(), String> {
    let target = inside(dir, file);
    let opened = std::fs::File::open(&target)
        .map_err(|e| format!("couldn't open {}: {e}", target.display()))?;
    let runtime = crate::portal::runtime()?;
    let (tx, rx) = mpsc::channel();
    runtime.spawn(async move {
        crate::portal::ready().await;
        let fd = opened.as_fd();
        let request = OpenDirectoryRequest::default().send(&fd);
        let result = match tokio::time::timeout(WAIT, request).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(format!("the desktop's file manager: {e}")),
            Err(_) => Err("the desktop didn't answer".to_string()),
        };
        let _ = tx.send(result);
    });
    rx.recv_timeout(WAIT + Duration::from_secs(5))
        .map_err(|_| "the desktop didn't answer".to_string())?
}

/// What to hand the portal for `dir`.
fn inside(dir: &Path, file: &str) -> PathBuf {
    let named = dir.join(file);
    if named.is_file() {
        return named;
    }
    std::fs::read_dir(dir)
        .ok()
        .and_then(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .min()
        })
        .unwrap_or_else(|| dir.to_path_buf())
}
