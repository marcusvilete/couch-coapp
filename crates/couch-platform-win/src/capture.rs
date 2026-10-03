//! Windows.Graphics.Capture (through windows-capture) as a [`Capturer`]: one
//! window or one monitor, BGRA, no yellow border.
//!
//! Frames are copied to CPU memory here. The zero-copy path that hands the
//! D3D11 texture straight to the encoder is M9.

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use couch_media::{BgraFrame, CaptureEvent, Capturer, MediaError, mailbox::Mailbox};
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

use crate::clock;

/// What to capture.
#[derive(Debug, Clone)]
pub enum CaptureSource {
    /// The first visible window whose title contains this text.
    WindowTitle(String),
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
    pub title: String,
    pub process: String,
    pub process_id: u32,
}

/// Lists capturable windows (visible, titled).
pub fn list_windows() -> Result<Vec<WindowInfo>, MediaError> {
    let windows = Window::enumerate().map_err(backend)?;
    Ok(windows
        .into_iter()
        .filter(|w| w.is_valid())
        .filter_map(|w| {
            let title = w.title().ok()?;
            (!title.trim().is_empty()).then(|| WindowInfo {
                title,
                process: w.process_name().unwrap_or_default(),
                process_id: w.process_id().unwrap_or_default(),
            })
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
        let control = match source {
            CaptureSource::WindowTitle(title) => {
                let window = Window::from_contains_name(title)
                    .map_err(|e| MediaError::Backend(format!("no window titled {title:?}: {e}")))?;
                Handler::start_free_threaded(settings(window, cursor, interval, shared.clone()))
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
        })
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
