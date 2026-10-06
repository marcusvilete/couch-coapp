//! The host's side of a session: one capture feeding an encoder per viewer,
//! one sound capture sent to every viewer, and one seat router taking every
//! viewer's input.
//!
//! Threads and tasks:
//! - **capture**, a thread that runs while anyone watches. It hands each
//!   frame to every viewer's latest-frame mailbox, so a slow viewer never
//!   holds up another.
//! - **an encoder thread per viewer**, with its own encoder, rate controller
//!   and epoch streams.
//! - **sound**, a thread that runs with the capture: one Opus encoder, every
//!   packet to every viewer.
//! - **input**, a task: the seat router, the injectors and the virtual pad.
//!   However it stops, it releases everything first.
//! - **housekeeping**, a task: once a second, each viewer's share of the
//!   upload cap, and whether a window nobody watches still exists.

use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{LineWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use couch_codec_opus::OpusEncoder;
use couch_input::{
    KbmInjector, MemberId, Permissions, Rumble, SeatEpoch, SeatId, VirtualPad,
    router::{self, Effect, Input, SeatRouter, SeatStatus},
};
use couch_media::{
    AudioSource, BgraFrame, CaptureEvent, Codec, EncoderConfig, EncoderInput, FeedbackSnapshot,
    MediaError, Micros, VideoEncoder, VideoParams, VideoSender,
    audio::Framer,
    clock::Clock,
    mailbox::Mailbox,
    ratecontrol::{RateAction, RateConfig, RateController, UploadShare},
};
use couch_net::{
    Connection,
    audio::send_audio,
    datagrams::Datagrams,
    input::{InputReceiver, send_rumble},
    streams::Streams,
    video::HostVideo,
};
use couch_proto::{
    input::RumbleDatagram,
    media::{AudioDatagram, reset_code},
};
use tokio::{
    sync::{mpsc, oneshot},
    task::AbortHandle,
    time::MissedTickBehavior,
};

use super::{AudioChoice, AudioKind, Backend, Focus, NativeId, Source};

/// Single player: one seat.
const SEAT: SeatId = SeatId(0);
/// How often the dead-man timer, the foreground check and rumble run.
const INPUT_TICK: Duration = Duration::from_millis(10);
/// A running rumble goes out again this often.
const RUMBLE_REPEAT: Duration = Duration::from_millis(500);
/// Each viewer's rate controller runs this often.
const RATE_TICK: Duration = Duration::from_millis(50);
/// Upload shares and the window check run this often.
const HOUSEKEEPING: Duration = Duration::from_secs(1);
/// How long the game's sound waits for the capture to find its window.
const WINDOW_WAIT: Duration = Duration::from_secs(10);
/// No share of an upload cap.
const NO_SHARE: u32 = u32::MAX;

/// Bitrate bounds for each viewer's rate controller, bit/s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rates {
    pub min_bps: u32,
    pub start_bps: u32,
    pub max_bps: u32,
    /// The ceiling while a viewer's path goes through a relay server.
    pub relay_max_bps: u32,
}

impl Default for Rates {
    fn default() -> Self {
        Self {
            min_bps: 1_500_000,
            start_bps: 10_000_000,
            max_bps: 20_000_000,
            relay_max_bps: 2_000_000,
        }
    }
}

/// A trace writer's clock for the netem link (`couch-cli host --netem`).
pub type NetemElapsed = Arc<dyn Fn() -> Option<Duration> + Send + Sync>;

#[derive(Clone)]
pub struct HostOptions {
    pub source: Source,
    pub fps: u32,
    pub rates: Rates,
    /// The host's upload cap, split between its viewers.
    pub upload_bps: Option<u32>,
    pub audio: AudioChoice,
    /// Plug the pad in with a real Xbox 360 controller's ids.
    pub real_pad_ids: bool,
    /// Named in every epoch preamble; viewers refuse other sessions' video.
    pub session: u64,
    /// The seat's epochs count on from here: random per session.
    pub first_epoch: SeatEpoch,
    /// One CSV of rate-controller ticks per viewer: the first viewer's at
    /// this path, the next ones numbered (`couch-cli host --rc-trace`).
    pub rc_trace: Option<PathBuf>,
    /// The netem link's age, for the trace's `netem_us` column.
    pub netem_elapsed: Option<NetemElapsed>,
}

/// What a host stream reports.
#[derive(Debug, Clone)]
pub enum HostEvent {
    /// The router handed the seat to `member`: tell it (`SeatAssigned`).
    Granted {
        member: MemberId,
        seat: SeatId,
        epoch: SeatEpoch,
        permissions: Permissions,
    },
    /// The router took the seat back from `member`: tell it
    /// (`SeatReleased`).
    Revoked {
        member: MemberId,
        seat: SeatId,
        epoch: SeatEpoch,
    },
    /// The seat's status changed, at `at` on the host's clock.
    Seat { status: SeatStatus, at: Micros },
    /// A viewer left; whatever it held was released at `at`.
    ViewerGone { member: MemberId, at: Micros },
    /// Stopping: everything was released at `at`.
    Released { at: Micros },
    /// No stream for this viewer after all: its encoder failed.
    ViewerFailed { member: MemberId, error: String },
    /// One viewer's last second.
    Stats(ViewerStats),
    /// The game set its rumble (`couch-cli host` prints it).
    Rumble(Rumble),
    /// The captured window or screen went away.
    SourceClosed,
    /// The capture failed: the stream can't go on.
    CaptureFailed(String),
    /// Something worth a line in a log.
    Note(String),
}

/// One viewer's last second.
#[derive(Debug, Clone)]
pub struct ViewerStats {
    pub member: MemberId,
    pub fps: f64,
    pub bps: f64,
    /// The bitrate the encoder runs at.
    pub applied_bps: u32,
    pub keyframes: u32,
    pub catch_ups: u32,
    pub pauses: u32,
    pub feedback: FeedbackSnapshot,
    /// Sound packets sent to every viewer in that second.
    pub audio_packets: u64,
    /// The slowest encode of the second, microseconds.
    pub encode_us_max: u32,
}

/// One session's stream, to however many viewers. Dropping it stops
/// everything and releases all input; [`HostStream::stop`] does it in order
/// and waits.
pub struct HostStream {
    shared: Arc<Shared>,
    input: mpsc::UnboundedSender<InputCmd>,
    input_task: Option<tokio::task::JoinHandle<()>>,
    housekeeping: AbortHandle,
}

struct Shared {
    backend: Arc<dyn Backend>,
    clock: Arc<dyn Clock>,
    options: HostOptions,
    events: mpsc::UnboundedSender<HostEvent>,
    /// The captured window: from the source, or once the capture finds it.
    window: AtomicIsize,
    state: Mutex<State>,
    /// Every viewer's mailbox and flags, for the capture thread.
    feeds: Mutex<Vec<Arc<Feed>>>,
    /// Every viewer's connection, for the sound.
    audio_to: Mutex<Vec<Connection>>,
    audio_sent: AtomicU64,
    /// Set by the panic key: grants are refused until the host allows
    /// control again.
    grants_paused: Arc<AtomicBool>,
}

#[derive(Default)]
struct State {
    viewers: BTreeMap<MemberId, Viewer>,
    /// The capture and sound threads, while anyone watches.
    running: Option<Running>,
    share: Option<UploadShare<MemberId>>,
    /// Viewers so far, to number their traces.
    traces: u32,
    /// Threads told to stop that may still be finishing: removed viewers'
    /// encoders, a capture nobody watches any more.
    retired: Vec<JoinHandle<()>>,
    /// The window was found gone while nobody watched.
    source_gone: bool,
    stopped: bool,
}

struct Viewer {
    feed: Arc<Feed>,
    thread: Option<JoinHandle<()>>,
}

/// What the capture thread and a viewer's encoder thread share.
struct Feed {
    frames: Mailbox<BgraFrame>,
    keyframe: AtomicBool,
    stop: AtomicBool,
    /// How its epoch stream ends: `VIEWER_REMOVED` or `SESSION_END`.
    end_code: AtomicU32,
    /// Its controller's target, for the upload share.
    target: AtomicU32,
    /// Its share of the upload cap, or [`NO_SHARE`].
    share: AtomicU32,
}

impl Feed {
    fn new(start_bps: u32) -> Self {
        Self {
            frames: Mailbox::new(),
            keyframe: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            end_code: AtomicU32::new(reset_code::VIEWER_REMOVED),
            target: AtomicU32::new(start_bps),
            share: AtomicU32::new(NO_SHARE),
        }
    }

    fn share(&self) -> Option<u32> {
        match self.share.load(Ordering::Relaxed) {
            NO_SHARE => None,
            share => Some(share),
        }
    }

    fn finish(&self, code: u32) {
        self.end_code.store(code, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        self.frames.close();
    }
}

struct Running {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn now(&self) -> Micros {
        self.clock.now()
    }

    fn event(&self, event: HostEvent) {
        let _ = self.events.send(event);
    }

    /// Whoever runs the stream decides how to show notes; the log only
    /// keeps them at debug level.
    fn note(&self, text: String) {
        tracing::debug!("{text}");
        self.event(HostEvent::Note(text));
    }
}

impl HostStream {
    /// Plugs in the virtual pad (which can take seconds) and starts the
    /// input task. Capture and sound start with the first viewer.
    pub async fn start(
        backend: Arc<dyn Backend>,
        options: HostOptions,
    ) -> (Self, mpsc::UnboundedReceiver<HostEvent>) {
        let (events, events_rx) = mpsc::unbounded_channel();
        let pad = {
            let backend = backend.clone();
            let real_ids = options.real_pad_ids;
            tokio::task::spawn_blocking(move || backend.plug_pad(real_ids)).await
        };
        let window = match options.source {
            Source::Window(id) => id,
            _ => 0,
        };
        let share = options.upload_bps.map(UploadShare::new);
        let shared = Arc::new(Shared {
            clock: backend.clock(),
            backend,
            options,
            events,
            window: AtomicIsize::new(window),
            state: Mutex::new(State {
                share,
                ..State::default()
            }),
            feeds: Mutex::default(),
            audio_to: Mutex::default(),
            audio_sent: AtomicU64::new(0),
            grants_paused: Arc::new(AtomicBool::new(false)),
        });
        let pad = match pad {
            Ok(Ok(pad)) => {
                shared.note("virtual Xbox 360 pad plugged in".into());
                Some(pad)
            }
            Ok(Err(e)) => {
                shared.note(format!(
                    "no virtual pad ({e}); gamepad input will be dropped"
                ));
                None
            }
            Err(e) => {
                shared.note(format!("no virtual pad ({e})"));
                None
            }
        };
        let (input, cmds) = mpsc::unbounded_channel();
        let input_task = tokio::spawn(input_loop(shared.clone(), pad, cmds));
        let housekeeping = tokio::spawn(housekeeping(Arc::downgrade(&shared))).abort_handle();
        (
            Self {
                shared,
                input,
                input_task: Some(input_task),
                housekeeping,
            },
            events_rx,
        )
    }

    /// The captured window, once known.
    pub fn window(&self) -> Option<NativeId> {
        match self.shared.window.load(Ordering::Acquire) {
            0 => None,
            window => Some(window),
        }
    }

    /// Starts streaming to `member` on `conn`, whose datagram reader and
    /// stream acceptor these are. Call inside a tokio runtime.
    pub fn add_viewer(
        &self,
        member: MemberId,
        conn: Connection,
        datagrams: &Datagrams,
        streams: &Streams,
    ) {
        let sh = &self.shared;
        let mut state = lock(&sh.state);
        if state.stopped || state.viewers.contains_key(&member) {
            return;
        }
        let feed = Arc::new(Feed::new(sh.options.rates.start_bps));
        let trace = sh
            .options
            .rc_trace
            .as_deref()
            .map(|path| numbered(path, state.traces));
        state.traces += 1;
        let ctx = EncodeCtx {
            member,
            video: HostVideo::start(
                conn.clone(),
                sh.options.session,
                datagrams,
                sh.clock.clone(),
            ),
            feed: feed.clone(),
            shared: sh.clone(),
            trace,
        };
        let thread = std::thread::Builder::new()
            .name("encode".into())
            .spawn(move || encode_loop(ctx));
        let thread = match thread {
            Ok(thread) => thread,
            Err(e) => {
                sh.event(HostEvent::ViewerFailed {
                    member,
                    error: e.to_string(),
                });
                return;
            }
        };
        state.viewers.insert(
            member,
            Viewer {
                feed: feed.clone(),
                thread: Some(thread),
            },
        );
        lock(&sh.feeds).push(feed);
        lock(&sh.audio_to).push(conn.clone());
        let _ = self.input.send(InputCmd::AddViewer {
            member,
            inputs: InputReceiver::start(datagrams, streams),
            conn,
        });
        reshare(&mut state);
        if state.running.is_none() {
            state.running = Some(start_capture(sh));
        }
    }

    /// Stops streaming to `member`, releasing what it held first. Its epoch
    /// stream is reset with `VIEWER_REMOVED`.
    pub fn remove_viewer(&self, member: MemberId) {
        let _ = self.input.send(InputCmd::RemoveViewer(member));
        let sh = &self.shared;
        let mut state = lock(&sh.state);
        let Some(viewer) = state.viewers.remove(&member) else {
            return;
        };
        viewer.feed.finish(reset_code::VIEWER_REMOVED);
        // Its thread ends within a tick; `stop` waits for it if it hasn't.
        state.retired.extend(viewer.thread);
        lock(&sh.feeds).retain(|f| !Arc::ptr_eq(f, &viewer.feed));
        lock(&sh.audio_to).retain(|c| c.remote_id().as_bytes() != &member.0);
        if state.viewers.is_empty()
            && let Some(running) = state.running.take()
        {
            // Nobody watches: capture and sound stop.
            running.stop.store(true, Ordering::SeqCst);
            state.retired.extend(running.threads);
        }
        reshare(&mut state);
    }

    /// `member`'s decoder needs a keyframe.
    pub fn keyframe(&self, member: MemberId) {
        if let Some(viewer) = lock(&self.shared.state).viewers.get(&member) {
            viewer.feed.keyframe.store(true, Ordering::SeqCst);
        }
    }

    /// Hands the seat to `member`, ending any other hold first.
    pub fn grant(&self, member: MemberId, permissions: Permissions) {
        let _ = self.input.send(InputCmd::Grant {
            member,
            permissions,
        });
    }

    /// The host plays again: from `holder` only if it still holds the seat,
    /// or from whoever does with `None`.
    pub fn revoke(&self, holder: Option<MemberId>) {
        let _ = self.input.send(InputCmd::Revoke(holder));
    }

    /// Every seat back to the host at once.
    pub fn revoke_all(&self) {
        let _ = self.input.send(InputCmd::RevokeAll);
    }

    /// The panic key: every seat back to the host, and no grant goes through
    /// until [`HostStream::allow_grants`], even one already on its way.
    pub fn panic(&self) {
        self.panic_handle().panic();
    }

    /// Control may be handed out again after the panic key.
    pub fn allow_grants(&self) {
        self.shared.grants_paused.store(false, Ordering::SeqCst);
    }

    /// For the panic key's hook: [`HostStream::panic`] without going through
    /// whoever owns the stream.
    pub fn panic_handle(&self) -> PanicHandle {
        PanicHandle {
            input: self.input.clone(),
            paused: self.shared.grants_paused.clone(),
        }
    }

    /// Stops in order: releases all input, ends every viewer's stream with
    /// `SESSION_END`, stops encoding, capture and sound, then unplugs the
    /// pad.
    pub async fn stop(mut self) {
        let (ack, released) = oneshot::channel();
        if self.input.send(InputCmd::ReleaseAll(ack)).is_ok() {
            let _ = released.await;
        }
        let threads = self.halt();
        let _ = tokio::task::spawn_blocking(move || {
            for thread in threads {
                let _ = thread.join();
            }
        })
        .await;
        let (ack, unplugged) = oneshot::channel();
        if self.input.send(InputCmd::Shutdown(ack)).is_ok() {
            let _ = unplugged.await;
        }
        if let Some(task) = self.input_task.take() {
            let _ = task.await;
        }
    }

    /// Ends every viewer's stream and stops the threads, returning them to
    /// wait for.
    fn halt(&self) -> Vec<JoinHandle<()>> {
        self.housekeeping.abort();
        let mut state = lock(&self.shared.state);
        state.stopped = true;
        let mut threads = std::mem::take(&mut state.retired);
        for (_, mut viewer) in std::mem::take(&mut state.viewers) {
            viewer.feed.finish(reset_code::SESSION_END);
            threads.extend(viewer.thread.take());
        }
        if let Some(running) = state.running.take() {
            running.stop.store(true, Ordering::SeqCst);
            threads.extend(running.threads);
        }
        lock(&self.shared.feeds).clear();
        lock(&self.shared.audio_to).clear();
        threads
    }
}

impl Drop for HostStream {
    fn drop(&mut self) {
        // The input task releases everything as it ends. Told explicitly: a
        // panic handle may still hold its channel open. The threads stop on
        // their own.
        let (ack, _) = oneshot::channel();
        let _ = self.input.send(InputCmd::Shutdown(ack));
        drop(self.halt());
    }
}

/// The panic key for a stream: see [`HostStream::panic_handle`].
#[derive(Clone)]
pub struct PanicHandle {
    input: mpsc::UnboundedSender<InputCmd>,
    paused: Arc<AtomicBool>,
}

impl PanicHandle {
    /// Every seat back to the host; grants refused until allowed again. The
    /// flag is set first, so a grant already queued is refused too.
    pub fn panic(&self) {
        self.paused.store(true, Ordering::SeqCst);
        let _ = self.input.send(InputCmd::RevokeAll);
    }
}

/// `path` for the first viewer; `name-2.csv` and so on after it.
fn numbered(path: &Path, n: u32) -> PathBuf {
    if n == 0 {
        return path.to_path_buf();
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let name = match path.extension() {
        Some(ext) => format!("{stem}-{}.{}", n + 1, ext.to_string_lossy()),
        None => format!("{stem}-{}", n + 1),
    };
    path.with_file_name(name)
}

/// Recomputes every viewer's share of the upload cap, if there is one.
fn reshare(state: &mut State) {
    let Some(share) = state.share.as_mut() else {
        return;
    };
    let targets: Vec<(MemberId, u32)> = state
        .viewers
        .iter()
        .map(|(member, v)| (*member, v.feed.target.load(Ordering::Relaxed)))
        .collect();
    for (member, bps) in share.update(&targets) {
        if let Some(v) = state.viewers.get(&member) {
            v.feed.share.store(bps, Ordering::Relaxed);
        }
    }
}

async fn housekeeping(shared: Weak<Shared>) {
    let mut tick = tokio::time::interval(HOUSEKEEPING);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let Some(sh) = shared.upgrade() else {
            return;
        };
        let mut state = lock(&sh.state);
        reshare(&mut state);
        state.retired.retain(|t| !t.is_finished());
        // While capturing, the capture notices a closed window itself.
        if let Source::Window(window) = sh.options.source
            && state.running.is_none()
            && !state.source_gone
            && !sh.backend.window_exists(window)
        {
            state.source_gone = true;
            sh.event(HostEvent::SourceClosed);
        }
    }
}

// ---- Capture and sound ----

fn start_capture(shared: &Arc<Shared>) -> Running {
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    let (sh, halt) = (shared.clone(), stop.clone());
    match std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || capture_loop(&sh, &halt))
    {
        Ok(thread) => threads.push(thread),
        Err(e) => shared.event(HostEvent::CaptureFailed(e.to_string())),
    }
    if shared.options.audio != AudioChoice::Off {
        let (sh, halt) = (shared.clone(), stop.clone());
        match std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || audio_loop(&sh, &halt))
        {
            Ok(thread) => threads.push(thread),
            Err(e) => shared.note(format!("no audio thread: {e}")),
        }
    }
    Running { stop, threads }
}

fn capture_loop(shared: &Shared, stop: &AtomicBool) {
    let options = &shared.options;
    let (mut capturer, window) = match shared.backend.capture(&options.source, options.fps) {
        Ok(capture) => capture,
        Err(e) => {
            shared.event(HostEvent::CaptureFailed(e.to_string()));
            return;
        }
    };
    if let Some(window) = window {
        shared.window.store(window, Ordering::Release);
    }
    while !stop.load(Ordering::SeqCst) {
        match capturer.next_event(Duration::from_millis(10)) {
            Ok(Some(CaptureEvent::Frame(frame))) => {
                let frame = frame.cropped_to_even();
                for feed in lock(&shared.feeds).iter() {
                    feed.frames.put(frame.clone());
                }
            }
            Ok(Some(CaptureEvent::SourceClosed)) => {
                shared.event(HostEvent::SourceClosed);
                return;
            }
            Ok(None) => {}
            Err(e) => {
                shared.event(HostEvent::CaptureFailed(e.to_string()));
                return;
            }
        }
    }
}

fn audio_loop(shared: &Shared, stop: &AtomicBool) {
    let result = audio_kind(shared, stop)
        .and_then(|kind| shared.backend.audio(kind))
        .and_then(|source| send_sound(source, shared, stop));
    if let Err(e) = result {
        shared.note(format!("audio stopped: {e}"));
    }
}

/// Which sound `--audio` and the source mean.
fn audio_kind(shared: &Shared, stop: &AtomicBool) -> Result<AudioKind, MediaError> {
    let source = &shared.options.source;
    Ok(match shared.options.audio {
        AudioChoice::Game => AudioKind::Game(wait_for_window(shared, stop)?),
        AudioChoice::Auto if source.is_window() => AudioKind::Game(wait_for_window(shared, stop)?),
        AudioChoice::Auto if matches!(source, Source::TestPattern { .. }) => AudioKind::Tone,
        AudioChoice::Auto | AudioChoice::System => AudioKind::System,
        AudioChoice::Tone => AudioKind::Tone,
        AudioChoice::Off => return Err(MediaError::Closed),
    })
}

/// The captured window, which the capture thread publishes once it has it.
fn wait_for_window(shared: &Shared, stop: &AtomicBool) -> Result<NativeId, MediaError> {
    let started = Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) {
            return Err(MediaError::Closed);
        }
        match shared.window.load(Ordering::Acquire) {
            0 if started.elapsed() < WINDOW_WAIT => {
                std::thread::sleep(Duration::from_millis(50));
            }
            0 => {
                return Err(MediaError::Backend(
                    "no game window to take the sound from".into(),
                ));
            }
            window => return Ok(window),
        }
    }
}

fn send_sound(
    mut source: Box<dyn AudioSource>,
    shared: &Shared,
    stop: &AtomicBool,
) -> Result<(), MediaError> {
    let mut framer = Framer::new();
    let mut encoder = OpusEncoder::new()?;
    let mut frames = Vec::new();
    let mut seq = 0u32;
    while !stop.load(Ordering::SeqCst) {
        match source.next_chunk(Duration::from_millis(100)) {
            Ok(Some(chunk)) => framer.push(&chunk, &mut frames)?,
            Ok(None) => continue,
            Err(MediaError::Closed) => return Ok(()),
            Err(e) => return Err(e),
        }
        for frame in frames.drain(..) {
            let packet = AudioDatagram {
                seq,
                capture_us: frame.captured_at.0,
                packet: encoder.encode(&frame.samples)?.to_vec(),
            };
            for conn in lock(&shared.audio_to).iter() {
                send_audio(conn, &packet);
            }
            seq = seq.wrapping_add(1);
            shared.audio_sent.fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(())
}

// ---- One viewer's video ----

struct EncodeCtx {
    member: MemberId,
    video: HostVideo,
    feed: Arc<Feed>,
    shared: Arc<Shared>,
    trace: Option<PathBuf>,
}

fn encode_loop(ctx: EncodeCtx) {
    let EncodeCtx {
        member,
        mut video,
        feed,
        shared,
        trace,
    } = ctx;
    match encode(member, &mut video, &feed, &shared, trace.as_deref()) {
        // The connection went: the viewer is gone, nothing failed.
        Ok(()) | Err(MediaError::Closed) => {}
        Err(e) => shared.event(HostEvent::ViewerFailed {
            member,
            error: e.to_string(),
        }),
    }
    video.end(feed.end_code.load(Ordering::SeqCst));
}

fn encode(
    member: MemberId,
    video: &mut HostVideo,
    feed: &Feed,
    shared: &Shared,
    trace: Option<&Path>,
) -> Result<(), MediaError> {
    let options = &shared.options;
    let mut trace = trace.map(RcTrace::create).transpose()?;
    let mut encoder: Option<Box<dyn VideoEncoder>> = None;
    let mut controller: Option<RateController> = None;
    let mut params: Option<VideoParams> = None;
    let mut force_keyframe = true;
    let mut last_tick = Instant::now();
    let mut stats = PeriodStats::new();

    while !feed.stop.load(Ordering::SeqCst) {
        if let (Some(c), Some(enc), Some(p)) = (controller.as_mut(), encoder.as_mut(), params)
            && last_tick.elapsed() >= RATE_TICK
        {
            last_tick = Instant::now();
            c.set_share(feed.share());
            let fb = video.feedback();
            let action = c.tick(last_tick, &fb);
            feed.target.store(c.target_bps(), Ordering::Relaxed);
            if let Some(trace) = trace.as_mut() {
                let netem = options.netem_elapsed.as_ref().and_then(|f| f());
                trace.write(shared.now(), &fb, c, action, netem)?;
            }
            match action {
                RateAction::Keep => {}
                RateAction::SetBitrate(bps) => enc.set_bitrate(bps)?,
                RateAction::CatchUp { bitrate } => {
                    enc.set_bitrate(bitrate)?;
                    video.start_epoch(&p)?;
                    force_keyframe = true;
                    stats.catch_ups += 1;
                }
                RateAction::Pause => {
                    // Drop what is queued; nothing more until reports return.
                    video.start_epoch(&p)?;
                    stats.pauses += 1;
                    shared.note("viewer silent: paused".into());
                }
                RateAction::Resume { bitrate } => {
                    enc.set_bitrate(bitrate)?;
                    force_keyframe = true;
                    shared.note(format!(
                        "viewer back: resuming at {:.1} Mbit/s",
                        f64::from(bitrate) / 1e6
                    ));
                }
            }
            stats.last_feedback = Some(fb);
        }

        let frame = match feed.frames.take_timeout(Duration::from_millis(10)) {
            Ok(Some(frame)) => frame,
            Ok(None) => continue,
            Err(_closed) => break,
        };

        // Nothing goes out while the viewer is silent, not even a new size:
        // the first frame after resuming reopens the encoder if it changed.
        if controller.as_ref().is_some_and(RateController::is_paused) {
            continue;
        }

        let size_changed =
            params.is_none_or(|p| (p.width, p.height) != (frame.width, frame.height));
        if size_changed {
            let r = options.rates;
            let share = feed.share();
            let start = controller
                .as_ref()
                .map_or(r.start_bps, |c| c.applied_bps())
                .min(share.unwrap_or(u32::MAX));
            let mut rate = RateConfig {
                min_bps: r.min_bps,
                max_bps: r.max_bps,
                relay_max_bps: r.relay_max_bps,
                start_bps: start,
                keyframe_costly: false,
            };
            // The encoder must start where the controller does.
            let start = rate.clamped_start();
            let enc = shared.backend.encoder(EncoderConfig {
                codec: Codec::H264,
                width: frame.width,
                height: frame.height,
                fps: options.fps,
                bitrate_bps: start,
            })?;
            shared.note(format!(
                "encoding {}x{} with {} at {:.1} Mbit/s",
                frame.width,
                frame.height,
                enc.caps().name,
                f64::from(start) / 1e6
            ));
            rate.keyframe_costly = enc.caps().bitrate_change_costs_keyframe;
            let mut c = RateController::new(rate);
            c.set_share(share);
            controller = Some(c);
            let p = VideoParams {
                codec: Codec::H264,
                width: frame.width,
                height: frame.height,
                fps: options.fps,
            };
            video.start_epoch(&p)?;
            params = Some(p);
            encoder = Some(enc);
            force_keyframe = true;
        }

        let enc = encoder.as_mut().expect("opened above");
        let key = std::mem::take(&mut force_keyframe) | feed.keyframe.swap(false, Ordering::SeqCst);
        for unit in enc.encode(EncoderInput::Cpu(&frame), key)? {
            stats.bytes += unit.data.len() as u64;
            stats.keyframes += u32::from(unit.keyframe);
            stats.encode_us_max = stats.encode_us_max.max(unit.encode_time_us);
            video.send_frame(&unit)?;
        }
        stats.frames += 1;

        if stats.started.elapsed() >= Duration::from_secs(1) {
            let secs = stats.started.elapsed().as_secs_f64();
            shared.event(HostEvent::Stats(ViewerStats {
                member,
                fps: f64::from(stats.frames) / secs,
                bps: stats.bytes as f64 * 8.0 / secs,
                applied_bps: controller.as_ref().map_or(0, RateController::applied_bps),
                keyframes: stats.keyframes,
                catch_ups: stats.catch_ups,
                pauses: stats.pauses,
                feedback: stats.last_feedback.unwrap_or_default(),
                audio_packets: shared.audio_sent.swap(0, Ordering::Relaxed),
                encode_us_max: stats.encode_us_max,
            }));
            stats = PeriodStats::new();
        }
    }
    Ok(())
}

struct PeriodStats {
    started: Instant,
    frames: u32,
    bytes: u64,
    keyframes: u32,
    encode_us_max: u32,
    catch_ups: u32,
    pauses: u32,
    last_feedback: Option<FeedbackSnapshot>,
}

impl PeriodStats {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            frames: 0,
            bytes: 0,
            keyframes: 0,
            encode_us_max: 0,
            catch_ups: 0,
            pauses: 0,
            last_feedback: None,
        }
    }
}

/// `--rc-trace`: one CSV row per controller tick. Times are on the host's
/// clock (QPC microseconds), like the viewer's `--timings-csv`, so the two
/// files line up. `netem_us` is the time since the netem link started
/// (empty without `--netem`), which places its scheduled changes. Written
/// line by line: `latency-probe` ends the host by killing it.
struct RcTrace(LineWriter<File>);

impl RcTrace {
    fn create(path: &Path) -> Result<Self, MediaError> {
        let io = |e: std::io::Error| MediaError::Backend(format!("{}: {e}", path.display()));
        let mut w = LineWriter::new(File::create(path).map_err(io)?);
        writeln!(
            w,
            "qpc_us,applied_bps,target_bps,delivery_bps,send_bps,outstanding_bytes,rtt_us,min_rtt_us,delay_trend_us,\
             sent_packets,lost_packets,congestion_events,cwnd_bytes,report_age_ms,relayed,netem_us,action"
        )
        .map_err(io)?;
        Ok(Self(w))
    }

    fn write(
        &mut self,
        now: Micros,
        fb: &FeedbackSnapshot,
        c: &RateController,
        action: RateAction,
        netem: Option<Duration>,
    ) -> Result<(), MediaError> {
        let action = match action {
            RateAction::Keep => String::new(),
            RateAction::SetBitrate(bps) => format!("set {bps}"),
            RateAction::CatchUp { bitrate } => format!("catch-up {bitrate}"),
            RateAction::Pause => "pause".into(),
            RateAction::Resume { bitrate } => format!("resume {bitrate}"),
        };
        let netem = netem.map_or(String::new(), |d| d.as_micros().to_string());
        writeln!(
            self.0,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            now.0,
            c.applied_bps(),
            c.target_bps(),
            fb.delivery_rate_bps,
            fb.send_rate_bps,
            fb.outstanding_bytes,
            fb.rtt_us,
            fb.min_rtt_us,
            fb.delay_trend_us,
            fb.sent_packets,
            fb.lost_packets,
            fb.congestion_events,
            fb.cwnd_bytes,
            fb.report_age_ms,
            u8::from(fb.relayed),
            netem,
            action
        )
        .map_err(|e| MediaError::Backend(format!("writing the trace: {e}")))
    }
}

// ---- Input ----

enum InputCmd {
    AddViewer {
        member: MemberId,
        inputs: InputReceiver,
        conn: Connection,
    },
    RemoveViewer(MemberId),
    Grant {
        member: MemberId,
        permissions: Permissions,
    },
    /// Only from this holder, or from whoever holds the seat.
    Revoke(Option<MemberId>),
    RevokeAll,
    /// Release everything, then answer: the first step of stopping.
    ReleaseAll(oneshot::Sender<()>),
    /// Unplug the pad and end, then answer: the last step.
    Shutdown(oneshot::Sender<()>),
}

/// The input task's state: the router, what it injects into, and where
/// rumble goes.
struct Inputs {
    shared: Arc<Shared>,
    router: SeatRouter,
    kbm: Box<dyn KbmInjector>,
    pads: Vec<Box<dyn VirtualPad>>,
    /// Each viewer's forwarding task, and its connection (for rumble).
    viewers: HashMap<MemberId, (tokio::task::JoinHandle<()>, Connection)>,
    status: Option<SeatStatus>,
    /// The game's current rumble, and what last went out, to whom, when.
    rumble: Rumble,
    rumble_sent: (Option<MemberId>, Rumble, Instant),
}

impl Inputs {
    /// Updates the router's view of where keyboard and mouse would land.
    /// Called right before every input, not just on the tick: a window that
    /// just took the foreground must not get a key meant for the game.
    fn refresh_focus(&mut self) -> Vec<Effect> {
        let focus = match self.shared.window.load(Ordering::Acquire) {
            0 => Focus::default(),
            window => self.shared.backend.focus(window),
        };
        self.router.set_pointer_inside(focus.pointer_inside);
        self.router.set_foreground(focus.foreground)
    }

    /// Injects the effects and reports grants, revocations and status
    /// changes.
    fn deliver(&mut self, effects: Vec<Effect>) {
        if let Err(e) = router::apply(&effects, &mut *self.kbm, &mut self.pads) {
            tracing::warn!("injection failed: {e}");
        }
        for effect in effects {
            match effect {
                Effect::Granted {
                    seat,
                    epoch,
                    member,
                    permissions,
                } => self.shared.event(HostEvent::Granted {
                    member,
                    seat,
                    epoch,
                    permissions,
                }),
                Effect::Revoked {
                    seat,
                    epoch,
                    member,
                } => self.shared.event(HostEvent::Revoked {
                    member,
                    seat,
                    epoch,
                }),
                _ => {}
            }
        }
        let status = self.router.seat(SEAT).map(|s| s.2);
        if status != self.status {
            self.status = status;
            if let Some(status) = status {
                let at = self.shared.now();
                self.shared.event(HostEvent::Seat { status, at });
            }
        }
    }

    fn tick(&mut self, now: Instant) {
        let mut effects = self.router.tick(now);
        effects.extend(self.refresh_focus());
        self.deliver(effects);
        self.send_rumble(now);
    }

    /// Rumble goes only to the seat's holder. A running rumble is repeated,
    /// so a lost "stop" can't leave a controller shaking: the viewer ends
    /// each one after a second.
    fn send_rumble(&mut self, now: Instant) {
        for pad in &mut self.pads {
            if let Some(r) = pad.take_rumble() {
                self.rumble = r;
                self.shared.event(HostEvent::Rumble(r));
            }
        }
        let holder = self.router.seat(SEAT).and_then(|s| s.1);
        let Some(holder) = holder else {
            self.rumble_sent = (None, Rumble::default(), now);
            return;
        };
        let (to, sent, at) = self.rumble_sent;
        let repeat = self.rumble != Rumble::default() && now.duration_since(at) >= RUMBLE_REPEAT;
        if to == Some(holder) && sent == self.rumble && !repeat {
            return;
        }
        if let Some((_, conn)) = self.viewers.get(&holder) {
            send_rumble(
                conn,
                RumbleDatagram {
                    seat: SEAT,
                    low: self.rumble.low_frequency,
                    high: self.rumble.high_frequency,
                },
            );
        }
        self.rumble_sent = (Some(holder), self.rumble, now);
    }
}

async fn input_loop(
    shared: Arc<Shared>,
    pad: Option<Box<dyn VirtualPad>>,
    mut cmds: mpsc::UnboundedReceiver<InputCmd>,
) {
    let (tx, mut inputs) = mpsc::channel::<(MemberId, Input)>(1024);
    let mut st = Inputs {
        kbm: shared.backend.kbm(),
        router: SeatRouter::starting_at(1, shared.options.first_epoch),
        pads: pad.into_iter().collect(),
        viewers: HashMap::new(),
        status: None,
        rumble: Rumble::default(),
        rumble_sent: (None, Rumble::default(), Instant::now()),
        shared,
    };
    st.status = st.router.seat(SEAT).map(|s| s.2);
    let mut tick = tokio::time::interval(INPUT_TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut done = None;

    loop {
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                Some(InputCmd::AddViewer { member, inputs: mut receiver, conn }) => {
                    let tx = tx.clone();
                    let task = tokio::spawn(async move {
                        while let Some(input) = receiver.inputs.recv().await {
                            if tx.send((member, input)).await.is_err() {
                                return;
                            }
                        }
                    });
                    if let Some((old, _)) = st.viewers.insert(member, (task, conn)) {
                        old.abort();
                    }
                }
                Some(InputCmd::RemoveViewer(member)) => {
                    if let Some((task, _)) = st.viewers.remove(&member) {
                        task.abort();
                    }
                    let effects = st.router.disconnected(member);
                    st.deliver(effects);
                    let at = st.shared.now();
                    st.shared.event(HostEvent::ViewerGone { member, at });
                }
                Some(InputCmd::Grant { member, permissions }) => {
                    if st.shared.grants_paused.load(Ordering::SeqCst) {
                        tracing::info!(member = ?member, "panic key: a grant on its way was refused");
                    } else {
                        let effects = st.router.grant(SEAT, member, permissions, Instant::now());
                        st.deliver(effects);
                    }
                }
                Some(InputCmd::Revoke(holder)) => {
                    let current = st.router.seat(SEAT).and_then(|s| s.1);
                    if holder.is_none_or(|h| current == Some(h)) {
                        let effects = st.router.revoke(SEAT);
                        st.deliver(effects);
                    }
                }
                Some(InputCmd::RevokeAll) => {
                    let effects = st.router.revoke_all();
                    st.deliver(effects);
                }
                Some(InputCmd::ReleaseAll(ack)) => {
                    let effects = st.router.revoke_all();
                    st.deliver(effects);
                    let at = st.shared.now();
                    st.shared.event(HostEvent::Released { at });
                    let _ = ack.send(());
                }
                Some(InputCmd::Shutdown(ack)) => {
                    done = Some(ack);
                    break;
                }
                // The stream was dropped without stopping.
                None => break,
            },
            Some((member, input)) = inputs.recv() => {
                let mut effects = st.refresh_focus();
                effects.extend(st.router.handle(member, input, Instant::now()));
                st.deliver(effects);
            }
            _ = tick.tick() => st.tick(Instant::now()),
        }
    }

    // However the loop ended: release everything, then unplug the pad.
    let effects = st.router.revoke_all();
    st.deliver(effects);
    for (_, (task, _)) in st.viewers.drain() {
        task.abort();
    }
    drop(st);
    if let Some(ack) = done {
        let _ = ack.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traces_are_numbered_after_the_first() {
        let path = Path::new("target/rc.csv");
        assert_eq!(numbered(path, 0), Path::new("target/rc.csv"));
        assert_eq!(numbered(path, 1), Path::new("target/rc-2.csv"));
        assert_eq!(numbered(Path::new("trace"), 2), Path::new("trace-3"));
    }
}
