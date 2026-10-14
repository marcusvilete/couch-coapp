//! `couch-viewer --clock`: a borderless window that paints the current QPC
//! time every vsync, as a barcode (for `couch-cli latency-probe`) and as
//! digits (for side-by-side screenshots). With `--clicks` it also clicks
//! twice a second (for `latency-probe --audio`); with `--fps N` it paints at
//! most N times a second, like a game capped at that rate (M9d: a game a
//! little faster than the stream). Esc closes it.

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use couch_media::{
    audio::{CHANNELS, ClickTrack, SAMPLE_RATE},
    barcode,
    clock::{
        LATENCY_CLOCK_CELL, LATENCY_CLOCK_DIGIT_HEIGHT, LATENCY_CLOCK_MARGIN, LATENCY_CLOCK_TITLE,
        latency_clock_size,
    },
};
use sdl3::{
    audio::{AudioCallback, AudioFormat, AudioSpec, AudioStream, AudioStreamWithCallback},
    event::Event,
    keyboard::Keycode,
    pixels::Color,
    render::FRect,
};

use crate::now_us;

/// Barcode position and cell size inside the clock window.
pub const MARGIN: u32 = LATENCY_CLOCK_MARGIN;
pub const CELL: u32 = LATENCY_CLOCK_CELL;

const DIGIT_W: f32 = 64.0;
const DIGIT_H: f32 = LATENCY_CLOCK_DIGIT_HEIGHT as f32;
const DIGIT_GAP: f32 = 20.0;
const SEGMENT: f32 = 12.0;
const DIGITS: usize = 8;

/// `clicks`: also play the click track. `fps`: paint at most this often.
pub fn run(x: i32, y: i32, clicks: bool, fps: Option<u32>) -> Result<()> {
    sdl3::hint::set("SDL_RENDER_VSYNC", "1");
    let sdl = sdl3::init().map_err(|e| anyhow!("SDL init: {e}"))?;
    let video = sdl.video().map_err(|e| anyhow!("SDL video: {e}"))?;
    let (width, height) = latency_clock_size();
    let window = video
        .window(LATENCY_CLOCK_TITLE, width, height)
        .position(x, y)
        .borderless()
        .build()?;
    let mut canvas = window.into_canvas();
    let mut events = sdl.event_pump().map_err(|e| anyhow!("SDL events: {e}"))?;
    // Clicks for the audio latency measurement. The clock still works
    // without an audio device.
    let _clicks = if clicks {
        match ClickStream::open(&sdl) {
            Ok(stream) => Some(stream),
            Err(e) => {
                tracing::warn!("no clicks: {e:#}");
                None
            }
        }
    } else {
        None
    };

    let interval = fps.map(|fps| Duration::from_secs(1) / fps.max(1));
    let mut next_frame = Instant::now();
    loop {
        for event in events.poll_iter() {
            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => return Ok(()),
                _ => {}
            }
        }

        // Capped: wait for the frame's time, on a schedule that doesn't
        // drift (and doesn't catch up after a stall).
        if let Some(interval) = interval {
            let now = Instant::now();
            if next_frame > now {
                std::thread::sleep(next_frame - now);
            }
            next_frame = (next_frame + interval).max(Instant::now());
        }

        let now = now_us();
        canvas.set_draw_color(Color::RGB(36, 36, 44));
        canvas.clear();

        for (i, white) in (0u32..).zip(barcode::cells(now.0 & barcode::MAX_VALUE)) {
            let shade = if white { 255 } else { 0 };
            canvas.set_draw_color(Color::RGB(shade, shade, shade));
            canvas.fill_rect(FRect::new(
                (MARGIN + i * CELL) as f32,
                MARGIN as f32,
                CELL as f32,
                CELL as f32,
            ))?;
        }

        // Milliseconds, last eight digits.
        let ms = (now.0 / 1000) % 10u64.pow(DIGITS as u32);
        canvas.set_draw_color(Color::RGB(120, 230, 140));
        let top = (MARGIN * 2 + CELL) as f32;
        for (i, digit) in format!("{ms:0width$}", width = DIGITS).bytes().enumerate() {
            let left = MARGIN as f32 + i as f32 * (DIGIT_W + DIGIT_GAP);
            for rect in segments(digit - b'0', left, top) {
                canvas.fill_rect(rect)?;
            }
        }
        canvas.present();
    }
}

/// Plays [`ClickTrack`] on SDL's audio thread.
struct Clicks {
    track: ClickTrack,
    buf: Vec<f32>,
}

impl AudioCallback<f32> for Clicks {
    fn callback(&mut self, stream: &mut AudioStream, requested: i32) {
        self.buf
            .resize(usize::try_from(requested).unwrap_or(0), 0.0);
        self.track.fill(&mut self.buf);
        let _ = stream.put_data_f32(&self.buf);
    }
}

/// The playing click stream. Stops its callbacks before the SDL wrapper
/// frees them (see `audio::AudioOut`'s Drop).
struct ClickStream(AudioStreamWithCallback<Clicks>);

impl ClickStream {
    fn open(sdl: &sdl3::Sdl) -> Result<Self> {
        let audio = sdl.audio().map_err(|e| anyhow!("SDL audio: {e}"))?;
        let spec = AudioSpec::new(
            Some(SAMPLE_RATE as i32),
            Some(i32::from(CHANNELS)),
            Some(AudioFormat::f32_sys()),
        );
        let stream = audio
            .open_playback_stream(
                &spec,
                Clicks {
                    track: ClickTrack::new(),
                    buf: Vec::new(),
                },
            )
            .map_err(|e| anyhow!("opening audio playback: {e}"))?;
        stream
            .resume()
            .map_err(|e| anyhow!("starting clicks: {e}"))?;
        Ok(Self(stream))
    }
}

impl Drop for ClickStream {
    fn drop(&mut self) {
        let _ = self.0.pause();
        drop(self.0.lock());
    }
}

/// Seven-segment rectangles for one digit.
fn segments(digit: u8, x: f32, y: f32) -> impl Iterator<Item = FRect> {
    // Bits: a b c d e f g (top, top-right, bottom-right, bottom, bottom-left, top-left, middle).
    const MAP: [u8; 10] = [
        0b1111110, 0b0110000, 0b1101101, 0b1111001, 0b0110011, 0b1011011, 0b1011111, 0b1110000,
        0b1111111, 0b1111011,
    ];
    let (w, h, s) = (DIGIT_W, DIGIT_H, SEGMENT);
    let half = (h - s) / 2.0;
    let rects = [
        FRect::new(x, y, w, s),                       // a
        FRect::new(x + w - s, y, s, half + s),        // b
        FRect::new(x + w - s, y + half, s, half + s), // c
        FRect::new(x, y + h - s, w, s),               // d
        FRect::new(x, y + half, s, half + s),         // e
        FRect::new(x, y, s, half + s),                // f
        FRect::new(x, y + half, w, s),                // g
    ];
    let bits = MAP[usize::from(digit % 10)];
    rects
        .into_iter()
        .enumerate()
        .filter(move |(i, _)| bits & (0b1000000 >> i) != 0)
        .map(|(_, r)| r)
}
