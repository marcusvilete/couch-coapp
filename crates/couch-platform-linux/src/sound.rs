//! A Linux host's sound (M16c, ADR 0012): what the PC plays, from the
//! default output's monitor through PipeWire, which converts it to the
//! stream's format (48 kHz stereo, 32-bit float).
//!
//! It is everything the PC plays, this app's own sounds included; leaving
//! those out, and taking one program's sound alone, come next (M16c-1's
//! fourth part). Until then the app makes no sound of its own while it
//! hosts on Linux.

use std::{
    sync::{Arc, Mutex, PoisonError, mpsc},
    thread::JoinHandle,
    time::Duration,
};

use couch_media::{AudioChunk, AudioSource, MediaError, Micros};
use pipewire as pw;
use pw::{
    properties::properties,
    spa::{
        self,
        param::{
            ParamType,
            audio::{AudioFormat, AudioInfoRaw},
        },
        pod::{Object, Pod, Value, serialize::PodSerializer},
        utils::{Direction, SpaTypes},
    },
    stream::{StreamFlags, StreamState},
};

/// The stream's format, which PipeWire converts to.
const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
/// How long starting may take: PipeWire answers at once where it runs.
const READY_WAIT: Duration = Duration::from_secs(3);
/// Chunks waiting for the pipeline before newer ones are dropped.
const QUEUE: usize = 64;

/// What this PC plays, until dropped.
pub struct SystemSound {
    chunks: mpsc::Receiver<AudioChunk>,
    quit: pw::channel::Sender<()>,
    error: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl SystemSound {
    /// Starts capturing; fails at once where PipeWire doesn't run.
    pub fn start() -> Result<Self, MediaError> {
        let (chunks_tx, chunks) = mpsc::sync_channel(QUEUE);
        let (ready_tx, ready) = mpsc::channel::<Result<(), String>>();
        let (quit, quit_rx) = pw::channel::channel::<()>();
        let error = Arc::new(Mutex::new(None));
        let thread_error = error.clone();
        let thread = std::thread::Builder::new()
            .name("pipewire-sound".into())
            .spawn(move || {
                if let Err(e) = capture(&chunks_tx, &ready_tx, &thread_error, quit_rx) {
                    let e = format!("PipeWire: {e}");
                    // Before it was ready, starting fails; after, the next
                    // read says why the sound stopped.
                    if ready_tx.send(Err(e.clone())).is_err() {
                        *thread_error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e);
                    }
                }
            })
            .map_err(|e| MediaError::Backend(format!("no thread for PipeWire: {e}")))?;
        let started = ready.recv_timeout(READY_WAIT);
        let sound = Self {
            chunks,
            quit,
            error,
            thread: Some(thread),
        };
        match started {
            Ok(Ok(())) => Ok(sound),
            Ok(Err(e)) => Err(MediaError::Backend(e)),
            Err(_) => Err(MediaError::Backend("PipeWire didn't answer".into())),
        }
    }
}

impl AudioSource for SystemSound {
    fn next_chunk(&mut self, timeout: Duration) -> Result<Option<AudioChunk>, MediaError> {
        match self.chunks.recv_timeout(timeout) {
            Ok(chunk) => Ok(Some(chunk)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self
                .error
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .map_or(MediaError::Closed, MediaError::Backend)),
        }
    }
}

impl Drop for SystemSound {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the stream's callbacks keep.
struct Sound {
    chunks: mpsc::SyncSender<AudioChunk>,
    error: Arc<Mutex<Option<String>>>,
    mainloop: pw::main_loop::MainLoopRc,
}

/// PipeWire's side, on its own thread, until `quit` or the stream fails.
fn capture(
    chunks: &mpsc::SyncSender<AudioChunk>,
    ready: &mpsc::Sender<Result<(), String>>,
    error: &Arc<Mutex<Option<String>>>,
    quit: pw::channel::Receiver<()>,
) -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let _quit = quit.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |()| mainloop.quit()
    });
    let stream = pw::stream::StreamBox::new(
        &core,
        "couch-coapp-sound",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Game",
            // The default output's monitor: what the PC plays.
            *pw::keys::STREAM_CAPTURE_SINK => "true",
            // 10 ms, the codec's frame.
            *pw::keys::NODE_LATENCY => "480/48000",
        },
    )?;
    let _listener = stream
        .add_local_listener_with_user_data(Sound {
            chunks: chunks.clone(),
            error: error.clone(),
            mainloop: mainloop.clone(),
        })
        .state_changed(|_, sound, _, state| {
            if let StreamState::Error(e) = state {
                *sound.error.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(format!("the sound capture failed: {e}"));
                sound.mainloop.quit();
            }
        })
        .process(|stream, sound| {
            if let Some(mut buffer) = stream.dequeue_buffer()
                && let Some(data) = buffer.datas_mut().first_mut()
            {
                let chunk = data.chunk();
                let (offset, size) = (chunk.offset() as usize, chunk.size() as usize);
                if let Some(bytes) = data.data().and_then(|b| b.get(offset..offset + size)) {
                    let samples = samples(bytes);
                    let frames = (samples.len() / usize::from(CHANNELS)) as u64;
                    // Stamped with its first sample's time.
                    let now = crate::clock::now().0;
                    let chunk = AudioChunk {
                        samples,
                        channels: CHANNELS,
                        sample_rate: RATE,
                        captured_at: Micros(
                            now.saturating_sub(frames * 1_000_000 / u64::from(RATE)),
                        ),
                    };
                    // A pipeline that fell behind loses the newest, as a full
                    // queue anywhere does.
                    let _ = sound.chunks.try_send(chunk);
                }
            }
        })
        .register()?;
    let params = format();
    let mut params = [Pod::from_bytes(&params).ok_or(pw::Error::CreationFailed)?];
    // On this thread's main loop, not PipeWire's realtime one: the callbacks
    // share their state, and 10 ms quanta need no more.
    stream.connect(
        Direction::Input,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    let _ = ready.send(Ok(()));
    mainloop.run();
    Ok(())
}

/// 48 kHz stereo, 32-bit float: PipeWire converts whatever plays to it.
fn format() -> Vec<u8> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(RATE);
    info.set_channels(u32::from(CHANNELS));
    let mut position = [0; spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    let object = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(object))
        .map(|(cursor, _)| cursor.into_inner())
        .unwrap_or_default()
}

/// Little-endian 32-bit floats; a trailing partial sample is left out.
fn samples(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&b| f32::from_le_bytes(b))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_are_read_little_endian() {
        let mut bytes = Vec::new();
        for v in [0.5f32, -1.0, 0.25] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes.push(7);
        assert_eq!(samples(&bytes), [0.5, -1.0, 0.25]);
    }

    #[test]
    fn the_format_serializes() {
        assert!(Pod::from_bytes(&format()).is_some());
    }

    /// Where PipeWire runs, with something playing (`linux-checks` plays a
    /// sine into a null output): chunks come in the stream's format, and
    /// carry it.
    #[test]
    #[ignore = "needs a running PipeWire, with something playing"]
    fn what_plays_is_captured() {
        let mut sound = SystemSound::start().expect("PipeWire runs");
        let mut peak = 0.0f32;
        for _ in 0..50 {
            if let Some(chunk) = sound.next_chunk(Duration::from_millis(100)).unwrap() {
                assert_eq!((chunk.sample_rate, chunk.channels), (RATE, CHANNELS));
                peak = chunk.samples.iter().fold(peak, |p, s| p.max(s.abs()));
                if peak > 0.1 {
                    return;
                }
            }
        }
        panic!("nothing heard within 5 s (peak {peak})");
    }
}
