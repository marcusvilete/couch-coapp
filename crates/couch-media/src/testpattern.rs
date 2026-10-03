//! A synthetic capture source: moving bars, a frame counter and a timestamp
//! [`barcode`](crate::barcode). Lets the whole pipeline run without a screen,
//! a GPU, or a game.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{BgraFrame, CaptureEvent, Capturer, MediaError, Micros, barcode, clock::Clock};

/// Barcode position and cell size used by the test pattern.
pub const BARCODE_X: u32 = 16;
pub const BARCODE_Y: u32 = 16;
pub const BARCODE_CELL: u32 = 16;

pub struct TestPattern {
    width: u32,
    height: u32,
    frame_interval: Duration,
    next_due: Instant,
    frame_index: u64,
    clock: Arc<dyn Clock>,
}

impl TestPattern {
    /// `width` must fit the barcode: at least `BARCODE_X + barcode::width(BARCODE_CELL)`.
    pub fn new(width: u32, height: u32, fps: u32, clock: Arc<dyn Clock>) -> Self {
        assert!(
            width >= BARCODE_X + barcode::width(BARCODE_CELL),
            "test pattern too narrow"
        );
        assert!(height >= BARCODE_Y + BARCODE_CELL, "test pattern too short");
        assert!(fps > 0);
        Self {
            width,
            height,
            frame_interval: Duration::from_secs(1) / fps,
            next_due: Instant::now(),
            frame_index: 0,
            clock,
        }
    }

    /// Renders the next frame immediately, stamped with the clock's current time.
    pub fn render(&mut self) -> BgraFrame {
        let captured_at = self.clock.now();
        let (w, h) = (self.width, self.height);
        let stride = w * 4;
        let mut data = vec![0u8; (stride * h) as usize];

        // Diagonal gradient background, so the encoder has texture to work on.
        for y in 0..h {
            let row = &mut data[(y * stride) as usize..((y + 1) * stride) as usize];
            for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let v = ((x as u32 + y) / 8 % 64 + 48) as u8;
                *px = [v, v / 2 + 64, 96, 255];
            }
        }

        // A vertical bar that sweeps across once every two seconds at 60 fps.
        let bar_width = (w / 24).max(8);
        let bar_x = (self.frame_index * u64::from(w) / 120 % u64::from(w)) as u32;
        for y in 0..h {
            for x in bar_x..(bar_x + bar_width).min(w) {
                let i = (y * stride + x * 4) as usize;
                data[i..i + 4].copy_from_slice(&[40, 200, 240, 255]);
            }
        }

        barcode::draw(
            &mut data,
            stride,
            BARCODE_X,
            BARCODE_Y,
            BARCODE_CELL,
            captured_at.0 & barcode::MAX_VALUE,
        );

        self.frame_index += 1;
        BgraFrame {
            width: w,
            height: h,
            stride,
            data: data.into(),
            captured_at,
        }
    }

    pub fn frames_rendered(&self) -> u64 {
        self.frame_index
    }
}

impl Capturer for TestPattern {
    fn next_event(&mut self, timeout: Duration) -> Result<Option<CaptureEvent>, MediaError> {
        let now = Instant::now();
        if self.next_due > now {
            let wait = self.next_due - now;
            if wait > timeout {
                std::thread::sleep(timeout);
                return Ok(None);
            }
            std::thread::sleep(wait);
        }
        // Skip missed slots instead of bursting to catch up, like a real display.
        let now = Instant::now();
        let floor = now.checked_sub(self.frame_interval).unwrap_or(now);
        self.next_due = self.next_due.max(floor) + self.frame_interval;
        Ok(Some(CaptureEvent::Frame(self.render())))
    }
}

impl std::fmt::Debug for TestPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestPattern")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("frame_index", &self.frame_index)
            .finish_non_exhaustive()
    }
}

/// Reads the capture timestamp back out of a test-pattern frame.
pub fn read_timestamp(frame: &BgraFrame) -> Option<Micros> {
    barcode::read(frame, BARCODE_X, BARCODE_Y, BARCODE_CELL).map(Micros)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ProcessClock;

    #[test]
    fn frames_carry_their_capture_time() {
        let mut pattern = TestPattern::new(1280, 720, 60, Arc::new(ProcessClock::new()));
        let frame = pattern.render();
        assert_eq!(
            (frame.width, frame.height, frame.stride),
            (1280, 720, 1280 * 4)
        );
        assert_eq!(read_timestamp(&frame), Some(frame.captured_at));
    }

    #[test]
    fn capturer_paces_to_the_frame_rate() {
        let mut pattern = TestPattern::new(1280, 720, 100, Arc::new(ProcessClock::new()));
        let start = Instant::now();
        for _ in 0..5 {
            let event = pattern.next_event(Duration::from_secs(1)).unwrap();
            assert!(matches!(event, Some(CaptureEvent::Frame(_))));
        }
        // Five frames at 100 fps: the first is immediate, then four 10 ms waits.
        assert!(start.elapsed() >= Duration::from_millis(35));
        assert_eq!(pattern.frames_rendered(), 5);
    }
}
