//! The Windows backend: Windows.Graphics.Capture, FFmpeg's encoders, WASAPI
//! loopback, `SendInput`, ViGEm pads, the host's controller through SDL3 and
//! HidHide, the panic-key hook, and `couch-viewer` as a child process.

use std::{any::Any, process::Stdio, sync::Arc, time::Duration};

use couch_codec_ffmpeg::FfmpegEncoder;
use couch_input::{
    DEV_PAD_IDS, HidUsage, InputError, KbmInjector, VirtualPad, VirtualPadFactory,
    local::{ControllerIds, LocalPads},
};
use couch_media::{
    AudioSource, Capturer, EncoderConfig, MediaError, VideoEncoder, audio::ToneSource,
    clock::Clock, testpattern::TestPattern,
};
use couch_platform_win::{
    audio::{LoopbackCapture, LoopbackTarget},
    capture::{self, CaptureOptions, CaptureSource, MonitorId, WgcCapturer},
    clock::QpcClock,
    foreground::{self, WindowId},
    game, held_input, hidhide,
    panic_key::PanicKey,
    sendinput::SendInputInjector,
    vigem::VigemPadFactory,
};

use super::{
    AudioKind, Backend, Focus, GameProcess, Hiding, NativeId, ScreenSource, Source, Sources,
    ViewerOptions, ViewerProcess, WindowSource,
};

/// The viewer, next to our own exe.
const VIEWER_EXE: &str = "couch-viewer.exe";
/// `CREATE_NO_WINDOW`: a console program started this way gets no console
/// window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct WindowsBackend;

struct WatchedGame(game::GameProcess);

impl GameProcess for WatchedGame {
    fn running(&self) -> bool {
        self.0.running()
    }

    fn windows(&self) -> Vec<NativeId> {
        self.0.windows().into_iter().map(|w| w.0).collect()
    }
}

type Capture = (Box<dyn Capturer>, Option<NativeId>);

fn wgc(source: CaptureSource, options: CaptureOptions) -> Result<Capture, MediaError> {
    let capturer = WgcCapturer::start(&source, options)?;
    let window = capturer.window().map(|w| w.0);
    Ok((Box::new(capturer), window))
}

impl Backend for WindowsBackend {
    fn clock(&self) -> Arc<dyn Clock> {
        Arc::new(QpcClock)
    }

    fn sources(&self) -> Result<Sources, MediaError> {
        let mut elevated = std::collections::HashMap::new();
        let windows = capture::list_windows()?
            .into_iter()
            .map(|w| WindowSource {
                id: w.window.0,
                title: w.title,
                app: w.process,
                elevated: *elevated
                    .entry(w.process_id)
                    .or_insert_with(|| game::runs_elevated(w.process_id)),
            })
            .collect();
        let screens = capture::list_monitors()?
            .into_iter()
            .map(|m| ScreenSource {
                id: m.monitor.0,
                name: m.name,
                width: m.width,
                height: m.height,
                primary: m.primary,
            })
            .collect();
        Ok(Sources { windows, screens })
    }

    fn capture(&self, source: &Source, fps: u32) -> Result<Capture, MediaError> {
        let options = CaptureOptions {
            min_interval: Duration::from_secs(1) / fps.max(1),
            cursor: true,
        };
        match source {
            Source::Window(id) => wgc(CaptureSource::Window(WindowId(*id)), options),
            Source::Screen(id) => wgc(CaptureSource::Monitor(MonitorId(*id)), options),
            Source::WindowTitle(title) => wgc(CaptureSource::WindowTitle(title.clone()), options),
            Source::PrimaryScreen => wgc(CaptureSource::PrimaryMonitor, options),
            &Source::TestPattern { width, height } => Ok((
                Box::new(TestPattern::new(width, height, fps, Arc::new(QpcClock))),
                None,
            )),
        }
    }

    fn encoder(&self, config: EncoderConfig) -> Result<Box<dyn VideoEncoder>, MediaError> {
        Ok(Box::new(FfmpegEncoder::open_best(config)?))
    }

    fn audio(&self, kind: AudioKind) -> Result<Box<dyn AudioSource>, MediaError> {
        Ok(match kind {
            AudioKind::Game(window) => {
                let pid = foreground::window_process(WindowId(window))
                    .ok_or_else(|| MediaError::Backend("the game window is gone".into()))?;
                tracing::info!("audio: process {pid} and its children");
                Box::new(LoopbackCapture::start(LoopbackTarget::ProcessTree(pid))?)
            }
            AudioKind::System => {
                tracing::info!("audio: everything this PC plays, except this app");
                Box::new(LoopbackCapture::start(LoopbackTarget::AllExcept(
                    std::process::id(),
                ))?)
            }
            AudioKind::Tone => Box::new(ToneSource::new(Arc::new(QpcClock))),
        })
    }

    fn plug_pad(&self, real_ids: bool) -> Result<Box<dyn VirtualPad>, InputError> {
        VigemPadFactory::connect((!real_ids).then_some(DEV_PAD_IDS))?.plug()
    }

    fn kbm(&self) -> Box<dyn KbmInjector> {
        Box::new(SendInputInjector::new())
    }

    fn controllers(&self) -> usize {
        couch_platform_win::controllers::count()
    }

    fn local_pads(&self) -> Option<&dyn LocalPads> {
        Some(couch_gamepad::host_pads())
    }

    fn hiding(&self) -> Hiding {
        match hidhide::state() {
            hidhide::State::NotInstalled => Hiding::Unavailable,
            hidhide::State::Ready => Hiding::Ready,
            hidhide::State::Busy => Hiding::Busy,
        }
    }

    fn hide_controller(&self, ids: ControllerIds) -> Result<Box<dyn Any + Send>, String> {
        hidhide::hide(ids)
            .map(|hidden| Box::new(hidden) as Box<dyn Any + Send>)
            .map_err(|e| e.to_string())
    }

    fn focus(&self, window: NativeId) -> Focus {
        let window = WindowId(window);
        Focus {
            foreground: foreground::is_foreground(window),
            pointer_inside: foreground::pointer_over(window),
        }
    }

    fn window_exists(&self, window: NativeId) -> bool {
        foreground::window_exists(WindowId(window))
    }

    fn release_left_input(&self) -> usize {
        held_input::release_on_panic();
        // A controller a crashed host left hidden works in games again.
        hidhide::restore_stale();
        held_input::release_stale()
    }

    fn game_process(&self, window: NativeId) -> Option<Box<dyn GameProcess>> {
        let game = game::GameProcess::of_window(WindowId(window))?;
        tracing::info!("watching the game's process {}", game.pid());
        Some(Box::new(WatchedGame(game)))
    }

    fn launch_viewer(&self, options: &ViewerOptions) -> Result<ViewerProcess, MediaError> {
        let backend = |e: std::io::Error| MediaError::Backend(e.to_string());
        let exe = std::env::current_exe()
            .map_err(backend)?
            .with_file_name(VIEWER_EXE);
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
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .map_err(|e| MediaError::Backend(format!("starting {}: {e}", exe.display())))?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        Ok(ViewerProcess {
            stdin: Box::pin(stdin),
            stdout: Box::pin(stdout),
            guard: Box::new(child),
        })
    }

    fn panic_key(
        &self,
        key: HidUsage,
        on_press: Box<dyn Fn() + Send>,
    ) -> Result<Box<dyn Any + Send>, MediaError> {
        let hook = PanicKey::install(key.0, on_press)
            .map_err(|e| MediaError::Backend(format!("the panic-key hook: {e}")))?;
        Ok(Box::new(hook))
    }
}
