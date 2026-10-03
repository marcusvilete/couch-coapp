//! Audio capture with WASAPI process loopback (Windows 10 2004 and later).
//!
//! Capturing the game's process tree, rather than everything the PC plays,
//! keeps the host's own notifications, voice chat and music out of the
//! stream. For a monitor source the capture takes everything except our own
//! processes instead.
//!
//! The capture runs on its own thread. Windows converts whatever the
//! processes play to the stream format (48 kHz stereo float), and stamps
//! each packet with QPC time, the clock video frames use.

use std::{
    mem::ManuallyDrop,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

use couch_media::{
    AudioChunk, AudioSource, MediaError, Micros,
    audio::{CHANNELS, SAMPLE_RATE, duration_us},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        Media::Audio::{
            AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
            AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
            ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
            IActivateAudioInterfaceCompletionHandler,
            IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
            PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX,
        },
        System::{
            Com::{
                BLOB, COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize,
                StructuredStorage::PROPVARIANT,
            },
            Threading::{CreateEventW, WaitForSingleObject},
            Variant::VT_BLOB,
        },
    },
    core::{HRESULT, IUnknown, Interface, implement},
};

use crate::clock;

/// Whose audio to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopbackTarget {
    /// A process and every process it started: the game.
    ProcessTree(u32),
    /// Everything except a process and its children: the system minus us.
    AllExcept(u32),
}

/// Chunks waiting for the consumer; beyond this the newest are dropped.
const QUEUE: usize = 64;
/// WASAPI's buffer. Packets come every 10 ms; this is slack for a slow thread.
const BUFFER_100NS: i64 = 200_000;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

pub struct LoopbackCapture {
    chunks: mpsc::Receiver<AudioChunk>,
    /// Why the capture thread ended, if it failed. Kept apart from the
    /// chunks, which may be a full queue when it happens.
    error: Arc<Mutex<Option<MediaError>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LoopbackCapture {
    /// Starts capturing. Fails if Windows refuses the loopback client (an
    /// older Windows, or no audio service).
    pub fn start(target: LoopbackTarget) -> Result<Self, MediaError> {
        let (tx, chunks) = mpsc::sync_channel(QUEUE);
        let (ready_tx, ready_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let thread = {
            let (stop, error) = (stop.clone(), error.clone());
            std::thread::Builder::new()
                .name("audio-capture".into())
                .spawn(move || capture_thread(target, &tx, &ready_tx, &stop, &error))
                .map_err(|e| MediaError::Backend(e.to_string()))?
        };
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                chunks,
                error,
                stop,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(MediaError::Backend("the audio capture thread died".into()))
            }
        }
    }
}

impl AudioSource for LoopbackCapture {
    fn next_chunk(&mut self, timeout: Duration) -> Result<Option<AudioChunk>, MediaError> {
        match self.chunks.recv_timeout(timeout) {
            Ok(chunk) => Ok(Some(chunk)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            // Everything captured has been read; say why it stopped.
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self
                .error
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .unwrap_or(MediaError::Closed)),
        }
    }
}

impl Drop for LoopbackCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn backend(context: &str) -> impl FnOnce(windows::core::Error) -> MediaError + '_ {
    move |e| MediaError::Backend(format!("{context}: {e}"))
}

fn capture_thread(
    target: LoopbackTarget,
    tx: &mpsc::SyncSender<AudioChunk>,
    ready: &mpsc::Sender<Result<(), MediaError>>,
    stop: &AtomicBool,
    error: &Mutex<Option<MediaError>>,
) {
    // SAFETY: initializes COM for this thread; balanced below.
    if let Err(e) = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok() {
        let _ = ready.send(Err(MediaError::Backend(format!("COM: {e}"))));
        return;
    }
    let stream = match Stream::open(target) {
        Ok(stream) => stream,
        Err(e) => {
            let _ = ready.send(Err(e));
            // SAFETY: pairs with the CoInitializeEx above.
            unsafe { CoUninitialize() };
            return;
        }
    };
    let _ = ready.send(Ok(()));
    if let Err(e) = stream.run(tx, stop) {
        *error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e);
    }
    drop(stream);
    // SAFETY: every COM object of this thread is released above.
    unsafe { CoUninitialize() };
}

/// An activated, started loopback client.
struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
}

impl Stream {
    fn open(target: LoopbackTarget) -> Result<Self, MediaError> {
        let client = activate(target)?;
        let channels = CHANNELS;
        let block = channels * 4;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
            nChannels: channels,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        // SAFETY: `format` outlives the call; the client was just activated.
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                BUFFER_100NS,
                0,
                &format,
                None,
            )
        }
        .map_err(backend("initializing the loopback client"))?;
        // SAFETY: plain event creation; closed in Drop.
        let event = unsafe { CreateEventW(None, false, false, None) }
            .map_err(backend("creating the capture event"))?;
        let stream = Self {
            capture: match unsafe { client.GetService::<IAudioCaptureClient>() } {
                Ok(capture) => capture,
                Err(e) => {
                    // SAFETY: the handle was created above and isn't used again.
                    unsafe {
                        let _ = CloseHandle(event);
                    }
                    return Err(backend("getting the capture service")(e));
                }
            },
            client,
            event,
        };
        // SAFETY: the event handle is valid for the stream's life.
        unsafe { stream.client.SetEventHandle(stream.event) }
            .map_err(backend("setting the capture event"))?;
        unsafe { stream.client.Start() }.map_err(backend("starting the capture"))?;
        Ok(stream)
    }

    fn run(&self, tx: &mpsc::SyncSender<AudioChunk>, stop: &AtomicBool) -> Result<(), MediaError> {
        let channels = usize::from(CHANNELS);
        while !stop.load(Ordering::SeqCst) {
            // Times out only if the engine stalls: process loopback delivers
            // silence while the target plays nothing.
            // SAFETY: the event handle is valid for the stream's life.
            if unsafe { WaitForSingleObject(self.event, 100) } != WAIT_OBJECT_0 {
                continue;
            }
            loop {
                // SAFETY: the capture client is started.
                let size = unsafe { self.capture.GetNextPacketSize() }
                    .map_err(backend("reading the capture"))?;
                if size == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                let mut qpc = 0u64;
                // SAFETY: the out-parameters are live locals; the buffer is
                // released below before the next GetBuffer.
                unsafe {
                    self.capture
                        .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))
                }
                .map_err(backend("reading the capture"))?;
                let len = frames as usize * channels;
                let samples = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null()
                {
                    vec![0.0; len]
                } else {
                    // SAFETY: WASAPI hands `frames` frames of the format we
                    // asked for: `channels` 32-bit floats each.
                    unsafe { std::slice::from_raw_parts(data.cast::<f32>(), len) }.to_vec()
                };
                // SAFETY: releases exactly what GetBuffer handed out.
                unsafe { self.capture.ReleaseBuffer(frames) }
                    .map_err(backend("reading the capture"))?;
                // QPC in 100 ns units, at the packet's first frame.
                let captured_at = if qpc != 0 {
                    Micros(qpc / 10)
                } else {
                    Micros(clock::now().0.saturating_sub(duration_us(len)))
                };
                let chunk = AudioChunk {
                    samples,
                    channels: CHANNELS,
                    sample_rate: SAMPLE_RATE,
                    captured_at,
                };
                match tx.try_send(chunk) {
                    Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                    Err(mpsc::TrySendError::Disconnected(_)) => return Ok(()),
                }
            }
        }
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: stopping a started client and closing our own event handle.
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// Signals a channel when the asynchronous activation completes.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Completion(mpsc::SyncSender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Completion_Impl {
    fn ActivateCompleted(
        &self,
        _operation: windows::core::Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let _ = self.0.try_send(());
        Ok(())
    }
}

fn activate(target: LoopbackTarget) -> Result<IAudioClient, MediaError> {
    let (pid, mode) = match target {
        LoopbackTarget::ProcessTree(pid) => {
            (pid, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE)
        }
        LoopbackTarget::AllExcept(pid) => (pid, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE),
    };
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: mode,
            },
        },
    };
    // PROPVARIANT's Drop calls PropVariantClear, which would hand our stack
    // copy of `params` to the COM allocator to free. Never drop it.
    let mut prop = ManuallyDrop::new(PROPVARIANT::default());
    // SAFETY: writes the blob variant of a zeroed PROPVARIANT. It points at
    // `params`, which outlives the activation below.
    unsafe {
        let inner = &mut *prop.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob = BLOB {
            cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            pBlobData: (&raw const params).cast_mut().cast(),
        };
    }
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let handler: IActivateAudioInterfaceCompletionHandler = Completion(done_tx).into();
    // SAFETY: the device path is a static string; `prop` and `handler` live
    // until the activation completes (we wait for it below).
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&raw const *prop),
            &handler,
        )
    }
    .map_err(backend("activating process loopback"))?;
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| MediaError::Backend("process loopback activation timed out".into()))?;
    let mut result = HRESULT(0);
    let mut client: Option<IUnknown> = None;
    // SAFETY: the operation has completed; both out-parameters are live locals.
    unsafe { operation.GetActivateResult(&mut result, &mut client) }
        .map_err(backend("activating process loopback"))?;
    result
        .ok()
        .map_err(backend("activating process loopback"))?;
    client
        .ok_or_else(|| MediaError::Backend("process loopback returned no client".into()))?
        .cast::<IAudioClient>()
        .map_err(backend("activating process loopback"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs the Windows audio service; a PC without one fails to activate.
    #[test]
    #[ignore = "needs the Windows audio service"]
    fn captures_the_system_minus_this_process() {
        let mut capture =
            LoopbackCapture::start(LoopbackTarget::AllExcept(std::process::id())).unwrap();
        // Whatever plays (or nothing): the call must not fail.
        let chunk = capture.next_chunk(Duration::from_millis(500)).unwrap();
        if let Some(chunk) = chunk {
            assert_eq!((chunk.channels, chunk.sample_rate), (2, 48_000));
            let age = clock::now().0.saturating_sub(chunk.captured_at.0);
            assert!(age < 1_000_000, "stamped on the QPC timeline: {age} µs old");
        }
    }
}
