//! Thin Tauri adapter over couch-engine: one command in, one event channel out.
//! UI logic belongs in the React app, behaviour in couch-engine.
//!
//! Each profile (`COUCH_PROFILE`) runs as its own single instance, with its
//! own webview data. `couchco://` invite links open in the default profile:
//! Windows runs one command per link scheme, without our environment.

use std::sync::atomic::{AtomicBool, Ordering};

use couch_engine::{Command, Engine, EngineOptions, Profile, Response};
use tauri::{AppHandle, Emitter, Manager, RunEvent, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_deep_link::DeepLinkExt;
use tokio::sync::broadcast::error::RecvError;

/// The event the UI listens on for `couch_engine::Event`s.
const ENGINE_EVENT: &str = "engine-event";
const MAIN_WINDOW: &str = "main";
const INVITE_SCHEME: &str = "couchco";

#[tauri::command]
async fn engine_command(
    engine: tauri::State<'_, Engine>,
    command: Command,
) -> Result<Response, String> {
    engine.handle(command).await.map_err(|e| e.to_string())
}

/// Starts the app again, as after a change in Settings → Network. It goes
/// through the usual exit, so the rooms hear we are going (`RunEvent::Exit`
/// below) and the single-instance lock is let go before the new one starts
/// ([`start_again`]).
#[tauri::command]
fn restart_app(app: AppHandle) {
    RESTART.store(true, Ordering::SeqCst);
    app.exit(0);
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
    let app = tauri::Builder::default()
        // First, so a second launch of this profile hands its arguments (an
        // invite link) to the running one and exits inside `build`, before
        // it brings our identity online.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .setup(move |app| {
            let config = app
                .config()
                .app
                .windows
                .iter()
                .find(|w| w.label == MAIN_WINDOW)
                .cloned()
                .ok_or("no main window in tauri.conf.json")?;
            let mut window = WebviewWindowBuilder::from_config(app.handle(), &config)?
                .data_directory(window_profile.data_dir().join("webview"));
            if !window_profile.is_default() {
                window = window.title(format!("{} · {}", config.title, window_profile.name()));
            }
            window.build()?;

            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                open_links(&handle, event.urls());
            });
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                open_links(app.handle(), urls);
            }
            // Development only: point `couchco://` at this build. The
            // installer registers it for release builds (M10).
            if cfg!(debug_assertions)
                && window_profile.is_default()
                && let Err(e) = app.deep_link().register_all()
            {
                tracing::warn!("could not register invite links: {e}");
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
    let options = EngineOptions::desktop(&profile);
    let engine =
        tauri::async_runtime::block_on(Engine::start(profile, options)).unwrap_or_else(|e| fail(e));
    app.manage(engine);

    app.run(|app, event| match event {
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

/// Hands invite links to the engine, which keeps them for the UI. Never
/// logged: an invite is half of what lets someone in.
fn open_links(app: &AppHandle, urls: impl IntoIterator<Item = tauri::Url>) {
    let engine = app.state::<Engine>();
    for url in urls {
        if url.scheme() == INVITE_SCHEME {
            engine.open_link(url.as_str());
        }
    }
    show_main_window(app);
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn fail(error: impl std::fmt::Display) -> ! {
    eprintln!("Couch Co-App: {error}");
    std::process::exit(2);
}
