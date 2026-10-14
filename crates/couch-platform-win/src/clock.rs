//! QPC, the clock frames are stamped with on Windows. Windows.Graphics.Capture
//! timestamps (`SystemRelativeTime`) are on the same timeline, so capture,
//! present and barcode times from any process on the machine compare directly
//! (though capture's run ahead of delivery: `capture` takes the earlier).

use std::sync::OnceLock;

use couch_media::{Micros, clock::Clock};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

#[derive(Debug, Clone, Copy, Default)]
pub struct QpcClock;

fn frequency() -> u64 {
    static FREQUENCY: OnceLock<u64> = OnceLock::new();
    *FREQUENCY.get_or_init(|| {
        let mut f = 0i64;
        // Cannot fail on any Windows version that runs this app.
        unsafe { QueryPerformanceFrequency(&mut f) }.expect("QueryPerformanceFrequency");
        f as u64
    })
}

/// Microseconds on the QPC timeline.
pub fn now() -> Micros {
    let mut counter = 0i64;
    unsafe { QueryPerformanceCounter(&mut counter) }.expect("QueryPerformanceCounter");
    Micros((counter as u128 * 1_000_000 / u128::from(frequency())) as u64)
}

/// Converts a WinRT `SystemRelativeTime` (100 ns units on the QPC timeline).
pub fn from_system_relative_time(hundred_ns: i64) -> Micros {
    Micros((hundred_ns.max(0) / 10) as u64)
}

impl Clock for QpcClock {
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
}
