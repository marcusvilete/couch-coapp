//! The clock frames are stamped with.
//!
//! Latency measurement needs a clock that two processes on one machine agree
//! on. The platform crates provide one (QPC on Windows); [`ProcessClock`] is
//! for tests and for code paths that only compare stamps within one process.

use std::time::Instant;

use crate::Micros;

/// Title of the latency clock window (`couch-viewer --clock`), which paints
/// the current time as a barcode for capture-to-present measurements.
pub const LATENCY_CLOCK_TITLE: &str = "Couch Co-App latency clock";

/// Barcode cell size and margin inside the latency clock window.
pub const LATENCY_CLOCK_CELL: u32 = 16;
pub const LATENCY_CLOCK_MARGIN: u32 = 16;
/// Height of the clock's millisecond digits.
pub const LATENCY_CLOCK_DIGIT_HEIGHT: u32 = 120;

/// The latency clock window's size: a barcode row over a row of digits.
pub const fn latency_clock_size() -> (u32, u32) {
    (
        LATENCY_CLOCK_MARGIN * 2 + crate::barcode::TOTAL_CELLS * LATENCY_CLOCK_CELL,
        LATENCY_CLOCK_MARGIN * 3 + LATENCY_CLOCK_CELL + LATENCY_CLOCK_DIGIT_HEIGHT,
    )
}

pub trait Clock: Send + Sync {
    fn now(&self) -> Micros;
}

/// Microseconds since this clock was created. Not comparable across processes.
#[derive(Debug, Clone, Copy)]
pub struct ProcessClock {
    origin: Instant,
}

impl ProcessClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for ProcessClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for ProcessClock {
    fn now(&self) -> Micros {
        Micros(self.origin.elapsed().as_micros() as u64)
    }
}
