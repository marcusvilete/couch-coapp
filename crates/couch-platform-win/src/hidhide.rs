//! HidHide, the controller firewall (github.com/nefarius/HidHide, MIT): hides
//! the host's own controller from games while it plays through a seat, so
//! every player in the game is one of our virtual pads (docs/p2-results.md).
//!
//! We talk to its control device, `\\.\HidHide`, through its documented
//! IOCTLs (its DEVELOPER.md); no code of it is used. That needs no
//! administrator rights, but only one process may hold the device open at a
//! time: while HidHide's own configuration window is open, we can't.
//!
//! - **What it hides.** A device on its blacklist refuses to be opened by any
//!   program not on its whitelist (program paths), while HidHide is active.
//!   We add every node of the chosen controller (they share a container id:
//!   its USB or Bluetooth node, its HID children, its XInput node) and put
//!   our own exe on the whitelist, where it stays (it only lets our exe see
//!   hidden devices).
//! - **Only new opens are refused.** A program that already had the
//!   controller open (Steam does, always) keeps it until the controller is
//!   reconnected; the app asks the host to do that once.
//! - **Undoing it.** HidHide's newer "session blacklist" forgets a process's
//!   entries when it exits, crash included; we use it when the driver has it
//!   (not yet in a release, as of 1.5.230). Otherwise the entries go on the
//!   persistent blacklist, and a marker file records exactly what we added
//!   ([`crate::markers`]), never an entry the user had there already:
//!   [`Hidden`]'s drop undoes it, and so does [`restore_stale`] at the next
//!   start after a crash, or at the next hide after an undo that failed.
//! - **HidHide's active flag.** If it was off, we turn it on, and the last of
//!   our hosts on this PC to stop turns it off again.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU32, Ordering},
    },
    thread,
    time::Duration,
};

use couch_input::local::ControllerIds;
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ACCESS_DENIED, ERROR_BUSY, ERROR_FILE_NOT_FOUND,
            ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_PATH_NOT_FOUND,
            ERROR_SHARING_VIOLATION, GENERIC_READ, HANDLE,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
            QueryDosDeviceW,
        },
        System::IO::DeviceIoControl,
    },
    core::{HRESULT, HSTRING, w},
};

use crate::{
    controllers::{self, Device},
    markers::{self, own_start_time, process_state},
};

/// What HidHide can do for us right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    NotInstalled,
    Ready,
    /// Another program held its control device for over a second: most
    /// likely its configuration window.
    Busy,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HideError {
    #[error("HidHide isn't installed")]
    NotInstalled,
    #[error("HidHide is busy (its configuration window may be open); try again in a moment")]
    Busy,
    #[error("that controller isn't plugged in")]
    NotFound,
    #[error("a controller with our virtual pads' ids can't be hidden")]
    Ours,
    #[error("HidHide: {0}")]
    Failed(String),
}

/// Whether HidHide is installed and free to use. Waits up to a second for
/// another holder of its control device to let go: its watchdog service
/// and our own calls hold it for moments too, not only its configuration
/// window.
pub fn state() -> State {
    match Control::open_patiently() {
        Ok(_) => State::Ready,
        Err(HideError::NotInstalled) => State::NotInstalled,
        Err(HideError::Busy) => {
            tracing::info!("HidHide's control device stayed busy for a second");
            State::Busy
        }
        // Not a known reason to wait: say ready, and let hiding report it.
        Err(e) => {
            tracing::warn!("HidHide: {e}");
            State::Ready
        }
    }
}

/// The host's controller, hidden from games until this drops.
#[derive(Debug)]
pub struct Hidden {
    /// Our record of what we changed, written before we changed it.
    marker: PathBuf,
    record: Record,
    /// The entries went on the session blacklist.
    session: bool,
}

/// What one hide changed, as its marker records it. Every hide writes one,
/// even with nothing on the persistent blacklist: other hosts on this PC
/// see from it that HidHide must stay active.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Record {
    started: Option<u64>,
    /// What we added to the persistent blacklist: never an entry that was
    /// there already, which is the user's own.
    devices: Vec<String>,
}

/// Markers of this process's live [`Hidden`]s. Any other marker of ours is
/// left from an undo that failed, and is undone again like a crashed host's.
static LIVE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn live() -> MutexGuard<'static, Vec<PathBuf>> {
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Hides the controller with `ids` (every node of it) from every program but
/// ours.
pub fn hide(ids: ControllerIds) -> Result<Hidden, HideError> {
    if ids.could_be_ours() {
        return Err(HideError::Ours);
    }
    let devices = to_hide(&controllers::present(), ids);
    if devices.is_empty() {
        return Err(HideError::NotFound);
    }
    // One handle throughout: HidHide takes one at a time, so an undo on the
    // way out must use this one, not open another.
    let control = Control::open_patiently()?;
    restore_with(&control);

    let exe = std::env::current_exe().map_err(|e| HideError::Failed(e.to_string()))?;
    let exe = dos_path(&exe)
        .ok_or_else(|| HideError::Failed(format!("no device path for {}", exe.display())))?;
    let whitelist = control.list(GET_WHITELIST)?;
    if !contains(&whitelist, &exe) {
        control.set_list(
            SET_WHITELIST,
            &with_added(&whitelist, std::slice::from_ref(&exe)),
        )?;
    }

    let was_active = control.active()?;
    let blacklist = control.list(GET_BLACKLIST)?;
    let session = control.add_session(&devices).is_ok();
    let record = Record {
        started: own_start_time(),
        devices: if session {
            Vec::new()
        } else {
            ours_to_add(&devices, &blacklist)
        },
    };
    // The marker goes down before the lists or the active flag change, so a
    // crash from here on is undone at the next start.
    let marker = match write_marker(&record) {
        Ok(marker) => marker,
        Err(e) => {
            if session {
                let _ = control.clear_session();
            }
            return Err(HideError::Failed(format!("its record: {e}")));
        }
    };
    let changed = (|| {
        if !record.devices.is_empty() {
            control.set_list(SET_BLACKLIST, &with_added(&blacklist, &record.devices))?;
        }
        if !was_active {
            // Noted first: whichever of our hosts stops last turns it off.
            set_turned_on(true).map_err(|e| HideError::Failed(format!("its record: {e}")))?;
            control.set_active(true)?;
        }
        Ok(())
    })();
    if let Err(e) = changed {
        if session {
            let _ = control.clear_session();
        }
        if let Err(undo) = undo_with(&control, &marker, &record) {
            tracing::warn!("undoing a failed hide: {undo}");
        }
        return Err(e);
    }
    live().push(marker.clone());
    tracing::info!(
        session,
        turned_on = !was_active,
        "hid {} device(s) of controller {:04x}:{:04x}",
        devices.len(),
        ids.vendor,
        ids.product
    );
    Ok(Hidden {
        marker,
        record,
        session,
    })
}

impl Drop for Hidden {
    fn drop(&mut self) {
        // Ours no longer: if the undo below fails, the next hide or start
        // undoes it again.
        live().retain(|m| *m != self.marker);
        let shown = Control::open_patiently().and_then(|control| {
            if self.session {
                // HidHide would drop them when we exit anyway.
                let _ = control.clear_session();
            }
            undo_with(&control, &self.marker, &self.record)
        });
        match shown {
            Ok(()) => tracing::info!("the host's controller is shown again"),
            // The marker stays: the next hide or start tries again.
            Err(e) => tracing::warn!("showing the host's controller again: {e}"),
        }
    }
}

/// Undoes what crashed hosts' markers recorded, and our own left by an undo
/// that failed. Call at start, before hiding anything (hiding does it too).
/// Returns how many were undone.
pub fn restore_stale() -> usize {
    if stale_markers().is_empty() {
        return 0;
    }
    match Control::open_patiently() {
        Ok(control) => restore_with(&control),
        Err(e) => {
            tracing::warn!("showing controllers hidden before a crash: {e}");
            0
        }
    }
}

fn restore_with(control: &Control) -> usize {
    let mut undone = 0;
    for (path, record) in stale_markers() {
        match undo_with(control, &path, &record) {
            Ok(()) => undone += 1,
            Err(e) => tracing::warn!("showing a controller hidden before a crash: {e}"),
        }
    }
    if undone > 0 {
        tracing::info!("showed {undone} controller(s) hidden by a host that crashed");
    }
    undone
}

/// Whether a marker's host is gone: another process that exited, or this
/// one's marker that no live [`Hidden`] owns.
fn marker_stale(path: &Path, pid: u32, record: &Record) -> bool {
    if pid == std::process::id() && record.started == own_start_time() {
        return !live().iter().any(|m| m == path);
    }
    markers::stale(process_state(pid), record.started)
}

fn stale_markers() -> Vec<(PathBuf, Record)> {
    let Some(dir) = markers_dir() else {
        return Vec::new();
    };
    read_markers(&dir)
        .into_iter()
        .filter(|(path, pid, record)| marker_stale(path, *pid, record))
        .map(|(path, _, record)| (path, record))
        .collect()
}

/// Takes our entries off the blacklist, except those another live host still
/// hides; turns HidHide off if one of our hosts turned it on and no other
/// live host remains; then removes the marker.
fn undo_with(control: &Control, marker: &Path, record: &Record) -> Result<(), HideError> {
    let others: Vec<Record> = markers_dir()
        .map(|dir| {
            read_markers(&dir)
                .into_iter()
                .filter(|(path, pid, r)| path != marker && !marker_stale(path, *pid, r))
                .map(|(_, _, r)| r)
                .collect()
        })
        .unwrap_or_default();
    if !record.devices.is_empty() {
        let keep: Vec<String> = others.iter().flat_map(|r| r.devices.clone()).collect();
        let blacklist = control.list(GET_BLACKLIST)?;
        control.set_list(SET_BLACKLIST, &without(&blacklist, &record.devices, &keep))?;
    }
    if others.is_empty() && turned_on() {
        control.set_active(false)?;
        let _ = set_turned_on(false);
    }
    let _ = fs::remove_file(marker);
    Ok(())
}

// ---- The control device ----

/// `CTL_CODE(32769, function, METHOD_BUFFERED, FILE_READ_DATA)`.
const fn ctl(function: u32) -> u32 {
    (32769 << 16) | (1 << 14) | (function << 2)
}

const GET_WHITELIST: u32 = ctl(2048);
const SET_WHITELIST: u32 = ctl(2049);
const GET_BLACKLIST: u32 = ctl(2050);
const SET_BLACKLIST: u32 = ctl(2051);
const GET_ACTIVE: u32 = ctl(2052);
const SET_ACTIVE: u32 = ctl(2053);
const ADD_SESSION_BLACKLIST: u32 = ctl(2056);
const CLR_SESSION_BLACKLIST: u32 = ctl(2057);

/// The driver has no session blacklist.
struct Unsupported;

/// The open control device; closed on drop.
struct Control(HANDLE);

impl Drop for Control {
    fn drop(&mut self) {
        // SAFETY: opened by `CreateFileW` and closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

impl Control {
    fn open() -> Result<Self, HideError> {
        // SAFETY: a static path, no security attributes or template.
        let handle = unsafe {
            CreateFileW(
                w!(r"\\.\HidHide"),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };
        match handle {
            Ok(handle) => Ok(Self(handle)),
            Err(e) if is(&e, &[ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND]) => {
                Err(HideError::NotInstalled)
            }
            Err(e)
                if is(
                    &e,
                    &[ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_BUSY],
                ) =>
            {
                Err(HideError::Busy)
            }
            Err(e) => Err(HideError::Failed(e.to_string())),
        }
    }

    /// Like [`Self::open`], waiting up to a second for the configuration
    /// window to let go.
    fn open_patiently() -> Result<Self, HideError> {
        for _ in 0..10 {
            match Self::open() {
                Err(HideError::Busy) => thread::sleep(Duration::from_millis(100)),
                other => return other,
            }
        }
        Self::open()
    }

    fn ioctl(&self, code: u32, input: &[u8], output: &mut [u8]) -> windows::core::Result<u32> {
        let mut returned = 0u32;
        // SAFETY: both buffers are valid for the lengths passed (null when
        // empty), and the call is synchronous (no OVERLAPPED).
        unsafe {
            DeviceIoControl(
                self.0,
                code,
                (!input.is_empty()).then_some(input.as_ptr().cast()),
                input.len() as u32,
                (!output.is_empty()).then_some(output.as_mut_ptr().cast()),
                output.len() as u32,
                Some(&mut returned),
                None,
            )
        }?;
        Ok(returned)
    }

    fn list(&self, code: u32) -> Result<Vec<String>, HideError> {
        // Without a buffer, the driver says how many bytes the list takes.
        let needed = self.ioctl(code, &[], &mut []).map_err(failed)? as usize;
        if needed == 0 {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; needed];
        let got = self.ioctl(code, &[], &mut buf).map_err(failed)? as usize;
        Ok(decode_multi_sz(&wide(&buf[..got.min(buf.len())])))
    }

    fn set_list(&self, code: u32, list: &[String]) -> Result<(), HideError> {
        self.ioctl(code, &bytes(&encode_multi_sz(list)), &mut [])
            .map(drop)
            .map_err(failed)
    }

    fn active(&self) -> Result<bool, HideError> {
        let mut out = [0u8];
        self.ioctl(GET_ACTIVE, &[], &mut out).map_err(failed)?;
        Ok(out[0] != 0)
    }

    fn set_active(&self, active: bool) -> Result<(), HideError> {
        self.ioctl(SET_ACTIVE, &[u8::from(active)], &mut [])
            .map(drop)
            .map_err(failed)
    }

    fn add_session(&self, devices: &[String]) -> Result<(), Unsupported> {
        match self.ioctl(
            ADD_SESSION_BLACKLIST,
            &bytes(&encode_multi_sz(devices)),
            &mut [],
        ) {
            Ok(_) => Ok(()),
            Err(e) => {
                if !is(&e, &[ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED]) {
                    tracing::debug!("HidHide's session blacklist: {e}");
                }
                Err(Unsupported)
            }
        }
    }

    fn clear_session(&self) -> windows::core::Result<u32> {
        self.ioctl(CLR_SESSION_BLACKLIST, &[], &mut [])
    }
}

fn is(e: &windows::core::Error, codes: &[windows::Win32::Foundation::WIN32_ERROR]) -> bool {
    codes.iter().any(|c| e.code() == HRESULT::from_win32(c.0))
}

fn failed(e: windows::core::Error) -> HideError {
    HideError::Failed(e.to_string())
}

/// `C:\dir\app.exe` as the kernel names it, `\Device\HarddiskVolume3\dir\app.exe`:
/// the form HidHide's whitelist takes.
fn dos_path(path: &Path) -> Option<String> {
    let path = path.to_str()?;
    let drive = path.get(..2).filter(|d| d.ends_with(':'))?;
    let mut target = [0u16; 512];
    // SAFETY: a drive name and a writable buffer whose length the binding
    // passes.
    let n = unsafe { QueryDosDeviceW(&HSTRING::from(drive), Some(&mut target)) } as usize;
    let device = decode_multi_sz(&target[..n.min(target.len())])
        .into_iter()
        .next()?;
    // A `subst` drive maps to `\??\C:\...`, which isn't the kernel's name for
    // our image: whitelisting it would let nothing through.
    device.starts_with(r"\Device\").then_some(())?;
    join_dos(&device, path)
}

fn join_dos(device: &str, path: &str) -> Option<String> {
    let rest = path.get(2..).filter(|r| r.starts_with('\\'))?;
    Some(format!("{device}{rest}"))
}

// ---- Lists ----

/// Strings as a `MULTI_SZ`: each null-terminated, then one more null. An
/// empty list is two nulls.
fn encode_multi_sz(list: &[String]) -> Vec<u16> {
    let mut out: Vec<u16> = list
        .iter()
        .flat_map(|s| s.encode_utf16().chain([0]))
        .collect();
    if out.is_empty() {
        out.push(0);
    }
    out.push(0);
    out
}

fn decode_multi_sz(wide: &[u16]) -> Vec<String> {
    wide.split(|&c| c == 0)
        .take_while(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

fn wide(bytes: &[u8]) -> Vec<u16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect()
}

fn bytes(wide: &[u16]) -> Vec<u8> {
    wide.iter().flat_map(|c| c.to_le_bytes()).collect()
}

/// HidHide compares paths and device ids ignoring case.
fn contains(list: &[String], item: &str) -> bool {
    list.iter().any(|s| s.eq_ignore_ascii_case(item))
}

fn with_added(list: &[String], add: &[String]) -> Vec<String> {
    let mut out = list.to_vec();
    for item in add {
        if !contains(&out, item) {
            out.push(item.clone());
        }
    }
    out
}

/// What we add to the blacklist, and so later take off: the controller's
/// devices not on it already. Those that are, the user hid themselves (for
/// DS4Windows, say), and they stay hidden after we stop.
fn ours_to_add(devices: &[String], blacklist: &[String]) -> Vec<String> {
    devices
        .iter()
        .filter(|d| !contains(blacklist, d))
        .cloned()
        .collect()
}

/// `list` without `remove`, except what `keep` still names; whatever else is
/// on it (the user's own entries) stays.
fn without(list: &[String], remove: &[String], keep: &[String]) -> Vec<String> {
    list.iter()
        .filter(|s| !contains(remove, s) || contains(keep, s))
        .cloned()
        .collect()
}

// ---- Which devices ----

/// Setup classes HidHide filters: HID devices, wired Xbox 360 pads
/// (XnaComposite), and Xbox One and later pads (XboxComposite).
const HIDDEN_CLASSES: [&str; 3] = [
    "{745a17a0-74d3-11d0-b6fe-00a0c90f57da}",
    "{d61ca365-5af4-4486-998b-9db4734c6ca3}",
    "{05f5cfe2-4733-4950-a6bb-07aad01a3a84}",
];

/// The container of devices built into the PC: hiding by it would hide them
/// all (a handheld's keyboard and touchpad), so only the matching devices
/// themselves are hidden then.
const THE_PC: &str = "{00000000-0000-0000-ffff-ffffffffffff}";

/// The instance ids to hide for the controller with `ids`: every device in
/// its containers, of the classes HidHide filters.
fn to_hide(devices: &[Device], ids: ControllerIds) -> Vec<String> {
    let matching: Vec<&Device> = devices.iter().filter(|d| has_ids(d, ids)).collect();
    let containers: Vec<&str> = matching
        .iter()
        .filter_map(|d| d.container.as_deref())
        .filter(|c| !c.eq_ignore_ascii_case(THE_PC))
        .collect();
    let mut out = Vec::new();
    for d in devices {
        let in_container = d
            .container
            .as_deref()
            .is_some_and(|c| containers.iter().any(|w| w.eq_ignore_ascii_case(c)));
        let filtered = HIDDEN_CLASSES
            .iter()
            .any(|c| d.class.eq_ignore_ascii_case(c));
        let matches = matching.iter().any(|m| m.instance_id == d.instance_id);
        if ((in_container && filtered) || matches) && !contains(&out, &d.instance_id) {
            out.push(d.instance_id.clone());
        }
    }
    out
}

/// Whether a device's hardware ids name `ids`: `VID_054C&PID_0CE6` (USB),
/// or `VID&0002054C_PID&0CE6` (Bluetooth, with a prefix before the vendor).
fn has_ids(device: &Device, ids: ControllerIds) -> bool {
    device.hardware_ids.iter().any(|id| {
        let id = id.to_ascii_uppercase();
        hex_after(&id, "VID") == Some(ids.vendor) && hex_after(&id, "PID") == Some(ids.product)
    })
}

/// The 16-bit hex value after `key` and a `_` or `&`: the last four digits
/// of a run of up to eight.
fn hex_after(id: &str, key: &str) -> Option<u16> {
    let at = id.find(key)? + key.len();
    let rest = id.get(at..)?;
    let rest = rest.strip_prefix('_').or_else(|| rest.strip_prefix('&'))?;
    let digits: String = rest
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .take(8)
        .collect();
    if digits.len() < 4 {
        return None;
    }
    u16::from_str_radix(&digits[digits.len() - 4..], 16).ok()
}

// ---- Markers ----

fn markers_dir() -> Option<PathBuf> {
    markers::dir("hidden-controllers")
}

fn write_marker(record: &Record) -> io::Result<PathBuf> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = markers_dir().ok_or_else(|| io::Error::other("LOCALAPPDATA is not set"))?;
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    write_marker_in(&dir, std::process::id(), n, record)
}

fn write_marker_in(dir: &Path, pid: u32, n: u32, record: &Record) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let name = format!("pid-{pid}-{n}");
    let path = dir.join(&name);
    // Written aside and renamed into place: never read half-written.
    let partial = dir.join(format!("{name}.partial"));
    fs::write(&partial, encode_record(record))?;
    fs::rename(&partial, &path)?;
    Ok(path)
}

/// Every marker in `dir`, with its process id.
fn read_markers(dir: &Path) -> Vec<(PathBuf, u32, Record)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let pid = parse_pid(&name)?;
            let record = decode_record(&fs::read_to_string(entry.path()).ok()?)?;
            Some((entry.path(), pid, record))
        })
        .collect()
}

fn parse_pid(name: &str) -> Option<u32> {
    let (pid, n) = name.strip_prefix("pid-")?.split_once('-')?;
    n.parse::<u32>().ok()?;
    pid.parse().ok()
}

/// A file beside the markers while one of our hosts has turned HidHide on
/// (it was off): the last of them to stop turns it off again, whichever
/// that is.
const TURNED_ON: &str = "turned-on";

fn turned_on() -> bool {
    markers_dir().is_some_and(|dir| dir.join(TURNED_ON).exists())
}

fn set_turned_on(on: bool) -> io::Result<()> {
    let dir = markers_dir().ok_or_else(|| io::Error::other("LOCALAPPDATA is not set"))?;
    if on {
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(TURNED_ON), "")
    } else {
        fs::remove_file(dir.join(TURNED_ON))
    }
}

/// One line each: the start time, then the devices.
fn encode_record(record: &Record) -> String {
    let mut out = format!("{}\n", record.started.unwrap_or(0));
    for device in &record.devices {
        out.push_str(device);
        out.push('\n');
    }
    out
}

fn decode_record(text: &str) -> Option<Record> {
    let mut lines = text.lines();
    let started = lines.next()?.trim().parse::<u64>().ok()?;
    Some(Record {
        started: (started != 0).then_some(started),
        devices: lines
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn control_codes_match_hidhides() {
        // CTL_CODE(32769, 2048, METHOD_BUFFERED, FILE_READ_DATA).
        assert_eq!(GET_WHITELIST, 0x8001_6000);
        assert_eq!(SET_ACTIVE, 0x8001_6014);
        assert_eq!(CLR_SESSION_BLACKLIST, 0x8001_6024);
    }

    #[test]
    fn multi_sz_round_trips() {
        let list = s(&[r"HID\VID_054C&PID_0CE6\7&1", "two"]);
        let encoded = encode_multi_sz(&list);
        assert_eq!(&encoded[encoded.len() - 2..], &[0, 0]);
        assert_eq!(decode_multi_sz(&encoded), list);
        assert_eq!(encode_multi_sz(&[]), vec![0, 0]);
        assert!(decode_multi_sz(&[0, 0]).is_empty());
        assert_eq!(decode_multi_sz(&wide(&bytes(&encoded))), list);
    }

    #[test]
    fn our_entries_go_and_the_users_stay() {
        let users = s(&[r"HID\VID_046D&PID_C21D\1"]);
        let ours = s(&[r"HID\VID_054C&PID_0CE6\7&1", r"USB\VID_054C&PID_0CE6\5"]);
        let hidden = with_added(&users, &ours);
        assert_eq!(hidden.len(), 3);
        // Adding again changes nothing, whatever the case.
        assert_eq!(
            with_added(&hidden, &s(&[r"usb\vid_054c&pid_0ce6\5"])),
            hidden
        );
        assert_eq!(without(&hidden, &ours, &[]), users);
        // Another live host still hides one of them.
        let kept = without(&hidden, &ours, &ours[..1]);
        assert_eq!(
            kept,
            s(&[r"HID\VID_046D&PID_C21D\1", r"HID\VID_054C&PID_0CE6\7&1"])
        );
    }

    /// The user hides part of the controller themselves (for DS4Windows,
    /// say): we record only the rest, so stopping leaves theirs hidden.
    #[test]
    fn the_users_own_entries_are_never_recorded() {
        let theirs = r"HID\VID_054C&PID_0CE6\7&1";
        let blacklist = s(&[r"HID\VID_046D&PID_C21D\1", &theirs.to_lowercase()]);
        let devices = s(&[theirs, r"USB\VID_054C&PID_0CE6\5"]);
        let ours = ours_to_add(&devices, &blacklist);
        assert_eq!(ours, s(&[r"USB\VID_054C&PID_0CE6\5"]));
        let hidden = with_added(&blacklist, &ours);
        assert_eq!(without(&hidden, &ours, &[]), blacklist, "theirs stay");
    }

    /// Our own marker counts as a live host's only while a guard owns it: one
    /// left by an undo that failed is undone again, not kept forever.
    #[test]
    fn our_own_markers_are_stale_unless_a_guard_owns_them() {
        let dir = std::env::temp_dir().join(format!("couch-hidhide-own-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let record = Record {
            started: own_start_time(),
            devices: Vec::new(),
        };
        let path = write_marker_in(&dir, std::process::id(), 7, &record).unwrap();
        assert!(marker_stale(&path, std::process::id(), &record));
        live().push(path.clone());
        assert!(!marker_stale(&path, std::process::id(), &record));
        live().retain(|m| *m != path);
        let _ = fs::remove_dir_all(&dir);
    }

    fn device(id: &str, hw: &str, class: &str, container: Option<&str>) -> Device {
        Device {
            instance_id: id.into(),
            hardware_ids: s(&[hw]),
            class: class.into(),
            container: container.map(String::from),
        }
    }

    const HID: &str = HIDDEN_CLASSES[0];
    const XUSB: &str = HIDDEN_CLASSES[1];
    const USB: &str = "{36fc9e60-c465-11cf-8056-444553540000}";
    const DUALSENSE: ControllerIds = ControllerIds {
        vendor: 0x054C,
        product: 0x0CE6,
    };

    #[test]
    fn a_controller_is_hidden_by_its_container() {
        let pad = Some("{aaaa}");
        let devices = [
            device(
                r"USB\VID_054C&PID_0CE6\5",
                r"USB\VID_054C&PID_0CE6&REV_0100",
                USB,
                pad,
            ),
            device(
                r"HID\VID_054C&PID_0CE6&MI_03\7&1",
                r"HID\VID_054C&PID_0CE6&MI_03",
                HID,
                pad,
            ),
            // A node of it without the ids in its own.
            device(r"HID\SOMETHING\8&2", r"HID_DEVICE_UP:0001_U:0005", HID, pad),
            // Another controller.
            device(
                r"HID\VID_045E&PID_0B12\9",
                r"HID\VID_045E&PID_0B12",
                XUSB,
                Some("{bbbb}"),
            ),
            // The keyboard built into the PC.
            device(r"HID\KBD\1", r"HID\VID_1234&PID_5678", HID, Some(THE_PC)),
        ];
        assert_eq!(
            to_hide(&devices, DUALSENSE),
            s(&[
                r"USB\VID_054C&PID_0CE6\5",
                r"HID\VID_054C&PID_0CE6&MI_03\7&1",
                r"HID\SOMETHING\8&2"
            ])
        );
        assert!(
            to_hide(
                &devices,
                ControllerIds {
                    vendor: 0x1111,
                    product: 0x2222
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn a_controller_built_into_the_pc_hides_only_itself() {
        let devices = [
            device(
                r"HID\VID_054C&PID_0CE6\1",
                r"HID\VID_054C&PID_0CE6",
                HID,
                Some(THE_PC),
            ),
            device(r"HID\KBD\1", r"HID\VID_1234&PID_5678", HID, Some(THE_PC)),
        ];
        assert_eq!(
            to_hide(&devices, DUALSENSE),
            s(&[r"HID\VID_054C&PID_0CE6\1"])
        );
    }

    #[test]
    fn bluetooth_ids_are_read() {
        let bt = device(
            r"HID\{00001124-0000-1000-8000-00805F9B34FB}_VID&0002054C_PID&0CE6\9&1",
            r"HID\{00001124-0000-1000-8000-00805f9b34fb}_VID&0002054c_PID&0ce6",
            HID,
            Some("{cccc}"),
        );
        assert!(has_ids(&bt, DUALSENSE));
        assert_eq!(hex_after("USB\\VID_054C&PID_0CE6", "VID"), Some(0x054C));
        assert_eq!(hex_after("HID\\VID_12", "VID"), None);
    }

    #[test]
    fn dos_paths() {
        assert_eq!(
            join_dos(r"\Device\HarddiskVolume3", r"C:\dev\app.exe").as_deref(),
            Some(r"\Device\HarddiskVolume3\dev\app.exe")
        );
        assert_eq!(join_dos(r"\Device\X", r"\\server\share\app.exe"), None);
        let ours = dos_path(&std::env::current_exe().unwrap()).unwrap();
        assert!(ours.starts_with(r"\Device\"), "{ours}");
    }

    #[test]
    fn records_round_trip_and_stale_ones_are_found() {
        let dir = std::env::temp_dir().join(format!("couch-hidhide-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let record = Record {
            started: Some(42),
            devices: s(&[r"HID\VID_054C&PID_0CE6\7&1"]),
        };
        assert_eq!(decode_record(&encode_record(&record)), Some(record.clone()));
        let none = Record::default();
        assert_eq!(decode_record(&encode_record(&none)), Some(none));

        let path = write_marker_in(&dir, 1234, 0, &record).unwrap();
        fs::write(dir.join("pid-99-0.partial"), "half").unwrap();
        let found = read_markers(&dir);
        assert_eq!(found, vec![(path, 1234, record)]);
        assert_eq!(parse_pid("pid-12-x"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Whatever is installed, asking doesn't fail or hang.
    #[test]
    fn its_state_can_be_read() {
        let _ = state();
    }

    #[test]
    fn our_own_pads_are_never_hidden() {
        let ours = ControllerIds {
            vendor: 0x045E,
            product: 0x028E,
        };
        assert_eq!(hide(ours).unwrap_err(), HideError::Ours);
    }
}
