// Prevents an additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // A release build ignores WebView2's switches in the environment (M21):
    // one of them opens a debugging port through which any program on this
    // PC could drive the app, and with it invites and seats. Development
    // builds keep them, to attach the screenshot tool.
    #[cfg(not(debug_assertions))]
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("WEBVIEW2_")
        {
            // SAFETY: first thing in `main`, before any other thread exists
            // to read the environment.
            unsafe { std::env::remove_var(key) };
        }
    }
    couch_desktop_lib::run()
}
