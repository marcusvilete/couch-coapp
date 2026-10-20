//! Which virtual pads on this PC are a Couch Co-App host's.
//!
//! A host's pad reports a real Xbox 360 controller's ids, because games and
//! Steam pick a controller's layout by its ids: with made-up ones they treat
//! it as a generic joystick, and only the D-pad works. So its ids can't tell
//! it from a real controller, nor from another program's virtual pad (a
//! remapper's, on the same ViGEm bus). What can: its ViGEm serial, the last
//! part of its device's instance id (P9, `crate::pad_origin`). Each host
//! leaves a marker per pad ([`crate::markers`]) naming its process and that
//! serial; a stream window on the same PC (two profiles, in testing) then
//! skips exactly those, and the host's own reader never lists them.
//!
//! The marker is written before the pad is plugged, without a serial, and
//! the serial is added once ViGEm has given one: meanwhile every ViGEm pad
//! counts as a host's ([`HostPads::plugging`]). A marker names its process's
//! start time too, so one left behind by a crash is recognised as stale even
//! if the process id is reused.
//!
//! The XInput slot alone never decides: in M5b a pad's slot as the plugging
//! process saw it (most likely ViGEm's own answer, which P9 found
//! unreliable) differed from SDL's in others. P9 checked the Xbox driver's
//! player light against SDL's `XInput#N` in the plugging process and in
//! another one: they agreed. The slot only finds a pad's device; its serial
//! decides, and any doubt in finding it makes the pad's origin unknown.

use std::{
    collections::BTreeSet,
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
    started: Option<u64>,
}

impl Drop for PadMarker {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl PadMarker {
    /// Records the pad's ViGEm serial, once it has one.
    pub fn set_serial(&self, serial: u32) -> io::Result<()> {
        write_atomically(&self.path, &encode(self.started, Some(serial)))
    }
}

/// The virtual pads of the hosts running on this PC, as their markers say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostPads {
    /// The ViGEm serials of their pads.
    pub serials: BTreeSet<u32>,
    /// A host is plugging a pad that has no serial yet: until it has, any
    /// ViGEm pad may be that one.
    pub plugging: bool,
}

impl HostPads {
    pub fn any(&self) -> bool {
        self.plugging || !self.serials.is_empty()
    }
}

/// Marks a virtual pad as plugged in by this process, before it is.
pub fn mark() -> io::Result<PadMarker> {
    let dir = markers_dir().ok_or_else(|| io::Error::other("LOCALAPPDATA is not set"))?;
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    mark_in(&dir, std::process::id(), n, own_start_time())
}

/// The pads of the hosts still running here.
pub fn host_pads() -> HostPads {
    markers_dir().map_or_else(HostPads::default, |dir| live_in(&dir, process_state))
}

/// A host that is still running has a virtual pad plugged in on this PC.
pub fn host_pads_here() -> bool {
    host_pads().any()
}

fn markers_dir() -> Option<PathBuf> {
    markers::dir("virtual-pads")
}

fn marker_name(pid: u32, n: u32) -> String {
    format!("pid-{pid}-pad-{n}")
}

/// A marker's text: its process's start time, then the pad's serial once
/// known.
fn encode(started: Option<u64>, serial: Option<u32>) -> String {
    let started = started.unwrap_or(0);
    match serial {
        Some(serial) => format!("{started} {serial}"),
        None => started.to_string(),
    }
}

/// The start time and serial a marker records; `None`s where unreadable.
fn decode(text: &str) -> (Option<u64>, Option<u32>) {
    let mut words = text.split_whitespace();
    let started = words.next().and_then(|w| w.parse().ok());
    let serial = words.next().and_then(|w| w.parse().ok());
    (started, serial)
}

/// Written aside and renamed into place, so a reader never sees half a
/// marker (a name without the `-pad-<n>` ending is ignored meanwhile).
fn write_atomically(path: &Path, text: &str) -> io::Result<()> {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    fs::write(&partial, text)?;
    // A reader (or an antivirus) holding the marker can make one rename
    // fail; a few tries, then the aside copy goes.
    let mut tries = 0;
    loop {
        match fs::rename(&partial, path) {
            Ok(()) => return Ok(()),
            Err(_) if tries < 4 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                let _ = fs::remove_file(&partial);
                return Err(e);
            }
        }
    }
}

fn mark_in(dir: &Path, pid: u32, n: u32, started: Option<u64>) -> io::Result<PadMarker> {
    fs::create_dir_all(dir)?;
    let path = dir.join(marker_name(pid, n));
    write_atomically(&path, &encode(started, None))?;
    Ok(PadMarker { path, started })
}

/// The pads whose markers belong to running hosts. A marker is removed only
/// when its process is known to be gone or the id now names another
/// process; any doubt keeps it, since a lost marker would let a viewer here
/// read the host's pad and send it back. A live marker whose serial can't be
/// read counts as a pad still being plugged: every ViGEm pad is then a
/// host's.
fn live_in(dir: &Path, process: impl Fn(u32) -> Process) -> HostPads {
    let mut pads = HostPads::default();
    let Ok(entries) = fs::read_dir(dir) else {
        return pads;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // An aside copy left by a process that's gone (a crash mid-write).
        if let Some(pid) = name.strip_suffix(".partial").and_then(parse_pid) {
            if process(pid) == Process::Gone {
                let _ = fs::remove_file(entry.path());
            }
            continue;
        }
        let Some(pid) = parse_pid(&name) else {
            continue;
        };
        let (started, serial) = fs::read_to_string(entry.path())
            .map(|text| decode(&text))
            .unwrap_or_default();
        if markers::stale(process(pid), started) {
            let _ = fs::remove_file(entry.path());
            continue;
        }
        match serial {
            Some(serial) => {
                pads.serials.insert(serial);
            }
            None => pads.plugging = true,
        }
    }
    pads
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
        live.set_serial(51).unwrap();
        let crashed = mark_in(&dir, 200, 0, Some(7)).unwrap();
        crashed.set_serial(52).unwrap();
        std::mem::forget(crashed);
        let _reused = mark_in(&dir, 300, 0, Some(9)).unwrap();
        // 100 runs; 200 has gone; 300 is another process that got the id.
        let process = |pid| match pid {
            100 => Process::Running(Some(5)),
            300 => Process::Running(Some(42)),
            _ => Process::Gone,
        };
        let pads = live_in(&dir, process);
        assert_eq!(pads.serials, BTreeSet::from([51]));
        assert!(!pads.plugging, "the reused id's marker went");
        assert!(
            !dir.join(marker_name(200, 0)).exists(),
            "stale markers are removed"
        );
        assert!(!dir.join(marker_name(300, 0)).exists());
        drop(live);
        assert!(!live_in(&dir, process).any());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A pad being plugged has no serial yet: until it has, any ViGEm pad
    /// may be it.
    #[test]
    fn a_marker_without_a_serial_means_one_is_being_plugged() {
        let dir = scratch("plugging");
        let marker = mark_in(&dir, std::process::id(), 0, own_start_time()).unwrap();
        let pads = live_in(&dir, process_state);
        assert!(pads.plugging && pads.serials.is_empty() && pads.any());
        marker.set_serial(7).unwrap();
        let pads = live_in(&dir, process_state);
        assert_eq!(pads.serials, BTreeSet::from([7]));
        assert!(!pads.plugging);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Doubt keeps a marker: losing a live one would let a viewer here read
    /// the host's pad and send it back.
    #[test]
    fn doubt_keeps_a_marker() {
        let dir = scratch("doubt");
        let denied = mark_in(&dir, 100, 0, Some(5)).unwrap();
        denied.set_serial(3).unwrap();
        // A garbled marker of a running process: a pad being plugged.
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(marker_name(200, 0)), "").unwrap();
        // Another marker still being written is not one yet; one a gone
        // process left half-written is removed.
        fs::write(dir.join(format!("{}.partial", marker_name(300, 0))), "").unwrap();
        fs::write(dir.join(format!("{}.partial", marker_name(400, 0))), "").unwrap();
        let process = |pid| match pid {
            100 => Process::Unknown,
            200 => Process::Running(Some(7)),
            300 => Process::Running(Some(9)),
            _ => Process::Gone,
        };
        let pads = live_in(&dir, process);
        assert_eq!(pads.serials, BTreeSet::from([3]));
        assert!(pads.plugging);
        assert!(dir.join(marker_name(100, 0)).exists());
        assert!(dir.join(marker_name(200, 0)).exists());
        let partial = |pid| dir.join(format!("{}.partial", marker_name(pid, 0)));
        assert!(partial(300).exists(), "still being written");
        assert!(!partial(400).exists(), "left by a gone process");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn our_own_markers_count() {
        let dir = scratch("own");
        let pid = std::process::id();
        let first = mark_in(&dir, pid, 0, own_start_time()).unwrap();
        let second = mark_in(&dir, pid, 1, own_start_time()).unwrap();
        first.set_serial(1).unwrap();
        second.set_serial(2).unwrap();
        assert_eq!(live_in(&dir, process_state).serials, BTreeSet::from([1, 2]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_and_texts() {
        assert_eq!(parse_pid(&marker_name(1234, 2)), Some(1234));
        assert_eq!(parse_pid("notes.txt"), None);
        assert_eq!(parse_pid("pid-12-pad-x"), None);
        assert_eq!(decode(&encode(Some(5), Some(51))), (Some(5), Some(51)));
        assert_eq!(decode(&encode(Some(5), None)), (Some(5), None));
        // Before P9, a marker held its start time alone.
        assert_eq!(decode("133700"), (Some(133700), None));
    }
}
