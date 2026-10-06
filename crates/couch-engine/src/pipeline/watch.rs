//! A member's side of watching: `couch-viewer` shows the session, and local
//! input goes to the host while this member holds the seat.
//!
//! The viewer process gets the frames and sound as they arrive, the HUD
//! state and seat changes; it sends back local input, frame timings and what
//! the user asks for. Input goes through `couch_input::forward`, whose
//! heartbeat stops when the viewer goes quiet, so a hung viewer lets the
//! host's dead-man timer release whatever it held.

use std::time::{Duration, Instant};

use couch_input::{Permissions, forward::Forwarder};
use couch_media::{MediaError, clock::Clock};
use couch_net::{
    Connection, audio::receive_audio, datagrams::Datagrams, input::InputSender, streams::Streams,
    video::ViewerVideo,
};
use couch_proto::{
    control::{SeatAssigned, SeatReleased},
    input::RumbleDatagram,
    ipc::{
        self, AudioPacket, AudioStats, FrameTiming, HudState, IpcFrame, LocalInput, NetStats,
        RumbleState, SeatState, UserAction, VideoFrameHeader,
    },
    media::{datagram, decode_payload},
};
use tokio::{io::AsyncWrite, sync::mpsc, task::JoinHandle, time::MissedTickBehavior};

use super::{Backend, ViewerOptions, ViewerProcess, ipc as pipe};

/// After asking the viewer to close, how long to let it finish (it releases
/// what it held) before ending it.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Default)]
pub struct WatchOptions {
    /// Video that names any other session is refused; `None` takes any
    /// (`couch-cli view`).
    pub session: Option<u64>,
    pub viewer: ViewerOptions,
    /// Report every presented frame's timing and the audio stats.
    pub measure: bool,
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
    /// The viewer window closed, or the viewer died: watching is over.
    ViewerClosed,
    /// The connection to the host went: watching is over.
    ConnectionLost,
}

enum WatchCmd {
    SeatAssigned(SeatAssigned),
    SeatReleased(SeatReleased),
    Hud(HudState),
    Stop,
}

/// One session being watched. Dropping it closes the viewer window.
pub struct Watch {
    cmds: mpsc::UnboundedSender<WatchCmd>,
    task: Option<JoinHandle<()>>,
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
        backend: &dyn Backend,
        options: WatchOptions,
    ) -> Result<(Self, mpsc::UnboundedReceiver<WatchEvent>), MediaError> {
        let viewer = backend.launch_viewer(&options.viewer)?;
        let clock = backend.clock();
        let ctx = Ctx {
            video: ViewerVideo::start(conn.clone(), streams, options.session, clock.clone()),
            audio: receive_audio(datagrams),
            rumbles: datagrams.subscribe(datagram::RUMBLE),
            input: InputSender::start(conn.clone()),
            conn,
            clock,
            measure: options.measure,
        };
        let (events, events_rx) = mpsc::unbounded_channel();
        let (cmds, cmds_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(ctx, viewer, cmds_rx, events));
        Ok((
            Self {
                cmds,
                task: Some(task),
            },
            events_rx,
        ))
    }

    /// The host granted this member a seat.
    pub fn seat_assigned(&self, message: SeatAssigned) {
        let _ = self.cmds.send(WatchCmd::SeatAssigned(message));
    }

    /// The host ended this member's hold on a seat.
    pub fn seat_released(&self, message: SeatReleased) {
        let _ = self.cmds.send(WatchCmd::SeatReleased(message));
    }

    pub fn set_hud(&self, hud: HudState) {
        let _ = self.cmds.send(WatchCmd::Hud(hud));
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

struct Ctx {
    conn: Connection,
    video: ViewerVideo,
    audio: mpsc::Receiver<couch_proto::media::AudioDatagram>,
    rumbles: mpsc::Receiver<bytes::Bytes>,
    input: InputSender,
    clock: std::sync::Arc<dyn Clock>,
    measure: bool,
}

/// Writes to the viewer; `false` once it is gone.
struct ToViewer {
    stdin: std::pin::Pin<Box<dyn AsyncWrite + Send>>,
    scratch: Vec<u8>,
}

impl ToViewer {
    async fn send(&mut self, kind: u8, payload: &[u8]) -> bool {
        pipe::write_frame(&mut self.stdin, kind, payload, &mut self.scratch)
            .await
            .is_ok()
    }
}

async fn run(
    mut ctx: Ctx,
    viewer: ViewerProcess,
    mut cmds: mpsc::UnboundedReceiver<WatchCmd>,
    events: mpsc::UnboundedSender<WatchEvent>,
) {
    let ViewerProcess {
        stdin,
        mut stdout,
        guard,
    } = viewer;
    let mut to_viewer = ToViewer {
        stdin,
        scratch: Vec::new(),
    };
    // What the viewer says, until its stdout closes.
    let (said_tx, mut said) = mpsc::unbounded_channel::<IpcFrame>();
    let reader = tokio::spawn(async move {
        while let Ok(Some(frame)) = pipe::read_frame(&mut stdout).await {
            if said_tx.send(frame).is_err() {
                return;
            }
        }
    });

    let end = watch(&mut ctx, &mut to_viewer, &mut said, &mut cmds, &events).await;
    if let Some(end) = end {
        let _ = events.send(end);
    }

    // Let the viewer close on its own (it releases what it held), then end
    // it if it doesn't.
    let _ = to_viewer.send(ipc::kinds::SHUTDOWN, &[]).await;
    drop(to_viewer);
    let _ = tokio::time::timeout(CLOSE_WAIT, reader).await;
    drop(guard);
}

/// The watching loop; returns how it ended, unless it was asked to stop.
async fn watch(
    ctx: &mut Ctx,
    to_viewer: &mut ToViewer,
    said: &mut mpsc::UnboundedReceiver<IpcFrame>,
    cmds: &mut mpsc::UnboundedReceiver<WatchCmd>,
    events: &mpsc::UnboundedSender<WatchEvent>,
) -> Option<WatchEvent> {
    let closed = Some(WatchEvent::ViewerClosed);
    if !to_viewer
        .send(ipc::kinds::HELLO, &ipc::IPC_VERSION.to_be_bytes())
        .await
    {
        return closed;
    }
    let mut forwarder = Forwarder::new();
    let mut payload = Vec::new();
    let mut input_tick = tokio::time::interval(Duration::from_millis(10));
    input_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut net_tick = tokio::time::interval(Duration::from_millis(250));
    let mut net_since = Instant::now();
    let mut received_bytes = 0u64;

    loop {
        tokio::select! {
            frame = ctx.video.frames.recv() => {
                let Some(frame) = frame else { return Some(WatchEvent::ConnectionLost) };
                received_bytes += frame.data.len() as u64;
                if frame.epoch < ctx.video.latest_epoch() {
                    continue;
                }
                payload.clear();
                VideoFrameHeader {
                    epoch: frame.epoch,
                    seq: frame.header.seq,
                    keyframe: frame.header.keyframe,
                    capture_us: frame.header.capture_us,
                    encode_us: frame.header.encode_us,
                    arrived_us: frame.arrived.0,
                }
                .write(&frame.data, &mut payload);
                if !to_viewer.send(ipc::kinds::VIDEO_FRAME, &payload).await {
                    return closed;
                }
            }
            Some(packet) = ctx.audio.recv() => {
                let packet = ipc::to_payload(&AudioPacket {
                    seq: packet.seq,
                    capture_us: packet.capture_us,
                    arrived_us: ctx.clock.now().0,
                    packet: packet.packet,
                });
                if !to_viewer.send(ipc::kinds::AUDIO_PACKET, &packet).await {
                    return closed;
                }
            }
            frame = said.recv() => {
                let Some(frame) = frame else { return closed };
                match frame.kind {
                    ipc::kinds::INPUT => {
                        if let Some(local) = ipc::from_payload::<LocalInput>(&frame.payload) {
                            for out in forwarder.local(local, Instant::now()) {
                                ctx.input.send(out);
                            }
                        }
                    }
                    ipc::kinds::FRAME_TIMING if ctx.measure => {
                        if let Some(timing) = FrameTiming::read(&frame.payload) {
                            let _ = events.send(WatchEvent::Timing(timing));
                        }
                    }
                    ipc::kinds::AUDIO_STATS if ctx.measure => {
                        if let Some(stats) = ipc::from_payload(&frame.payload) {
                            let _ = events.send(WatchEvent::AudioStats(stats));
                        }
                    }
                    ipc::kinds::KEYFRAME_REQUEST => {
                        ctx.video.note_decode_error();
                        let epoch = ctx.video.latest_epoch();
                        let _ = events.send(WatchEvent::KeyframeNeeded { epoch });
                    }
                    ipc::kinds::USER_ACTION => {
                        if let Some(action) = ipc::from_payload(&frame.payload) {
                            let _ = events.send(WatchEvent::Action(action));
                        }
                    }
                    _ => {}
                }
            }
            _ = input_tick.tick() => {
                for out in forwarder.tick(Instant::now()) {
                    ctx.input.send(out);
                }
            }
            cmd = cmds.recv() => {
                match cmd {
                    Some(WatchCmd::SeatAssigned(s)) => {
                        let out = forwarder.seat_assigned(s.seat, s.seat_epoch, s.permissions, Instant::now());
                        for out in out {
                            ctx.input.send(out);
                        }
                    }
                    Some(WatchCmd::SeatReleased(s)) => forwarder.seat_released(s.seat, s.seat_epoch),
                    Some(WatchCmd::Hud(hud)) => {
                        if !to_viewer.send(ipc::kinds::HUD_STATE, &ipc::to_payload(&hud)).await {
                            return closed;
                        }
                        continue;
                    }
                    Some(WatchCmd::Stop) | None => return None,
                }
                let state = SeatState {
                    holding: forwarder.holding().is_some(),
                    permissions: forwarder.holding().map_or(Permissions::empty(), |h| h.1),
                };
                if !to_viewer.send(ipc::kinds::SEAT, &ipc::to_payload(&state)).await {
                    return closed;
                }
            }
            Some(rumble) = ctx.rumbles.recv() => {
                let Some(r) = decode_payload::<RumbleDatagram>(&rumble) else {
                    continue;
                };
                let state = RumbleState { low: r.low, high: r.high };
                let _ = events.send(WatchEvent::Rumble(state));
                if !to_viewer.send(ipc::kinds::RUMBLE, &ipc::to_payload(&state)).await {
                    return closed;
                }
            }
            _ = net_tick.tick() => {
                let path = couch_net::video::selected_path(&ctx.conn);
                let secs = net_since.elapsed().as_secs_f64().max(1e-3);
                net_since = Instant::now();
                let stats = NetStats {
                    rtt_us: path.rtt.as_micros().min(u128::from(u32::MAX)) as u32,
                    relayed: path.relayed,
                    receive_bps: (std::mem::take(&mut received_bytes) as f64 * 8.0 / secs) as u32,
                };
                let _ = events.send(WatchEvent::Net(stats));
                payload.clear();
                stats.write(&mut payload);
                if !to_viewer.send(ipc::kinds::NET_STATS, &payload).await {
                    return closed;
                }
            }
            _ = ctx.conn.closed() => return Some(WatchEvent::ConnectionLost),
        }
    }
}
