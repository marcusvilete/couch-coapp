//! A member's side of watching: `couch-viewer` shows the session, and local
//! input goes to the host while this member holds the seat.
//!
//! The viewer process gets the frames and sound as they arrive, the HUD
//! state and seat changes; it sends back local input, frame timings and what
//! the user asks for. Input goes through `couch_input::forward`, whose
//! heartbeat stops when the viewer goes quiet, so a hung viewer lets the
//! host's dead-man timer release whatever it held.
//!
//! Two things can go and come back without ending the watch:
//! - **the link to the host.** When the connection closes, the window stays
//!   open and [`WatchEvent::ConnectionLost`] is reported; whoever runs the
//!   watch either stops it or hands it a new connection
//!   ([`Watch::reconnect`]). The seat is gone with the old connection.
//! - **the viewer window**, with [`WatchOptions::restart_viewer`]: a viewer
//!   that exits without the user leaving (a crash) is started again, at most
//!   once a minute, and the host is asked for a keyframe.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use couch_input::forward::Forwarder;
use couch_media::{MediaError, clock::Clock};
use couch_net::{
    Connection,
    audio::receive_audio,
    datagrams::Datagrams,
    input::InputSender,
    streams::Streams,
    video::{VideoFrameIn, ViewerVideo},
};
use couch_proto::{
    board::{BoardSnapshot, PictureId},
    control::{SeatAssigned, SeatController, SeatReleased},
    input::RumbleDatagram,
    ipc::{
        self, AudioPacket, AudioStats, BoardNews, ControllerNames, Draw, FrameTiming, HudState,
        IpcFrame, LocalInput, NetStats, Picture, PictureUp, RumbleState, ShortcutKeys,
        StreamVolume, UserAction, VideoFrameHeader, WindowRequest, WindowState,
    },
    media::{AudioDatagram, datagram, decode_payload},
    session::ControllerReport,
};
use couch_room::board::Replica;
use tokio::{io::AsyncWrite, sync::mpsc, task::JoinHandle, time::MissedTickBehavior};

use super::{Backend, ViewerOptions, ViewerProcess, ipc as pipe};

/// After asking the viewer to close, how long to let it finish (it releases
/// what it held) before ending it.
const CLOSE_WAIT: Duration = Duration::from_secs(2);
/// A viewer window that crashes again within this long stays closed.
pub const RESTART_LIMIT: Duration = Duration::from_secs(60);
/// How long a window that went away gets to finish saying what it said.
const LAST_WORDS: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Default)]
pub struct WatchOptions {
    /// Video that names any other session is refused; `None` takes any
    /// (`couch-cli view`).
    pub session: Option<u64>,
    pub viewer: ViewerOptions,
    /// Report every presented frame's timing and the audio stats.
    pub measure: bool,
    /// Start the viewer window again if it crashes (the app). `couch-cli
    /// view` leaves it off: `release-check` kills the window on purpose.
    pub restart_viewer: bool,
    /// The user's shortcut keys, sent to every window right after `HELLO`;
    /// empty for the defaults.
    pub shortcuts: ShortcutKeys,
    /// The stream volume, sent to every window after the shortcuts (P6). A
    /// change in the window replaces it, so a restarted window keeps it.
    pub volume: StreamVolume,
}

/// What watching reports.
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// The decoder needs a keyframe: ask the host (`KeyframeRequest`).
    KeyframeNeeded { epoch: u32 },
    /// The user asked for something in the viewer.
    Action(UserAction),
    /// A presented frame (with `measure`).
    Timing(FrameTiming),
    /// The audio player's counters, every second (with `measure`).
    AudioStats(AudioStats),
    /// The path to the host, four times a second.
    Net(NetStats),
    /// The host's game rumbles this member's gamepad (`couch-cli view`
    /// prints it).
    Rumble(RumbleState),
    /// The viewer window's controllers changed, or which drives a seat we
    /// hold (M18, P12): tell the host (`ViewerControllers`).
    Controllers(ControllerReport),
    /// The user changed the stream volume in the window (P6): remember it.
    Volume(StreamVolume),
    /// The window's full screen, bar or stats, as they are now (P11).
    Window(WindowState),
    /// The user closed the viewer window: watching is over.
    ViewerClosed,
    /// The viewer window crashed, and it can't (or may not yet) be started
    /// again: watching is over.
    ViewerCrashed,
    /// The viewer window crashed and was started again.
    ViewerRestarted,
    /// The connection to the host went. The window stays open until the
    /// watch is stopped or given a new connection. `link` numbers the
    /// connection: 0 for the first, then what [`Watch::reconnect`] returned,
    /// so a late report about an earlier one can be told apart.
    ConnectionLost { link: u32 },
    /// The user drew on the stream (M27a): for the host (`BoardOps`).
    Draw(Draw),
    /// The user's picture (M27e): for the host, on a picture stream.
    Picture(PictureUp),
}

enum WatchCmd {
    /// What the host said of the board (M27a).
    Board(BoardNews),
    SeatAssigned(SeatAssigned),
    SeatReleased(SeatReleased),
    SeatController(SeatController),
    Hud(HudState),
    /// The app changed the stream volume (P11).
    Volume(StreamVolume),
    /// Something the app asks of the window (P11).
    Window(WindowRequest),
    Reconnect(Box<Link>, u32),
    Stop,
}

/// One session being watched. Dropping it closes the viewer window.
pub struct Watch {
    cmds: mpsc::UnboundedSender<WatchCmd>,
    task: Option<JoinHandle<()>>,
    clock: Arc<dyn Clock>,
    session: Option<u64>,
    /// The number of the latest connection handed over.
    links: AtomicU32,
    /// The network tuned for streaming while watching (Wi-Fi media
    /// streaming mode, M9a); undone when this drops.
    _network: Option<Box<dyn std::any::Any + Send>>,
}

impl Watch {
    /// Takes the host's video and sound on `conn` (whose datagram reader and
    /// stream acceptor these are), starts the viewer, and runs. The
    /// subscriptions are in place before this returns, so ask the host for
    /// the stream only after. Call inside a tokio runtime.
    pub fn start(
        conn: Connection,
        datagrams: &Datagrams,
        streams: &Streams,
        backend: Arc<dyn Backend>,
        options: WatchOptions,
    ) -> Result<(Self, mpsc::UnboundedReceiver<WatchEvent>), MediaError> {
        let window = Window::launch(&*backend, &options.viewer)?;
        let network = backend.low_latency_network();
        let clock = backend.clock();
        let link = Link::start(conn, datagrams, streams, options.session, clock.clone());
        let (events, events_rx) = mpsc::unbounded_channel();
        let (cmds, cmds_rx) = mpsc::unbounded_channel();
        let session = options.session;
        let ctx = Ctx {
            link: Some(link),
            link_id: 0,
            window,
            backend,
            options,
            clock: clock.clone(),
            events,
            last_hud: None,
            last_restart: None,
            reported: ControllerReport::default(),
            board: Replica::default(),
            pictures: Pictures::default(),
        };
        let task = tokio::spawn(run(ctx, cmds_rx));
        Ok((
            Self {
                cmds,
                task: Some(task),
                clock,
                session,
                links: AtomicU32::new(0),
                _network: network,
            },
            events_rx,
        ))
    }

    /// Takes the host's video and sound on a new connection, after
    /// [`WatchEvent::ConnectionLost`]. As with [`start`](Self::start), the
    /// subscriptions are in place when this returns: ask to join after.
    /// Returns the new connection's number, as `ConnectionLost` will name it.
    pub fn reconnect(&self, conn: Connection, datagrams: &Datagrams, streams: &Streams) -> u32 {
        let id = self.links.fetch_add(1, Ordering::Relaxed) + 1;
        let link = Link::start(conn, datagrams, streams, self.session, self.clock.clone());
        let _ = self.cmds.send(WatchCmd::Reconnect(Box::new(link), id));
        id
    }

    /// The host granted this member a seat.
    pub fn seat_assigned(&self, message: SeatAssigned) {
        let _ = self.cmds.send(WatchCmd::SeatAssigned(message));
    }

    /// The host ended this member's hold on a seat.
    pub fn seat_released(&self, message: SeatReleased) {
        let _ = self.cmds.send(WatchCmd::SeatReleased(message));
    }

    /// The host moved a seat we hold to another of our controllers (P12).
    pub fn seat_controller(&self, message: SeatController) {
        let _ = self.cmds.send(WatchCmd::SeatController(message));
    }

    pub fn set_hud(&self, hud: HudState) {
        let _ = self.cmds.send(WatchCmd::Hud(hud));
    }

    /// What the host said of the board (M27a): the window shows it, and a
    /// restarted one gets it again.
    pub fn board(&self, news: BoardNews) {
        let _ = self.cmds.send(WatchCmd::Board(news));
    }

    /// The app changed the stream volume (P11): the window plays at it, and
    /// a restarted one starts at it.
    pub fn set_volume(&self, volume: StreamVolume) {
        let _ = self.cmds.send(WatchCmd::Volume(volume));
    }

    /// Asks the window for full screen, its bar, its stats, or to come to the
    /// front (P11). Its new state comes back as [`WatchEvent::Window`].
    pub fn window(&self, request: WindowRequest) {
        let _ = self.cmds.send(WatchCmd::Window(request));
    }

    /// Closes the viewer window and waits until it has gone.
    pub async fn stop(mut self) {
        let _ = self.cmds.send(WatchCmd::Stop);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.cmds.send(WatchCmd::Stop);
    }
}

/// Everything that comes from one connection to the host.
struct Link {
    conn: Connection,
    video: ViewerVideo,
    audio: mpsc::Receiver<AudioDatagram>,
    rumbles: mpsc::Receiver<Bytes>,
    input: InputSender,
    /// Pictures on the board from the host (M27e), read and checked.
    pictures: mpsc::Receiver<Picture>,
    _pictures: PictureReader,
}

impl Link {
    fn start(
        conn: Connection,
        datagrams: &Datagrams,
        streams: &Streams,
        session: Option<u64>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let (reader, pictures_rx) = receive_pictures(streams, session);
        Self {
            video: ViewerVideo::start(conn.clone(), streams, session, clock),
            audio: receive_audio(datagrams),
            rumbles: datagrams.subscribe(datagram::RUMBLE),
            input: InputSender::start(conn.clone()),
            pictures: pictures_rx,
            _pictures: reader,
            conn,
        }
    }
}

enum FromHost {
    Frame(VideoFrameIn),
    Audio(AudioDatagram),
    Rumble(Bytes),
    Picture(Picture),
    Lost,
}

/// The next thing from the host; never, while there is no connection.
async fn from_host(link: &mut Option<Link>) -> FromHost {
    let Some(l) = link.as_mut() else {
        return std::future::pending().await;
    };
    tokio::select! {
        frame = l.video.frames.recv() => frame.map_or(FromHost::Lost, FromHost::Frame),
        Some(packet) = l.audio.recv() => FromHost::Audio(packet),
        Some(rumble) = l.rumbles.recv() => FromHost::Rumble(rumble),
        Some(picture) = l.pictures.recv() => FromHost::Picture(picture),
        _ = l.conn.closed() => FromHost::Lost,
    }
}

/// The viewer process: what we write to it and what it says.
pub(super) struct Window {
    pub(super) to: ToViewer,
    pub(super) said: mpsc::UnboundedReceiver<IpcFrame>,
    reader: JoinHandle<()>,
    guard: Box<dyn std::any::Any + Send>,
    /// Its process, to let it come to the front (P11).
    pub(super) pid: Option<u32>,
    /// The user left (the window's close button, or Leave): its exit is no
    /// crash.
    pub(super) left: bool,
}

impl Window {
    pub(super) fn launch(
        backend: &dyn Backend,
        options: &ViewerOptions,
    ) -> Result<Self, MediaError> {
        let ViewerProcess {
            stdin,
            mut stdout,
            guard,
            pid,
        } = backend.launch_viewer(options)?;
        // What the viewer says, until its stdout closes.
        let (said_tx, said) = mpsc::unbounded_channel::<IpcFrame>();
        let reader = tokio::spawn(async move {
            while let Ok(Some(frame)) = pipe::read_frame(&mut stdout).await {
                if said_tx.send(frame).is_err() {
                    return;
                }
            }
        });
        Ok(Self {
            to: ToViewer {
                stdin,
                scratch: Vec::new(),
            },
            said,
            reader,
            guard,
            pid,
            left: false,
        })
    }

    /// Lets the viewer close on its own (it releases what it held), then
    /// ends it if it doesn't.
    pub(super) async fn close(mut self) {
        let _ = self.to.send(ipc::kinds::SHUTDOWN, &[]).await;
        drop(self.to);
        let _ = tokio::time::timeout(CLOSE_WAIT, &mut self.reader).await;
        drop(self.guard);
    }
}

/// Writes to the viewer; `false` once it is gone.
pub(super) struct ToViewer {
    stdin: std::pin::Pin<Box<dyn AsyncWrite + Send>>,
    scratch: Vec<u8>,
}

impl ToViewer {
    pub(super) async fn send(&mut self, kind: u8, payload: &[u8]) -> bool {
        pipe::write_frame(&mut self.stdin, kind, payload, &mut self.scratch)
            .await
            .is_ok()
    }
}

struct Ctx {
    link: Option<Link>,
    /// The number of `link` (see [`WatchEvent::ConnectionLost`]).
    link_id: u32,
    window: Window,
    backend: Arc<dyn Backend>,
    options: WatchOptions,
    clock: Arc<dyn Clock>,
    events: mpsc::UnboundedSender<WatchEvent>,
    /// For a restarted window.
    last_hud: Option<HudState>,
    last_restart: Option<Instant>,
    /// What the host was last told of our controllers (P12).
    reported: ControllerReport,
    /// The board as the window has it (M27a), for a restarted window.
    board: Replica,
    /// Its pictures (M27e), for a restarted window.
    pictures: Pictures,
}

impl Ctx {
    fn event(&self, event: WatchEvent) {
        let _ = self.events.send(event);
    }

    /// Tells the host of our controllers if that changed (P12).
    fn report(&mut self, forwarder: &Forwarder) {
        let report = forwarder.report();
        if report != self.reported {
            self.reported = report.clone();
            self.event(WatchEvent::Controllers(report));
        }
    }
}

async fn run(mut ctx: Ctx, mut cmds: mpsc::UnboundedReceiver<WatchCmd>) {
    let end = watch(&mut ctx, &mut cmds).await;
    if let Some(end) = end {
        ctx.event(end);
    }
    ctx.window.close().await;
}

/// The window went without the user leaving: start it again if allowed.
/// `false` when watching must end.
async fn restart(ctx: &mut Ctx, forwarder: &mut Forwarder) -> bool {
    let recent = ctx
        .last_restart
        .is_some_and(|t| t.elapsed() < RESTART_LIMIT);
    if !ctx.options.restart_viewer || recent {
        return false;
    }
    tracing::warn!("the viewer window went without leaving; starting it again");
    let Ok(window) = Window::launch(&*ctx.backend, &ctx.options.viewer) else {
        return false;
    };
    let old = std::mem::replace(&mut ctx.window, window);
    drop(old.guard);
    old.reader.abort();
    ctx.last_restart = Some(Instant::now());
    // Whatever the old window had down is up now: tell the host at once,
    // or the heartbeat would keep repeating it.
    let released = forwarder.viewer_replaced(Instant::now());
    if let Some(link) = &ctx.link {
        for out in released {
            link.input.send(out);
        }
    }
    // The new window counts its controllers again as it opens them.
    ctx.report(forwarder);
    if !hello(ctx, forwarder).await {
        return false;
    }
    // Its decoder starts empty.
    if let Some(link) = &ctx.link {
        link.video.note_decode_error();
        let epoch = link.video.latest_epoch();
        ctx.event(WatchEvent::KeyframeNeeded { epoch });
    }
    ctx.event(WatchEvent::ViewerRestarted);
    true
}

/// What a new window needs first: the version, the user's shortcut keys,
/// the HUD, the seat.
async fn hello(ctx: &mut Ctx, forwarder: &Forwarder) -> bool {
    let to = &mut ctx.window.to;
    if !to
        .send(ipc::kinds::HELLO, &ipc::IPC_VERSION.to_be_bytes())
        .await
        || !to
            .send(
                ipc::kinds::SHORTCUTS,
                &ipc::to_payload(&ctx.options.shortcuts),
            )
            .await
        || !to
            .send(ipc::kinds::VOLUME, &ipc::to_payload(&ctx.options.volume))
            .await
    {
        return false;
    }
    if let Some(hud) = &ctx.last_hud
        && !to.send(ipc::kinds::HUD_STATE, &ipc::to_payload(hud)).await
    {
        return false;
    }
    // The board as it was (M27a), but what was fading: gone by now.
    let mut board = ctx.board.clone();
    board.retain(|_, e| !(e.done && e.fades));
    let session = couch_proto::ids::SessionId(ctx.options.session.unwrap_or(0));
    for part in BoardSnapshot::parts(session, board.generation(), board.snapshot()) {
        let news = BoardNews::Snapshot(part);
        if !to.send(ipc::kinds::BOARD, &ipc::to_payload(&news)).await {
            return false;
        }
    }
    // Its pictures (M27e).
    for picture in ctx.pictures.shown(&ctx.board) {
        if !to
            .send(ipc::kinds::PICTURE, &ipc::to_payload(&picture))
            .await
        {
            return false;
        }
    }
    if !forwarder.holding().is_empty() {
        return send_seat(to, forwarder).await;
    }
    true
}

/// The seats held and which controller drives each (the forwarder decides;
/// the window rumbles and shows by it).
async fn send_seat(to: &mut ToViewer, forwarder: &Forwarder) -> bool {
    to.send(ipc::kinds::SEAT, &ipc::to_payload(&forwarder.seat_state()))
        .await
}

/// The watching loop; returns how it ended, unless it was asked to stop.
async fn watch(ctx: &mut Ctx, cmds: &mut mpsc::UnboundedReceiver<WatchCmd>) -> Option<WatchEvent> {
    let mut forwarder = Forwarder::new();
    if !hello(ctx, &forwarder).await && !restart(ctx, &mut forwarder).await {
        return Some(WatchEvent::ViewerCrashed);
    }
    let mut payload = Vec::new();
    let mut input_tick = tokio::time::interval(Duration::from_millis(10));
    input_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut net_tick = tokio::time::interval(Duration::from_millis(250));
    let mut net_since = Instant::now();
    let mut received_bytes = 0u64;

    // Each arm says whether the viewer window took what it was sent.
    loop {
        let delivered = tokio::select! {
            from = from_host(&mut ctx.link) => match from {
                FromHost::Frame(frame) => {
                    received_bytes += frame.data.len() as u64;
                    let stale = ctx
                        .link
                        .as_ref()
                        .is_some_and(|l| frame.epoch < l.video.latest_epoch());
                    if stale {
                        continue;
                    }
                    payload.clear();
                    VideoFrameHeader {
                        epoch: frame.epoch,
                        seq: frame.header.seq,
                        keyframe: frame.header.keyframe,
                        codec: frame.codec,
                        capture_us: frame.header.capture_us,
                        encode_us: frame.header.encode_us,
                        arrived_us: frame.arrived.0,
                    }
                    .write(&frame.data, &mut payload);
                    ctx.window.to.send(ipc::kinds::VIDEO_FRAME, &payload).await
                }
                FromHost::Audio(packet) => {
                    let packet = ipc::to_payload(&AudioPacket {
                        seq: packet.seq,
                        capture_us: packet.capture_us,
                        arrived_us: ctx.clock.now().0,
                        packet: packet.packet,
                    });
                    ctx.window.to.send(ipc::kinds::AUDIO_PACKET, &packet).await
                }
                FromHost::Picture(picture) => {
                    ctx.pictures.keep(picture.picture, &picture.bytes, &ctx.board);
                    ctx.window.to.send(ipc::kinds::PICTURE, &ipc::to_payload(&picture)).await
                }
                FromHost::Rumble(rumble) => {
                    let Some(r) = decode_payload::<RumbleDatagram>(&rumble) else {
                        continue;
                    };
                    let state = RumbleState { seat: r.seat.0, low: r.low, high: r.high };
                    ctx.event(WatchEvent::Rumble(state));
                    ctx.window.to.send(ipc::kinds::RUMBLE, &ipc::to_payload(&state)).await
                }
                FromHost::Lost => {
                    // The seat went with the connection; whatever was held
                    // was released on the host when it closed.
                    ctx.link = None;
                    forwarder.connection_lost();
                    ctx.event(WatchEvent::ConnectionLost { link: ctx.link_id });
                    ctx.report(&forwarder);
                    send_seat(&mut ctx.window.to, &forwarder).await
                }
            },
            frame = ctx.window.said.recv() => match frame {
                None => false,
                // A controller came, went or took over a seat: the window
                // hears which drives what.
                Some(frame) => {
                    if said(ctx, &mut forwarder, frame) {
                        send_seat(&mut ctx.window.to, &forwarder).await
                    } else {
                        true
                    }
                }
            },
            _ = input_tick.tick() => {
                if let Some(link) = &ctx.link {
                    for out in forwarder.tick(Instant::now()) {
                        link.input.send(out);
                    }
                }
                true
            }
            cmd = cmds.recv() => match cmd {
                Some(WatchCmd::SeatAssigned(s)) => {
                    let out = forwarder.seat_assigned(
                        s.seat,
                        s.seat_epoch,
                        s.permissions,
                        s.controller,
                        Instant::now(),
                    );
                    if let Some(link) = &ctx.link {
                        for out in out {
                            link.input.send(out);
                        }
                    }
                    ctx.report(&forwarder);
                    send_seat(&mut ctx.window.to, &forwarder).await
                }
                Some(WatchCmd::SeatReleased(s)) => {
                    forwarder.seat_released(s.seat, s.seat_epoch);
                    ctx.report(&forwarder);
                    send_seat(&mut ctx.window.to, &forwarder).await
                }
                Some(WatchCmd::SeatController(s)) => {
                    let out = forwarder.drive(s.seat, s.controller, Instant::now());
                    if let Some(link) = &ctx.link {
                        for out in out {
                            link.input.send(out);
                        }
                    }
                    ctx.report(&forwarder);
                    send_seat(&mut ctx.window.to, &forwarder).await
                }
                Some(WatchCmd::Hud(hud)) => {
                    let sent = ctx.window.to.send(ipc::kinds::HUD_STATE, &ipc::to_payload(&hud)).await;
                    ctx.last_hud = Some(hud);
                    sent
                }
                Some(WatchCmd::Board(news)) => {
                    match &news {
                        BoardNews::Snapshot(s) => {
                            ctx.board.snapshot_part(s.generation, s.first, &s.elements);
                        }
                        BoardNews::Update(u) => ctx.board.heard(u.author, &u.ops),
                        BoardNews::Cleared(c) => {
                            let me = ctx.last_hud.as_ref().and_then(|h| h.board.me);
                            ctx.board.cleared(c.generation, c.author, me);
                        }
                        BoardNews::Refused(_) => {}
                    }
                    ctx.window.to.send(ipc::kinds::BOARD, &ipc::to_payload(&news)).await
                }
                Some(WatchCmd::Volume(volume)) => {
                    ctx.options.volume = volume;
                    ctx.window.to.send(ipc::kinds::VOLUME, &ipc::to_payload(&volume)).await
                }
                Some(WatchCmd::Window(request)) => {
                    // Windows lets a background process take the foreground
                    // only if the one in front, the app, allows it first.
                    if request == WindowRequest::Raise
                        && let Some(pid) = ctx.window.pid
                    {
                        ctx.backend.allow_foreground(pid);
                    }
                    ctx.window.to.send(ipc::kinds::WINDOW, &ipc::to_payload(&request)).await
                }
                Some(WatchCmd::Reconnect(link, id)) => {
                    // Whatever the old connection's loss hasn't said yet: no
                    // seat survives a connection.
                    ctx.link = Some(*link);
                    ctx.link_id = id;
                    forwarder.connection_lost();
                    ctx.report(&forwarder);
                    send_seat(&mut ctx.window.to, &forwarder).await
                }
                Some(WatchCmd::Stop) | None => return None,
            },
            _ = net_tick.tick() => {
                let Some(link) = &ctx.link else { continue };
                let path = couch_net::video::selected_path(&link.conn);
                let secs = net_since.elapsed().as_secs_f64().max(1e-3);
                net_since = Instant::now();
                let stats = NetStats {
                    rtt_us: path.rtt.as_micros().min(u128::from(u32::MAX)) as u32,
                    relayed: path.relayed,
                    receive_bps: (std::mem::take(&mut received_bytes) as f64 * 8.0 / secs) as u32,
                };
                ctx.event(WatchEvent::Net(stats));
                payload.clear();
                stats.write(&mut payload);
                ctx.window.to.send(ipc::kinds::NET_STATS, &payload).await
            }
        };
        if !delivered {
            if ctx.window.left {
                return Some(WatchEvent::ViewerClosed);
            }
            // Its last words may still be on the way (in the pipe, or not yet
            // passed on by the reader): a Leave says the user closed it, not
            // a crash. Read until its output ends, briefly.
            let deadline = tokio::time::Instant::now() + LAST_WORDS;
            while let Ok(Some(frame)) =
                tokio::time::timeout_at(deadline, ctx.window.said.recv()).await
            {
                let _ = said(ctx, &mut forwarder, frame);
            }
            if ctx.window.left {
                return Some(WatchEvent::ViewerClosed);
            }
            if !restart(ctx, &mut forwarder).await {
                return Some(WatchEvent::ViewerCrashed);
            }
        }
    }
}

/// Something the viewer said. `true` if which seat each controller drives
/// changed: the window should hear it.
fn said(ctx: &mut Ctx, forwarder: &mut Forwarder, frame: IpcFrame) -> bool {
    match frame.kind {
        ipc::kinds::INPUT => {
            if let Some(local) = ipc::from_payload::<LocalInput>(&frame.payload) {
                let before = (forwarder.controllers(), forwarder.seat_state());
                let out = forwarder.local(local, Instant::now());
                if let Some(link) = &ctx.link {
                    for out in out {
                        link.input.send(out);
                    }
                }
                let seats = forwarder.seat_state();
                if forwarder.controllers() != before.0 || seats != before.1 {
                    ctx.report(forwarder);
                }
                // With several seats, a controller that went leaves its seat
                // (M18): given back, as the user would.
                for seat in forwarder.take_left() {
                    ctx.event(WatchEvent::Action(UserAction::GiveBack(seat.0)));
                }
                return seats != before.1;
            }
        }
        // Right after the slots that say they changed (P12).
        ipc::kinds::CONTROLLER_NAMES => {
            if let Some(names) = ipc::from_payload::<ControllerNames>(&frame.payload) {
                forwarder.name_controllers(&names.controllers);
                ctx.report(forwarder);
            }
        }
        ipc::kinds::FRAME_TIMING if ctx.options.measure => {
            if let Some(timing) = FrameTiming::read(&frame.payload) {
                ctx.event(WatchEvent::Timing(timing));
            }
        }
        ipc::kinds::AUDIO_STATS if ctx.options.measure => {
            if let Some(stats) = ipc::from_payload(&frame.payload) {
                ctx.event(WatchEvent::AudioStats(stats));
            }
        }
        ipc::kinds::KEYFRAME_REQUEST => {
            if let Some(link) = &ctx.link {
                link.video.note_decode_error();
                let epoch = link.video.latest_epoch();
                ctx.event(WatchEvent::KeyframeNeeded { epoch });
            }
        }
        ipc::kinds::VOLUME_CHANGED => {
            if let Some(volume) = ipc::from_payload::<StreamVolume>(&frame.payload) {
                ctx.options.volume = volume;
                ctx.event(WatchEvent::Volume(volume));
            }
        }
        ipc::kinds::WINDOW_STATE => {
            if let Some(state) = ipc::from_payload::<WindowState>(&frame.payload) {
                // A restarted window starts with the bar and stats as they
                // were (full screen is the user's to choose again).
                ctx.options.viewer.hide_bar = !state.bar;
                ctx.options.viewer.hide_stats = !state.stats;
                ctx.event(WatchEvent::Window(state));
            }
        }
        // The user's own drawing (M27a), as the window did it: the window's
        // copy of the board has it already, so ours does too.
        ipc::kinds::DRAW => {
            if let Some(draw) = ipc::from_payload::<Draw>(&frame.payload) {
                // Made before a clear of ours: the host drops it, and the
                // window has dropped it too.
                if let Some(me) = ctx.last_hud.as_ref().and_then(|h| h.board.me)
                    && draw.generation == ctx.board.generation()
                {
                    ctx.board.heard(me, &draw.ops);
                }
                ctx.event(WatchEvent::Draw(draw));
            }
        }
        // The user's picture (M27e), as the window made it: ours too, as
        // its drawing is, and kept for a restarted window.
        ipc::kinds::PICTURE_UP => {
            if let Some(up) = ipc::from_payload::<PictureUp>(&frame.payload)
                && let Some(picture) = up.element.picture
            {
                if let Some(me) = ctx.last_hud.as_ref().and_then(|h| h.board.me)
                    && up.generation == ctx.board.generation()
                {
                    ctx.board
                        .heard(me, &[couch_proto::board::BoardOp::Put(up.element.clone())]);
                }
                ctx.pictures.keep(picture, &up.bytes, &ctx.board);
                ctx.event(WatchEvent::Picture(up));
            }
        }
        ipc::kinds::USER_ACTION => {
            if let Some(action) = ipc::from_payload::<UserAction>(&frame.payload) {
                if matches!(action, UserAction::Leave) {
                    ctx.window.left = true;
                }
                ctx.event(WatchEvent::Action(action));
            }
        }
        _ => {}
    }
    false
}

// ---- Pictures on the board (M27e) ----

/// Reads the host's pictures as they come, until dropped.
struct PictureReader(JoinHandle<()>);

impl Drop for PictureReader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The host's pictures on `streams`: each read whole within its limits, of
/// our session, and what its id says (the SHA-256 of its bytes), or
/// dropped. The window decodes it, with limits of its own.
fn receive_pictures(
    streams: &Streams,
    session: Option<u64>,
) -> (PictureReader, mpsc::Receiver<Picture>) {
    /// Pictures read at once: the most a newcomer gets is the board's.
    const AT_ONCE: usize = 4;
    let mut incoming = streams.subscribe(couch_proto::media::stream_kind::PICTURE);
    let (tx, rx) = mpsc::channel(couch_proto::board::MAX_BOARD_PICTURES);
    let task = tokio::spawn(async move {
        let permits = Arc::new(tokio::sync::Semaphore::new(AT_ONCE));
        let mut readers = tokio::task::JoinSet::new();
        // Each stream taken at once, then read as a permit frees: one left
        // waiting in the acceptor's queue could be stopped, and the host
        // sends each picture once.
        while let Some(recv) = incoming.recv().await {
            let permits = permits.clone();
            let tx = tx.clone();
            readers.spawn(async move {
                let Ok(_permit) = permits.acquire_owned().await else {
                    return;
                };
                let Some((header, bytes)) = couch_net::pictures::read_picture(recv).await else {
                    return;
                };
                if session.is_some_and(|s| header.session.0 != s) {
                    return;
                }
                let bytes = tokio::task::spawn_blocking(move || {
                    (couch_media::picture::picture_id(&bytes) == header.picture).then_some(bytes)
                })
                .await
                .ok()
                .flatten();
                if let Some(bytes) = bytes {
                    let _ = tx
                        .send(Picture {
                            picture: header.picture,
                            bytes,
                        })
                        .await;
                }
            });
            // Finished readers go as they finish.
            while readers.try_join_next().is_some() {}
        }
    });
    (PictureReader(task), rx)
}

/// The pictures the window has (M27e), for a restarted one: every one the
/// board shows, and a few more lately gone (an element put back by an undo
/// shows its picture again), within [`Pictures::KEEP`].
#[derive(Default)]
struct Pictures {
    /// Oldest first.
    order: std::collections::VecDeque<PictureId>,
    bytes: std::collections::HashMap<PictureId, Vec<u8>>,
    total: usize,
}

impl Pictures {
    /// Bytes kept at most, but for those the board shows.
    const KEEP: usize = 2 * couch_proto::board::MAX_BOARD_PICTURE_BYTES;
    /// Pictures kept at most, but for those the board shows.
    const KEEP_COUNT: usize = 2 * couch_proto::board::MAX_BOARD_PICTURES;

    fn keep(&mut self, id: PictureId, bytes: &[u8], board: &Replica) {
        if self.bytes.contains_key(&id) {
            return;
        }
        self.total += bytes.len();
        self.bytes.insert(id, bytes.to_vec());
        self.order.push_back(id);
        if self.total <= Self::KEEP && self.order.len() <= Self::KEEP_COUNT {
            return;
        }
        // Those shown are kept, but never more than the board holds: a host
        // naming more can't make us keep them all.
        let shown: std::collections::BTreeSet<PictureId> = shown_by(board)
            .into_iter()
            .take(couch_proto::board::MAX_BOARD_PICTURES)
            .collect();
        let mut kept = std::collections::VecDeque::new();
        while let Some(old) = self.order.pop_front() {
            let over = self.total > Self::KEEP || self.order.len() + kept.len() >= Self::KEEP_COUNT;
            if over && old != id && !shown.contains(&old) {
                if let Some(b) = self.bytes.remove(&old) {
                    self.total -= b.len();
                }
            } else {
                kept.push_back(old);
            }
        }
        self.order = kept;
    }

    /// Those the board shows, for a new window.
    fn shown(&self, board: &Replica) -> Vec<Picture> {
        shown_by(board)
            .into_iter()
            .filter_map(|id| {
                self.bytes.get(&id).map(|bytes| Picture {
                    picture: id,
                    bytes: bytes.clone(),
                })
            })
            .collect()
    }
}

fn shown_by(board: &Replica) -> std::collections::BTreeSet<PictureId> {
    board
        .snapshot()
        .into_iter()
        .filter_map(|a| a.element.picture)
        .collect()
}
