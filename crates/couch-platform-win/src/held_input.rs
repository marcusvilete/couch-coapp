//! Keys and mouse buttons this process holds down through `SendInput`, kept
//! where a crash can't lose them.
//!
//! Windows keeps an injected key down until something releases it, even
//! after the process that pressed it is gone. The host releases on every
//! normal end; this covers the others:
//! - **a panic:** [`release_on_panic`] installs a hook that lets go of
//!   everything at once, before the process unwinds or aborts;
//! - **a kill or a hard crash:** the held set is mirrored to a marker file
//!   ([`crate::markers`]), written by a thread of its own within about 20 ms
//!   of a change, so the input path never waits on the disk. The next start
//!   of the app or of `couch-cli host` calls [`release_stale`], which lets go
//!   of what a dead process left held.
//!
//! Only releases are ever sent from here; nothing is pressed.

use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    sync::{Condvar, Mutex, Once, OnceLock},
    time::Duration,
};

use couch_input::{HidUsage, MouseButton};

use crate::markers::{self, Process, own_start_time, process_state};

/// How long the writer lets changes gather before writing.
const GATHER: Duration = Duration::from_millis(20);
/// After a failed write, how long before the next try.
const RETRY: Duration = Duration::from_millis(100);

/// Something held down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Held {
    Key(u16),
    Button(u8),
}

const BUTTONS: [MouseButton; 5] = [
    MouseButton::Left,
    MouseButton::Right,
    MouseButton::Middle,
    MouseButton::Back,
    MouseButton::Forward,
];

impl Held {
    pub(crate) fn button(button: MouseButton) -> Self {
        let index = BUTTONS.iter().position(|b| *b == button).unwrap_or(0);
        Self::Button(index as u8)
    }

    fn line(self) -> String {
        match self {
            Self::Key(usage) => format!("key {usage}"),
            Self::Button(index) => format!("button {index}"),
        }
    }

    fn parse(line: &str) -> Option<Self> {
        let (kind, value) = line.trim().split_once(' ')?;
        match kind {
            "key" => Some(Self::Key(value.parse().ok()?)),
            "button" => {
                let index: u8 = value.parse().ok()?;
                (usize::from(index) < BUTTONS.len()).then_some(Self::Button(index))
            }
            _ => None,
        }
    }

    /// Lets go of it, without recording anything.
    fn release(self) {
        let _ = match self {
            Self::Key(usage) => crate::sendinput::send_key(HidUsage(usage), false),
            Self::Button(index) => {
                crate::sendinput::send_button(BUTTONS[usize::from(index)], false)
            }
        };
    }
}

struct State {
    held: BTreeSet<Held>,
    /// Changed since the writer last wrote.
    dirty: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

/// The held set, and its writer thread, started on first use.
fn shared() -> &'static Shared {
    static WRITER: Once = Once::new();
    let shared = SHARED.get_or_init(|| Shared {
        state: Mutex::new(State {
            held: BTreeSet::new(),
            dirty: false,
        }),
        changed: Condvar::new(),
    });
    WRITER.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("held-input".into())
            .spawn(move || writer(shared));
    });
    shared
}

/// Records that `item` went down or up. Called after `SendInput` took it.
pub(crate) fn note(item: Held, down: bool) {
    let shared = shared();
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    let changed = if down {
        state.held.insert(item)
    } else {
        state.held.remove(&item)
    };
    if changed {
        state.dirty = true;
        shared.changed.notify_one();
    }
}

/// Mirrors the held set to this process's marker: written while anything is
/// held, removed when nothing is.
fn writer(shared: &'static Shared) {
    let Some(dir) = markers::dir("held-input") else {
        return;
    };
    let path = dir.join(marker_name(std::process::id()));
    let started = own_start_time();
    loop {
        {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            while !state.dirty {
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|e| e.into_inner());
            }
        }
        std::thread::sleep(GATHER);
        let held: Vec<Held> = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.dirty = false;
            state.held.iter().copied().collect()
        };
        let written = if held.is_empty() {
            match fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        } else {
            write_marker(&dir, &path, started, &held)
        };
        if written.is_err() {
            // Something held the file (an antivirus scan, say): try again.
            shared.state.lock().unwrap_or_else(|e| e.into_inner()).dirty = true;
            std::thread::sleep(RETRY);
        }
    }
}

fn marker_name(pid: u32) -> String {
    format!("pid-{pid}")
}

fn parse_pid(name: &str) -> Option<u32> {
    name.strip_prefix("pid-")?.parse().ok()
}

fn write_marker(
    dir: &Path,
    path: &Path,
    started: Option<u64>,
    held: &[Held],
) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let mut text = format!("{}\n", started.unwrap_or(0));
    for item in held {
        text.push_str(&item.line());
        text.push('\n');
    }
    // Written aside and renamed into place, so a reader never sees half.
    let partial = path.with_extension("partial");
    fs::write(&partial, text)?;
    fs::rename(&partial, path)
}

fn read_marker(text: &str) -> (Option<u64>, Vec<Held>) {
    let mut lines = text.lines();
    let started = lines.next().and_then(|l| l.trim().parse().ok());
    (started, lines.filter_map(Held::parse).collect())
}

/// What stale markers in `dir` say was left held; the markers go.
fn stale_in(dir: &Path, process: impl Fn(u32) -> Process) -> Vec<Held> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut left = BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // A half-written marker left by a kill: what it held is in the
        // marker beside it, if anything.
        if let Some(pid) = name.strip_suffix(".partial").and_then(parse_pid) {
            if markers::stale(process(pid), None) {
                let _ = fs::remove_file(entry.path());
            }
            continue;
        }
        let Some(pid) = parse_pid(&name) else {
            continue;
        };
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let (started, held) = read_marker(&text);
        if markers::stale(process(pid), started) {
            left.extend(held);
            let _ = fs::remove_file(entry.path());
        }
    }
    left.into_iter().collect()
}

/// Lets go of whatever a crashed or killed host on this PC left held, and
/// says how many keys and buttons that was. Call at start, before any input.
pub fn release_stale() -> usize {
    let Some(dir) = markers::dir("held-input") else {
        return 0;
    };
    let left = stale_in(&dir, process_state);
    for item in &left {
        item.release();
    }
    left.len()
}

/// Installs, once, a panic hook that lets go of everything this process
/// holds before the usual hook runs.
pub fn release_on_panic() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            release_all_now();
            previous(info);
        }));
    });
}

/// Lets go of everything held, now. Never blocks: a panic may have struck
/// while the lock was held.
fn release_all_now() {
    // Nothing was ever held if nothing started it.
    let Some(shared) = SHARED.get() else {
        return;
    };
    // The writer holds the lock only for a moment; the panicking thread
    // itself may hold it for good, so don't wait long.
    let mut taken = None;
    for _ in 0..50 {
        match shared.state.try_lock() {
            Ok(mut state) => {
                state.dirty = true;
                taken = Some(std::mem::take(&mut state.held));
                break;
            }
            Err(std::sync::TryLockError::Poisoned(e)) => {
                let mut state = e.into_inner();
                state.dirty = true;
                taken = Some(std::mem::take(&mut state.held));
                break;
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
    let Some(held) = taken else {
        return;
    };
    for item in held {
        item.release();
    }
    shared.changed.notify_one();
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("couch-held-input-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn markers_round_trip() {
        let held = [
            Held::Key(0x1A),
            Held::Key(0xE1),
            Held::button(MouseButton::Back),
        ];
        let dir = scratch("roundtrip");
        let path = dir.join(marker_name(42));
        write_marker(&dir, &path, Some(7), &held).unwrap();
        let (started, read) = read_marker(&fs::read_to_string(&path).unwrap());
        assert_eq!(started, Some(7));
        assert_eq!(read, held);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_stale_markers_are_released_and_removed() {
        let dir = scratch("stale");
        let marker = |pid: u32, started: u64, held: &[Held]| {
            write_marker(&dir, &dir.join(marker_name(pid)), Some(started), held).unwrap();
        };
        marker(100, 5, &[Held::Key(0x04)]);
        marker(200, 7, &[Held::Key(0x05), Held::Button(0)]);
        marker(300, 9, &[Held::Key(0x06)]);
        marker(400, 3, &[Held::Key(0x07)]);
        // 100 runs; 200 has gone; 300's id was reused; 400 can't be asked.
        let process = |pid| match pid {
            100 => Process::Running(Some(5)),
            300 => Process::Running(Some(42)),
            400 => Process::Unknown,
            _ => Process::Gone,
        };
        let left = stale_in(&dir, process);
        assert_eq!(
            left,
            vec![Held::Key(0x05), Held::Key(0x06), Held::Button(0)]
        );
        assert!(dir.join(marker_name(100)).exists());
        assert!(!dir.join(marker_name(200)).exists());
        assert!(!dir.join(marker_name(300)).exists());
        assert!(dir.join(marker_name(400)).exists(), "doubt keeps it");
        // A second look finds nothing more.
        assert!(stale_in(&dir, process).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn half_written_markers_of_dead_processes_go() {
        let dir = scratch("partial");
        fs::write(dir.join("pid-100.partial"), "5\nkey 4\n").unwrap();
        fs::write(dir.join("pid-200.partial"), "7\nkey 5\n").unwrap();
        let process = |pid| match pid {
            100 => Process::Running(Some(5)),
            _ => Process::Gone,
        };
        assert!(stale_in(&dir, process).is_empty(), "never acted on");
        assert!(dir.join("pid-100.partial").exists());
        assert!(!dir.join("pid-200.partial").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn garbage_is_ignored() {
        assert_eq!(Held::parse("key x"), None);
        assert_eq!(Held::parse("button 9"), None);
        assert_eq!(Held::parse("pad 1"), None);
        assert_eq!(read_marker("").1, Vec::new());
        assert_eq!(parse_pid("pid-12.partial"), None);
        assert_eq!(parse_pid("pid-12"), Some(12));
    }
}
