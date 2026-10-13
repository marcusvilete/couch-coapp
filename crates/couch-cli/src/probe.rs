//! `couch-cli probe`: one line per capability, then a verdict.

use std::{process::ExitCode, time::Duration};

use couch_net::endpoint::RelayChoice;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    /// Works without it for now, but a later milestone needs it.
    Warn,
    Fail,
    /// Context only, e.g. an encoder this GPU doesn't have.
    Info,
}

#[derive(Default)]
struct Report {
    failures: usize,
    warnings: usize,
}

impl Report {
    fn line(&mut self, status: Status, label: &str, detail: impl AsRef<str>) {
        let tag = match status {
            Status::Ok => " ok ",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
            Status::Info => " -- ",
        };
        match status {
            Status::Fail => self.failures += 1,
            Status::Warn => self.warnings += 1,
            Status::Ok | Status::Info => {}
        }
        println!("[{tag}] {label:<24} {}", detail.as_ref());
    }
}

pub async fn run(net_timeout: Duration) -> ExitCode {
    let mut report = Report::default();

    probe_ffmpeg(&mut report).await;
    probe_network(&mut report, net_timeout).await;
    probe_viewer(&mut report);
    probe_virtual_pad(&mut report);

    println!();
    if report.failures == 0 {
        println!(
            "Probe passed{}.",
            if report.warnings > 0 {
                format!(" with {} warning(s)", report.warnings)
            } else {
                String::new()
            }
        );
        ExitCode::SUCCESS
    } else {
        println!("Probe failed: {} check(s) failed.", report.failures);
        ExitCode::FAILURE
    }
}

async fn probe_ffmpeg(report: &mut Report) {
    let ffmpeg = tokio::task::spawn_blocking(couch_codec_ffmpeg::probe::probe)
        .await
        .expect("FFmpeg probe panicked");
    report.line(Status::Info, "FFmpeg", &ffmpeg.version);
    for encoder in &ffmpeg.encoders {
        match &encoder.result {
            Ok(options) => report.line(
                Status::Ok,
                encoder.name,
                format!("opens at 1080p60 (options: {options})"),
            ),
            Err(e) => report.line(Status::Info, encoder.name, format!("unavailable: {e}")),
        }
    }
    match ffmpeg.best_hardware_encoder() {
        Some(name) => report.line(Status::Ok, "H.264 hardware encode", name),
        None => report.line(
            Status::Fail,
            "H.264 hardware encode",
            "no hardware encoder opened",
        ),
    }
    match &ffmpeg.d3d11va {
        Ok(()) => report.line(Status::Ok, "D3D11VA decode", "device created"),
        Err(e) => report.line(Status::Fail, "D3D11VA decode", e),
    }
}

async fn probe_network(report: &mut Report, timeout: Duration) {
    match couch_net::endpoint::probe(RelayChoice::N0Public, timeout).await {
        Ok(net) => match net.online_after {
            Some(after) => report.line(
                Status::Ok,
                "iroh",
                format!(
                    "online in {:.1} s via {}; {} direct address(es); id {}",
                    after.as_secs_f32(),
                    net.relay_urls.first().map_or("no relay", String::as_str),
                    net.direct_addrs,
                    &net.endpoint_id[..10]
                ),
            ),
            None => report.line(
                Status::Fail,
                "iroh",
                format!("no relay reached within {} s", timeout.as_secs()),
            ),
        },
        Err(e) => report.line(Status::Fail, "iroh", e.to_string()),
    }
}

fn probe_viewer(report: &mut Report) {
    match run_viewer_probe() {
        Ok(json) => {
            let version = json["sdl_version"].as_str().unwrap_or("?");
            let pads: Vec<&str> = json["gamepads"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            report.line(Status::Ok, "Viewer / SDL3", format!("SDL {version}"));
            let detail = if pads.is_empty() {
                "none connected".to_string()
            } else {
                pads.join(", ")
            };
            report.line(Status::Info, "Gamepads", detail);
        }
        Err(e) => report.line(Status::Fail, "Viewer / SDL3", e),
    }
}

/// Runs `couch-viewer --probe` from next to this executable.
fn run_viewer_probe() -> Result<serde_json::Value, String> {
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name(format!("couch-viewer{}", std::env::consts::EXE_SUFFIX));
    if !exe.exists() {
        return Err(format!(
            "{} not found; build it with `cargo build -p couch-viewer`",
            exe.display()
        ));
    }
    let output = std::process::Command::new(&exe)
        .arg("--probe")
        .output()
        .map_err(|e| format!("could not start {}: {e}", exe.display()))?;
    if !output.status.success() {
        // 0xC0000135 is STATUS_DLL_NOT_FOUND.
        if output.status.code() == Some(0xC000_0135_u32 as i32) {
            return Err(
                "SDL3.dll not found next to the viewer; run `cargo xtask stage-dlls`".into(),
            );
        }
        return Err(format!(
            "exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("unreadable probe output: {e}"))
}

fn probe_virtual_pad(report: &mut Report) {
    #[cfg(windows)]
    match couch_platform_win::vigem::probe() {
        Ok(()) => report.line(Status::Ok, "ViGEmBus", "connected"),
        Err(e) => report.line(
            Status::Warn,
            "ViGEmBus",
            format!("{e}. Needed from M2: install ViGEmBus 1.22.0"),
        ),
    }
    #[cfg(not(windows))]
    report.line(
        Status::Info,
        "Virtual gamepad",
        "no backend on this platform yet",
    );
}
