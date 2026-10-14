//! The Windows backend: Windows.Graphics.Capture, FFmpeg's encoders, WASAPI
//! loopback, `SendInput`, ViGEm pads, the host's controller through SDL3 and
//! HidHide, the panic-key hook, and `couch-viewer` as a child process.

use std::{any::Any, process::Stdio, sync::Arc, time::Duration};

use couch_codec_ffmpeg::{FfmpegEncoder, encoder::GPU_INPUT_ENCODER};
use couch_input::{
    DEV_PAD_IDS, HidUsage, InputError, KbmInjector, VirtualPad, VirtualPadFactory,
    local::{ControllerIds, LocalPads},
    shortcuts::Shortcut,
};
use couch_media::{
    AudioSource, Capturer, EncoderConfig, GpuFrame, MediaError, VideoEncoder, audio::ToneSource,
    clock::Clock, testpattern::TestPattern,
};
use couch_platform_win::{
    audio::{LoopbackCapture, LoopbackTarget},
    capture::{self, CaptureOptions, CaptureSource, FrameOutput, MonitorId, WgcCapturer},
    clock::QpcClock,
    drivers,
    foreground::{self, WindowId},
    game, held_input, hidhide,
    host_keys::HostKeys,
    monitor,
    sendinput::SendInputInjector,
    vigem::VigemPadFactory,
};

use super::{
    AudioKind, Backend, CaptureRequest, Focus, GameProcess, Hiding, NativeId, ScreenSource, Source,
    Sources, ViewerOptions, ViewerProcess, WindowSource,
};
use crate::{DriverName, DriverState, ScreenArea};

fn platform_driver(name: DriverName) -> drivers::Driver {
    match name {
        DriverName::Vigembus => drivers::Driver::ViGEmBus,
        DriverName::Hidhide => drivers::Driver::HidHide,
    }
}

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

/// Captured frames stay on the GPU (M9b) unless `COUCH_HOST_ZERO_COPY=0`, a
/// switch for measuring against the copy through memory from before.
fn zero_copy() -> bool {
    std::env::var("COUCH_HOST_ZERO_COPY").map_or(true, |v| v.trim() != "0")
}

/// Windows' minimum interval between captured frames on the GPU when the
/// stream paces them itself (M9d): up to 250 a second, as Sunshine asks.
/// Each is a GPU copy into the pool, a fraction of a millisecond.
const PACED_INTERVAL: Duration = Duration::from_millis(4);

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
                refresh_hz: monitor::refresh_of_window(w.window.0),
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
                refresh_hz: monitor::refresh_of_monitor(m.monitor.0),
            })
            .collect();
        Ok(Sources { windows, screens })
    }

    fn capture(&self, source: &Source, request: CaptureRequest) -> Result<Capture, MediaError> {
        let fps = request.fps.max(1);
        let output = if zero_copy() && !request.in_memory {
            FrameOutput::Gpu
        } else {
            FrameOutput::Cpu
        };
        // Faster than the stream only on the GPU: frames in memory cost a
        // copy each, so those hosts keep Windows' own limit (see
        // `CaptureOptions::min_interval` for what that costs).
        let min_interval = if request.paced && output == FrameOutput::Gpu {
            PACED_INTERVAL
        } else {
            Duration::from_secs(1) / fps
        };
        if !matches!(source, Source::TestPattern { .. }) {
            tracing::info!(
                "capturing at {fps} fps: frames {}, at most every {:.1} ms{}",
                if output == FrameOutput::Gpu {
                    "on the GPU"
                } else {
                    "in memory"
                },
                min_interval.as_secs_f64() * 1e3,
                if request.paced {
                    ", paced by the stream"
                } else {
                    ", not paced (COUCH_HOST_PACING=0)"
                },
            );
        }
        let options = CaptureOptions {
            min_interval,
            cursor: true,
            output,
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

    fn encoder(
        &self,
        config: EncoderConfig,
        gpu: Option<&GpuFrame>,
    ) -> Result<Box<dyn VideoEncoder>, MediaError> {
        // A developer switch: one encoder by FFmpeg's name, taking frames in
        // memory, to measure another machine's path here (M9d; e.g.
        // `libopenh264`, the software fallback a host without a hardware
        // encoder uses).
        if let Ok(name) = std::env::var("COUCH_HOST_ENCODER")
            && !name.trim().is_empty()
        {
            tracing::info!("encoder {name}, frames in memory (COUCH_HOST_ENCODER)");
            return Ok(Box::new(FfmpegEncoder::open(name.trim(), config)?));
        }
        if let Some(frame) = gpu {
            match FfmpegEncoder::open_gpu(config, frame) {
                Ok(encoder) => return Ok(Box::new(encoder)),
                // Not NVIDIA, or the capture runs on another GPU (a laptop's
                // integrated graphics): frames go through memory, as before M9b.
                Err(e) => tracing::info!(
                    "{GPU_INPUT_ENCODER} can't read the captured frames on the GPU ({e}); \
                     copying them to memory"
                ),
            }
        }
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

    fn low_latency_network(&self) -> Option<Box<dyn Any + Send>> {
        couch_platform_win::wifi::MediaStreaming::enable()
            .map(|mode| Box::new(mode) as Box<dyn Any + Send>)
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

    fn drivers(&self) -> Vec<(DriverName, DriverState, bool)> {
        [DriverName::Vigembus, DriverName::Hidhide]
            .into_iter()
            .map(|name| {
                let driver = platform_driver(name);
                let state = match drivers::state(driver) {
                    drivers::State::Ready => DriverState::Ready,
                    drivers::State::NeedsRestart => DriverState::NeedsRestart,
                    drivers::State::Busy => DriverState::Busy,
                    drivers::State::Missing => DriverState::Missing,
                };
                (name, state, drivers::installer(driver).is_some())
            })
            .collect()
    }

    fn virtual_pads(&self) -> bool {
        couch_platform_win::vigem::probe().is_ok()
    }

    fn install_driver(&self, driver: DriverName) -> Result<(), String> {
        drivers::install(platform_driver(driver))
    }

    fn overlay_area(&self, source: &Source) -> Option<ScreenArea> {
        let area = match source {
            Source::Window(window) => monitor::of_window(*window),
            Source::Screen(screen) => monitor::of_monitor(*screen),
            _ => monitor::primary(),
        }?;
        Some(ScreenArea {
            x: area.x,
            y: area.y,
            width: area.width,
            height: area.height,
        })
    }

    fn host_keys(
        &self,
        keys: &[(HidUsage, Shortcut)],
        on_press: Box<dyn Fn(Shortcut) + Send>,
    ) -> Result<Box<dyn Any + Send>, MediaError> {
        let usages: Vec<u16> = keys.iter().map(|(key, _)| key.0).collect();
        let shortcuts: Vec<Shortcut> = keys.iter().map(|&(_, s)| s).collect();
        let hook = HostKeys::install(&usages, move |i| {
            if let Some(&shortcut) = shortcuts.get(i) {
                on_press(shortcut);
            }
        })
        .map_err(|e| MediaError::Backend(format!("the host-keys hook: {e}")))?;
        Ok(Box::new(hook))
    }
}
