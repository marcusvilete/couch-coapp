fn main() {
    // The app's own commands get permissions (`allow-…`), so only the
    // windows whose capabilities grant them may call them: the main window,
    // never the host overlay (M8), which only draws what it is sent.
    let attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(&["engine_command", "restart_app"]));
    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}
