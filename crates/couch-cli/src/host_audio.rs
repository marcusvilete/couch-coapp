//! The host's audio: capture the game (or the system, or the test tone),
//! encode it with Opus and send one datagram per 10 ms.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::Context;
use couch_codec_opus::OpusEncoder;
use couch_media::{AudioSource, MediaError, audio::Framer, audio::ToneSource};
use couch_net::{Connection, audio::send_audio};
use couch_platform_win::{
    audio::{LoopbackCapture, LoopbackTarget},
    clock::QpcClock,
    foreground::{WindowId, window_process},
};
use couch_proto::media::AudioDatagram;

/// `host --audio`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioChoice {
    /// The game for a window source, everything but us for the monitor, the
    /// test tone for the test pattern.
    Auto,
    /// The captured window's process and its children.
    Game,
    /// Everything this PC plays, except this app.
    System,
    /// The click track.
    Tone,
    Off,
}

pub struct Options {
    pub choice: AudioChoice,
    /// What the video source is, for `Auto`.
    pub video: crate::host::Source,
    /// The captured window, once the capture thread has it.
    pub game_window: Arc<AtomicIsize>,
}

/// Starts the audio thread, unless audio is off. It stops when `stop` is set
/// or the connection goes.
pub fn spawn(
    conn: Connection,
    options: Options,
    stop: Arc<AtomicBool>,
    sent: Arc<AtomicU64>,
) -> Option<JoinHandle<()>> {
    let choice = match (options.choice, &options.video) {
        (AudioChoice::Off, _) => return None,
        (AudioChoice::Auto, crate::host::Source::Window(_)) => AudioChoice::Game,
        (AudioChoice::Auto, crate::host::Source::Monitor) => AudioChoice::System,
        (AudioChoice::Auto, crate::host::Source::TestPattern { .. }) => AudioChoice::Tone,
        (choice, _) => choice,
    };
    let thread = std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            let result = open_source(choice, &options.game_window, &stop)
                .and_then(|source| run(source, &conn, &stop, &sent));
            if let Err(e) = result {
                println!("audio stopped: {e:#}");
            }
        })
        .expect("spawning the audio thread");
    Some(thread)
}

fn open_source(
    choice: AudioChoice,
    game_window: &AtomicIsize,
    stop: &AtomicBool,
) -> anyhow::Result<Box<dyn AudioSource>> {
    Ok(match choice {
        AudioChoice::Game => {
            // The capture thread publishes the window once it has found it.
            let started = Instant::now();
            let window = loop {
                anyhow::ensure!(!stop.load(Ordering::SeqCst), "stopped");
                match game_window.load(Ordering::Acquire) {
                    0 if started.elapsed() < Duration::from_secs(10) => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    0 => anyhow::bail!("no game window to take the audio from"),
                    w => break WindowId(w),
                }
            };
            let pid = window_process(window).context("the game window is gone")?;
            let capture = LoopbackCapture::start(LoopbackTarget::ProcessTree(pid))
                .context("capturing the game's audio")?;
            println!("audio: process {pid} and its children");
            Box::new(capture)
        }
        AudioChoice::System => {
            let capture = LoopbackCapture::start(LoopbackTarget::AllExcept(std::process::id()))
                .context("capturing the system's audio")?;
            println!("audio: everything this PC plays, except this app");
            Box::new(capture)
        }
        AudioChoice::Tone => {
            println!("audio: test clicks");
            Box::new(ToneSource::new(Arc::new(QpcClock)))
        }
        AudioChoice::Auto | AudioChoice::Off => unreachable!("resolved in spawn"),
    })
}

fn run(
    mut source: Box<dyn AudioSource>,
    conn: &Connection,
    stop: &AtomicBool,
    sent: &AtomicU64,
) -> anyhow::Result<()> {
    let mut framer = Framer::new();
    let mut encoder = OpusEncoder::new()?;
    let mut frames = Vec::new();
    let mut seq = 0u32;
    while !stop.load(Ordering::SeqCst) {
        match source.next_chunk(Duration::from_millis(100)) {
            Ok(Some(chunk)) => framer.push(&chunk, &mut frames)?,
            Ok(None) => continue,
            Err(MediaError::Closed) => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        for frame in frames.drain(..) {
            let packet = AudioDatagram {
                seq,
                capture_us: frame.captured_at.0,
                packet: encoder.encode(&frame.samples)?.to_vec(),
            };
            if !send_audio(conn, &packet) {
                return Ok(());
            }
            seq = seq.wrapping_add(1);
            sent.fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(())
}
