//! Starting the stream window, `couch-viewer`, beside our own executable: the
//! same on every system (M16b), but for Windows' console flag.

use std::process::Stdio;

use couch_media::MediaError;

use super::{ViewerOptions, ViewerProcess};

/// `CREATE_NO_WINDOW`: a console program started this way gets no console
/// window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Starts `couch-viewer` from our own executable's folder, its stdin and
/// stdout piped for `couch_proto::ipc`.
pub fn launch(options: &ViewerOptions) -> Result<ViewerProcess, MediaError> {
    let backend = |e: std::io::Error| MediaError::Backend(e.to_string());
    let exe = std::env::current_exe()
        .map_err(backend)?
        .with_file_name(format!("couch-viewer{}", std::env::consts::EXE_SUFFIX));
    if !exe.exists() {
        return Err(MediaError::Backend(format!(
            "{} is missing; build it with `cargo build -p couch-viewer`",
            exe.display()
        )));
    }
    let mut command = tokio::process::Command::new(&exe);
    if let Some((x, y)) = options.position {
        command.args(["--pos", &x.to_string(), &y.to_string()]);
    }
    if let Some((w, h)) = options.size {
        command.args(["--size", &w.to_string(), &h.to_string()]);
    }
    if options.hide_hud {
        command.arg("--no-hud");
    }
    if options.hide_bar {
        command.arg("--no-bar");
    }
    if options.hide_stats {
        command.arg("--hide-stats");
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    if let Some(log) = &options.log {
        // Appended, so an earlier window's log survives the next one.
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .map_err(|e| MediaError::Backend(format!("{}: {e}", log.display())))?;
        command.stderr(Stdio::from(file));
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|e| MediaError::Backend(format!("starting {}: {e}", exe.display())))?;
    let stdin = child.stdin.take().expect("piped");
    let stdout = child.stdout.take().expect("piped");
    let pid = child.id();
    Ok(ViewerProcess {
        stdin: Box::pin(stdin),
        stdout: Box::pin(stdout),
        guard: Box::new(child),
        pid,
    })
}
