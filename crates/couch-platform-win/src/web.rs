//! Opening a web page in the user's browser (M26c-2: the update notice's
//! release page, the latest release, a problem's report). The engine makes
//! every address from the build's repository; the webview never names one.

use windows::{
    Win32::{
        System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
        },
        UI::{
            Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

/// Opens `url` with whatever opens https addresses here (the default
/// browser). Only `https://` addresses: anything else could name a program
/// or a file. Blocks briefly: call it off the async runtime.
pub fn open(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") || url.chars().any(|c| c.is_whitespace() || c == '"') {
        return Err("not a web page's address".into());
    }
    // The shell may hand the work to COM components: an apartment for this
    // call alone, as its documentation asks.
    // SAFETY: paired with `CoUninitialize` below whenever it succeeded.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    let file = HSTRING::from(url);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        // Waits for the shell's work (no thread of ours left behind), and
        // shows no error dialog of its own: the app says what failed.
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("open"),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` is a live, initialised SHELLEXECUTEINFOW whose strings
    // (`file` and a static) outlive the call.
    let opened = unsafe { ShellExecuteExW(&mut info) };
    if com.is_ok() {
        // SAFETY: this thread's `CoInitializeEx` above succeeded.
        unsafe { CoUninitialize() };
    }
    opened.map_err(|e| format!("the browser didn't open: {e}"))
}

#[cfg(test)]
mod tests {
    use super::open;

    #[test]
    fn opens_nothing_but_https_addresses() {
        for url in [
            "http://example.com",
            "file:///C:/Windows/System32/calc.exe",
            "C:\\Windows\\System32\\calc.exe",
            "calc",
            "https://example.com/\" calc",
            "https://example.com/ x",
        ] {
            assert!(open(url).is_err(), "{url}");
        }
    }
}
