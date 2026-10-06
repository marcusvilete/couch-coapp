//! Whether a Couch Co-App host on this PC has a virtual pad plugged in.
//!
//! A host's pad reports a real Xbox 360 controller's ids, because games and
//! Steam pick a controller's layout by its ids: with made-up ones they treat
//! it as a generic joystick, and only the D-pad works. So a stream window on
//! the same PC (two profiles, in testing) can't tell the host's pad from a
//! real one, and would read it and send it back to the host. While a host
//! here has a pad plugged in, the viewer therefore skips every pad with Xbox
//! 360 ids (`couch_input::XBOX_360_IDS`). Two PCs are never affected: the
//! guest's PC has no marker.
//!
//! Telling the pads apart by XInput slot doesn't work: the same pad had
//! slot 0 in the process that plugged it and slot 1 in others (measured in
//! M5b, with Steam running).
//!
//! The host leaves a marker file per pad. A marker names the host process
//! and its start time, so one left behind by a crash is recognised as stale
//! even if the process id is reused.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

use windows::Win32::{
    Foundation::{CloseHandle, E_INVALIDARG, FILETIME, HANDLE, STILL_ACTIVE},
    System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, OpenProcess,
        PROCESS_QUERY_LIMITED_INFORMATION,
    },
};

/// Tells apart the markers of several pads in one process.
static NEXT: AtomicU32 = AtomicU32::new(0);

/// A plugged pad's marker. Dropping it removes the marker.
#[derive(Debug)]
pub struct PadMarker {
    path: PathBuf,
}

impl Drop for PadMarker {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Marks a virtual pad as plugged in by this process.
pub fn mark() -> io::Result<PadMarker> {
    let dir = markers_dir().ok_or_else(|| io::Error::other("LOCALAPPDATA is not set"))?;
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    mark_in(&dir, std::process::id(), n, own_start_time())
}

/// A host that is still running has a virtual pad plugged in on this PC.
pub fn host_pads_here() -> bool {
    markers_dir().is_some_and(|dir| live_markers_in(&dir, process_state) > 0)
}

fn markers_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(Path::new(&base).join("CouchCoApp").join("virtual-pads"))
}

fn marker_name(pid: u32, n: u32) -> String {
    format!("pid-{pid}-pad-{n}")
}

fn mark_in(dir: &Path, pid: u32, n: u32, started: Option<u64>) -> io::Result<PadMarker> {
    fs::create_dir_all(dir)?;
    let path = dir.join(marker_name(pid, n));
    // Written aside and renamed into place, so a viewer never reads a
    // half-written marker (its name has no `-pad-<n>` ending, so it's
    // ignored meanwhile).
    let partial = dir.join(format!("{}.partial", marker_name(pid, n)));
    fs::write(&partial, started.unwrap_or(0).to_string())?;
    fs::rename(&partial, &path)?;
    Ok(PadMarker { path })
}

/// A marker's process, as far as can be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Process {
    /// Running, with its start time if it could be read.
    Running(Option<u64>),
    /// Exited, or no such process.
    Gone,
    /// Couldn't be asked (access denied, say): assume it runs.
    Unknown,
}

/// How many markers belong to running hosts. A marker is removed only when
/// its process is known to be gone or the id now names another process; any
/// doubt keeps it, since a lost marker would let a viewer here read the
/// host's pad and send it back.
fn live_markers_in(dir: &Path, process: impl Fn(u32) -> Process) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut live = 0;
    for entry in entries.flatten() {
        let Some(pid) = parse_pid(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        let recorded = fs::read_to_string(entry.path())
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok());
        let stale = match (process(pid), recorded) {
            (Process::Gone, _) => true,
            // The id was reused by a process that started at another time.
            (Process::Running(Some(now)), Some(then)) => then != 0 && now != then,
            _ => false,
        };
        if stale {
            let _ = fs::remove_file(entry.path());
        } else {
            live += 1;
        }
    }
    live
}

fn parse_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("pid-")?;
    let (pid, n) = rest.split_once("-pad-")?;
    n.parse::<u32>().ok()?;
    pid.parse().ok()
}

fn own_start_time() -> Option<u64> {
    // SAFETY: the pseudo-handle for this process needs no closing.
    process_start(unsafe { GetCurrentProcess() })
}

/// Whether `pid` runs, and since when.
fn process_state(pid: u32) -> Process {
    // SAFETY: opening a process by id for a query; closed below.
    let process = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(process) => process,
        // No process has that id.
        Err(e) if e.code() == E_INVALIDARG => return Process::Gone,
        Err(_) => return Process::Unknown,
    };
    let mut code = 0;
    // SAFETY: a process handle with query rights and a valid out-parameter.
    // An exited process still answers until its last handle closes, so
    // only STILL_ACTIVE means it runs.
    let state = if unsafe { GetExitCodeProcess(process, &mut code) }.is_err() {
        Process::Unknown
    } else if code == STILL_ACTIVE.0 as u32 {
        Process::Running(process_start(process))
    } else {
        Process::Gone
    };
    // SAFETY: we opened it above and close it once.
    let _ = unsafe { CloseHandle(process) };
    state
}

fn process_start(process: HANDLE) -> Option<u64> {
    let (mut created, mut exited, mut kernel, mut user) = Default::default();
    // SAFETY: a process handle with query rights and four valid
    // out-parameters.
    unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    let FILETIME {
        dwLowDateTime: low,
        dwHighDateTime: high,
    } = created;
    Some(u64::from(high) << 32 | u64::from(low))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("couch-virtual-pads-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn live_markers_count_and_stale_ones_go() {
        let dir = scratch("stale");
        let live = mark_in(&dir, 100, 0, Some(5)).unwrap();
        let _crashed = mark_in(&dir, 200, 0, Some(7)).unwrap();
        let _reused = mark_in(&dir, 300, 0, Some(9)).unwrap();
        // 100 runs; 200 has gone; 300 is another process that got the id.
        let process = |pid| match pid {
            100 => Process::Running(Some(5)),
            300 => Process::Running(Some(42)),
            _ => Process::Gone,
        };
        assert_eq!(live_markers_in(&dir, process), 1);
        assert!(
            !dir.join(marker_name(200, 0)).exists(),
            "stale markers are removed"
        );
        assert!(!dir.join(marker_name(300, 0)).exists());
        drop(live);
        assert_eq!(live_markers_in(&dir, process), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Doubt keeps a marker: losing a live one would let a viewer here read
    /// the host's pad and send it back.
    #[test]
    fn doubt_keeps_a_marker() {
        let dir = scratch("doubt");
        let _denied = mark_in(&dir, 100, 0, Some(5)).unwrap();
        // A marker caught half-written, or garbled, of a running process.
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(marker_name(200, 0)), "").unwrap();
        // Another marker still being written is not one yet.
        fs::write(dir.join(format!("{}.partial", marker_name(300, 0))), "").unwrap();
        let process = |pid| match pid {
            100 => Process::Unknown,
            200 => Process::Running(Some(7)),
            _ => Process::Gone,
        };
        assert_eq!(live_markers_in(&dir, process), 2);
        assert!(dir.join(marker_name(100, 0)).exists());
        assert!(dir.join(marker_name(200, 0)).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn our_own_markers_count_and_a_missing_process_is_gone() {
        let dir = scratch("own");
        let pid = std::process::id();
        let _first = mark_in(&dir, pid, 0, own_start_time()).unwrap();
        let _second = mark_in(&dir, pid, 1, own_start_time()).unwrap();
        assert_eq!(live_markers_in(&dir, process_state), 2);
        // Process ids are multiples of 4; no process has this one.
        assert_eq!(process_state(u32::MAX - 2), Process::Gone);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn names() {
        assert_eq!(parse_pid(&marker_name(1234, 2)), Some(1234));
        assert_eq!(parse_pid("notes.txt"), None);
        assert_eq!(parse_pid("pid-12-pad-x"), None);
    }
}
