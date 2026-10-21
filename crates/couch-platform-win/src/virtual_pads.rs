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
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU32, Ordering},
    },
};

use crate::markers::{self, Process, own_start_time, process_state};

/// Tells apart the markers of several pads in one process.
static NEXT: AtomicU32 = AtomicU32::new(0);

/// This process's own pads, by marker number: their serials once known.
/// Kept beside the files (M22: a host reads other programs' virtual pads,
/// and must never take its own for one, whatever happens to the files).
static OWN: Mutex<BTreeMap<u32, Option<u32>>> = Mutex::new(BTreeMap::new());

fn own() -> MutexGuard<'static, BTreeMap<u32, Option<u32>>> {
    OWN.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A plugged pad's marker. Dropping it removes the marker.
#[derive(Debug)]
pub struct PadMarker {
    /// Its file, unless it couldn't be written: the pad is then this
    /// process's in memory alone, which other processes can't see.
    path: Option<PathBuf>,
    started: Option<u64>,
    /// Its number in [`OWN`], for a marker of this process's own pad.
    own: Option<u32>,
}

impl Drop for PadMarker {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = fs::remove_file(path);
        }
        if let Some(n) = self.own {
            own().remove(&n);
        }
    }
}

impl PadMarker {
    /// Records the pad's ViGEm serial, once it has one: in memory always,
    /// in its file if it has one.
    pub fn set_serial(&self, serial: u32) -> io::Result<()> {
        if let Some(n) = self.own {
            own().insert(n, Some(serial));
        }
        match &self.path {
            Some(path) => write_atomically(path, &encode(self.started, Some(serial))),
            None => Ok(()),
        }
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

/// Marks a virtual pad as plugged in by this process, before it is: in
/// memory always (this process never takes the pad for another program's,
/// M22), and in a file for the others. A file that can't be written is
/// logged: other processes on this PC then can't tell the pad is a host's.
pub fn mark() -> PadMarker {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let marker = match markers_dir() {
        Some(dir) => mark_own_in(&dir, n),
        None => Err((own_marker(n), io::Error::other("LOCALAPPDATA is not set"))),
    };
    marker.unwrap_or_else(|(marker, e)| {
        tracing::warn!("marking the virtual pad for other processes: {e}");
        marker
    })
}

/// A marker of this process's own pad, in memory alone.
fn own_marker(n: u32) -> PadMarker {
    // Ours before the file: a pad without a serial yet makes every ViGEm
    // pad possibly ours.
    own().insert(n, None);
    PadMarker {
        path: None,
        started: own_start_time(),
        own: Some(n),
    }
}

/// [`own_marker`], with its file in `dir`; the marker without a file if it
/// can't be written.
fn mark_own_in(dir: &Path, n: u32) -> Result<PadMarker, (PadMarker, io::Error)> {
    let mut marker = own_marker(n);
    match mark_in(dir, std::process::id(), n, own_start_time()) {
        Ok(mut file) => {
            marker.path = file.path.take();
            Ok(marker)
        }
        Err(e) => Err((marker, e)),
    }
}

/// The pads of the hosts still running here, this process's own included
/// even if their files can't be read.
pub fn host_pads() -> HostPads {
    let mut pads = markers_dir().map_or_else(HostPads::default, |dir| live_in(&dir, process_state));
    for serial in own().values() {
        match serial {
            Some(serial) => {
                pads.serials.insert(*serial);
            }
            None => pads.plugging = true,
        }
    }
    pads
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
    Ok(PadMarker {
        path: Some(path),
        started,
        own: None,
    })
}

/// The pads whose markers belong to running hosts. A marker is removed only
/// when its process is known to be gone or the id now names another
/// process; any doubt keeps it, since a lost marker would let a viewer here
/// read the host's pad and send it back. A live marker whose serial can't be
/// read counts as a pad still being plugged: every ViGEm pad is then a
/// host's.
fn live_in(dir: &Path, process: impl Fn(u32) -> Process) -> HostPads {
    let mut pads = HostPads::default();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        // No host has made one yet.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return pads,
        // Markers that can't be read may name any ViGEm pad.
        Err(_) => {
            pads.plugging = true;
            return pads;
        }
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

    /// M22: a host reads other programs' virtual pads, so it must know its
    /// own without their files.
    #[test]
    fn this_processs_pads_count_without_their_files() {
        let dir = scratch("in-memory");
        // A number no other marker of this process takes.
        let n = u32::MAX - 7;
        let marker = mark_own_in(&dir, n).unwrap();
        assert!(
            host_pads().plugging,
            "no serial yet: any ViGEm pad may be it"
        );
        marker.set_serial(4242).unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert!(host_pads().serials.contains(&4242), "its file is gone");
        drop(marker);
        assert!(!host_pads().serials.contains(&4242));

        // A file that can't be written: still ours in this process.
        let not_a_dir = scratch("in-memory-file");
        fs::write(&not_a_dir, "").unwrap();
        let (marker, _) = mark_own_in(&not_a_dir, n - 1).unwrap_err();
        marker.set_serial(4243).unwrap();
        assert!(host_pads().serials.contains(&4243));
        drop(marker);
        assert!(!host_pads().serials.contains(&4243));
        let _ = fs::remove_file(&not_a_dir);
    }

    #[test]
    fn markers_that_cant_be_read_may_name_any_pad() {
        let dir = scratch("unreadable");
        fs::create_dir_all(&dir).unwrap();
        let not_a_dir = dir.join("file");
        fs::write(&not_a_dir, "").unwrap();
        assert!(live_in(&not_a_dir, process_state).plugging);
        assert!(
            !live_in(&dir.join("missing"), process_state).any(),
            "no folder: no host yet"
        );
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
