//! Thin Tauri adapter over couch-engine: one command in, one event channel out.
//! UI logic belongs in the React app, behaviour in couch-engine.
//!
//! Each profile (`COUCH_PROFILE`) runs as its own single instance, with its
//! own webview data. `couchco://` invite links open in the default profile:
//! Windows runs one command per link scheme, without our environment.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use couch_engine::{BackupFiles, Command, Engine, EngineOptions, Profile, Response};
use tauri::{AppHandle, Emitter, Manager, RunEvent, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_dialog::{DialogExt, FileDialogBuilder};
use tokio::sync::broadcast::error::RecvError;

mod window_state;

/// The event the UI listens on for `couch_engine::Event`s.
const ENGINE_EVENT: &str = "engine-event";
const MAIN_WINDOW: &str = "main";
const INVITE_SCHEME: &str = "couchco";

#[tauri::command]
async fn engine_command(
    app: AppHandle,
    engine: tauri::State<'_, Engine>,
    command: Command,
) -> Result<Response, String> {
    // The app's icon follows its setting at once (M25c).
    let tray = match command {
        Command::SetTray { on } => Some(on),
        _ => None,
    };
    let reply = engine.handle(command).await.map_err(|e| e.to_string())?;
    if let Some(on) = tray {
        show_tray(&app, on);
    }
    Ok(reply)
}

/// Starts the app again, as after a change in Settings → Network. It goes
/// through the usual exit, so the rooms hear we are going (`RunEvent::Exit`
/// below) and the single-instance lock is let go before the new one starts
/// ([`start_again`]).
#[tauri::command]
fn restart_app(app: AppHandle) {
    save_window(&app);
    RESTART.store(true, Ordering::SeqCst);
    app.exit(0);
}

/// Where the main window's place is remembered: this profile's
/// `window.json` (M21).
struct WindowFile(PathBuf);

fn save_window(app: &AppHandle) {
    // Never placed (started at sign-in and never opened, M25c): its place
    // isn't the user's, and mustn't replace the one saved.
    if !PLACED.load(Ordering::SeqCst) {
        return;
    }
    if let (Some(window), Some(file)) = (
        app.get_webview_window(MAIN_WINDOW),
        app.try_state::<WindowFile>(),
    ) {
        window_state::save(&window, &file.0);
    }
}

/// Set by [`restart_app`]: the exit handler starts the app again.
static RESTART: AtomicBool = AtomicBool::new(false);

/// Starts this app again for Restart now: the same exe and environment (so
/// the same profile), without the arguments it was first given. Tauri's own
/// restart passes those on, and they may hold the invite link the app was
/// opened with, whose join screen would come back (P4). By now the
/// single-instance plugin has let go of its lock (on `RunEvent::Exit`, before
/// our handler), and the new engine waits for this one's profile lock.
fn start_again() {
    let started = std::env::current_exe().and_then(|exe| std::process::Command::new(exe).spawn());
    if let Err(e) = started {
        tracing::error!("could not start the app again: {e}");
    }
}

pub fn run() {
    let profile = Profile::from_env().unwrap_or_else(|e| fail(e));
    // To the console at once in development. The release log file waits
    // until we are the profile's only instance (below): a second launch, a
    // clicked invite link, must not set aside the running one's log.
    if cfg!(debug_assertions) {
        start_logging(&profile);
    }
    let mut context = tauri::generate_context!();
    if !profile.is_default() {
        // The single-instance lock is named after the identifier, so this
        // lets alice, bob and carol run side by side.
        let config = context.config_mut();
        config.identifier = format!("{}.{}", config.identifier, profile.name());
    }

    let window_profile = profile.clone();
    let background = std::env::args().any(|a| a == BACKGROUND);
    let app = tauri::Builder::default()
        // First, so a second launch of this profile hands its arguments (an
        // invite link) to the running one and exits inside `build`, before
        // it brings our identity online. A second start at sign-in (signing
        // out and in without the app quitting) shows nothing.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !args.iter().any(|a| a == BACKGROUND) {
                show_main_window(app);
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        // Where a profile's backup goes, and which to restore (M13): opened
        // from Rust only ([`BackupDialogs`]); no window may call it.
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let config = app
                .config()
                .app
                .windows
                .iter()
                .find(|w| w.label == MAIN_WINDOW)
                .cloned()
                .ok_or("no main window in tauri.conf.json")?;
            // Hidden until it is back where it was (M21).
            let mut window = WebviewWindowBuilder::from_config(app.handle(), &config)?
                .data_directory(window_profile.data_dir().join("webview"))
                .visible(false);
            if !window_profile.is_default() {
                window = window.title(format!("{} · {}", config.title, window_profile.name()));
            }
            let window = window.build()?;
            let file = window_state::path(window_profile.data_dir());
            // Started at sign-in (M25c): the notification area only, until
            // the window is first opened, which places it then.
            if !background {
                window_state::restore(&window, &file);
                PLACED.store(true, Ordering::SeqCst);
            }
            app.manage(WindowFile(file));
            tray(app.handle())?;

            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                open_links(&handle, event.urls());
            });
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                open_links(app.handle(), urls);
            }
            if cfg!(debug_assertions) && window_profile.is_default() {
                dev_links(app.handle());
            }

            let mut events = app.state::<Engine>().subscribe();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            if let Err(e) = handle.emit(ENGINE_EVENT, &event) {
                                tracing::warn!("could not forward engine event: {e}");
                            }
                        }
                        Err(RecvError::Lagged(missed)) => {
                            tracing::warn!(missed, "UI fell behind on engine events");
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![engine_command, restart_app])
        // No raw keyboard and mouse input for this process. We don't use
        // device events, and while a process registered for raw keyboard
        // input is in front, Windows skips its own low-level keyboard hooks:
        // the panic key would go blind whenever this window has the focus.
        .device_event_filter(tauri::DeviceEventFilter::Always)
        .build(context)
        .expect("error while building Couch Co-App");

    // Past `build`, we are this profile's only instance.
    if !cfg!(debug_assertions) {
        start_logging(&profile);
    }
    tracing::info!(profile = profile.name(), data_dir = %profile.data_dir().display(), "starting");
    // With the relay chosen in Settings → Network.
    let options = EngineOptions {
        backup_files: Some(Arc::new(BackupDialogs(app.handle().clone()))),
        ..EngineOptions::desktop(&profile)
    };
    let engine =
        tauri::async_runtime::block_on(Engine::start(profile, options)).unwrap_or_else(|e| fail(e));
    app.manage(engine);

    app.run(move |app, event| match event {
        // Setup has made the window and the icon by now: the notification
        // area as the user chose (M25c), and started at sign-in without it,
        // the window opens after all.
        RunEvent::Ready => {
            let tray = app.state::<Engine>().tray_on();
            show_tray(app, tray);
            if background && !tray {
                show_main_window(app);
            }
        }
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::CloseRequested { api, .. },
            ..
        } if label == MAIN_WINDOW => {
            save_window(app);
            // With its icon in the notification area, the app keeps running
            // there (M25c): a device of ours may need it. Quit from the icon
            // ends it.
            if app.try_state::<Engine>().is_some_and(|e| e.tray_on()) {
                api.prevent_close();
                if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                    let _ = window.hide();
                }
                tracing::info!("the window went to the notification area");
            }
        }
        // The app is its main window: closing it while the host overlay is
        // up (M8) must still end the app.
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::Destroyed,
            ..
        } if label == MAIN_WINDOW => app.exit(0),
        // Tell the rooms we are going, so members don't wait for a timeout.
        RunEvent::Exit => {
            let engine = app.state::<Engine>();
            tauri::async_runtime::block_on(engine.shutdown());
            if RESTART.load(Ordering::SeqCst) {
                start_again();
            }
        }
        _ => {}
    });
}

/// A profile's backup files (M13), chosen in the system's dialogs opened from
/// here: the webview never names a path, so a script in it can't write the
/// identity elsewhere or read another file.
struct BackupDialogs(AppHandle);

impl BackupDialogs {
    fn dialog(&self) -> FileDialogBuilder<tauri::Wry> {
        let dialog = self
            .0
            .dialog()
            .file()
            .add_filter("Couch Co-App backup", &["couchbackup"]);
        match self.0.get_webview_window(MAIN_WINDOW) {
            Some(window) => dialog.set_parent(&window),
            None => dialog,
        }
    }
}

impl BackupFiles for BackupDialogs {
    fn save_to(&self, suggested: &str) -> Option<PathBuf> {
        self.dialog()
            .set_title("Save a backup of this profile")
            .set_file_name(suggested)
            .blocking_save_file()?
            .into_path()
            .ok()
    }

    fn open_from(&self) -> Option<PathBuf> {
        self.dialog()
            .set_title("Restore a backup")
            .blocking_pick_file()?
            .into_path()
            .ok()
    }
}

/// The log: in development, the console (or the file `profiles.ps1` sends
/// it to); in a release build, which has no console (an installed app),
/// `desktop.log` in the profile's folder, beside the stream window's
/// `viewer.log`. `RUST_LOG` overrides the level.
fn start_logging(profile: &Profile) {
    use tracing_subscriber::EnvFilter;

    // Without RUST_LOG (a terminal that didn't run scripts/dev.ps1), the same
    // level as an installed app, rather than errors only.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("couch=info,iroh=warn"));
    if cfg!(debug_assertions) {
        tracing_subscriber::fmt()
            // Plain text when logging to a file (scripts/profiles.ps1).
            .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
            .with_env_filter(filter)
            .init();
        return;
    }
    let dir = profile.data_dir();
    let path = dir.join("desktop.log");
    // One older log kept, so the file never grows without end.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_LIMIT) {
        let _ = std::fs::rename(&path, dir.join("desktop.old.log"));
    }
    let file = std::fs::create_dir_all(dir).and_then(|()| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
    });
    match file {
        Ok(file) => tracing_subscriber::fmt()
            .with_ansi(false)
            .with_env_filter(filter)
            .with_writer(std::sync::Mutex::new(file))
            .init(),
        // Without a log file, the app still runs.
        Err(_) => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}

/// `desktop.log` is set aside as `desktop.old.log` at start past this size.
const LOG_LIMIT: u64 = 10 * 1024 * 1024;

/// Development only: `couchco://` links open this build if
/// `COUCH_DEV_LINKS=1` (P5). The installer registers them for an installed
/// app (M10a), and this build's registration, per user, would outrank an
/// install for everyone: its links would start the debug exe, whose window
/// needs the dev server. So without the switch, this user's registration is
/// removed if it points at this build (not with the plugin's `unregister`,
/// which goes for the installer's machine-wide one first).
fn dev_links(app: &AppHandle) {
    if std::env::var("COUCH_DEV_LINKS").is_ok_and(|v| v.trim() == "1") {
        if let Err(e) = app.deep_link().register_all() {
            tracing::warn!("could not register invite links: {e}");
        }
        return;
    }
    match couch_engine::forget_own_link_registration(INVITE_SCHEME) {
        Ok(true) => tracing::info!(
            "invite links no longer open this development build (COUCH_DEV_LINKS=1 to keep them)"
        ),
        Ok(false) => {}
        Err(e) => tracing::warn!("invite links: {e}"),
    }
}

/// Hands invite links to the engine, which keeps them for the UI. Never
/// logged: an invite is half of what lets someone in.
fn open_links(app: &AppHandle, urls: impl IntoIterator<Item = tauri::Url>) {
    let engine = app.state::<Engine>();
    let mut opened = false;
    for url in urls {
        if url.scheme() == INVITE_SCHEME {
            engine.open_link(url.as_str());
            opened = true;
        }
    }
    // Only for a link: a start at sign-in (M25c) may come with none, and
    // stays in the notification area.
    if opened {
        show_main_window(app);
    }
}

fn show_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    // Started in the notification area: placed where it was, the first time.
    if !PLACED.swap(true, Ordering::SeqCst)
        && let Some(file) = app.try_state::<WindowFile>()
    {
        window_state::restore(&window, &file.0);
        return;
    }
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

/// The argument the app starts with at sign-in (M25c,
/// `couch_platform_win::startup`): the notification area only.
const BACKGROUND: &str = "--background";

/// The main window was placed where it was last ([`window_state::restore`]).
static PLACED: AtomicBool = AtomicBool::new(false);

/// Shows or hides the app's icon in the notification area (M25c).
fn show_tray(app: &AppHandle, on: bool) {
    if let Some(tray) = app.tray_by_id(TRAY)
        && let Err(e) = tray.set_visible(on)
    {
        tracing::warn!("the notification area icon: {e}");
    }
}

const TRAY: &str = "main";

/// The app's icon in the notification area (M25c), made hidden: shown while
/// the user keeps the app there ([`show_tray`]), when closing the window
/// hides it there. A click opens the window, its menu opens it or quits.
fn tray(app: &AppHandle) -> tauri::Result<()> {
    use tauri::{
        menu::{Menu, MenuItem},
        tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    };
    let open = MenuItem::with_id(app, "open", "Open Couch Co-App", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut tray = TrayIconBuilder::with_id(TRAY)
        .tooltip("Couch Co-App")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "quit" => {
                save_window(app);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    // Hidden until the engine says the user keeps it (after `build`).
    tray.build(app)?.set_visible(false)?;
    Ok(())
}

fn fail(error: impl std::fmt::Display) -> ! {
    eprintln!("Couch Co-App: {error}");
    std::process::exit(2);
}
