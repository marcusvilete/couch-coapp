//! Keeping a process's stdout for one writer (M16b).

use std::{fs::File, io, os::fd::AsFd};

/// Takes stdout for the caller alone: returns it as a file of its own, and
/// points fd 1 at stderr. A native library that prints to stdout (a VAAPI
/// driver logging, say) then lands in the log, not in what stdout carried.
pub fn take_stdout() -> io::Result<File> {
    let ours = rustix::io::dup(io::stdout().as_fd())?;
    rustix::stdio::dup2_stdout(io::stderr().as_fd())?;
    Ok(File::from(ours))
}
