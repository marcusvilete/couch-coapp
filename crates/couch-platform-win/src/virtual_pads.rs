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
//! The host leaves a marker file per pad ([`crate::markers`]). A marker
//! names the host process and its start time, so one left behind by a crash
//! is recognised as stale even if the process id is reused.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

use crate::markers::{self, Process, own_start_time, process_state};

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
    markers::dir("virtual-pads")
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
        if markers::stale(process(pid), recorded) {
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
    fn our_own_markers_count() {
        let dir = scratch("own");
        let pid = std::process::id();
        let _first = mark_in(&dir, pid, 0, own_start_time()).unwrap();
        let _second = mark_in(&dir, pid, 1, own_start_time()).unwrap();
        assert_eq!(live_markers_in(&dir, process_state), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn names() {
        assert_eq!(parse_pid(&marker_name(1234, 2)), Some(1234));
        assert_eq!(parse_pid("notes.txt"), None);
        assert_eq!(parse_pid("pid-12-pad-x"), None);
    }
}
