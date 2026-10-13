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
//!
//! When the captured window goes, the capture (or housekeeping, while
//! nobody watches) looks for the game's next window for a grace period
//! ([`super::source`]): it follows a new one, ends with `GameExited` when the
//! process is gone, or with `SourceClosed` when the grace runs out.

use std::{
    any::Any,
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
    KbmInjector, MemberId, PadState, Permissions, Rumble, SeatEpoch, SeatId, VirtualPad,
    local::{ControllerIds, LocalPad},
    router::{self, Effect, Input, SeatRouter, SeatStatus},
};
use couch_media::{
    AudioSource, CaptureEvent, Codec, EncoderConfig, EncoderInput, FeedbackSnapshot, MediaError,
    Micros, VideoEncoder, VideoFrame, VideoParams, VideoSender,
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
    input::{PadDatagram, RumbleDatagram},
    media::{AudioDatagram, reset_code},
    session::SeatKind,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::AbortHandle,
    time::MissedTickBehavior,
};

use super::{
    AudioChoice, AudioKind, Backend, Focus, GameProcess, NativeId, Source,
    source::{self, Game, SourceWatch, Verdict},
};

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
/// Changes of the game window's focus closer together than this are
/// reported once, as the latest: switching windows mustn't flood viewers.
const FOCUS_COALESCE: Duration = Duration::from_millis(250);

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
    /// The seats, by seat id; one virtual pad is plugged in for each that
    /// has one, in seat order.
    pub seats: Vec<SeatKind>,
    /// Plug the pads in with a real Xbox 360 controller's ids.
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
    /// How long a game may take to show a new window before the stream ends
    /// ([`source::REPLACE_GRACE`]; shorter in tests).
    pub replace_grace: Duration,
    /// The host's own controller, played through a seat.
    pub local: Option<LocalSeat>,
}

/// The host's own controller, read on this PC and played through a seat like
/// a viewer's: its input goes through the seat router as `member`'s, which
/// [`HostStream::claim`] and [`HostStream::grant`] give a seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalSeat {
    /// The host's own id, as the router's holder.
    pub member: MemberId,
    pub controller: ControllerIds,
    /// Hide it from games first, or they would see it beside its seat's pad.
    pub hide: bool,
}

/// What a host stream reports.
#[derive(Debug, Clone)]
pub enum HostEvent {
    /// The router handed `seat` to `member`: tell it (`SeatAssigned`).
    Granted {
        member: MemberId,
        seat: SeatId,
        epoch: SeatEpoch,
        permissions: Permissions,
    },
    /// The router took `seat` back from `member`: tell it (`SeatReleased`).
    Revoked {
        member: MemberId,
        seat: SeatId,
        epoch: SeatEpoch,
    },
    /// A seat's status changed, at `at` on the host's clock.
    Seat {
        seat: SeatId,
        status: SeatStatus,
        at: Micros,
    },
    /// The streamed window came to the front on the host, or left it.
    /// Changes within [`FOCUS_COALESCE`] are reported once, as the latest;
    /// never reported for a screen.
    GameFocus(bool),
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
    /// The captured window or screen went away, and no new window of the
    /// game took its place within the grace.
    SourceClosed,
    /// The game's process exited.
    GameExited,
    /// The game replaced its window, and the capture follows the new one.
    SourceFollowed { window: NativeId, at: Micros },
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
    /// Whether the host's controller plays through a seat, or why not.
    local: Option<Result<(), String>>,
    /// The network tuned for streaming while live (Wi-Fi media streaming
    /// mode, M9a); undone when this drops.
    _network: Option<Box<dyn std::any::Any + Send>>,
}

struct Shared {
    backend: Arc<dyn Backend>,
    clock: Arc<dyn Clock>,
    options: HostOptions,
    events: mpsc::UnboundedSender<HostEvent>,
    /// The captured window: from the source, or once the capture finds it,
    /// and the game's next window when it replaces it.
    window: AtomicIsize,
    /// The process behind the window, once known. Locked before `known`.
    game: Mutex<Option<Box<dyn GameProcess>>>,
    /// The game's other windows, which are never taken for its new one.
    known: Mutex<Known>,
    state: Mutex<State>,
    /// Every viewer's mailbox and flags, for the capture thread.
    feeds: Mutex<Vec<Arc<Feed>>>,
    /// Every viewer's connection, for the sound.
    audio_to: Mutex<Vec<Connection>>,
    audio_sent: AtomicU64,
    /// Set by the panic key: grants are refused until the host allows
    /// control again.
    grants_paused: Arc<AtomicBool>,
    /// No encoder reads GPU frames in this stream (M9b): one opened for a
    /// GPU frame couldn't take it (not NVIDIA, or another GPU than the
    /// capture's), or one reading them failed. Encoders opened from then on
    /// take frames in memory, and the capture is told to deliver them so.
    gpu_input_off: AtomicBool,
}

/// The game's windows other than its new one ([`Shared::remember_windows`]).
#[derive(Default)]
struct Known {
    /// Never followed: there when streaming started, or seen on two looks
    /// in a row while the captured window was there.
    known: Vec<NativeId>,
    /// The last look.
    last_seen: Vec<NativeId>,
}

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
    /// Whether the window went, and what came of it.
    source: SourceWatch,
    stopped: bool,
}

struct Viewer {
    feed: Arc<Feed>,
    thread: Option<JoinHandle<()>>,
}

/// What the capture thread and a viewer's encoder thread share.
struct Feed {
    frames: Mailbox<VideoFrame>,
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

    /// Starts watching the process behind `window`, if not yet.
    fn watch_game(&self, window: NativeId) {
        let mut game = lock(&self.game);
        if game.is_none() {
            *game = self.backend.game_process(window);
            let windows = game.as_ref().map(|g| g.windows()).unwrap_or_default();
            *lock(&self.known) = Known {
                known: windows.clone(),
                last_seen: windows,
            };
        }
    }

    /// While the captured window is there: remembers the game's other
    /// windows, which are never followed (another browser window, the
    /// desktop of Explorer). A window counts only once seen on two looks in
    /// a row: a game's new window, opened a moment before it closes the old
    /// one, must stay followable. Lists windows: call without holding
    /// `state`, and off the async workers.
    fn remember_windows(&self) {
        let game = lock(&self.game);
        let Some(g) = game.as_ref() else {
            return;
        };
        let windows = g.windows();
        // The captured window went meanwhile: what was listed may include
        // its replacement.
        let captured = self.window.load(Ordering::Acquire);
        if captured == 0 || !self.backend.window_exists(captured) {
            return;
        }
        let mut k = lock(&self.known);
        let known: Vec<NativeId> = windows
            .iter()
            .copied()
            .filter(|w| k.known.contains(w) || k.last_seen.contains(w))
            .collect();
        *k = Known {
            known,
            last_seen: windows,
        };
    }

    /// The game as it is now: only windows that weren't there while the
    /// captured one was count as its new window. The process that exited may
    /// have left a child showing one (a launcher). Lists windows: call
    /// without holding `state`, and off the async workers.
    fn game_now(&self) -> Game {
        let game = lock(&self.game);
        let Some(g) = game.as_ref() else {
            return Game::Unknown;
        };
        let known = lock(&self.known);
        let fresh: Vec<NativeId> = g
            .windows()
            .into_iter()
            .filter(|w| !known.known.contains(w))
            .collect();
        if fresh.is_empty() && !g.running() {
            Game::Exited
        } else {
            Game::Running(fresh)
        }
    }

    /// The capture follows `window`: from now on its process is the game.
    fn follow(&self, window: NativeId) {
        self.window.store(window, Ordering::Release);
        let mut game = lock(&self.game);
        if let Some(next) = self.backend.game_process(window) {
            *game = Some(next);
        }
        let windows = game.as_ref().map(|g| g.windows()).unwrap_or_default();
        *lock(&self.known) = Known {
            known: windows.clone(),
            last_seen: windows,
        };
    }

    /// One look while the window is gone: what the source watch says, done.
    /// Returns the window to capture from now on, if the game showed one.
    fn look_for_window(&self) -> (Verdict, Option<NativeId>) {
        let game = self.game_now();
        let verdict = lock(&self.state).source.check(&game, Instant::now());
        let follow = match verdict {
            Verdict::Wait => None,
            Verdict::Follow(window) => {
                self.follow(window);
                self.event(HostEvent::SourceFollowed {
                    window,
                    at: self.now(),
                });
                Some(window)
            }
            Verdict::GameExited => {
                self.event(HostEvent::GameExited);
                None
            }
            Verdict::SourceLost => {
                self.event(HostEvent::SourceClosed);
                None
            }
        };
        (verdict, follow)
    }
}

impl HostStream {
    /// Plugs in a virtual pad for each seat that has one, in seat order
    /// (which can take seconds), and starts the input task. Capture and
    /// sound start with the first viewer.
    pub async fn start(
        backend: Arc<dyn Backend>,
        options: HostOptions,
    ) -> (Self, mpsc::UnboundedReceiver<HostEvent>) {
        let (events, events_rx) = mpsc::unbounded_channel();
        let (local_tx, local_rx) = mpsc::channel::<PadState>(LOCAL_QUEUE);
        let plugged = {
            let backend = backend.clone();
            let real_ids = options.real_pad_ids;
            let seats = options.seats.clone();
            let local = options.local;
            tokio::task::spawn_blocking(move || {
                let pads = seats
                    .iter()
                    .map(|kind| kind.has_pad().then(|| backend.plug_pad(real_ids)))
                    .collect::<Vec<_>>();
                let local = local.map(|local| start_local(&*backend, local, local_tx));
                (pads, local)
            })
            .await
        };
        let (plugged, local) = match plugged {
            Ok((pads, local)) => (Ok(pads), local),
            Err(e) => (Err(e), None),
        };
        let (local, local_status, hidden) = match local {
            Some(Ok((pad, hidden))) => (Some(pad), Some(Ok(())), hidden),
            Some(Err(e)) => (None, Some(Err(e)), None),
            None => (
                None,
                options.local.map(|_| Err("it didn't start".to_string())),
                None,
            ),
        };
        let window = match options.source {
            Source::Window(id) => id,
            _ => 0,
        };
        let share = options.upload_bps.map(UploadShare::new);
        let source = SourceWatch::new(options.replace_grace);
        let shared = Arc::new(Shared {
            clock: backend.clock(),
            backend,
            options,
            events,
            window: AtomicIsize::new(window),
            game: Mutex::new(None),
            known: Mutex::new(Known::default()),
            state: Mutex::new(State {
                viewers: BTreeMap::new(),
                running: None,
                share,
                traces: 0,
                retired: Vec::new(),
                source,
                stopped: false,
            }),
            feeds: Mutex::default(),
            audio_to: Mutex::default(),
            audio_sent: AtomicU64::new(0),
            grants_paused: Arc::new(AtomicBool::new(false)),
            gpu_input_off: AtomicBool::new(false),
        });
        // By seat: a seat without a pad (a keyboard seat, or one whose pad
        // failed) drops gamepad input.
        let pads: Vec<Option<Box<dyn VirtualPad>>> = match plugged {
            Ok(plugged) => plugged
                .into_iter()
                .enumerate()
                .map(|(seat, pad)| match pad {
                    None => None,
                    Some(Ok(pad)) => {
                        shared.note(format!("seat {seat}: virtual Xbox 360 pad plugged in"));
                        Some(pad)
                    }
                    Some(Err(e)) => {
                        shared.note(format!(
                            "seat {seat}: no virtual pad ({e}); its gamepad input will be dropped"
                        ));
                        None
                    }
                })
                .collect(),
            Err(e) => {
                shared.note(format!("no virtual pads ({e})"));
                Vec::new()
            }
        };
        if window != 0 {
            let sh = shared.clone();
            let _ = tokio::task::spawn_blocking(move || sh.watch_game(window)).await;
        }
        let local_seat = shared.options.local;
        match (local_seat, &local_status) {
            (Some(l), Some(Ok(()))) => shared.note(format!(
                "the host's controller {:04x}:{:04x} plays through a seat{}",
                l.controller.vendor,
                l.controller.product,
                if hidden.is_some() {
                    ", hidden from games"
                } else {
                    ""
                }
            )),
            (Some(_), Some(Err(e))) => shared.note(format!(
                "the host's controller can't play through a seat: {e}"
            )),
            _ => {}
        }
        let local = local.zip(local_seat).map(|(pad, seat)| LocalFeed {
            member: seat.member,
            pad,
            states: local_rx,
        });
        let (input, cmds) = mpsc::unbounded_channel();
        let input_task = tokio::spawn(input_loop(shared.clone(), pads, local, hidden, cmds));
        let housekeeping = tokio::spawn(housekeeping(Arc::downgrade(&shared))).abort_handle();
        let network = shared.backend.low_latency_network();
        (
            Self {
                shared,
                input,
                input_task: Some(input_task),
                housekeeping,
                local: local_status,
                _network: network,
            },
            events_rx,
        )
    }

    /// Whether the host's controller plays through a seat: `None` if it
    /// wasn't asked to, or why it can't.
    pub fn local_seat(&self) -> Option<&Result<(), String>> {
        self.local.as_ref()
    }

    /// Hands `seat` to `member` only if nobody holds it and `member` holds no
    /// other: the host's own seat coming back to them. Only the host's own
    /// controller may claim; other claims are ignored.
    pub fn claim(&self, seat: SeatId, member: MemberId) {
        let _ = self.input.send(InputCmd::Claim { seat, member });
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

    /// Hands `seat` to `member`, ending any other hold on it first. Commands
    /// reach the router in the order they were sent.
    pub fn grant(&self, seat: SeatId, member: MemberId, permissions: Permissions) {
        let _ = self.input.send(InputCmd::Grant {
            seat,
            member,
            permissions,
        });
    }

    /// The host has `seat` again: from `holder` only if it still holds it,
    /// or from whoever does with `None`.
    pub fn revoke(&self, seat: SeatId, holder: Option<MemberId>) {
        let _ = self.input.send(InputCmd::Revoke { seat, holder });
    }

    /// Every seat back to the host at once, their own controller's included.
    pub fn revoke_all(&self) {
        let _ = self.input.send(InputCmd::RevokeAll);
    }

    /// Every seat a viewer holds back to the host at once; the one the
    /// host's own controller plays stays theirs.
    pub fn revoke_viewers(&self) {
        let _ = self.input.send(InputCmd::RevokeViewers);
    }

    /// The panic key: every seat a viewer holds back to the host (their own
    /// controller keeps the seat it plays), and no grant to a viewer, nor any
    /// claim, goes through until [`HostStream::allow_grants`], even one
    /// already on its way.
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
    /// Every seat a viewer holds back to the host; grants to viewers and
    /// claims refused until allowed again. The flag is set first, so one
    /// already queued is refused too.
    pub fn panic(&self) {
        self.paused.store(true, Ordering::SeqCst);
        let _ = self.input.send(InputCmd::RevokeViewers);
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
        let (looking, watching_game) = {
            let mut state = lock(&sh.state);
            reshare(&mut state);
            state.retired.retain(|t| !t.is_finished());
            // While capturing, the capture watches the window itself.
            let window = sh.window.load(Ordering::Acquire);
            if state.running.is_none()
                && !state.source.ended()
                && sh.options.source.is_window()
                && window != 0
                && !state.source.looking()
                && !sh.backend.window_exists(window)
            {
                state.source.lost(Instant::now());
            }
            (
                state.running.is_none() && state.source.looking(),
                !state.source.looking() && !state.source.ended(),
            )
        };
        // Listing windows blocks for a moment: not on an async worker.
        let sh2 = sh.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if looking {
                // Nobody watches: once a second is soon enough. The
                // capture, when someone comes, starts on whatever window
                // this found.
                sh2.look_for_window();
            } else if watching_game {
                sh2.remember_windows();
            }
        })
        .await;
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
    // A window found earlier (or followed while nobody watched) wins over
    // the source as first given.
    let mut source = match shared.window.load(Ordering::Acquire) {
        0 => options.source.clone(),
        window if options.source.is_window() => Source::Window(window),
        _ => options.source.clone(),
    };
    loop {
        if stop.load(Ordering::SeqCst) || lock(&shared.state).source.ended() {
            return;
        }
        // The window may be gone already (someone came to watch during the
        // grace, or it closed while nobody watched): look for the next one
        // rather than fail to capture it.
        let gone = match source {
            Source::Window(window) => {
                lock(&shared.state).source.looking() || !shared.backend.window_exists(window)
            }
            _ => false,
        };
        if gone {
            match follow_game(shared, stop) {
                Some(window) => {
                    source = Source::Window(window);
                    continue;
                }
                None => return,
            }
        }
        let (capturer, window) = match shared.backend.capture(&source, options.fps) {
            Ok(capture) => capture,
            Err(e) => {
                shared.event(HostEvent::CaptureFailed(e.to_string()));
                return;
            }
        };
        if let Some(window) = window {
            shared.window.store(window, Ordering::Release);
            shared.watch_game(window);
        }
        match capture_frames(shared, capturer, stop) {
            Some(window) => source = Source::Window(window),
            None => return,
        }
    }
}

/// Hands every frame to every viewer until stopped or the window goes;
/// returns the game's new window, if it showed one.
fn capture_frames(
    shared: &Shared,
    mut capturer: Box<dyn couch_media::Capturer>,
    stop: &AtomicBool,
) -> Option<NativeId> {
    while !stop.load(Ordering::SeqCst) {
        match capturer.next_event(Duration::from_millis(10)) {
            Ok(Some(CaptureEvent::Frame(frame))) => {
                if matches!(frame, VideoFrame::Gpu(_))
                    && shared.gpu_input_off.load(Ordering::SeqCst)
                {
                    capturer.frames_in_memory();
                }
                // (A GPU frame is already even, and the copies share it.)
                let frame = frame.cropped_to_even();
                for feed in lock(&shared.feeds).iter() {
                    feed.frames.put(frame.clone());
                }
            }
            Ok(Some(CaptureEvent::SourceClosed)) => {
                drop(capturer);
                return follow_game(shared, stop);
            }
            Ok(None) => {}
            Err(e) => {
                shared.event(HostEvent::CaptureFailed(e.to_string()));
                return None;
            }
        }
    }
    None
}

/// The captured window went. Looks for the game's next window until the
/// source watch decides, or the capture is stopped (housekeeping then goes on
/// looking, if the window is still being looked for).
fn follow_game(shared: &Shared, stop: &AtomicBool) -> Option<NativeId> {
    lock(&shared.state).source.lost(Instant::now());
    while !stop.load(Ordering::SeqCst) {
        match shared.look_for_window() {
            (Verdict::Wait, _) => std::thread::sleep(source::REPLACE_POLL),
            (_, follow) => return follow,
        }
    }
    None
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
    // The texture pool the encoder reads GPU frames from, if it does.
    let mut gpu_pool: Option<u64> = None;
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

        // A GPU frame goes to the encoder as it is, unless GPU input failed.
        let gpu = match &frame {
            VideoFrame::Gpu(f) if !shared.gpu_input_off.load(Ordering::SeqCst) => Some(f),
            _ => None,
        };
        let size_changed =
            params.is_none_or(|p| (p.width, p.height) != (frame.width(), frame.height()));
        // An encoder reading GPU frames stays with the texture pool it was
        // opened for: a new capture device or size makes a new pool, whose
        // textures it must not mistake for the old ones it remembers.
        let pool_changed = encoder.as_ref().is_some_and(|e| e.caps().gpu_input)
            && gpu.map(|f| f.source) != gpu_pool;
        if size_changed || pool_changed {
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
            let (width, height) = (frame.width(), frame.height());
            // Closed first: it holds its pool's textures and an NVENC session.
            drop(encoder.take());
            let enc = shared.backend.encoder(
                EncoderConfig {
                    codec: Codec::H264,
                    width,
                    height,
                    fps: options.fps,
                    bitrate_bps: start,
                },
                gpu,
            )?;
            gpu_pool = gpu.filter(|_| enc.caps().gpu_input).map(|f| f.source);
            if gpu.is_some() && !enc.caps().gpu_input {
                // This machine's encoder can't read them: copying each frame
                // back from the GPU would cost more than capturing to memory.
                shared.gpu_input_off.store(true, Ordering::SeqCst);
            }
            shared.note(format!(
                "encoding {width}x{height} with {} (options: {}, {}) at {:.1} Mbit/s",
                enc.caps().name,
                enc.caps().options,
                if enc.caps().gpu_input {
                    "GPU frames"
                } else {
                    "frames in memory"
                },
                f64::from(start) / 1e6
            ));
            rate.keyframe_costly = enc.caps().bitrate_change_costs_keyframe;
            let mut c = RateController::new(rate);
            c.set_share(share);
            controller = Some(c);
            let p = VideoParams {
                codec: Codec::H264,
                width,
                height,
                fps: options.fps,
            };
            video.start_epoch(&p)?;
            params = Some(p);
            encoder = Some(enc);
            force_keyframe = true;
        }

        let enc = encoder.as_mut().expect("opened above");
        let key = std::mem::take(&mut force_keyframe) | feed.keyframe.swap(false, Ordering::SeqCst);
        let encoded = match gpu {
            Some(f) if enc.caps().gpu_input => enc.encode(EncoderInput::Gpu(f), key),
            // Copied to memory once per frame, for every viewer that needs it.
            _ => frame
                .cpu()
                .and_then(|cpu| enc.encode(EncoderInput::Cpu(&cpu), key)),
        };
        let units = match encoded {
            Ok(units) => units,
            // Rather than lose the viewer: frames in memory from now on, for
            // every encoder opened in this stream. The reopen sends a keyframe.
            Err(e) if enc.caps().gpu_input => {
                tracing::warn!("encoding GPU frames failed ({e}); copying frames to memory");
                shared.gpu_input_off.store(true, Ordering::SeqCst);
                params = None;
                continue;
            }
            Err(e) => return Err(e),
        };
        for unit in units {
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

/// The host's controller states waiting for the input task; full snapshots,
/// so dropping some when it falls behind loses nothing.
const LOCAL_QUEUE: usize = 64;

/// The host's controller while it plays through a seat.
struct LocalFeed {
    member: MemberId,
    pad: Box<dyn LocalPad>,
    states: mpsc::Receiver<PadState>,
}

/// Keeps the host's controller hidden from games until dropped.
type HiddenGuard = Box<dyn Any + Send>;

/// Hides the host's controller if asked, then follows it, its states going
/// to `states`. Blocks: hiding and starting the reader take a moment.
/// Returns the followed pad and the hiding guard.
fn start_local(
    backend: &dyn Backend,
    local: LocalSeat,
    states: mpsc::Sender<PadState>,
) -> Result<(Box<dyn LocalPad>, Option<HiddenGuard>), String> {
    let pads = backend
        .local_pads()
        .ok_or_else(|| "this PC's controllers can't be read".to_string())?;
    let hidden = if local.hide {
        Some(backend.hide_controller(local.controller)?)
    } else {
        None
    };
    // A failure here drops `hidden`: the controller is shown again.
    let pad = pads
        .follow(
            local.controller,
            Box::new(move |state| {
                let _ = states.try_send(state);
            }),
        )
        .map_err(|e| e.to_string())?;
    Ok((pad, hidden))
}

enum InputCmd {
    AddViewer {
        member: MemberId,
        inputs: InputReceiver,
        conn: Connection,
    },
    RemoveViewer(MemberId),
    Grant {
        seat: SeatId,
        member: MemberId,
        permissions: Permissions,
    },
    /// Only from this holder, or from whoever holds the seat.
    Revoke {
        seat: SeatId,
        holder: Option<MemberId>,
    },
    RevokeAll,
    /// Every seat but the one the host's own controller plays.
    RevokeViewers,
    /// Grant only if the seat is free, `member`, the host, holds none, and
    /// the panic key hasn't paused control.
    Claim {
        seat: SeatId,
        member: MemberId,
    },
    /// Release everything, then answer: the first step of stopping.
    ReleaseAll(oneshot::Sender<()>),
    /// Unplug the pads and end, then answer: the last step.
    Shutdown(oneshot::Sender<()>),
}

/// One seat's rumble: the game's current one, and what last went out, to
/// whom, when.
#[derive(Clone, Copy)]
struct SeatRumble {
    now: Rumble,
    sent: (Option<MemberId>, Rumble, Instant),
}

/// The input task's state: the router, what it injects into, and where
/// rumble goes.
struct Inputs {
    shared: Arc<Shared>,
    router: SeatRouter,
    kbm: Box<dyn KbmInjector>,
    /// By seat; `None` for a seat without a pad.
    pads: Vec<Option<Box<dyn VirtualPad>>>,
    /// Each viewer's forwarding task, and its connection (for rumble).
    viewers: HashMap<MemberId, (tokio::task::JoinHandle<()>, Connection)>,
    /// By seat, as last reported.
    status: Vec<Option<SeatStatus>>,
    rumble: Vec<SeatRumble>,
    /// Whether the game window is in front now, and what was last reported
    /// and when. `None` for a screen: nothing to report.
    focus: Option<FocusReport>,
    /// The host's controller, while it plays through a seat.
    local: Option<Local>,
}

/// The host's controller, in the input task.
struct Local {
    member: MemberId,
    pad: Box<dyn LocalPad>,
    /// Its states, numbered as a viewer numbers its pad datagrams.
    seq: u32,
}

struct FocusReport {
    now: bool,
    reported: Option<bool>,
    at: Instant,
}

impl Inputs {
    fn seats(&self) -> impl Iterator<Item = SeatId> + use<> {
        (0..self.shared.options.seats.len()).map(|i| SeatId(i as u8))
    }

    /// Updates the router's view of where keyboard and mouse would land.
    /// Called right before every input, not just on the tick: a window that
    /// just took the foreground must not get a key meant for the game.
    fn refresh_focus(&mut self) -> Vec<Effect> {
        let focus = match self.shared.window.load(Ordering::Acquire) {
            0 => Focus::default(),
            window => self.shared.backend.focus(window),
        };
        if let Some(report) = &mut self.focus {
            report.now = focus.foreground;
        }
        self.router.set_pointer_inside(focus.pointer_inside);
        self.router.set_foreground(focus.foreground)
    }

    /// Reports the game window's focus when it changed, at most once per
    /// [`FOCUS_COALESCE`]; the tick catches up with the latest.
    fn report_focus(&mut self, now: Instant) {
        let Some(report) = &mut self.focus else {
            return;
        };
        if report.reported == Some(report.now) || now.duration_since(report.at) < FOCUS_COALESCE {
            return;
        }
        report.reported = Some(report.now);
        report.at = now;
        self.shared.event(HostEvent::GameFocus(report.now));
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
        for seat in self.seats() {
            let status = self.router.seat(seat).map(|s| s.2);
            let slot = &mut self.status[usize::from(seat.0)];
            if status != *slot {
                *slot = status;
                if let Some(status) = status {
                    let at = self.shared.now();
                    self.shared.event(HostEvent::Seat { seat, status, at });
                }
            }
        }
    }

    fn tick(&mut self, now: Instant) {
        let mut effects = self.router.tick(now);
        effects.extend(self.refresh_focus());
        self.deliver(effects);
        self.report_focus(now);
        self.send_rumble(now);
    }

    /// The seat `member` holds in the router.
    fn seat_of(&self, member: MemberId) -> Option<SeatId> {
        self.seats()
            .find(|seat| self.router.seat(*seat).is_some_and(|s| s.1 == Some(member)))
    }

    /// The host's controller moved, or reports itself still: into the router
    /// as the host's input on the seat they hold, like a viewer's pad
    /// datagram. Dropped while they hold none.
    fn local_input(&mut self, state: PadState) {
        let Some(member) = self.local.as_ref().map(|l| l.member) else {
            return;
        };
        let Some(seat) = self.seat_of(member) else {
            return;
        };
        let Some((epoch, ..)) = self.router.seat(seat) else {
            return;
        };
        let Some(local) = self.local.as_mut() else {
            return;
        };
        local.seq = local.seq.wrapping_add(1);
        let datagram = PadDatagram {
            seat,
            seat_epoch: epoch,
            seq: local.seq,
            pad: state.into(),
        };
        let mut effects = self.refresh_focus();
        effects.extend(
            self.router
                .handle(member, Input::Pad(datagram), Instant::now()),
        );
        self.deliver(effects);
    }

    /// A seat's rumble goes only to its holder: a viewer's stream window, or
    /// the host's controller. A running rumble is repeated, so a lost "stop"
    /// can't leave a controller shaking: each one ends after a second.
    fn send_rumble(&mut self, now: Instant) {
        let host = self.local.as_ref().map(|l| l.member);
        let host_seated = host.and_then(|h| self.seat_of(h)).is_some();
        for seat in self.seats() {
            let i = usize::from(seat.0);
            if let Some(Some(pad)) = self.pads.get_mut(i)
                && let Some(r) = pad.take_rumble()
            {
                self.rumble[i].now = r;
                self.shared.event(HostEvent::Rumble(r));
            }
            let holder = self.router.seat(seat).and_then(|s| s.1);
            let rumble = &mut self.rumble[i];
            let (to, sent, at) = rumble.sent;
            // The seat left the host, and they hold no other: their
            // controller stops at once. (A viewer's stream window stops when
            // it hears its seat went. A host who moved gets the new seat's
            // rumble from its own turn of this loop.)
            if to.is_some()
                && to == host
                && holder != host
                && !host_seated
                && let Some(local) = &self.local
            {
                local.pad.rumble(Rumble::default());
            }
            let Some(holder) = holder else {
                rumble.sent = (None, Rumble::default(), now);
                continue;
            };
            let repeat = rumble.now != Rumble::default() && now.duration_since(at) >= RUMBLE_REPEAT;
            if to == Some(holder) && sent == rumble.now && !repeat {
                continue;
            }
            if Some(holder) == host {
                if let Some(local) = &self.local {
                    local.pad.rumble(rumble.now);
                }
            } else if let Some((_, conn)) = self.viewers.get(&holder) {
                send_rumble(
                    conn,
                    RumbleDatagram {
                        seat,
                        low: rumble.now.low_frequency,
                        high: rumble.now.high_frequency,
                    },
                );
            }
            rumble.sent = (Some(holder), rumble.now, now);
        }
    }
}

/// `hidden` keeps the host's controller hidden from games; it is dropped
/// last, after the pads are unplugged, so no game sees it while it still has
/// a seat's pad.
async fn input_loop(
    shared: Arc<Shared>,
    pads: Vec<Option<Box<dyn VirtualPad>>>,
    local: Option<LocalFeed>,
    hidden: Option<HiddenGuard>,
    mut cmds: mpsc::UnboundedReceiver<InputCmd>,
) {
    let (tx, mut inputs) = mpsc::channel::<(MemberId, Input)>(1024);
    let (local, mut local_states) = match local {
        Some(feed) => (
            Some(Local {
                member: feed.member,
                pad: feed.pad,
                seq: 0,
            }),
            Some(feed.states),
        ),
        None => (None, None),
    };
    let seats = shared.options.seats.len();
    let start = Instant::now();
    let mut st = Inputs {
        kbm: shared.backend.kbm(),
        router: SeatRouter::starting_at(seats as u8, shared.options.first_epoch),
        pads,
        viewers: HashMap::new(),
        status: vec![None; seats],
        rumble: vec![
            SeatRumble {
                now: Rumble::default(),
                sent: (None, Rumble::default(), start),
            };
            seats
        ],
        focus: shared.options.source.is_window().then(|| FocusReport {
            now: false,
            reported: None,
            // The first report may go at once.
            at: start.checked_sub(FOCUS_COALESCE).unwrap_or(start),
        }),
        local,
        shared,
    };
    for seat in st.seats() {
        st.status[usize::from(seat.0)] = st.router.seat(seat).map(|s| s.2);
    }
    let host = st.local.as_ref().map(|l| l.member);
    let mut tick = tokio::time::interval(INPUT_TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut done = None;
    // Stopping released everything: nobody gets a seat again.
    let mut released = false;

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
                Some(InputCmd::Grant { seat, member, permissions }) => {
                    // The panic key gives control back to the host: their own
                    // controller may still move between seats.
                    if released {
                        tracing::info!(member = ?member, "stopping: a grant was refused");
                    } else if st.shared.grants_paused.load(Ordering::SeqCst) && Some(member) != host {
                        tracing::info!(member = ?member, "panic key: a grant on its way was refused");
                    } else {
                        let effects = st.router.grant(seat, member, permissions, Instant::now());
                        if effects.is_empty() {
                            tracing::warn!(?seat, "the seat router refused a grant");
                        }
                        st.deliver(effects);
                    }
                }
                Some(InputCmd::Revoke { seat, holder }) => {
                    let current = st.router.seat(seat).and_then(|s| s.1);
                    if holder.is_none_or(|h| current == Some(h)) {
                        let effects = st.router.revoke(seat);
                        st.deliver(effects);
                    }
                }
                Some(InputCmd::RevokeAll) => {
                    let effects = st.router.revoke_all();
                    st.deliver(effects);
                }
                Some(InputCmd::RevokeViewers) => {
                    let ours = host.and_then(|h| st.seat_of(h));
                    let seats: Vec<SeatId> = st.seats().filter(|s| Some(*s) != ours).collect();
                    for seat in seats {
                        let effects = st.router.revoke(seat);
                        st.deliver(effects);
                    }
                }
                Some(InputCmd::Claim { seat, member }) => {
                    let free = st.router.seat(seat).is_some_and(|s| s.1.is_none());
                    let paused = st.shared.grants_paused.load(Ordering::SeqCst);
                    if !released && !paused && host == Some(member) && free && st.seat_of(member).is_none() {
                        let effects = st.router.grant(seat, member, Permissions::GAMEPAD, Instant::now());
                        st.deliver(effects);
                    }
                }
                Some(InputCmd::ReleaseAll(ack)) => {
                    released = true;
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
            state = next_local(&mut local_states) => match state {
                Some(state) => st.local_input(state),
                // The reader stopped: the host's seat goes quiet, and the
                // dead-man timer releases it.
                None => local_states = None,
            },
            _ = tick.tick() => st.tick(Instant::now()),
        }
    }

    // However the loop ended: release everything, then unplug the pads.
    let effects = st.router.revoke_all();
    st.deliver(effects);
    for (_, (task, _)) in st.viewers.drain() {
        task.abort();
    }
    drop(st);
    // The pads are gone: games may see the host's controller again. Showing
    // it talks to the driver, off the runtime's threads.
    if let Some(hidden) = hidden {
        let _ = tokio::task::spawn_blocking(move || drop(hidden)).await;
    }
    if let Some(ack) = done {
        let _ = ack.send(());
    }
}

/// The host controller's next state; never, without one.
async fn next_local(states: &mut Option<mpsc::Receiver<PadState>>) -> Option<PadState> {
    match states {
        Some(states) => states.recv().await,
        None => std::future::pending().await,
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
