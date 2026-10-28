//! `CLOCK_MONOTONIC`, the clock frames are timed on on Linux: one timeline
//! for every process since boot, as QPC is on Windows, so the main process and
//! the stream window compare their times directly.

use couch_media::{Micros, clock::Clock};
use rustix::time::{ClockId, clock_gettime};

#[derive(Debug, Clone, Copy, Default)]
pub struct MonotonicClock;

/// Microseconds on the `CLOCK_MONOTONIC` timeline.
pub fn now() -> Micros {
    let t = clock_gettime(ClockId::Monotonic);
    Micros(t.tv_sec as u64 * 1_000_000 + t.tv_nsec as u64 / 1_000)
}

impl Clock for MonotonicClock {
    fn now(&self) -> Micros {
        now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advances_monotonically() {
        let a = now();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = now();
        assert!(b.0 >= a.0 + 4_000, "{a:?} -> {b:?}");
    }

    /// Another process's reading falls between two of ours: one timeline for
    /// every process, as the stream window and the main process need.
    #[test]
    fn is_shared_with_other_processes() {
        let before = now();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "clock::tests::print_now",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .unwrap();
        let after = now();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let theirs: u64 = stdout
            .lines()
            .find_map(|l| l.strip_prefix("NOW "))
            .unwrap_or_else(|| panic!("no reading from the child: {stdout}"))
            .trim()
            .parse()
            .unwrap();
        assert!(
            (before.0..=after.0).contains(&theirs),
            "{theirs} not within {before:?}..{after:?}"
        );
    }

    /// For `is_shared_with_other_processes`, run as a child process.
    #[test]
    #[ignore = "run by is_shared_with_other_processes in a process of its own"]
    fn print_now() {
        println!("NOW {}", now().0);
    }
}
