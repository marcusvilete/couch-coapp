//! Windows.Graphics.Capture (through windows-capture) as a [`Capturer`]: one
//! window or one monitor, BGRA, no yellow border where Windows allows hiding
//! it (Windows 11; see `settings`).
//!
//! Frames come out on the GPU ([`FrameOutput::Gpu`], M9b: copied into a
//! [`TexturePool`] for an encoder that reads textures) or in memory
//! ([`FrameOutput::Cpu`], for tools that read pixels).

use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use couch_media::{
    BgraFrame, CaptureEvent, Capturer, MediaError, Micros, VideoFrame, mailbox::Mailbox,
};
use windows::Win32::{
    Foundation::HWND,
    Graphics::{
        Direct3D11::D3D11_TEXTURE2D_DESC,
        Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
    },
};
use windows_capture::{
    capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::{GraphicsCaptureApi, InternalCaptureControl},
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
    window::Window,
};

use crate::{clock, foreground::WindowId, gpu_frames::TexturePool};

/// A monitor, by its handle value, as [`list_monitors`] found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MonitorId(pub isize);

/// What to capture.
#[derive(Debug, Clone)]
pub enum CaptureSource {
    /// The first visible window whose title contains this text.
    WindowTitle(String),
    /// One window, as [`list_windows`] found it.
    Window(WindowId),
    /// One monitor, as [`list_monitors`] found it.
    Monitor(MonitorId),
    PrimaryMonitor,
}

#[derive(Debug, Clone, Copy)]
pub struct CaptureOptions {
    /// Lower bound between delivered frames. Windows only delivers a frame when
    /// the content changed, and only at the display's refresh: the first
    /// refresh after the interval. So this is a cap, not a rate, and a cap of
    /// one frame at 60 fps gives 48 fps on a 144 Hz screen (M9d); the host
    /// pipeline asks for more and paces them itself.
    pub min_interval: Duration,
    pub cursor: bool,
    pub output: FrameOutput,
}

/// Where captured frames go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOutput {
    /// Copied to memory ([`VideoFrame::Cpu`]).
    Cpu,
    /// Kept on the capture's GPU ([`VideoFrame::Gpu`]). If that fails, the
    /// capture logs why, copies to memory from then on, and says so once
    /// ([`CaptureEvent::GpuFramesFailed`]).
    Gpu,
}

/// A visible top-level window that can be captured.
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub window: WindowId,
    pub title: String,
    pub process: String,
    pub process_id: u32,
}

/// Lists capturable windows: visible, titled, not cloaked (a suspended
/// Store app's window is "visible" but shows nothing), and not ours (this
/// process and its children, such as a viewer).
pub fn list_windows() -> Result<Vec<WindowInfo>, MediaError> {
    let me = std::process::id();
    let mut ours = vec![me];
    ours.extend(crate::process::child_processes(me).unwrap_or_default());
    let windows = Window::enumerate().map_err(backend)?;
    Ok(windows
        .into_iter()
        .filter(|w| w.is_valid())
        .filter_map(|w| {
            let window = WindowId(w.as_raw_hwnd() as isize);
            let process_id = w.process_id().unwrap_or_default();
            if ours.contains(&process_id) || is_cloaked(window) {
                return None;
            }
            let title = w.title().ok()?;
            (!title.trim().is_empty()).then(|| WindowInfo {
                window,
                title,
                process: w.process_name().unwrap_or_default(),
                process_id,
            })
        })
        .collect())
}

pub(crate) fn is_cloaked(window: WindowId) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: the out-parameter is a live local of the size passed.
    let result = unsafe {
        DwmGetWindowAttribute(
            HWND(window.0 as *mut c_void),
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            size_of::<u32>() as u32,
        )
    };
    result.is_ok() && cloaked != 0
}

/// A monitor that can be captured.
#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub monitor: MonitorId,
    /// The monitor's own name ("DELL U2720Q"), or "Screen N".
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

pub fn list_monitors() -> Result<Vec<MonitorInfo>, MediaError> {
    let primary = Monitor::primary()
        .ok()
        .map(|m| m.as_raw_hmonitor() as isize);
    let monitors = Monitor::enumerate().map_err(backend)?;
    Ok(monitors
        .into_iter()
        .enumerate()
        .map(|(i, m)| {
            let id = m.as_raw_hmonitor() as isize;
            MonitorInfo {
                monitor: MonitorId(id),
                name: m
                    .name()
                    .ok()
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| format!("Screen {}", i + 1)),
                width: m.width().unwrap_or(0),
                height: m.height().unwrap_or(0),
                primary: primary == Some(id),
            }
        })
        .collect())
}

struct Shared {
    frames: Mailbox<VideoFrame>,
    output: FrameOutput,
    /// Keeping frames on the GPU failed, and they come in memory now: not
    /// yet reported ([`CaptureEvent::GpuFramesFailed`]).
    gpu_failed: AtomicBool,
}

struct Handler {
    shared: Arc<Shared>,
    output: FrameOutput,
    pool: Option<TexturePool>,
}

impl Handler {
    /// The frame copied into the pool, which is replaced first if the device,
    /// format or size changed. `None`: every texture is still in use.
    fn on_gpu(
        &mut self,
        frame: &Frame,
        captured_at: Micros,
    ) -> Result<Option<VideoFrame>, MediaError> {
        let texture = frame.as_raw_texture();
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: fills a local description of the live frame texture.
        unsafe { texture.GetDesc(&mut desc) };
        // 4:2:0 encoders need even sizes; the copy drops an odd last row or column.
        let (width, height) = (frame.width() & !1, frame.height() & !1);
        if width == 0 || height == 0 {
            // A window a pixel wide (minimized, or mid-resize): nothing to send.
            return Ok(None);
        }
        let device = frame.device();
        let pool = match self.pool.take() {
            Some(pool) if pool.fits(device, desc.Format, width, height) => pool,
            _ => TexturePool::new(device, desc.Format, width, height)?,
        };
        let pool = self.pool.insert(pool);
        Ok(pool.copy(texture, captured_at)?.map(VideoFrame::Gpu))
    }
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Arc<Shared>;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            output: ctx.flags.output,
            shared: ctx.flags,
            pool: None,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let captured_at = frame
            .timestamp()
            .map(|t| clock::from_system_relative_time(t.Duration))
            .unwrap_or_else(|_| clock::now());
        if self.output == FrameOutput::Gpu {
            match self.on_gpu(frame, captured_at) {
                Ok(Some(frame)) => {
                    self.shared.frames.put(frame);
                }
                // Every texture in use: dropped, as a full mailbox would.
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        "keeping frames on the GPU failed ({e}); copying them to memory"
                    );
                    self.output = FrameOutput::Cpu;
                    self.pool = None;
                    self.shared.gpu_failed.store(true, Ordering::Relaxed);
                }
            }
            if self.output == FrameOutput::Gpu {
                return Ok(());
            }
        }
        let (width, height) = (frame.width(), frame.height());
        let mut buffer = frame.buffer()?;
        let stride = buffer.row_pitch();
        let data = Bytes::copy_from_slice(buffer.as_raw_buffer());
        self.shared.frames.put(VideoFrame::Cpu(BgraFrame {
            width,
            height,
            stride,
            data,
            captured_at,
        }));
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        self.shared.frames.close();
        Ok(())
    }
}

/// Captures one source until dropped.
pub struct WgcCapturer {
    shared: Arc<Shared>,
    control: Option<CaptureControl<Handler, Box<dyn std::error::Error + Send + Sync>>>,
    closed_reported: bool,
    window: Option<WindowId>,
}

impl WgcCapturer {
    pub fn start(source: &CaptureSource, options: CaptureOptions) -> Result<Self, MediaError> {
        let shared = Arc::new(Shared {
            frames: Mailbox::new(),
            output: options.output,
            gpu_failed: AtomicBool::new(false),
        });
        let cursor = if options.cursor {
            CursorCaptureSettings::WithCursor
        } else {
            CursorCaptureSettings::WithoutCursor
        };
        let interval = MinimumUpdateIntervalSettings::Custom(options.min_interval);
        let mut captured = None;
        let control = match source {
            CaptureSource::WindowTitle(title) => {
                let window = Window::from_contains_name(title)
                    .map_err(|e| MediaError::Backend(format!("no window titled {title:?}: {e}")))?;
                captured = Some(WindowId(window.as_raw_hwnd() as isize));
                Handler::start_free_threaded(settings(window, cursor, interval, shared.clone()))
            }
            CaptureSource::Window(id) => {
                let window = Window::from_raw_hwnd(id.0 as *mut c_void);
                if !window.is_valid() {
                    return Err(MediaError::Backend(
                        "the window is gone, or can't be captured".into(),
                    ));
                }
                captured = Some(*id);
                Handler::start_free_threaded(settings(window, cursor, interval, shared.clone()))
            }
            CaptureSource::Monitor(id) => {
                let monitor = Monitor::from_raw_hmonitor(id.0 as *mut c_void);
                Handler::start_free_threaded(settings(monitor, cursor, interval, shared.clone()))
            }
            CaptureSource::PrimaryMonitor => {
                let monitor = Monitor::primary().map_err(backend)?;
                Handler::start_free_threaded(settings(monitor, cursor, interval, shared.clone()))
            }
        }
        .map_err(backend)?;
        Ok(Self {
            shared,
            control: Some(control),
            closed_reported: false,
            window: captured,
        })
    }

    /// The captured window; `None` for a monitor.
    pub fn window(&self) -> Option<WindowId> {
        self.window
    }

    /// Frames dropped because the consumer fell behind.
    pub fn dropped_frames(&self) -> u64 {
        self.shared.frames.replaced_count()
    }
}

/// Our capture settings, each one only where this Windows supports it.
/// Windows.Graphics.Capture grew its options over versions, and a capture
/// that asks for one this Windows lacks fails to start: on a Windows 10 PC
/// every stream ended at its first viewer ("Toggling the capture border is
/// not supported"). Where an option is missing, Windows' default applies:
/// - the yellow border around the captured window (Windows 11 lets us hide
///   it; it is drawn on the host's screen, not in the stream);
/// - frames as often as the screen changes, which the pipeline paces
///   anyway (the minimum interval is newer still);
/// - the cursor captured (choosing needs Windows 10 2004).
fn settings<T>(
    item: T,
    cursor: CursorCaptureSettings,
    interval: MinimumUpdateIntervalSettings,
    shared: Arc<Shared>,
) -> Settings<Arc<Shared>, T>
where
    T: TryInto<windows_capture::settings::GraphicsCaptureItemType>,
{
    let can = |check: Result<bool, _>| check.unwrap_or(false);
    let border = if can(GraphicsCaptureApi::is_border_settings_supported()) {
        DrawBorderSettings::WithoutBorder
    } else {
        DrawBorderSettings::Default
    };
    let interval = if can(GraphicsCaptureApi::is_minimum_update_interval_supported()) {
        interval
    } else {
        MinimumUpdateIntervalSettings::Default
    };
    let cursor = if can(GraphicsCaptureApi::is_cursor_settings_supported()) {
        cursor
    } else {
        CursorCaptureSettings::Default
    };
    tracing::debug!(
        ?border,
        ?interval,
        ?cursor,
        "capture settings this Windows supports"
    );
    Settings::new(
        item,
        cursor,
        border,
        SecondaryWindowSettings::Default,
        interval,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        shared,
    )
}

impl Capturer for WgcCapturer {
    fn next_event(&mut self, timeout: Duration) -> Result<Option<CaptureEvent>, MediaError> {
        if self.shared.gpu_failed.swap(false, Ordering::Relaxed) {
            return Ok(Some(CaptureEvent::GpuFramesFailed));
        }
        match self.shared.frames.take_timeout(timeout) {
            Ok(Some(frame)) => Ok(Some(CaptureEvent::Frame(frame))),
            Ok(None) => Ok(None),
            Err(_closed) if !self.closed_reported => {
                self.closed_reported = true;
                Ok(Some(CaptureEvent::SourceClosed))
            }
            Err(_closed) => Err(MediaError::Closed),
        }
    }
}

impl Drop for WgcCapturer {
    fn drop(&mut self) {
        if let Some(control) = self.control.take()
            && let Err(e) = control.stop()
        {
            tracing::warn!("stopping capture failed: {e}");
        }
    }
}

fn backend(e: impl std::fmt::Display) -> MediaError {
    MediaError::Backend(e.to_string())
}
