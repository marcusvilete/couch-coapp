//! Thin Tauri adapter over couch-engine: one command in, one event channel out.
//! UI logic belongs in the React app, behaviour in couch-engine.

use couch_engine::{Command, Engine, Profile, Response};
use tauri::Emitter;
use tokio::sync::broadcast::error::RecvError;

/// The event the UI listens on for `couch_engine::Event`s.
const ENGINE_EVENT: &str = "engine-event";

#[tauri::command]
async fn engine_command(
    engine: tauri::State<'_, Engine>,
    command: Command,
) -> Result<Response, String> {
    engine.handle(command).await.map_err(|e| e.to_string())
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let profile = match Profile::from_env() {
        Ok(profile) => profile,
        Err(e) => {
            eprintln!("Couch Co-App: {e}");
            std::process::exit(2);
        }
    };
    tracing::info!(profile = profile.name(), data_dir = %profile.data_dir().display(), "starting");
    let engine = Engine::new(profile);
    let mut events = engine.subscribe();

    tauri::Builder::default()
        .manage(engine)
        .setup(move |app| {
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
        .invoke_handler(tauri::generate_handler![engine_command])
        .run(tauri::generate_context!())
        .expect("error while running Couch Co-App");
}
