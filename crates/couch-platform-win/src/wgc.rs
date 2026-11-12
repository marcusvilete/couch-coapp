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
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
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
    /// The frames' pixel format.
    pub format: Format,
}

/// The pixel format Windows hands frames over in (M29).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// 8-bit BGRA, sRGB: what an SDR desktop is composed in. Of a monitor in
    /// HDR, Windows converts its scRGB to this itself.
    Bgra8,
    /// 16-bit floating point RGBA, linear scRGB (1.0 = 80 nits): what
    /// Windows composes a monitor in HDR in.
    Fp16,
}

impl Format {
    fn pixel_format(self) -> DirectXPixelFormat {
        match self {
            Self::Bgra8 => DirectXPixelFormat::B8G8R8A8UIntNormalized,
            Self::Fp16 => DirectXPixelFormat::R16G16B16A16Float,
        }
    }
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
    /// The format asked for (FP16 when set), outside the lock: a sink may
    /// ask for another from inside its own call ([`FormatSwitch`]), which
    /// runs under it. And the one the pool makes now: they differ until
    /// the next frame remakes the pool.
    wanted: Arc<AtomicBool>,
    pool_format: Format,
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

/// Changes a running session's format from any thread, its sink's own call
/// included (M29: the monitor went in or out of HDR, or converting failed);
/// harmless once the session is gone.
#[derive(Clone)]
pub struct FormatSwitch(Arc<AtomicBool>);

impl FormatSwitch {
    /// A switch for a session to start with ([`Session::start_switched`]),
    /// first asking for `format`: whoever holds it can change the format
    /// from the session's first frame on.
    pub fn new(format: Format) -> Self {
        Self(Arc::new(AtomicBool::new(format == Format::Fp16)))
    }

    fn get(&self) -> Format {
        if self.0.load(Ordering::SeqCst) {
            Format::Fp16
        } else {
            Format::Bgra8
        }
    }

    /// Frames in `format` from the next one on; the frame that comes first
    /// is still in the old one.
    pub fn set(&self, format: Format) {
        // Never the session's lock: this may run inside a frame's delivery,
        // which holds it (M29's fallback deadlocked so).
        self.0.store(format == Format::Fp16, Ordering::SeqCst);
    }
}

impl Session {
    pub fn start(
        target: Target,
        options: Options,
        device: &ID3D11Device,
        sink: impl Sink,
    ) -> windows_core::Result<Self> {
        Self::start_switched(
            target,
            options,
            device,
            sink,
            FormatSwitch::new(options.format),
        )
    }

    /// [`Self::start`] with a format switch made beforehand, which the sink
    /// may hold to change the format from the first frame on;
    /// `options.format` is ignored for the switch's.
    pub fn start_switched(
        target: Target,
        options: Options,
        device: &ID3D11Device,
        sink: impl Sink,
        switch: FormatSwitch,
    ) -> windows_core::Result<Self> {
        winrt_ready();
        let format = switch.get();
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
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt,
            format.pixel_format(),
            buffers,
            size,
        )?;
        let wanted = switch.0;
        let state = Arc::new(Mutex::new(State {
            sink: Box::new(sink),
            device: Device {
                d3d: device.clone(),
                winrt,
            },
            size,
            wanted,
            pool_format: format,
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
        let wanted = if state.wanted.load(Ordering::SeqCst) {
            Format::Fp16
        } else {
            Format::Bgra8
        };
        if content != state.size {
            pool.Recreate(
                &state.device.winrt,
                wanted.pixel_format(),
                state.buffers,
                content,
            )?;
            state.size = content;
            state.pool_format = wanted;
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
        // Another format asked for (the monitor went in or out of HDR):
        // the pool remade after this frame, which the sink took as it came,
        // so a still window still has its picture.
        // Read again: the sink may have asked for another during its call
        // (the capture giving up on converting HDR), and a still window may
        // send no next frame.
        let wanted = if state.wanted.load(Ordering::SeqCst) {
            Format::Fp16
        } else {
            Format::Bgra8
        };
        if wanted != state.pool_format {
            pool.Recreate(
                &state.device.winrt,
                wanted.pixel_format(),
                state.buffers,
                content,
            )?;
            state.pool_format = wanted;
        }
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
