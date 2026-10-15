//! Windows.Graphics.Capture, used directly (M9f). Until then the
//! `windows-capture` crate ran it for us, with a frame pool of one buffer,
//! fixed. With one buffer, Windows can't write the next frame until the
//! GPU has run our copy of the last one; under a game that keeps the GPU
//! busy that copy waits, and a 100 fps game was captured at about 70. Two
//! buffers, as Sunshine uses, gave about 100 (`docs/m9f-results.md`).
//!
//! The frame pool is free-threaded: frames arrive on a thread of Windows'
//! pool, one at a time through the session's lock, and the sink sees each
//! texture only for the length of its call.

use std::{
    ffi::c_void,
    sync::{Arc, Mutex, OnceLock, PoisonError},
    time::Duration,
};

use couch_media::Micros;
use windows::{
    Foundation::{Metadata::ApiInformation, TimeSpan, TypedEventHandler},
    Graphics::{
        Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
        SizeInt32,
    },
    Win32::{
        Foundation::{HMODULE, HWND},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::{
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
                D3D11CreateDevice, ID3D11Device, ID3D11Texture2D,
            },
            Dxgi::IDXGIDevice,
            Gdi::HMONITOR,
        },
        System::{
            Com::CoIncrementMTAUsage,
            WinRT::{
                Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
                Graphics::Capture::IGraphicsCaptureItemInterop,
            },
        },
    },
};
use windows_core::{HSTRING, IInspectable, Interface};

use crate::clock;

/// What to capture: a window or a monitor, by handle value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Window(isize),
    Monitor(isize),
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// The frame pool's buffers.
    pub buffers: i32,
    /// At most one frame per this interval (where Windows supports it).
    pub min_interval: Duration,
    pub cursor: bool,
}

/// A captured frame, valid for the sink's call only.
pub struct Arrived<'a> {
    pub texture: &'a ID3D11Texture2D,
    pub device: &'a ID3D11Device,
    /// The content's size: the texture's, or less while the pool catches up
    /// with a resize.
    pub width: u32,
    pub height: u32,
    /// Windows' time for the frame, on our clock.
    pub timestamp: Option<Micros>,
}

/// Where a session's frames go. Called on Windows' threads, one call at a
/// time; it must not panic (it runs inside a Windows callback).
pub trait Sink: Send + 'static {
    fn frame(&mut self, frame: Arrived<'_>);
    /// The window closed, or the monitor went: no more frames.
    fn closed(&mut self);
}

/// A device for capturing: on the default adapter, with BGRA support.
pub fn new_device() -> windows_core::Result<ID3D11Device> {
    let mut device = None;
    // SAFETY: out-parameters are live locals.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    device.ok_or_else(|| windows_core::Error::from_hresult(windows_core::HRESULT(-1)))
}

/// Whether this Windows has the session property `name` (they came over
/// versions: the cursor choice, the border, the minimum interval).
pub fn has(name: &str) -> bool {
    winrt_ready();
    ApiInformation::IsPropertyPresent(
        &HSTRING::from("Windows.Graphics.Capture.GraphicsCaptureSession"),
        &HSTRING::from(name),
    )
    .unwrap_or(false)
}

/// A running capture; dropping it stops it.
pub struct Session {
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    /// The event handlers, once registered.
    frame_token: Option<i64>,
    closed_token: Option<i64>,
    state: Arc<Mutex<State>>,
}

struct State {
    sink: Box<dyn Sink>,
    device: Device,
    /// The pool's frame size.
    size: SizeInt32,
    buffers: i32,
    stopped: bool,
}

/// The capture's devices: COM objects Windows lets any thread use.
struct Device {
    d3d: ID3D11Device,
    winrt: IDirect3DDevice,
}

// SAFETY: D3D11 devices are free-threaded (and ours is multithread-protected
// once a texture pool uses it), and the WinRT device wraps the same device.
unsafe impl Send for Device {}

const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;

impl Session {
    pub fn start(
        target: Target,
        options: Options,
        device: &ID3D11Device,
        sink: impl Sink,
    ) -> windows_core::Result<Self> {
        winrt_ready();
        let interop = windows_core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        // SAFETY: handle values are checked by Windows, which fails for
        // anything that isn't a window or monitor.
        let item: GraphicsCaptureItem = unsafe {
            match target {
                Target::Window(w) => interop.CreateForWindow(HWND(w as *mut c_void))?,
                Target::Monitor(m) => interop.CreateForMonitor(HMONITOR(m as *mut c_void))?,
            }
        };
        let dxgi: IDXGIDevice = device.cast()?;
        // SAFETY: a live DXGI device.
        let winrt: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;
        let size = item.Size()?;
        let buffers = options.buffers.max(1);
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&winrt, FORMAT, buffers, size)?;
        let state = Arc::new(Mutex::new(State {
            sink: Box::new(sink),
            device: Device {
                d3d: device.clone(),
                winrt,
            },
            size,
            buffers,
            stopped: false,
        }));
        let session = pool.CreateCaptureSession(&item)?;
        // From here on, an error drops `this`, which takes off what was
        // registered and closes the session and pool.
        let mut this = Self {
            item,
            pool,
            session,
            frame_token: None,
            closed_token: None,
            state,
        };
        this.frame_token = Some(this.pool.FrameArrived(&TypedEventHandler::<
            Direct3D11CaptureFramePool,
            IInspectable,
        >::new({
            let state = this.state.clone();
            move |pool, _| {
                if let Some(pool) = pool.as_ref() {
                    on_frame(pool, &state);
                }
                Ok(())
            }
        }))?);
        this.closed_token = Some(this.item.Closed(&TypedEventHandler::<
            GraphicsCaptureItem,
            IInspectable,
        >::new({
            let state = this.state.clone();
            move |_, _| {
                let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
                if !state.stopped {
                    state.stopped = true;
                    state.sink.closed();
                }
                Ok(())
            }
        }))?);
        let session = &this.session;
        // Each where this Windows has it; a session asked for one it lacks
        // fails to start. Missing, Windows' default applies: the cursor
        // captured, the yellow border drawn (on the host's screen only), and
        // frames as often as the screen changes (which pacing handles).
        if has("IsCursorCaptureEnabled") {
            session.SetIsCursorCaptureEnabled(options.cursor)?;
        }
        if has("IsBorderRequired") {
            session.SetIsBorderRequired(false)?;
        }
        if has("MinUpdateInterval") {
            session.SetMinUpdateInterval(TimeSpan::from(options.min_interval))?;
        }
        session.StartCapture()?;
        Ok(this)
    }
}

/// The next frame: to the sink, or, if its size changed, the pool remade at
/// the new size (this frame's surface is still the old size).
fn on_frame(pool: &Direct3D11CaptureFramePool, state: &Mutex<State>) {
    // The lock first: Windows may call this on two threads at once, and a
    // call waiting here must not hold a frame, or every buffer is taken and
    // Windows can't write the next frame (M9f measured it: later and fewer
    // frames than one buffer gave).
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    let Ok(frame) = pool.TryGetNextFrame() else {
        return;
    };
    let delivered = (|| -> windows_core::Result<()> {
        if state.stopped {
            return Ok(());
        }
        let content = frame.ContentSize()?;
        if content.Width <= 0 || content.Height <= 0 {
            return Ok(());
        }
        if content != state.size {
            pool.Recreate(&state.device.winrt, FORMAT, state.buffers, content)?;
            state.size = content;
            return Ok(());
        }
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        // SAFETY: the surface of a live frame; the texture is used only
        // until the frame is closed below.
        let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: fills a local description of a live texture.
        unsafe { texture.GetDesc(&mut desc) };
        let timestamp = frame
            .SystemRelativeTime()
            .ok()
            .map(|t| clock::from_system_relative_time(t.Duration));
        let State { sink, device, .. } = &mut *state;
        sink.frame(Arrived {
            texture: &texture,
            device: &device.d3d,
            width: (content.Width as u32).min(desc.Width),
            height: (content.Height as u32).min(desc.Height),
            timestamp,
        });
        Ok(())
    })();
    if let Err(e) = delivered {
        tracing::debug!("a captured frame was skipped: {e}");
    }
    // Back to the pool now, for Windows' next frame.
    let _ = frame.Close();
}

impl Drop for Session {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stopped = true;
        if let Some(token) = self.frame_token {
            let _ = self.pool.RemoveFrameArrived(token);
        }
        if let Some(token) = self.closed_token {
            let _ = self.item.RemoveClosed(token);
        }
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// WinRT on every thread of this process: the capture is started from our
/// own threads, and its events come on Windows' pool. Once per process.
fn winrt_ready() {
    static READY: OnceLock<()> = OnceLock::new();
    READY.get_or_init(|| {
        // SAFETY: no arguments. The usage is never released
        // (CoDecrementMTAUsage), which keeps the MTA for the process's life.
        let _ = unsafe { CoIncrementMTAUsage() };
    });
}
