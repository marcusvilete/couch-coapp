//! Marker files that outlive a crash: what a process leaves in
//! `%LOCALAPPDATA%\CouchCoApp\<kind>` while it holds something the rest of
//! the PC should know about (a virtual pad plugged in, keys held down). Each
//! names its process and the process's start time, so one left behind by a
//! crash is recognised as stale even if the process id is reused.

use std::path::{Path, PathBuf};

use windows::Win32::{
    Foundation::{CloseHandle, E_INVALIDARG, FILETIME, HANDLE, STILL_ACTIVE},
    System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, OpenProcess,
        PROCESS_QUERY_LIMITED_INFORMATION,
    },
};

/// The folder for one kind of marker.
pub(crate) fn dir(kind: &str) -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(Path::new(&base).join("CouchCoApp").join(kind))
}

/// A marker's process, as far as can be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Process {
    /// Running, with its start time if it could be read.
    Running(Option<u64>),
    /// Exited, or no such process.
    Gone,
    /// Couldn't be asked (access denied, say): assume it runs.
    Unknown,
}

/// Whether a marker of `process` that recorded `recorded` as its start time
/// is stale: its process is known to be gone, or the id now names another
/// process. Any doubt keeps it.
pub(crate) fn stale(process: Process, recorded: Option<u64>) -> bool {
    match (process, recorded) {
        (Process::Gone, _) => true,
        // The id was reused by a process that started at another time.
        (Process::Running(Some(now)), Some(then)) => then != 0 && now != then,
        _ => false,
    }
}

pub(crate) fn own_start_time() -> Option<u64> {
    // SAFETY: the pseudo-handle for this process needs no closing.
    process_start(unsafe { GetCurrentProcess() })
}

/// Whether `pid` runs, and since when.
pub(crate) fn process_state(pid: u32) -> Process {
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

    #[test]
    fn our_own_process_runs_and_a_missing_one_is_gone() {
        assert_eq!(
            process_state(std::process::id()),
            Process::Running(own_start_time())
        );
        // Process ids are multiples of 4; no process has this one.
        assert_eq!(process_state(u32::MAX - 2), Process::Gone);
    }

    #[test]
    fn staleness() {
        assert!(stale(Process::Gone, Some(5)));
        assert!(stale(Process::Running(Some(6)), Some(5)), "a reused id");
        assert!(!stale(Process::Running(Some(5)), Some(5)));
        assert!(!stale(Process::Unknown, Some(5)), "doubt keeps it");
        assert!(!stale(Process::Running(Some(6)), None), "unreadable");
        assert!(!stale(Process::Running(Some(6)), Some(0)), "never recorded");
    }
}
