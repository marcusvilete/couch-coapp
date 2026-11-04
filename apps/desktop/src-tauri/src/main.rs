// Prevents an additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // A release build ignores WebView2's switches in the environment (M21),
    // and WebKitGTK's inspector server on Linux (M16b): either opens a
    // debugging port through which any program on this PC could drive the
    // app, and with it invites and seats. Development builds keep them, to
    // attach the screenshot tool.
    #[cfg(not(debug_assertions))]
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("WEBVIEW2_") || name.starts_with("WEBKIT_INSPECTOR") {
            // SAFETY: first thing in `main`, before any other thread exists
            // to read the environment.
            unsafe { std::env::remove_var(key) };
        }
    }
    // WebKitGTK's DMA-BUF renderer left the window blank on a virtual
    // machine's GPU: off there, unless the user chose themselves.
    if couch_engine::webview_needs_plain_renderer()
        && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none()
    {
        // SAFETY: as above, before any other thread exists.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
    couch_desktop_lib::run()
}
