//! Capture on a Linux host (M16c, ADR 0012): the desktop's screen-sharing
//! portal (`org.freedesktop.portal.ScreenCast`) asks the user which window
//! or screen to share, and its frames come over PipeWire.
//!
//! - **Starting returns at once:** the portal's dialog waits on the user,
//!   and the capture is asked for its first frame meanwhile; dropping the
//!   capture closes the portal's session, which ends a dialog still open.
//! - **The choice is remembered** (the portal's restore token, kept until
//!   the user revokes it): the next stream starts without the dialog.
//! - **Frames in memory** (BGRx, BGRA, RGBx or RGBA in shared memory), each
//!   copied out as BGRA and stamped on [`crate::clock`]'s timeline. The
//!   newest frame waits for the pipeline; an older one not yet taken is
//!   replaced.
//! - **The shared window closing, or the user stopping the share** from the
//!   desktop, ends the capture as its source closing.

use std::{
    os::fd::OwnedFd,
    sync::{Arc, Condvar, Mutex, PoisonError, mpsc},
    time::{Duration, Instant},
};

use ashpd::desktop::{
    PersistMode, ResponseError,
    screencast::{CursorMode, Screencast, SourceType},
};
use couch_media::{BgraFrame, CaptureEvent, Capturer, MediaError, VideoFrame};
use futures_util::StreamExt;
use pipewire as pw;
use pw::{
    properties::properties,
    spa::{
        self,
        buffer::ChunkFlags,
        param::{
            ParamType,
            format::{FormatProperties, MediaSubtype, MediaType},
            format_utils,
            video::{VideoFormat, VideoInfoRaw},
        },
        pod::{Pod, Value, serialize::PodSerializer},
        utils::{Direction, Fraction, Rectangle, SpaTypes},
    },
    stream::{StreamFlags, StreamState},
};

/// How long the portal may take to open a session before capture fails.
const OPEN_WAIT: Duration = Duration::from_secs(5);
/// How long dropping the capture waits for the portal's session to close.
const CLOSE_WAIT: Duration = Duration::from_secs(2);
/// The largest picture asked for.
const MAX_SIZE: u32 = 8192;

/// What to start a capture with.
pub struct Options {
    /// The stream's frame rate: the most frames a second asked for.
    pub fps: u32,
    /// The portal's token for the last choice, to share it again without
    /// the dialog. Each is good for one start.
    pub restore_token: Option<String>,
    /// Called with the next token once the user has chosen (never logged).
    pub on_token: Box<dyn FnOnce(String) + Send>,
    /// How long the dialog may wait for an answer before nothing counts as
    /// shared and it is closed: `Some` when nobody may be at the PC (a
    /// stream started from afar, M16c); `None` waits on the user.
    pub answer_within: Option<Duration>,
}

/// A capture through the portal, until dropped.
pub struct PortalCapture {
    inbox: Arc<Inbox>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    /// Closed when the portal task ends, its session closed.
    done: mpsc::Receiver<()>,
}

/// Starts a capture: the portal's dialog (or the remembered choice), then
/// frames. Returns at once; what fails later comes as the capture's error.
pub fn start(options: Options) -> Result<PortalCapture, String> {
    let runtime = crate::portal::runtime()?;
    let inbox = Arc::new(Inbox::default());
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let task_inbox = inbox.clone();
    runtime.spawn(async move {
        let _done = done_tx;
        share(options, &task_inbox, stop_rx).await;
    });
    Ok(PortalCapture {
        inbox,
        stop: Some(stop_tx),
        done: done_rx,
    })
}

impl Capturer for PortalCapture {
    fn next_event(&mut self, timeout: Duration) -> Result<Option<CaptureEvent>, MediaError> {
        let deadline = Instant::now() + timeout;
        let mut state = self.inbox.lock();
        loop {
            if let Some(frame) = state.frame.take() {
                return Ok(Some(CaptureEvent::Frame(VideoFrame::Cpu(frame))));
            }
            match state.end.take() {
                Some(End::Closed) => return Ok(Some(CaptureEvent::SourceClosed)),
                Some(End::Failed(e)) => return Err(MediaError::Backend(e)),
                Some(End::NothingShared(e)) => return Err(MediaError::NothingShared(e)),
                None => {}
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            state = self
                .inbox
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

impl Drop for PortalCapture {
    fn drop(&mut self) {
        // The task stops PipeWire's thread and closes the session (and any
        // dialog of it still open).
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = self.done.recv_timeout(CLOSE_WAIT);
    }
}

/// How a capture ended.
enum End {
    /// The shared window or screen went, or the user stopped sharing.
    Closed,
    Failed(String),
    /// The dialog was dismissed, or not answered in time.
    NothingShared(String),
}

/// What the capture's threads hand the pipeline: the newest frame, and how
/// it ended. The first end is kept.
#[derive(Default)]
struct Inbox {
    state: Mutex<InboxState>,
    changed: Condvar,
}

#[derive(Default)]
struct InboxState {
    frame: Option<BgraFrame>,
    end: Option<End>,
    ended: bool,
}

impl Inbox {
    fn lock(&self) -> std::sync::MutexGuard<'_, InboxState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn frame(&self, frame: BgraFrame) {
        let mut state = self.lock();
        if !state.ended {
            state.frame = Some(frame);
            self.changed.notify_all();
        }
    }

    fn end(&self, end: End) {
        let mut state = self.lock();
        if !state.ended {
            state.ended = true;
            state.end = Some(end);
            self.changed.notify_all();
        }
    }
}

/// The portal's side, on the portals' runtime: a session, the user's
/// choice, then PipeWire's thread until the stream ends or `stop`.
async fn share(options: Options, inbox: &Arc<Inbox>, mut stop: tokio::sync::oneshot::Receiver<()>) {
    let Options {
        fps,
        restore_token,
        on_token,
        answer_within,
    } = options;
    let opened = tokio::time::timeout(OPEN_WAIT, async {
        let proxy = Screencast::new().await?;
        let session = proxy.create_session().await?;
        Ok::<_, ashpd::Error>((proxy, session))
    });
    // Not cut short by `stop`: a session the portal made would be left open.
    let (proxy, session) = match opened.await {
        Ok(Ok(opened)) => opened,
        Ok(Err(e)) => {
            inbox.end(End::Failed(format!(
                "the desktop can't share its screen here: {e}"
            )));
            return;
        }
        Err(_) => {
            inbox.end(End::Failed(
                "the desktop's screen-sharing portal didn't answer".into(),
            ));
            return;
        }
    };
    // Stopped meanwhile (or the capture dropped).
    if !matches!(
        stop.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ) {
        let _ = session.close().await;
        return;
    }
    // The dialog, unless the token still names a choice: nothing to wait on
    // but the user, or Stop, or the time to answer in.
    let unanswered = async {
        match answer_within {
            Some(wait) => tokio::time::sleep(wait).await,
            None => std::future::pending().await,
        }
    };
    let chosen = async {
        let cursor = if proxy
            .available_cursor_modes()
            .await?
            .contains(CursorMode::Embedded)
        {
            CursorMode::Embedded
        } else {
            CursorMode::Hidden
        };
        proxy
            .select_sources(
                &session,
                cursor,
                SourceType::Monitor | SourceType::Window,
                false,
                restore_token.as_deref(),
                PersistMode::ExplicitlyRevoked,
            )
            .await?;
        let streams = proxy.start(&session, None).await?.response()?;
        let fd = proxy.open_pipe_wire_remote(&session).await?;
        Ok::<_, ashpd::Error>((streams, fd))
    };
    // In this order: a choice made as the time runs out still counts (and
    // its token is kept).
    let (streams, fd) = tokio::select! {
        biased;
        _ = &mut stop => {
            let _ = session.close().await;
            return;
        }
        chosen = chosen => match chosen {
            Ok(chosen) => chosen,
            Err(e) => {
                inbox.end(match e {
                    ashpd::Error::Response(ResponseError::Cancelled) => {
                        End::NothingShared("nothing was chosen to share".into())
                    }
                    e => End::Failed(format!("the desktop didn't share a window or screen: {e}")),
                });
                let _ = session.close().await;
                return;
            }
        },
        // Closing the session closes its dialog.
        () = unanswered => {
            inbox.end(End::NothingShared(
                "nobody answered the screen-sharing dialog at this PC".into(),
            ));
            let _ = session.close().await;
            return;
        }
    };
    if let Some(token) = streams.restore_token() {
        on_token(token.to_owned());
    }
    let Some(node) = streams.streams().first().map(|s| s.pipe_wire_node_id()) else {
        inbox.end(End::Failed("the desktop shared nothing".into()));
        let _ = session.close().await;
        return;
    };
    let (quit_tx, quit_rx) = pw::channel::channel::<()>();
    let (ended_tx, ended_rx) = tokio::sync::oneshot::channel::<()>();
    let thread_inbox = inbox.clone();
    let thread = std::thread::Builder::new()
        .name("pipewire-video".into())
        .spawn(move || {
            if let Err(e) = video(Some(fd), node, fps, &thread_inbox, quit_rx) {
                thread_inbox.end(End::Failed(format!("PipeWire: {e}")));
            }
            let _ = ended_tx.send(());
        });
    let thread = match thread {
        Ok(thread) => thread,
        Err(e) => {
            inbox.end(End::Failed(format!("no thread for PipeWire: {e}")));
            let _ = session.close().await;
            return;
        }
    };
    // The user stopping the share from the desktop closes the session.
    let mut closed = session.receive_closed().await.ok().map(Box::pin);
    let session_closed = async {
        match closed.as_mut() {
            Some(closed) => closed.next().await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        _ = &mut stop => {}
        ended = ended_rx => {
            if ended.is_err() {
                inbox.end(End::Failed("PipeWire's thread stopped".into()));
            }
        }
        _ = session_closed => inbox.end(End::Closed),
    }
    let _ = quit_tx.send(());
    let _ = tokio::task::spawn_blocking(move || thread.join()).await;
    let _ = session.close().await;
}

/// What the stream's callbacks keep.
struct Video {
    format: VideoInfoRaw,
    streamed: bool,
    inbox: Arc<Inbox>,
    mainloop: pw::main_loop::MainLoopRc,
}

/// PipeWire's side, on its own thread: the shared stream's frames into
/// `inbox` until it ends or `quit`. Through the portal's remote (`fd`), or
/// the user's own PipeWire (tests).
fn video(
    fd: Option<OwnedFd>,
    node: u32,
    fps: u32,
    inbox: &Arc<Inbox>,
    quit: pw::channel::Receiver<()>,
) -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = match fd {
        Some(fd) => context.connect_fd_rc(fd, None)?,
        None => context.connect_rc(None)?,
    };
    let _quit = quit.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |()| mainloop.quit()
    });
    let stream = pw::stream::StreamBox::new(
        &core,
        "couch-coapp-capture",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;
    let _listener = stream
        .add_local_listener_with_user_data(Video {
            format: VideoInfoRaw::default(),
            streamed: false,
            inbox: inbox.clone(),
            mainloop: mainloop.clone(),
        })
        .state_changed(|_, video, _, state| match state {
            StreamState::Streaming => video.streamed = true,
            StreamState::Error(e) => {
                video.inbox.end(if video.streamed {
                    End::Closed
                } else {
                    End::Failed(format!("the shared stream failed: {e}"))
                });
                video.mainloop.quit();
            }
            StreamState::Unconnected if video.streamed => {
                video.inbox.end(End::Closed);
                video.mainloop.quit();
            }
            _ => {}
        })
        .param_changed(|_, video, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != ParamType::Format.as_raw() {
                return;
            }
            if let Ok((MediaType::Video, MediaSubtype::Raw)) = format_utils::parse_format(param) {
                let _ = video.format.parse(param);
            }
        })
        .process(|stream, video| {
            if let Some(mut buffer) = stream.dequeue_buffer()
                && let Some(frame) = copy_frame(&mut buffer, &video.format)
            {
                video.inbox.frame(frame);
            }
        })
        .register()?;
    let params = formats(fps);
    let mut params = [Pod::from_bytes(&params).ok_or(pw::Error::CreationFailed)?];
    stream.connect(
        Direction::Input,
        Some(node),
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    mainloop.run();
    Ok(())
}

/// The formats asked for: 32-bit RGB in memory, BGR first (the pipeline's
/// own order), up to the stream's rate.
fn formats(fps: u32) -> Vec<u8> {
    let fps = fps.max(1);
    let object = spa::pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        spa::pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA
        ),
        spa::pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            Rectangle {
                width: 1920,
                height: 1080
            },
            Rectangle {
                width: 1,
                height: 1
            },
            Rectangle {
                width: MAX_SIZE,
                height: MAX_SIZE
            }
        ),
        spa::pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: fps, denom: 1 },
            Fraction { num: 0, denom: 1 },
            Fraction { num: fps, denom: 1 }
        ),
    );
    PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(object))
        .map(|(cursor, _)| cursor.into_inner())
        .unwrap_or_default()
}

/// The buffer's picture as a BGRA frame of its own, or `None` for a buffer
/// with no picture (a cursor-only update, a corrupted one, a format not
/// asked for).
fn copy_frame(buffer: &mut pw::buffer::Buffer<'_>, format: &VideoInfoRaw) -> Option<BgraFrame> {
    let swap = match format.format() {
        VideoFormat::BGRx | VideoFormat::BGRA => false,
        VideoFormat::RGBx | VideoFormat::RGBA => true,
        _ => return None,
    };
    let size = format.size();
    let data = buffer.datas_mut().first_mut()?;
    let chunk = data.chunk();
    if chunk.size() == 0 || chunk.flags().contains(ChunkFlags::CORRUPTED) {
        return None;
    }
    let (offset, stride) = (chunk.offset() as usize, chunk.stride());
    let captured_at = crate::clock::now();
    let bytes = data.data()?;
    pack(
        bytes.get(offset..)?,
        size.width,
        size.height,
        usize::try_from(stride).ok().filter(|&s| s > 0),
        swap,
    )
    .map(|data| BgraFrame {
        width: size.width,
        height: size.height,
        stride: size.width * 4,
        data: data.into(),
        captured_at,
    })
}

/// `width` by `height` pixels of 4 bytes from rows `stride` apart (packed
/// if `None`), packed, with red and blue swapped if `swap`. `None` if `src`
/// is too short or the size is empty or too large.
fn pack(src: &[u8], width: u32, height: u32, stride: Option<usize>, swap: bool) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width > MAX_SIZE || height > MAX_SIZE {
        return None;
    }
    let row = width as usize * 4;
    let stride = stride.unwrap_or(row);
    if stride < row {
        return None;
    }
    let rows = height as usize;
    let src = src.get(..stride * (rows - 1) + row)?;
    let mut out = Vec::with_capacity(row * rows);
    for line in src.chunks(stride).take(rows) {
        out.extend_from_slice(&line[..row]);
    }
    if swap {
        for pixel in out.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_packed_and_rgb_becomes_bgr() {
        // 2x2 pixels, rows 12 bytes apart (4 bytes of padding each).
        let src = [
            1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, //
            9, 10, 11, 12, 13, 14, 15, 16, 0, 0, 0, 0,
        ];
        assert_eq!(
            pack(&src, 2, 2, Some(12), false).unwrap(),
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert_eq!(
            pack(&src, 2, 2, Some(12), true).unwrap(),
            [3, 2, 1, 4, 7, 6, 5, 8, 11, 10, 9, 12, 15, 14, 13, 16]
        );
        // The last row needs no padding after it.
        assert!(pack(&src[..20], 2, 2, Some(12), false).is_some());
        assert!(pack(&src[..19], 2, 2, Some(12), false).is_none());
        // A stride shorter than a row, an empty picture.
        assert!(pack(&src, 2, 2, Some(4), false).is_none());
        assert!(pack(&src, 0, 2, None, false).is_none());
    }

    #[test]
    fn formats_serialize() {
        let params = formats(60);
        assert!(Pod::from_bytes(&params).is_some());
    }

    /// A capture's end is kept, and frames after it are dropped.
    #[test]
    fn the_first_end_is_kept() {
        let inbox = Inbox::default();
        inbox.end(End::Closed);
        inbox.end(End::Failed("later".into()));
        inbox.frame(BgraFrame {
            width: 2,
            height: 2,
            stride: 8,
            data: vec![0; 16].into(),
            captured_at: couch_media::Micros(1),
        });
        let state = inbox.lock();
        assert!(state.frame.is_none());
        assert!(matches!(state.end, Some(End::Closed)));
    }

    /// The PipeWire half, without the portal: a red 640×480 RGBx test
    /// source in the user's own PipeWire (node `COUCH_TEST_VIDEO_NODE`, as
    /// `linux-checks` starts it) comes out as BGRA frames, and stopping
    /// ends the thread at once.
    #[test]
    #[ignore = "needs PipeWire with a test video source"]
    fn a_pipewire_video_source_is_captured() {
        let node: u32 = std::env::var("COUCH_TEST_VIDEO_NODE")
            .expect("COUCH_TEST_VIDEO_NODE")
            .parse()
            .expect("a node id");
        let inbox = Arc::new(Inbox::default());
        let (quit, quit_rx) = pw::channel::channel::<()>();
        let thread = {
            let inbox = inbox.clone();
            std::thread::spawn(move || video(None, node, 30, &inbox, quit_rx))
        };
        let mut capture = PortalCapture {
            inbox: inbox.clone(),
            stop: None,
            done: mpsc::channel().1,
        };
        let frame = (0..50)
            .find_map(|_| match capture.next_event(Duration::from_millis(100)) {
                Ok(Some(CaptureEvent::Frame(VideoFrame::Cpu(frame)))) => Some(frame),
                Ok(None) => None,
                other => panic!("{:?}", other.map(|e| e.map(|_| ()))),
            })
            .expect("a frame within 5 s");
        assert_eq!((frame.width, frame.height, frame.stride), (640, 480, 2560));
        assert_eq!(&frame.data[..3], [0, 0, 255], "red, as BGRA");
        let stopped = Instant::now();
        quit.send(()).expect("the loop listens");
        thread.join().expect("no panic").expect("PipeWire ran");
        assert!(stopped.elapsed() < Duration::from_secs(1));
    }
}
