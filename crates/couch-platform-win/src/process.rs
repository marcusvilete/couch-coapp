//! Doing things to other processes, for test tools: find a process's
//! children, freeze it, close its windows, send it Ctrl+Break.
//! `couch-cli release-check` uses these to stage a viewer that dies, hangs
//! or is closed, and a host that is stopped.

use std::{io, mem::size_of};

use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM},
    System::{
        Console::GenerateConsoleCtrlEvent,
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        Threading::{
            OpenProcess, OpenThread, PROCESS_TERMINATE, ResumeThread, SuspendThread,
            THREAD_SUSPEND_RESUME, TerminateProcess,
        },
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
    },
};
use windows::core::BOOL;

/// For `std::os::windows::process::CommandExt::creation_flags`: the child
/// gets its own process group, so [`ctrl_break`] can reach it alone.
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CTRL_BREAK_EVENT: u32 = 1;

fn os(e: windows::core::Error) -> io::Error {
    io::Error::other(e.to_string())
}

/// Closes a handle when dropped.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// The ids of `pid`'s direct children.
pub fn child_processes(pid: u32) -> io::Result<Vec<u32>> {
    // SAFETY: a snapshot of all processes; the handle is closed by `Handle`.
    let snapshot = Handle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.map_err(os)?);
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut children = Vec::new();
    // SAFETY: `entry` is a live, correctly sized out-parameter.
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_ok();
    while more {
        if entry.th32ParentProcessID == pid {
            children.push(entry.th32ProcessID);
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot.0, &mut entry) }.is_ok();
    }
    Ok(children)
}

/// Ends `pid` at once, as a crash would.
pub fn terminate(pid: u32) -> io::Result<()> {
    // SAFETY: opening a process by id; the handle is owned by `Handle`.
    let process = Handle(unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) }.map_err(os)?);
    // SAFETY: a valid handle with terminate rights.
    unsafe { TerminateProcess(process.0, 1) }.map_err(os)
}

/// A process with every thread suspended; they resume when this is dropped.
pub struct Frozen {
    threads: Vec<Handle>,
}

impl Drop for Frozen {
    fn drop(&mut self) {
        for thread in &self.threads {
            // SAFETY: a thread handle opened with THREAD_SUSPEND_RESUME.
            unsafe { ResumeThread(thread.0) };
        }
    }
}

/// Suspends every thread of `pid`, as if it hung.
pub fn freeze(pid: u32) -> io::Result<Frozen> {
    // SAFETY: a snapshot of all threads; the handle is closed by `Handle`.
    let snapshot = Handle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }.map_err(os)?);
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut frozen = Frozen {
        threads: Vec::new(),
    };
    // SAFETY: `entry` is a live, correctly sized out-parameter.
    let mut more = unsafe { Thread32First(snapshot.0, &mut entry) }.is_ok();
    while more {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: opening a thread by id; the handle is owned by `Handle`.
            if let Ok(thread) =
                unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) }
            {
                let thread = Handle(thread);
                // SAFETY: a valid handle with suspend rights.
                if unsafe { SuspendThread(thread.0) } != u32::MAX {
                    frozen.threads.push(thread);
                }
            }
        }
        // SAFETY: as above.
        more = unsafe { Thread32Next(snapshot.0, &mut entry) }.is_ok();
    }
    if frozen.threads.is_empty() {
        return Err(io::Error::other(format!("no threads of process {pid}")));
    }
    Ok(frozen)
}

/// Posts WM_CLOSE to `pid`'s visible top-level windows, as their close
/// button would. Returns how many it closed.
pub fn close_windows(pid: u32) -> usize {
    struct Search {
        pid: u32,
        found: Vec<HWND>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `Search` below, alive for the whole call.
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut owner = 0u32;
        // SAFETY: `owner` is a live out-parameter.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
        // SAFETY: any handle value is accepted.
        if owner == search.pid && unsafe { IsWindowVisible(hwnd) }.as_bool() {
            search.found.push(hwnd);
        }
        BOOL(1)
    }
    let mut search = Search {
        pid,
        found: Vec::new(),
    };
    // SAFETY: the callback only touches `search`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize)) };
    search
        .found
        .iter()
        // SAFETY: posting a message never dereferences anything of ours.
        .filter(|&&hwnd| {
            unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_ok()
        })
        .count()
}

/// Sends Ctrl+Break to a process group: a child started with
/// [`CREATE_NEW_PROCESS_GROUP`], whose group id is its process id, and that
/// shares this process's console.
pub fn ctrl_break(process_group: u32) -> io::Result<()> {
    // SAFETY: no pointers.
    unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, process_group) }.map_err(os)
}
