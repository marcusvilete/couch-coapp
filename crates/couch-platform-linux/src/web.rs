//! Opening a web page in the user's browser (M26c-2: the update notice's
//! release page, the latest release, a problem's report), through the
//! OpenURI portal, like [`crate::folder`]: the browser starts outside our
//! process, so an AppImage's environment never reaches it. The engine makes
//! every address from the build's repository; the webview never names one.

use std::{sync::mpsc, time::Duration};

use ashpd::{desktop::open_uri::OpenFileRequest, url::Url};

/// How long the portal may take to answer.
const WAIT: Duration = Duration::from_secs(10);

/// Opens `url` in the browser. Only `https://` addresses. Waits for the
/// portal's answer.
pub fn open(url: &str) -> Result<(), String> {
    let url = Url::parse(url).map_err(|e| format!("not a web page's address: {e}"))?;
    if url.scheme() != "https" {
        return Err("not a web page's address".into());
    }
    let runtime = crate::portal::runtime()?;
    let (tx, rx) = mpsc::channel();
    runtime.spawn(async move {
        crate::portal::ready().await;
        let request = OpenFileRequest::default().send_uri(&url);
        let result = match tokio::time::timeout(WAIT, request).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(format!("the desktop's browser: {e}")),
            Err(_) => Err("the desktop didn't answer".to_string()),
        };
        let _ = tx.send(result);
    });
    rx.recv_timeout(WAIT + Duration::from_secs(5))
        .map_err(|_| "the desktop didn't answer".to_string())?
}
