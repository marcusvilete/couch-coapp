//! Windows timer resolution. The default system tick (up to 15.6 ms) makes
//! every sleep and timeout that coarse, including tokio's timers.

use windows::Win32::Media::timeBeginPeriod;

/// Asks for 1 ms timer resolution for the rest of the process's life.
pub fn request_1ms_resolution() {
    // TIMERR_NOERROR is 0; failure only means coarser timers.
    if unsafe { timeBeginPeriod(1) } != 0 {
        tracing::warn!("could not raise the timer resolution to 1 ms");
    }
}
