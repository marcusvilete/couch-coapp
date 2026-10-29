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
    couch_desktop_lib::run()
}
