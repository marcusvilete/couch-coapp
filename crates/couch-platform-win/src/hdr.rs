//! A host whose monitor is in HDR (M29). Windows composes such a monitor's
//! desktop in scRGB: 16-bit floating point, linear, BT.709 primaries, 1.0
//! being 80 nits, with SDR content raised to the monitor's SDR white level.
//! A friend's guests saw washed-out colours from such a host. Measured on a
//! virtual HDR monitor (Windows 11 25H2), SDR content comes out right in an
//! 8-bit capture: Windows divides by the SDR white level itself.
//!
//! For `couch-cli hdr-probe`: [`grab`] takes one frame in either format, to
//! show what a capture holds, and [`Pattern`] shows content of known scRGB
//! values, HDR included, to capture.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{SyncSender, sync_channel},
    },
    thread::JoinHandle,
    time::Duration,
};

use couch_media::MediaError;
use windows::{
    Win32::{
        Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::{
                D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
                D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA,
                D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device,
                ID3D11Texture2D,
            },
            Dxgi::{
                Common::{
                    DXGI_ALPHA_MODE_IGNORE, DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709,
                    DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC,
                },
                CreateDXGIFactory1, DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
                DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIFactory2,
                IDXGISwapChain1, IDXGISwapChain3,
            },
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE,
            PeekMessageW, RegisterClassW, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
            TranslateMessage, WINDOW_EX_STYLE, WNDCLASSW, WS_POPUP, WS_VISIBLE,
        },
    },
    core::{HSTRING, Interface, w},
};

use crate::{
    gpu_frames::staging_texture,
    wgc::{self, Arrived, Format, Sink, Target},
};

/// The pattern window's title.
pub const PATTERN_TITLE: &str = "Couch HDR pattern";

/// One patch of the pattern: where it is in the window, and its colour.
#[derive(Debug, Clone)]
pub struct Patch {
    pub name: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Linear scRGB: 1.0 = 80 nits, BT.709 primaries (negative values are
    /// colours outside them).
    pub scrgb: [f32; 3],
}

impl Patch {
    pub fn centre(&self) -> (u32, u32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }
}

/// The pattern's patches in a `width` x `height` window, eight columns of
/// three rows: greys from 20 to 1,600 nits; colours at `sdr_white` (scRGB,
/// the monitor's SDR white level over 80), as SDR content shows; and the
/// same colours at 1,000 nits, as a game in HDR shows highlights.
pub fn patches(width: u32, height: u32, sdr_white: f32) -> Vec<Patch> {
    // BT.2020's green primary in scRGB: a colour no SDR picture holds.
    const BT2020_GREEN: [f32; 3] = [-0.5876, 1.1329, -0.1006];
    let colours: [(&str, [f32; 3]); 8] = [
        ("red", [1.0, 0.0, 0.0]),
        ("green", [0.0, 1.0, 0.0]),
        ("blue", [0.0, 0.0, 1.0]),
        ("yellow", [1.0, 1.0, 0.0]),
        ("cyan", [0.0, 1.0, 1.0]),
        ("magenta", [1.0, 0.0, 1.0]),
        // sRGB 0.5, linear.
        ("grey 50%", [0.214, 0.214, 0.214]),
        ("BT.2020 green", BT2020_GREEN),
    ];
    let (w, h) = (width / 8, height / 3);
    let mut out = Vec::new();
    for (i, nits) in [20.0, 80.0, 160.0, 240.0, 400.0, 600.0, 1000.0, 1600.0]
        .into_iter()
        .enumerate()
    {
        out.push(Patch {
            name: format!("grey {nits:.0} nits"),
            x: i as u32 * w,
            y: 0,
            width: w,
            height: h,
            scrgb: [nits / 80.0; 3],
        });
    }
    for (row, (scale, at)) in [(sdr_white, "SDR white"), (1000.0 / 80.0, "1000 nits")]
        .into_iter()
        .enumerate()
    {
        for (i, (name, rgb)) in colours.iter().enumerate() {
            out.push(Patch {
                name: format!("{name} at {at}"),
                x: i as u32 * w,
                y: (row as u32 + 1) * h,
                width: w,
                height: h,
                scrgb: rgb.map(|v| v * scale),
            });
        }
    }
    out
}

/// A window showing [`patches`] in FP16 scRGB, as an HDR game presents, at
/// (`x`, `y`) on the desktop, until dropped. Its window and swap chain live
/// on a thread of their own.
pub struct Pattern {
    window: isize,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Pattern {
    pub fn open(
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        sdr_white: f32,
    ) -> Result<Self, MediaError> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("hdr-pattern".into())
            .spawn({
                let stop = stop.clone();
                move || pattern_thread(x, y, width, height, sdr_white, &stop, &tx)
            })
            .map_err(backend)?;
        match rx.recv() {
            Ok(Ok(window)) => Ok(Self {
                window,
                stop,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(MediaError::Backend("the pattern's thread ended".into()))
            }
        }
    }

    /// The window's handle value, to capture it.
    pub fn window(&self) -> isize {
        self.window
    }

    /// Moves the window's top-left to (`x`, `y`) on the desktop, keeping its
    /// size: onto another monitor, for the capture to follow.
    pub fn move_to(&self, x: i32, y: i32) -> Result<(), MediaError> {
        // SAFETY: our window, alive until drop; its thread pumps messages,
        // which a move from this thread waits on.
        unsafe {
            SetWindowPos(
                HWND(self.window as *mut std::ffi::c_void),
                None,
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        }
        .map_err(backend)
    }
}

impl Drop for Pattern {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

extern "system" fn pattern_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: the arguments are the ones Windows passed us.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn pattern_thread(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    sdr_white: f32,
    stop: &AtomicBool,
    ready: &SyncSender<Result<isize, MediaError>>,
) {
    let window = match open_pattern_window(x, y, width, height) {
        Ok(window) => window,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let shown = (|| -> Result<(IDXGISwapChain3, ID3D11Device, ID3D11Texture2D), MediaError> {
        let device = hardware_device()?;
        let chain = swap_chain(&device, window, width, height)?;
        let pattern = pattern_texture(&device, width, height, sdr_white)?;
        Ok((chain, device, pattern))
    })();
    let (chain, device, pattern) = match shown {
        Ok(shown) => shown,
        Err(e) => {
            // SAFETY: our own window, made on this thread.
            let _ = unsafe { DestroyWindow(window) };
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(window.0 as isize));
    let mut msg = MSG::default();
    while !stop.load(Ordering::Relaxed) {
        // SAFETY: the swap chain, device and texture are live, of the same
        // size and format; Present shows what was copied.
        let presented = unsafe {
            chain.GetBuffer::<ID3D11Texture2D>(0).and_then(|back| {
                device
                    .GetImmediateContext()
                    .map(|context| context.CopyResource(&back, &pattern))?;
                chain.Present(1, DXGI_PRESENT(0)).ok()
            })
        };
        if let Err(e) = presented {
            tracing::debug!("the HDR pattern didn't present: {e}");
        }
        // SAFETY: a live MSG; messages for this thread's window.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    drop((chain, pattern, device));
    // SAFETY: our own window, made on this thread.
    let _ = unsafe { DestroyWindow(window) };
}

fn open_pattern_window(x: i32, y: i32, width: u32, height: u32) -> Result<HWND, MediaError> {
    // SAFETY: a null name asks for this executable's module.
    let instance = unsafe { GetModuleHandleW(None) }.map_err(backend)?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(pattern_proc),
        hInstance: instance.into(),
        lpszClassName: w!("CouchHdrPattern"),
        ..Default::default()
    };
    // SAFETY: the class's strings are static. Registering twice in one
    // process fails harmlessly; the first registration stands.
    unsafe { RegisterClassW(&class) };
    // SAFETY: the class is registered above; the strings outlive the call.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("CouchHdrPattern"),
            &HSTRING::from(PATTERN_TITLE),
            WS_POPUP | WS_VISIBLE,
            x,
            y,
            width as i32,
            height as i32,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(backend)
}

fn hardware_device() -> Result<ID3D11Device, MediaError> {
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
        )
        .map_err(backend)?;
    }
    device.ok_or_else(|| MediaError::Backend("D3D11CreateDevice made no device".into()))
}

/// A flip-model FP16 swap chain in scRGB: what an HDR game presents.
fn swap_chain(
    device: &ID3D11Device,
    window: HWND,
    width: u32,
    height: u32,
) -> Result<IDXGISwapChain3, MediaError> {
    // SAFETY: no arguments; the factory is ours.
    let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory1() }.map_err(backend)?;
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        Flags: 0,
        ..Default::default()
    };
    // SAFETY: a live device and window of this thread; the description
    // outlives the call.
    let chain: IDXGISwapChain1 =
        unsafe { factory.CreateSwapChainForHwnd(device, window, &desc, None, None) }
            .map_err(backend)?;
    let chain: IDXGISwapChain3 = chain.cast().map_err(backend)?;
    // SAFETY: a plain setter on a live swap chain. FP16 is scRGB by
    // default; said anyway.
    unsafe { chain.SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709) }.map_err(backend)?;
    Ok(chain)
}

/// The pattern drawn in memory, as an FP16 texture.
fn pattern_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    sdr_white: f32,
) -> Result<ID3D11Texture2D, MediaError> {
    let mut pixels = vec![0u16; width as usize * height as usize * 4];
    for patch in patches(width, height, sdr_white) {
        // A black margin of 4 pixels around each patch, so its edges
        // never blend into the next.
        for y in patch.y + 4..(patch.y + patch.height).saturating_sub(4) {
            for x in patch.x + 4..(patch.x + patch.width).saturating_sub(4) {
                let at = (y as usize * width as usize + x as usize) * 4;
                for (c, v) in patch.scrgb.iter().enumerate() {
                    pixels[at + c] = f32_to_half(*v);
                }
                pixels[at + 3] = f32_to_half(1.0);
            }
        }
    }
    for alpha in pixels.iter_mut().skip(3).step_by(4) {
        *alpha = f32_to_half(1.0);
    }
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let initial = D3D11_SUBRESOURCE_DATA {
        pSysMem: pixels.as_ptr().cast(),
        SysMemPitch: width * 8,
        SysMemSlicePitch: 0,
    };
    let mut made = None;
    // SAFETY: the description and pixels outlive the call; the pixels hold
    // `height` rows of `width * 8` bytes.
    unsafe {
        device
            .CreateTexture2D(&desc, Some(&initial), Some(&mut made))
            .map_err(backend)?;
    }
    made.ok_or_else(|| MediaError::Backend("CreateTexture2D returned no texture".into()))
}

/// One captured frame, in memory as Windows handed it over.
pub struct Grabbed {
    pub width: u32,
    pub height: u32,
    pub format: Format,
    /// Bytes from one row to the next.
    pub row_pitch: u32,
    pub data: Vec<u8>,
}

impl Grabbed {
    /// The pixel at (`x`, `y`) as red, green and blue: for 8-bit BGRA the
    /// stored values over 255 (sRGB-encoded), for FP16 the scRGB values
    /// themselves (linear; 1.0 = 80 nits).
    pub fn rgb(&self, x: u32, y: u32) -> [f32; 3] {
        let row = y as usize * self.row_pitch as usize;
        match self.format {
            Format::Bgra8 => {
                let p = &self.data[row + x as usize * 4..][..4];
                [p[2], p[1], p[0]].map(|v| f32::from(v) / 255.0)
            }
            Format::Fp16 => {
                let p = &self.data[row + x as usize * 8..][..6];
                [0, 2, 4].map(|i| half_to_f32(u16::from_le_bytes([p[i], p[i + 1]])))
            }
        }
    }
}

/// One frame of `target` in `format`, or an error if none comes within
/// `timeout` (a still window sends its first frame at once).
pub fn grab(target: Target, format: Format, timeout: Duration) -> Result<Grabbed, MediaError> {
    let device = wgc::new_device().map_err(backend)?;
    let (tx, rx) = sync_channel(1);
    let options = wgc::Options {
        buffers: 1,
        min_interval: Duration::from_millis(1),
        cursor: false,
        format,
    };
    let session =
        wgc::Session::start(target, options, &device, Once(Some(tx), format)).map_err(backend)?;
    let grabbed = rx
        .recv_timeout(timeout)
        .map_err(|_| MediaError::Backend("no frame came".into()))?;
    drop(session);
    grabbed
}

/// Reads the first frame back and sends it; ignores the rest.
struct Once(Option<SyncSender<Result<Grabbed, MediaError>>>, Format);

impl Sink for Once {
    fn frame(&mut self, frame: Arrived<'_>) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(read_back(&frame, self.1));
        }
    }

    fn closed(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(Err(MediaError::Closed));
        }
    }
}

fn read_back(frame: &Arrived<'_>, format: Format) -> Result<Grabbed, MediaError> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: fills a local description of the live frame texture.
    unsafe { frame.texture.GetDesc(&mut desc) };
    let staging = staging_texture(frame.device, &desc)?;
    // SAFETY: a plain getter on a live device.
    let context = unsafe { frame.device.GetImmediateContext() }.map_err(backend)?;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: the same device, size and format; Map waits for the copy, and
    // the mapping is read and then unmapped before the staging texture goes.
    let data = unsafe {
        context.CopyResource(&staging, frame.texture);
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(backend)?;
        let len = mapped.RowPitch as usize * frame.height as usize;
        let data = std::slice::from_raw_parts(mapped.pData.cast::<u8>(), len).to_vec();
        context.Unmap(&staging, 0);
        data
    };
    Ok(Grabbed {
        width: frame.width,
        height: frame.height,
        format,
        row_pitch: mapped.RowPitch,
        data,
    })
}

/// An IEEE half-precision value as `f32`.
pub fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((h >> 10) & 0x1f);
    let mantissa = f32::from(h & 0x3ff);
    sign * match exponent {
        0 => mantissa * 2f32.powi(-24),
        31 if mantissa == 0.0 => f32::INFINITY,
        31 => f32::NAN,
        e => (1.0 + mantissa / 1024.0) * 2f32.powi(e - 15),
    }
}

/// An `f32` as IEEE half precision, for test pictures: normal values rounded
/// half away from zero (not to even), subnormals truncated; beyond 65504 it is
/// infinite, and below half's smallest values zero.
pub fn f32_to_half(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    if v.is_nan() {
        return 0x7e00;
    }
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x7f_ffff;
    if exponent >= 31 {
        return sign | 0x7c00;
    }
    if exponent <= 0 {
        if exponent < -10 {
            return sign;
        }
        // Subnormal: the implicit bit made explicit, shifted into place.
        let m = (mantissa | 0x80_0000) >> (14 - exponent);
        return sign | m as u16;
    }
    // A carry out of the mantissa rounds up into the exponent, as it should.
    let rounded = ((exponent as u32) << 10) + ((mantissa + 0x1000) >> 13);
    sign | rounded.min(0x7c00) as u16
}

fn backend(e: impl std::fmt::Display) -> MediaError {
    MediaError::Backend(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves_read_as_floats() {
        assert_eq!(half_to_f32(0x0000), 0.0);
        assert_eq!(half_to_f32(0x3c00), 1.0);
        assert_eq!(half_to_f32(0xc000), -2.0);
        assert_eq!(half_to_f32(0x4100), 2.5);
        assert_eq!(half_to_f32(0x7bff), 65504.0);
        assert_eq!(half_to_f32(0x0001), 2f32.powi(-24));
        assert_eq!(half_to_f32(0x7c00), f32::INFINITY);
        assert!(half_to_f32(0x7e00).is_nan());
    }

    #[test]
    fn floats_become_halves_and_back() {
        for v in [
            0.0,
            1.0,
            -2.0,
            2.5,
            0.214,
            12.5,
            20.0,
            -0.5876,
            65504.0,
            2f32.powi(-24),
        ] {
            let back = half_to_f32(f32_to_half(v));
            assert!(
                (back - v).abs() <= v.abs() / 1024.0,
                "{v} came back as {back}"
            );
        }
        assert_eq!(f32_to_half(1.0), 0x3c00);
        assert_eq!(f32_to_half(1e6), 0x7c00);
        assert_eq!(f32_to_half(-1e-9), 0x8000);
        assert_eq!(f32_to_half(f32::NAN), 0x7e00);
    }

    #[test]
    fn the_pattern_has_three_rows_of_eight() {
        let p = patches(640, 360, 3.0);
        assert_eq!(p.len(), 24);
        assert_eq!(p[3].scrgb, [3.0; 3], "240 nits");
        assert_eq!(p[8].scrgb, [3.0, 0.0, 0.0], "red at SDR white");
        assert_eq!(p[16].scrgb, [12.5, 0.0, 0.0], "red at 1000 nits");
        assert_eq!(p[23].centre(), (600, 300));
    }
}
