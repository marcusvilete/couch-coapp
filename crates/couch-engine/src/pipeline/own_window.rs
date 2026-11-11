//! The host's own window of its stream (M27d, ADR 0013): `couch-viewer
//! --own-stream`, fed frames the host pipeline encodes for it alone
//! ([`super::host::HostPipeline::open_own`]), never anything from the
//! network.
//!
//! It is a stream window with no seat and no input path: whatever input it
//! says is dropped here, unread, and never reaches a forwarder, the seat
//! router or an injector. What it gives back is the host's own drawing
//! ([`OwnEvent::Draw`], taken by the session as the host's, under the same
//! checks), keyframe requests, its state, and its closing.

use std::{sync::Arc, time::Duration};

use couch_media::{MediaError, clock::Clock};
use couch_proto::ipc::{
    self, BoardNews, Draw, HudState, IpcFrame, ShortcutKeys, UserAction, VideoFrameHeader,
    WindowRequest, WindowState,
};
use tokio::{sync::mpsc, task::JoinHandle};

use super::{
    Backend, ViewerOptions,
    host::OwnFrame,
    watch::{ToViewer, Window},
};

/// What the host's own window reports.
#[derive(Debug, Clone)]
pub enum OwnEvent {
    /// The host drew (as itself).
    Draw(Draw),
    /// Its decoder needs a keyframe.
    KeyframeNeeded,
    /// Its full screen, bar, stats and drawing mode, as they are now.
    Window(WindowState),
    /// The host closed it.
    Closed,
    /// It went without being closed (a crash), or its video ended (its
    /// encoder failed): it isn't started again; the console opens another.
    Gone,
}

enum OwnCmd {
    Hud(HudState),
    Board(BoardNews),
    Window(WindowRequest),
    Stop,
}

/// The host's own window, while open. Dropping it closes the window.
pub struct OwnWindow {
    cmds: mpsc::UnboundedSender<OwnCmd>,
    task: Option<JoinHandle<()>>,
    /// Its process, to tell when it is in front; `None` for fakes.
    pid: Option<u32>,
}

impl OwnWindow {
    /// Starts the window on `frames` (from `open_own`). Call inside a tokio
    /// runtime.
    pub fn start(
        backend: Arc<dyn Backend>,
        frames: mpsc::Receiver<OwnFrame>,
        viewer: &ViewerOptions,
        shortcuts: ShortcutKeys,
    ) -> Result<(Self, mpsc::UnboundedReceiver<OwnEvent>), MediaError> {
        let options = ViewerOptions {
            own_stream: true,
            ..viewer.clone()
        };
        let window = Window::launch(&*backend, &options)?;
        let pid = window.pid;
        let (events, events_rx) = mpsc::unbounded_channel();
        let (cmds, cmds_rx) = mpsc::unbounded_channel();
        let clock = backend.clock();
        let task = tokio::spawn(run(
            Ctx {
                window,
                backend,
                clock,
                frames,
                events,
                shortcuts,
            },
            cmds_rx,
        ));
        Ok((
            Self {
                cmds,
                task: Some(task),
                pid,
            },
            events_rx,
        ))
    }

    /// Its process, if known.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn set_hud(&self, hud: HudState) {
        let _ = self.cmds.send(OwnCmd::Hud(hud));
    }

    /// What the session says of the board, to the host.
    pub fn board(&self, news: BoardNews) {
        let _ = self.cmds.send(OwnCmd::Board(news));
    }

    /// Something asked of the window: drawing mode from the host's key, to
    /// come to the front.
    pub fn window(&self, request: WindowRequest) {
        let _ = self.cmds.send(OwnCmd::Window(request));
    }

    /// Closes the window, letting it finish first.
    pub async fn stop(mut self) {
        let _ = self.cmds.send(OwnCmd::Stop);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for OwnWindow {
    fn drop(&mut self) {
        // The task closes the window on its way out.
        let _ = self.cmds.send(OwnCmd::Stop);
    }
}

struct Ctx {
    window: Window,
    backend: Arc<dyn Backend>,
    clock: Arc<dyn Clock>,
    frames: mpsc::Receiver<OwnFrame>,
    events: mpsc::UnboundedSender<OwnEvent>,
    shortcuts: ShortcutKeys,
}

async fn run(mut ctx: Ctx, mut cmds: mpsc::UnboundedReceiver<OwnCmd>) {
    let end = serve(&mut ctx, &mut cmds).await;
    if let Some(end) = end {
        let _ = ctx.events.send(end);
    }
    // A window that stops reading can't keep it: dropped, it is ended.
    let _ = tokio::time::timeout(CLOSE_LIMIT, ctx.window.close()).await;
}

/// How long a write to the window may take before it counts as gone, once it
/// has spoken: it is on this PC, so only a hung window takes this long.
const SEND_LIMIT: Duration = Duration::from_secs(2);
/// The same until it first speaks: a cold start (its DLLs scanned, a slow
/// disk) may keep it from reading for a while, and a keyframe fills the pipe.
const START_LIMIT: Duration = Duration::from_secs(15);
/// How long closing the window may take, past its own wait.
const CLOSE_LIMIT: Duration = Duration::from_secs(5);

/// Writes to the window; `false` once it is gone, or hung past `limit`.
async fn send(to: &mut ToViewer, kind: u8, payload: &[u8], limit: Duration) -> bool {
    tokio::time::timeout(limit, to.send(kind, payload))
        .await
        .unwrap_or(false)
}

/// The window's life; how it ended, unless it was asked to stop.
async fn serve(ctx: &mut Ctx, cmds: &mut mpsc::UnboundedReceiver<OwnCmd>) -> Option<OwnEvent> {
    // The write limit, a long one until the window first speaks.
    let mut limit = START_LIMIT;
    let to = &mut ctx.window.to;
    let hello = send(
        to,
        ipc::kinds::HELLO,
        &ipc::IPC_VERSION.to_be_bytes(),
        limit,
    )
    .await
        && send(
            to,
            ipc::kinds::SHORTCUTS,
            &ipc::to_payload(&ctx.shortcuts),
            limit,
        )
        .await;
    if !hello {
        return Some(OwnEvent::Gone);
    }
    let mut payload = Vec::new();
    // The HUD as last sent: the actor sends it after every change of the
    // room, most of which change nothing here.
    let mut last_hud: Option<HudState> = None;
    loop {
        let delivered = tokio::select! {
            frame = ctx.frames.recv() => {
                // The pipeline closed its side: the encoder failed, or the
                // session is ending.
                let Some(frame) = frame else {
                    return Some(OwnEvent::Gone);
                };
                payload.clear();
                VideoFrameHeader {
                    epoch: frame.epoch,
                    seq: frame.seq,
                    keyframe: frame.keyframe,
                    codec: frame.codec,
                    capture_us: frame.capture_us,
                    encode_us: frame.encode_us,
                    arrived_us: ctx.clock.now().0,
                }
                .write(&frame.data, &mut payload);
                send(&mut ctx.window.to, ipc::kinds::VIDEO_FRAME, &payload, limit).await
            }
            said = ctx.window.said.recv() => match said {
                Some(frame) => {
                    limit = SEND_LIMIT;
                    if let Some(event) = said_event(&mut ctx.window, frame) {
                        let closed = matches!(event, OwnEvent::Closed);
                        let _ = ctx.events.send(event);
                        if closed {
                            return None;
                        }
                    }
                    true
                }
                None => false,
            },
            cmd = cmds.recv() => match cmd {
                Some(OwnCmd::Hud(hud)) if last_hud.as_ref() == Some(&hud) => true,
                Some(OwnCmd::Hud(hud)) => {
                    let sent = send(&mut ctx.window.to, ipc::kinds::HUD_STATE, &ipc::to_payload(&hud), limit).await;
                    last_hud = Some(hud);
                    sent
                }
                Some(OwnCmd::Board(news)) => {
                    send(&mut ctx.window.to, ipc::kinds::BOARD, &ipc::to_payload(&news), limit).await
                }
                Some(OwnCmd::Window(request)) => {
                    // Windows lets a background process take the foreground
                    // only if the one in front allows it first.
                    if request == WindowRequest::Raise
                        && let Some(pid) = ctx.window.pid
                    {
                        ctx.backend.allow_foreground(pid);
                    }
                    send(&mut ctx.window.to, ipc::kinds::WINDOW, &ipc::to_payload(&request), limit).await
                }
                Some(OwnCmd::Stop) | None => return None,
            },
        };
        if !delivered {
            // The window went: closed by the host (its last word said so),
            // or not.
            return Some(if ctx.window.left {
                OwnEvent::Closed
            } else {
                OwnEvent::Gone
            });
        }
    }
}

/// What the window said, as an event; its input is dropped unread: the
/// host's own window has no seat and no input path.
fn said_event(window: &mut Window, frame: IpcFrame) -> Option<OwnEvent> {
    match frame.kind {
        ipc::kinds::KEYFRAME_REQUEST => Some(OwnEvent::KeyframeNeeded),
        ipc::kinds::WINDOW_STATE => ipc::from_payload(&frame.payload).map(OwnEvent::Window),
        ipc::kinds::DRAW => ipc::from_payload(&frame.payload).map(OwnEvent::Draw),
        ipc::kinds::USER_ACTION => match ipc::from_payload::<UserAction>(&frame.payload)? {
            UserAction::Leave => {
                window.left = true;
                Some(OwnEvent::Closed)
            }
            // Asking for or giving back a seat: there is none.
            _ => None,
        },
        _ => None,
    }
}
