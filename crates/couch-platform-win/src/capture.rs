//! Windows.Graphics.Capture (our own, [`crate::wgc`], since M9f) as a
//! [`Capturer`]: one window or one monitor, BGRA, no yellow border where
//! Windows allows hiding it (Windows 11).
//!
//! Frames come out on the GPU ([`FrameOutput::Gpu`], M9b: copied into a
//! [`TexturePool`] for an encoder that reads textures) or in memory
//! ([`FrameOutput::Cpu`], for tools that read pixels).
//!
//! `COUCH_CAPTURE_BUFFERS` sets the frame pool's buffers (2;
//! 1 as before M9f). `COUCH_CAPTURE_TRACE=<file>` appends each GPU frame's
//! timings: `entry_us,windows_us,lock_us,copy_us,flush_us,total_us,outcome`.

use std::{
    ffi::c_void,
    io::{LineWriter, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use couch_media::{
    BgraFrame, CaptureEvent, Capturer, MediaError, Micros, VideoFrame, mailbox::Mailbox,
};
use windows::Win32::{
    Foundation::HWND,
    Graphics::{
        Direct3D11::{
            D3D11_BOX, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC,
            ID3D11Texture2D,
        },
        Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
    },
    UI::WindowsAndMessaging::IsWindow,
};

use crate::{
    clock,
    foreground::WindowId,
    gpu_frames::{TexturePool, staging_texture},
    monitor, top_windows,
    wgc::{self, Arrived, Sink, Target},
};

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
    Ok(top_windows::capturable()
        .into_iter()
        .filter_map(|window| {
            let process_id = top_windows::process_id(window).unwrap_or_default();
            if ours.contains(&process_id) || is_cloaked(window) {
                return None;
            }
            let title = top_windows::title(window);
            (!title.trim().is_empty()).then(|| WindowInfo {
                window,
                title,
                process: top_windows::process_name(process_id).unwrap_or_default(),
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
    let primary = monitor::primary_handle();
    Ok(monitor::handles()
        .into_iter()
        .enumerate()
        .map(|(i, id)| {
            let (width, height) = monitor::size(id).unwrap_or((0, 0));
            MonitorInfo {
                monitor: MonitorId(id),
                name: monitor::name(id).unwrap_or_else(|| format!("Screen {}", i + 1)),
                width,
                height,
                primary: id == primary,
            }
        })
        .collect())
}

/// How often a capture checks its window or monitor still exists.
const SOURCE_CHECK: Duration = Duration::from_millis(100);

/// The smallest frame worth streaming, width and height. NVENC refuses H.264
/// frames under about 145x49; nothing a game shows is this small.
const MIN_FRAME: (u32, u32) = (160, 64);

/// The frame pool's buffers: two, so Windows can write the next frame while
/// the GPU still has to copy the last (M9f); `COUCH_CAPTURE_BUFFERS` (1 to 4)
/// for measuring.
fn buffers() -> i32 {
    std::env::var("COUCH_CAPTURE_BUFFERS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|n| (1..=4).contains(n))
        .unwrap_or(2)
}

struct Shared {
    frames: Mailbox<VideoFrame>,
    /// Keeping frames on the GPU failed, and they come in memory now: not
    /// yet reported ([`CaptureEvent::GpuFramesFailed`]).
    gpu_failed: AtomicBool,
}

/// Takes each frame on Windows' thread: into the texture pool, or to memory.
struct Handler {
    shared: Arc<Shared>,
    output: FrameOutput,
    pool: Option<TexturePool>,
    /// For frames to memory: the texture they are copied into to be read.
    staging: Option<ID3D11Texture2D>,
    /// Frames are being skipped as too small (logged once each time).
    too_small: bool,
    trace: Option<LineWriter<std::fs::File>>,
}

impl Handler {
    /// The frame copied into the pool, which is replaced first if the device,
    /// format or size changed. `None`: every texture is still in use.
    fn on_gpu(
        &mut self,
        frame: &Arrived<'_>,
        captured_at: Micros,
    ) -> Result<Option<VideoFrame>, MediaError> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: fills a local description of the live frame texture.
        unsafe { frame.texture.GetDesc(&mut desc) };
        // 4:2:0 encoders need even sizes; the copy drops an odd last row or column.
        let (width, height) = (frame.width & !1, frame.height & !1);
        let pool = match self.pool.take() {
            Some(pool) if pool.fits(frame.device, desc.Format, width, height) => pool,
            _ => TexturePool::new(frame.device, desc.Format, width, height)?,
        };
        let pool = self.pool.insert(pool);
        Ok(pool.copy(frame.texture, captured_at)?.map(VideoFrame::Gpu))
    }

    /// The frame read into memory, through a staging texture kept while the
    /// size and format stay.
    fn on_cpu(
        &mut self,
        frame: &Arrived<'_>,
        captured_at: Micros,
    ) -> Result<BgraFrame, MediaError> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: fills a local description of the live frame texture.
        unsafe { frame.texture.GetDesc(&mut desc) };
        let fits = self.staging.as_ref().is_some_and(|s| {
            let mut have = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: as above.
            unsafe { s.GetDesc(&mut have) };
            (have.Width, have.Height, have.Format) == (desc.Width, desc.Height, desc.Format)
        });
        if !fits {
            self.staging = Some(staging_texture(frame.device, &desc)?);
        }
        let staging = self.staging.as_ref().expect("made above");
        // SAFETY: a plain getter on a live device.
        let context = unsafe { frame.device.GetImmediateContext() }.map_err(backend)?;
        let region = D3D11_BOX {
            right: frame.width,
            bottom: frame.height,
            back: 1,
            ..Default::default()
        };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: both textures are live on this device, in the same format,
        // and the region lies within both; Map waits for the copy, and the
        // mapping is read and then unmapped before anything else uses it.
        let data = unsafe {
            context.CopySubresourceRegion(staging, 0, 0, 0, 0, frame.texture, 0, Some(&region));
            context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(backend)?;
            let len = mapped.RowPitch as usize * frame.height as usize;
            let data =
                Bytes::copy_from_slice(std::slice::from_raw_parts(mapped.pData.cast::<u8>(), len));
            context.Unmap(staging, 0);
            data
        };
        Ok(BgraFrame {
            width: frame.width,
            height: frame.height,
            stride: mapped.RowPitch,
            data,
            captured_at,
        })
    }
}

impl Sink for Handler {
    fn frame(&mut self, frame: Arrived<'_>) {
        // A minimized window (146x28 on this PC) or one mid-resize: nothing to
        // stream, and too small for the encoders. Sent on, it once made
        // NVENC refuse to open, which turned GPU frames off for the whole
        // stream (M9f: a game minimized as someone started watching).
        if frame.width < MIN_FRAME.0 || frame.height < MIN_FRAME.1 {
            if !self.too_small {
                self.too_small = true;
                tracing::info!(
                    "the captured {}x{} is too small to stream (minimized?): no frames until it grows",
                    frame.width,
                    frame.height
                );
            }
            return;
        }
        self.too_small = false;
        // Windows stamps a frame with a time still ahead of us when it
        // arrives (measured in P5: about when it will be on screen; for a
        // window whose own frames queue up, after we had already encoded
        // and decoded it). A capture time is never later than its delivery,
        // or every capture-to-present figure reads too low.
        let now = clock::now();
        let captured_at = frame.timestamp.map_or(now, |t| t.min(now));
        if self.output == FrameOutput::Gpu {
            let result = self.on_gpu(&frame, captured_at);
            if let Some(trace) = self.trace.as_mut() {
                let (lock, copy, flush) = self.pool.as_ref().map_or((0, 0, 0), |p| p.last_timings);
                let outcome = match &result {
                    Ok(Some(_)) => "put",
                    Ok(None) => "full",
                    Err(_) => "failed",
                };
                let _ = writeln!(
                    trace,
                    "{},{},{lock},{copy},{flush},{},{outcome}",
                    now.0,
                    frame.timestamp.map_or(0, |t| t.0),
                    clock::now().0.saturating_sub(now.0)
                );
            }
            match result {
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
                return;
            }
        }
        match self.on_cpu(&frame, captured_at) {
            Ok(frame) => {
                self.shared.frames.put(VideoFrame::Cpu(frame));
            }
            Err(e) => tracing::debug!("a captured frame couldn't be read: {e}"),
        }
    }

    fn closed(&mut self) {
        self.shared.frames.close();
    }
}

/// Captures one source until dropped.
pub struct WgcCapturer {
    shared: Arc<Shared>,
    /// Running until dropped.
    _session: wgc::Session,
    closed_reported: bool,
    window: Option<WindowId>,
    target: Target,
    /// When the captured window or monitor was last seen to exist.
    source_checked: Instant,
}

impl WgcCapturer {
    pub fn start(source: &CaptureSource, options: CaptureOptions) -> Result<Self, MediaError> {
        let (target, window) = match source {
            CaptureSource::WindowTitle(title) => {
                let window = top_windows::find_by_title(title)
                    .ok_or_else(|| MediaError::Backend(format!("no window titled {title:?}")))?;
                (Target::Window(window.0), Some(window))
            }
            CaptureSource::Window(id) => {
                if !top_windows::is_capturable(*id) {
                    return Err(MediaError::Backend(
                        "the window is gone, or can't be captured".into(),
                    ));
                }
                (Target::Window(id.0), Some(*id))
            }
            CaptureSource::Monitor(id) => (Target::Monitor(id.0), None),
            CaptureSource::PrimaryMonitor => (Target::Monitor(monitor::primary_handle()), None),
        };
        let device = wgc::new_device().map_err(backend)?;
        let shared = Arc::new(Shared {
            frames: Mailbox::new(),
            gpu_failed: AtomicBool::new(false),
        });
        let trace = std::env::var_os("COUCH_CAPTURE_TRACE").and_then(|path| {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()?;
            Some(LineWriter::new(file))
        });
        let handler = Handler {
            shared: shared.clone(),
            output: options.output,
            pool: None,
            staging: None,
            too_small: false,
            trace,
        };
        let wgc_options = wgc::Options {
            buffers: buffers(),
            min_interval: options.min_interval,
            cursor: options.cursor,
        };
        tracing::debug!(
            buffers = wgc_options.buffers,
            cursor_choice = wgc::has("IsCursorCaptureEnabled"),
            border_choice = wgc::has("IsBorderRequired"),
            min_interval = wgc::has("MinUpdateInterval"),
            "capture settings this Windows supports"
        );
        let session =
            wgc::Session::start(target, wgc_options, &device, handler).map_err(backend)?;
        Ok(Self {
            shared,
            _session: session,
            closed_reported: false,
            window,
            target,
            source_checked: Instant::now(),
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

impl Capturer for WgcCapturer {
    fn next_event(&mut self, timeout: Duration) -> Result<Option<CaptureEvent>, MediaError> {
        // A window that closed, or a monitor that went (unplugged, or the
        // display mode changed), ends the capture. Windows also raises the
        // item's Closed event, but a free-threaded frame pool has no thread
        // of ours to deliver it on, so it can't be relied on (M9f).
        if self.source_checked.elapsed() >= SOURCE_CHECK {
            self.source_checked = Instant::now();
            let gone = match self.target {
                // SAFETY: IsWindow takes any handle value.
                Target::Window(w) => !unsafe { IsWindow(Some(HWND(w as *mut c_void))) }.as_bool(),
                // (GetMonitorInfoW answers even for a stale handle.)
                Target::Monitor(m) => !monitor::handles().contains(&m),
            };
            if gone {
                self.shared.frames.close();
            }
        }
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

fn backend(e: impl std::fmt::Display) -> MediaError {
    MediaError::Backend(e.to_string())
}
