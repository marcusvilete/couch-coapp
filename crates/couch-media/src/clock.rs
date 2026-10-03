//! The clock frames are stamped with.
//!
//! Latency measurement needs a clock that two processes on one machine agree
//! on. The platform crates provide one (QPC on Windows); [`ProcessClock`] is
//! for tests and for code paths that only compare stamps within one process.

use std::time::Instant;

use crate::Micros;

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
