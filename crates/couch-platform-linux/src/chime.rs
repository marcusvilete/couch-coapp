//! The chime when someone asks for a seat, played by this process on a Linux
//! host (M16c).
//!
//! On Windows the webview plays it (Web Audio, `chime.ts`). The AppImage's
//! WebKitGTK has no sound: it looks for GStreamer's plugins inside the
//! AppImage, which carries none, and the system's are for another GStreamer
//! than the one it carries. So here it's a short PipeWire playback stream of
//! the same two tones. It is this process's own, so the host's sound
//! capture leaves it out of the stream (`sound`, by process): guests never
//! hear it.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use pipewire as pw;
use pw::{properties::properties, spa::pod::Pod, spa::utils::Direction, stream::StreamFlags};

use crate::sound::{CHANNELS, RATE, format};

/// The two tones, as `chime.ts` has them: each starts `TONE_GAP` after the
/// last, swells in `ATTACK`, fades by `FADE`, and stops at `LENGTH`.
const TONES: [f32; 2] = [660.0, 880.0];
const LEAD: f32 = 0.02;
const TONE_GAP: f32 = 0.14;
const ATTACK: f32 = 0.015;
const FADE: f32 = 0.2;
const LENGTH: f32 = 0.22;
const PEAK: f32 = 0.25;
const FLOOR: f32 = 0.0001;
/// How long a chime may take, its stream's setting up included: past it, the
/// thread ends whatever PipeWire did.
const AT_MOST: Duration = Duration::from_secs(3);

/// One chime at a time: a second request while one plays doesn't stack.
static PLAYING: AtomicBool = AtomicBool::new(false);

/// Plays the chime on a thread of its own, and returns at once. Failures are
/// logged; it's only a convenience.
pub fn play() {
    if PLAYING.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("chime".into()).spawn(|| {
        // Let go however the thread ends, a panic in PipeWire's binding
        // included: else no chime would play again.
        let _done = Done;
        if let Err(e) = run() {
            tracing::debug!("the chime didn't play: {e}");
        }
    });
    if spawned.is_err() {
        PLAYING.store(false, Ordering::SeqCst);
    }
}

/// Clears [`PLAYING`] when dropped.
struct Done;

impl Drop for Done {
    fn drop(&mut self) {
        PLAYING.store(false, Ordering::SeqCst);
    }
}

/// What the stream's callback keeps: the samples, and how far it got.
struct Playing {
    samples: Vec<f32>,
    at: usize,
    mainloop: pw::main_loop::MainLoopRc,
}

fn run() -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "couch-coapp-chime",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Notification",
            *pw::keys::APP_NAME => "Couch Co-App",
        },
    )?;
    let frame = usize::from(CHANNELS) * size_of::<f32>();
    let _listener = stream
        .add_local_listener_with_user_data(Playing {
            samples: samples(),
            at: 0,
            mainloop: mainloop.clone(),
        })
        .process(move |stream, playing| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let left = playing.samples.len() - playing.at;
            let frames = match data.data() {
                Some(slice) => {
                    let frames = (slice.len() / frame).min(left / usize::from(CHANNELS));
                    let end = playing.at + frames * usize::from(CHANNELS);
                    for (bytes, sample) in slice
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(&playing.samples[playing.at..end])
                    {
                        *bytes = sample.to_le_bytes();
                    }
                    playing.at = end;
                    frames
                }
                None => 0,
            };
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = frame as _;
            *chunk.size_mut() = (frames * frame) as _;
            // The tail is silence already: ending with the last samples
            // queued loses none worth hearing.
            if playing.at >= playing.samples.len() {
                playing.mainloop.quit();
            }
        })
        .register()?;
    let params = format();
    let mut params = [Pod::from_bytes(&params).ok_or(pw::Error::CreationFailed)?];
    stream.connect(
        Direction::Output,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    let timer = mainloop.loop_().add_timer({
        let mainloop = mainloop.clone();
        move |_| mainloop.quit()
    });
    let _ = timer.update_timer(Some(AT_MOST), None);
    mainloop.run();
    let _ = stream.disconnect();
    Ok(())
}

/// The chime, interleaved stereo at [`RATE`], with a short silence after
/// so its last samples are played before the stream goes.
fn samples() -> Vec<f32> {
    let total = LEAD + TONE_GAP * (TONES.len() - 1) as f32 + LENGTH + 0.1;
    let frames = (total * RATE as f32) as usize;
    let mut out = Vec::with_capacity(frames * usize::from(CHANNELS));
    for n in 0..frames {
        let t = n as f32 / RATE as f32;
        let value: f32 = TONES
            .iter()
            .enumerate()
            .map(|(i, &frequency)| {
                let start = LEAD + TONE_GAP * i as f32;
                let local = t - start;
                if !(0.0..LENGTH).contains(&local) {
                    return 0.0;
                }
                (std::f32::consts::TAU * frequency * local).sin() * level(local)
            })
            .sum();
        out.extend(std::iter::repeat_n(value, usize::from(CHANNELS)));
    }
    out
}

/// Web Audio's exponential ramps: up to [`PEAK`] in [`ATTACK`], down to
/// [`FLOOR`] by [`FADE`].
fn level(t: f32) -> f32 {
    if t < ATTACK {
        FLOOR * (PEAK / FLOOR).powf(t / ATTACK)
    } else if t < FADE {
        PEAK * (FLOOR / PEAK).powf((t - ATTACK) / (FADE - ATTACK))
    } else {
        FLOOR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chime_is_two_short_tones_within_full_scale() {
        let samples = samples();
        let seconds = samples.len() as f32 / f32::from(CHANNELS) / RATE as f32;
        assert!((0.4..0.6).contains(&seconds), "{seconds} s");
        let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((0.2..=0.5).contains(&peak), "peak {peak}");
        // Silent before the first tone and after the last.
        let frame = |t: f32| samples[(t * RATE as f32) as usize * usize::from(CHANNELS)];
        assert_eq!(frame(0.01), 0.0);
        assert_eq!(frame(seconds - 0.05), 0.0);
        // Both channels alike.
        assert!(samples.as_chunks::<2>().0.iter().all(|[l, r]| l == r));
    }

    #[test]
    fn the_level_swells_then_fades() {
        assert!(level(0.0) < 0.001);
        assert!((level(ATTACK) - PEAK).abs() < 0.01);
        assert!(level(0.1) < PEAK && level(0.1) > level(0.19));
        assert_eq!(level(0.21), FLOOR);
    }
}
