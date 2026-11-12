//! The app's own title bar (P15), on Windows: the main window has no frame
//! of the system's, and the page draws the bar (`shell/TitleBar.tsx`). Its
//! drag strip is the webview's own non-client region (`app-region: drag`,
//! which wry turns on: moving, snapping, the system menu, double-click to
//! maximize); the frame's resize edges and shadow are Tauri's. What only
//! Windows can do, snap layouts on the maximize button, is an invisible
//! window of ours over the page's button (`couch_platform_win::snap_button`).
//!
//! Linux keeps its desktop's title bar (the author's choice): there the page
//! is told there is none of ours.

use serde::Serialize;
use tauri::WebviewWindow;

/// Whether the main window draws its own title bar.
pub const OWN: bool = cfg!(windows);

/// The bar's height and its buttons' width, in logical pixels: the page
/// lays its bar out with these (`platform/tauri.ts`), and the snap button
/// lies over its maximize button by them.
const HEIGHT: f64 = 32.0;
const BUTTON_WIDTH: f64 = 46.0;

/// The event telling the page how to draw its maximize button.
#[cfg(windows)]
const LOOK_EVENT: &str = "title-bar-maximize";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Bar<'a> {
    title: &'a str,
    height: f64,
    button_width: f64,
}

/// The page's word on its title bar, set before any of its own scripts run
/// (so the bar is there from the first frame): `null` without one of ours.
pub fn script(title: &str) -> String {
    let bar = OWN.then_some(Bar {
        title,
        height: HEIGHT,
        button_width: BUTTON_WIDTH,
    });
    let json = serde_json::to_string(&bar).unwrap_or_else(|_| "null".into());
    format!("window.__COUCH_TITLE_BAR__ = {json};")
}

#[cfg(windows)]
mod snap {
    use std::sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    };

    use couch_platform_win::snap_button::{self, SnapButton};
    use serde::Serialize;
    use tauri::{Emitter, Listener, Manager, WebviewWindow};

    use super::{BUTTON_WIDTH, HEIGHT, LOOK_EVENT};
    use crate::MAIN_WINDOW;

    /// The main window's snap button, once made (on the UI thread, which
    /// alone places it).
    static MADE: OnceLock<SnapButton> = OnceLock::new();

    /// The page's own scale (its `devicePixelRatio`, as an `f64`'s bits; 0
    /// until it says): WebView2 draws at the screen's scale times Windows'
    /// text size (Settings → Accessibility), which the window's own scale
    /// leaves out.
    static PAGE_SCALE: AtomicU64 = AtomicU64::new(0);

    /// The page saying its scale, at start and on each change.
    const SCALE_EVENT: &str = "title-bar-scale";

    #[derive(Serialize, Clone, Copy)]
    struct Look {
        hover: bool,
        pressed: bool,
    }

    pub fn attach(window: &WebviewWindow) {
        let Ok(hwnd) = window.hwnd() else {
            return;
        };
        let app = window.app_handle().clone();
        let made = snap_button::attach(hwnd.0 as isize, move |look| {
            let look = Look {
                hover: look.hover,
                pressed: look.pressed,
            };
            let _ = app.emit_to(MAIN_WINDOW, LOOK_EVENT, look);
        });
        match made {
            Ok(button) => {
                let _ = MADE.set(button);
                place(window);
            }
            Err(e) => {
                tracing::warn!("the title bar's snap layouts: {e}");
                return;
            }
        }
        // Any page could say it (events are open to every window): at worst
        // it misplaces the button in our own window.
        let window = window.clone();
        window.clone().listen_any(SCALE_EVENT, move |event| {
            let Ok(scale) = serde_json::from_str::<f64>(event.payload()) else {
                return;
            };
            if !(scale.is_finite() && (0.25..=16.0).contains(&scale)) {
                return;
            }
            PAGE_SCALE.store(scale.to_bits(), Ordering::SeqCst);
            let target = window.clone();
            let _ = window.run_on_main_thread(move || place(&target));
        });
    }

    /// Over the page's maximize button: second from the right, at the top.
    pub fn place(window: &WebviewWindow) {
        let (Some(button), Ok(size), Ok(window_scale)) =
            (MADE.get(), window.inner_size(), window.scale_factor())
        else {
            return;
        };
        let scale = match f64::from_bits(PAGE_SCALE.load(Ordering::SeqCst)) {
            page if page > 0.0 => page,
            _ => window_scale,
        };
        let px = |logical: f64| (logical * scale).round() as i32;
        let right = size.width as i32 - px(BUTTON_WIDTH);
        let left = size.width as i32 - px(2.0 * BUTTON_WIDTH);
        snap_button::place(*button, left, 0, right - left, px(HEIGHT));
    }
}

/// Lays the snap button over the main window's maximize button (Windows).
pub fn attach(window: &WebviewWindow) {
    #[cfg(windows)]
    snap::attach(window);
    #[cfg(not(windows))]
    let _ = window;
}

/// Follows the main window's size and scale.
pub fn place(window: &WebviewWindow) {
    #[cfg(windows)]
    snap::place(window);
    #[cfg(not(windows))]
    let _ = window;
}
