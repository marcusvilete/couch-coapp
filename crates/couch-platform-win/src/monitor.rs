//! The monitor showing the streamed window, or the streamed screen:
//! - its work area (the monitor without the taskbar), where the host's
//!   overlay goes (M8), in physical pixels and desktop coordinates;
//! - its refresh rate, which caps how many frames a capture gets (M9d);
//! - a streamed screen's whole area, where desktop control keeps the
//!   pointer (M25b).
//!
//! It also lists the monitors a capture can take, with their names and sizes
//! (M9f, replacing the `windows-capture` crate's listing).

use std::ffi::c_void;

use windows::Win32::{
    Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS,
        QueryDisplayConfig,
    },
    Foundation::{HWND, LPARAM, POINT, RECT},
    Graphics::{
        Dxgi::{
            Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, CreateDXGIFactory1,
            DXGI_OUTPUT_DESC1, IDXGIFactory1, IDXGIOutput6,
        },
        Gdi::{
            DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplayMonitors, EnumDisplaySettingsW,
            GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MONITORINFOEXW,
            MonitorFromPoint, MonitorFromWindow,
        },
    },
};
use windows_core::{BOOL, Interface, PCWSTR};

/// A rectangle on the desktop, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// The work area of the monitor showing most of `window` (a window handle
/// value); the primary monitor's if the window is gone.
pub fn of_window(window: isize) -> Option<Area> {
    work_area(showing(window)).or_else(primary)
}

/// The refresh rate, in whole hertz as Windows reports it (59 for 59.94),
/// of the monitor showing most of `window`; the primary monitor's if the
/// window is gone. `None` if unknown.
pub fn refresh_of_window(window: isize) -> Option<u32> {
    refresh(showing(window)).or_else(|| refresh(primary_monitor()))
}

/// The refresh rate of `monitor` (a monitor handle value, as screens are
/// listed). `None` if unknown.
pub fn refresh_of_monitor(monitor: isize) -> Option<u32> {
    refresh(HMONITOR(monitor as *mut c_void))
}

fn showing(window: isize) -> HMONITOR {
    // SAFETY: MonitorFromWindow accepts any handle value. For a window off
    // every monitor it returns the primary one; for a handle that isn't a
    // window, none, which the callers' calls then fail on.
    unsafe { MonitorFromWindow(HWND(window as *mut c_void), MONITOR_DEFAULTTOPRIMARY) }
}

fn refresh(monitor: HMONITOR) -> Option<u32> {
    // 0 and 1 mean "the hardware's default", which says nothing.
    mode(monitor)
        .map(|mode| mode.dmDisplayFrequency)
        .filter(|&hz| hz > 1)
}

/// The work area of `monitor` (a monitor handle value, as screens are
/// listed); the primary monitor's if it is gone.
pub fn of_monitor(monitor: isize) -> Option<Area> {
    work_area(HMONITOR(monitor as *mut c_void)).or_else(primary)
}

/// The primary monitor's work area.
pub fn primary() -> Option<Area> {
    work_area(primary_monitor())
}

fn primary_monitor() -> HMONITOR {
    // SAFETY: no pointers; with MONITOR_DEFAULTTOPRIMARY it always returns
    // a monitor.
    unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }
}

/// Every monitor's handle value, in Windows' order.
pub fn handles() -> Vec<isize> {
    let mut found: Vec<isize> = Vec::new();
    // SAFETY: the callback runs during this call only, and gets a pointer to
    // `found`, live for the whole call.
    let _ = unsafe {
        EnumDisplayMonitors(None, None, Some(collect), LPARAM((&raw mut found) as isize))
    };
    found
}

unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, found: LPARAM) -> BOOL {
    // SAFETY: `handles` passes a pointer to its live Vec, used only during
    // its EnumDisplayMonitors call, on its thread.
    let found = unsafe { &mut *(found.0 as *mut Vec<isize>) };
    found.push(monitor.0 as isize);
    BOOL::from(true)
}

/// The primary monitor's handle value.
pub fn primary_handle() -> isize {
    primary_monitor().0 as isize
}

/// A monitor's size in pixels, from its current display mode.
pub fn size(monitor: isize) -> Option<(u32, u32)> {
    let mode = mode(HMONITOR(monitor as *mut c_void))?;
    Some((mode.dmPelsWidth, mode.dmPelsHeight))
}

/// A monitor's own name ("DELL U2720Q"), as Windows' display settings show
/// it; `None` if it reports none.
pub fn name(monitor: isize) -> Option<String> {
    let path = display_path(HMONITOR(monitor as *mut c_void))?;
    let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
            adapterId: path.sourceInfo.adapterId,
            id: path.targetInfo.id,
        },
        ..Default::default()
    };
    // SAFETY: the header names the struct around it and its size.
    if unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } != 0 {
        return None;
    }
    let name = wide(&target.monitorFriendlyDeviceName);
    (!name.trim().is_empty()).then_some(name)
}

/// How a monitor shows colour (M29), for the capture to read it right.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colour {
    /// The monitor is in HDR: Windows composes its desktop in scRGB (FP16,
    /// linear, 1.0 = 80 nits) and sends it out as BT.2100 PQ.
    pub hdr: bool,
    /// DXGI's colour space (`DXGI_COLOR_SPACE_TYPE`), for the log: 12 is
    /// HDR (G2084, BT.2020), 0 SDR (G22, BT.709).
    pub colour_space: i32,
    pub bits_per_colour: u32,
    /// Where Windows shows SDR white on this monitor, in nits (the "SDR
    /// content brightness" setting); `None` if it doesn't say.
    pub sdr_white_nits: Option<f32>,
    /// The panel's own word on its peak and full-frame brightness, in nits.
    pub max_nits: f32,
    pub max_full_frame_nits: f32,
}

/// The colour of `monitor` (a monitor handle value, as screens are listed);
/// `None` if DXGI can't find it.
pub fn colour(monitor: isize) -> Option<Colour> {
    let monitor = HMONITOR(monitor as *mut c_void);
    let desc = output_desc(monitor)?;
    Some(Colour {
        hdr: desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020,
        colour_space: desc.ColorSpace.0,
        bits_per_colour: desc.BitsPerColor,
        sdr_white_nits: sdr_white_nits(monitor),
        max_nits: desc.MaxLuminance,
        max_full_frame_nits: desc.MaxFullFrameLuminance,
    })
}

/// The colour of the monitor showing most of `window`.
pub fn colour_of_window(window: isize) -> Option<Colour> {
    colour(showing(window).0 as isize)
}

/// The monitor's output description, found among every adapter's outputs.
fn output_desc(monitor: HMONITOR) -> Option<DXGI_OUTPUT_DESC1> {
    // SAFETY: no arguments; the factory is ours.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    for a in 0.. {
        // SAFETY: a plain enumeration; it fails past the last adapter.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(a) }) else {
            break;
        };
        for o in 0.. {
            // SAFETY: as above, past the last output.
            let Ok(output) = (unsafe { adapter.EnumOutputs(o) }) else {
                break;
            };
            let Ok(output) = output.cast::<IDXGIOutput6>() else {
                continue;
            };
            // SAFETY: a plain getter on a live output.
            if let Ok(desc) = unsafe { output.GetDesc1() }
                && desc.Monitor == monitor
            {
                return Some(desc);
            }
        }
    }
    None
}

/// The SDR white level Windows set for `monitor`, in nits.
fn sdr_white_nits(monitor: HMONITOR) -> Option<f32> {
    let path = display_path(monitor)?;
    let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
            size: size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32,
            adapterId: path.targetInfo.adapterId,
            id: path.targetInfo.id,
        },
        ..Default::default()
    };
    // SAFETY: the header names the struct around it and its size.
    if unsafe { DisplayConfigGetDeviceInfo(&mut white.header) } != 0 || white.SDRWhiteLevel == 0 {
        return None;
    }
    // In thousandths of the 80-nit reference.
    Some(white.SDRWhiteLevel as f32 / 1000.0 * 80.0)
}

/// The active display path whose source is `monitor`.
fn display_path(monitor: HMONITOR) -> Option<DISPLAYCONFIG_PATH_INFO> {
    let device = device_name(monitor)?;
    let (mut paths, mut modes) = (0u32, 0u32);
    // SAFETY: live out-parameters.
    unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths, &mut modes) }
        .ok()
        .ok()?;
    let mut path_info = vec![DISPLAYCONFIG_PATH_INFO::default(); paths as usize];
    let mut mode_info = vec![DISPLAYCONFIG_MODE_INFO::default(); modes as usize];
    // SAFETY: both buffers hold the counts passed with them.
    unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut paths,
            path_info.as_mut_ptr(),
            &mut modes,
            mode_info.as_mut_ptr(),
            None,
        )
    }
    .ok()
    .ok()?;
    for path in &path_info[..paths as usize] {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: path.sourceInfo.adapterId,
                id: path.sourceInfo.id,
            },
            ..Default::default()
        };
        // SAFETY: the header names the struct around it and its size.
        if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } == 0
            && wide(&source.viewGdiDeviceName) == device
        {
            return Some(*path);
        }
    }
    None
}

/// Text up to its NUL.
fn wide(text: &[u16]) -> String {
    let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    String::from_utf16_lossy(&text[..len])
}

/// The monitor's GDI device name (`\\.\DISPLAY1`).
fn device_name(monitor: HMONITOR) -> Option<String> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: a live MONITORINFOEXW whose size lets the call fill the name.
    unsafe { GetMonitorInfoW(monitor, (&raw mut info).cast()) }
        .as_bool()
        .then(|| wide(&info.szDevice))
}

/// The monitor's current display mode.
fn mode(monitor: HMONITOR) -> Option<DEVMODEW> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: `info` is a live MONITORINFOEXW, whose size tells the call it
    // may fill the device name too; a handle that isn't a monitor makes it
    // fail. The device name it filled is NUL-terminated within the array,
    // and `mode` is a live DEVMODEW with its size set.
    let found = unsafe {
        GetMonitorInfoW(monitor, (&raw mut info).cast()).as_bool()
            && EnumDisplaySettingsW(
                PCWSTR(info.szDevice.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut mode,
            )
            .as_bool()
    };
    found.then_some(mode)
}

/// The whole of `monitor` (a monitor handle value, as screens are listed),
/// taskbar included: what a capture of it shows (M25b, the pointer kept on a
/// streamed screen). `None` if it is gone: no other screen stands in.
pub fn whole(monitor: isize) -> Option<Area> {
    info(HMONITOR(monitor as *mut c_void)).map(|info| rect(info.rcMonitor))
}

fn work_area(monitor: HMONITOR) -> Option<Area> {
    info(monitor).map(|info| rect(info.rcWork))
}

fn info(monitor: HMONITOR) -> Option<MONITORINFO> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is a live MONITORINFO with its size set; a handle that
    // isn't a monitor makes the call fail, which we report as `None`.
    unsafe { GetMonitorInfoW(monitor, &mut info) }
        .as_bool()
        .then_some(info)
}

fn rect(r: RECT) -> Area {
    Area {
        x: r.left,
        y: r.top,
        width: r.right - r.left,
        height: r.bottom - r.top,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_primary_monitor_has_an_area_and_strangers_fall_back_to_it() {
        // A machine without a display (a service session) has none to test.
        let Some(primary) = primary() else { return };
        assert!(primary.width > 0 && primary.height > 0);
        // Not a window: the primary monitor's.
        assert_eq!(of_window(0x7fff_0001), Some(primary));
        // Not a monitor: some monitor's, never none. With several monitors
        // Windows may answer a made-up handle with another one's (a virtual
        // display did, M29), as it answers stale ones.
        let stranger = of_monitor(0x7fff_0001);
        assert!(
            handles()
                .into_iter()
                .any(|m| work_area(HMONITOR(m as *mut c_void)) == stranger),
            "{stranger:?}"
        );
    }

    #[test]
    fn monitors_are_listed_with_their_sizes() {
        let Some(_) = primary() else { return };
        let all = handles();
        assert!(all.contains(&primary_handle()), "{all:?}");
        for monitor in all {
            let (width, height) = size(monitor).unwrap();
            assert!(width > 0 && height > 0);
            // A name is optional (some displays report none), never blank.
            assert!(name(monitor).is_none_or(|n| !n.trim().is_empty()));
        }
    }

    #[test]
    fn the_primary_monitor_has_a_refresh_rate() {
        let Some(_) = primary() else { return };
        // Not a window: the primary monitor's.
        let hz = refresh_of_window(0x7fff_0001);
        assert!(hz.is_some_and(|hz| (24..=1000).contains(&hz)), "{hz:?}");
        assert_eq!(refresh_of_monitor(primary_monitor().0 as isize), hz);
    }
}
