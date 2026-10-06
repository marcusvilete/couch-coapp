//! Windows.Graphics.Capture (through windows-capture) as a [`Capturer`]: one
//! window or one monitor, BGRA, no yellow border.
//!
//! Frames are copied to CPU memory here. The zero-copy path that hands the
//! D3D11 texture straight to the encoder is M9.

use std::{ffi::c_void, sync::Arc, time::Duration};

use bytes::Bytes;
use couch_media::{BgraFrame, CaptureEvent, Capturer, MediaError, mailbox::Mailbox};
use windows::Win32::{
    Foundation::HWND,
    Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
};
use windows_capture::{
    capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
    window::Window,
};

use crate::{clock, foreground::WindowId};

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
    /// the content changed, so this is a cap, not a rate.
    pub min_interval: Duration,
    pub cursor: bool,
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

fn is_cloaked(window: WindowId) -> bool {
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
    frames: Mailbox<BgraFrame>,
}

struct Handler {
    shared: Arc<Shared>,
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Arc<Shared>;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { shared: ctx.flags })
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
        let (width, height) = (frame.width(), frame.height());
        let mut buffer = frame.buffer()?;
        let stride = buffer.row_pitch();
        let data = Bytes::copy_from_slice(buffer.as_raw_buffer());
        self.shared.frames.put(BgraFrame {
            width,
            height,
            stride,
            data,
            captured_at,
        });
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

fn settings<T>(
    item: T,
    cursor: CursorCaptureSettings,
    interval: MinimumUpdateIntervalSettings,
    shared: Arc<Shared>,
) -> Settings<Arc<Shared>, T>
where
    T: TryInto<windows_capture::settings::GraphicsCaptureItemType>,
{
    Settings::new(
        item,
        cursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        interval,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        shared,
    )
}

impl Capturer for WgcCapturer {
    fn next_event(&mut self, timeout: Duration) -> Result<Option<CaptureEvent>, MediaError> {
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
