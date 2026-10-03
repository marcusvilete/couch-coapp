//! Video over iroh: one unidirectional QUIC stream per epoch (ADR 0003), and
//! receiver reports back as datagrams.
//!
//! The host side is a [`VideoSender`]: `send_frame` never waits on the
//! network; a writer task drains a queue onto the current epoch's stream.
//! Starting an epoch resets the old stream at once, dropping whatever it still
//! had queued: that is how the catch-up rule sheds latency.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use bytes::Bytes;
use couch_media::{
    EncodedFrame, EpochId, FeedbackSnapshot, MediaError, Micros, VideoParams, VideoSender,
    clock::Clock,
    ratecontrol::{FeedbackTracker, PathSample},
};
use couch_proto::media::{
    DelaySample, EpochPreamble, FrameHeader, MAX_DELAY_SAMPLES, ReceiverReport, datagram,
    datagram_tag, decode_payload, encode_datagram, reset_code, stream_kind,
};
use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt};
use tokio::sync::{mpsc, watch};

/// The selected path's statistics, in the shape the rate controller wants.
pub fn selected_path(conn: &Connection) -> PathSample {
    let paths = conn.paths();
    paths
        .iter()
        .find(|p| p.is_selected())
        .map(|p| {
            let s = p.stats();
            PathSample {
                rtt: s.rtt,
                sent_packets: s.udp_tx.datagrams,
                lost_packets: s.lost_packets,
                congestion_events: s.congestion_events,
                cwnd: s.cwnd,
                relayed: p.is_relay(),
            }
        })
        .unwrap_or_default()
}

enum WriterCmd {
    StartEpoch(EpochPreamble),
    Frame { epoch: u32, bytes: Bytes },
}

/// Host side of one viewer's video.
pub struct HostVideo {
    conn: Connection,
    commands: mpsc::UnboundedSender<WriterCmd>,
    epoch_tx: watch::Sender<u32>,
    tracker: Arc<Mutex<FeedbackTracker>>,
    session: u64,
    epoch: Option<u32>,
    next_epoch: u32,
    seq: u32,
}

impl HostVideo {
    /// Starts the writer and report-reader tasks. Call inside a tokio runtime.
    pub fn start(conn: Connection, session: u64) -> Self {
        let (commands, rx) = mpsc::unbounded_channel();
        let (epoch_tx, epoch_rx) = watch::channel(0);
        let tracker = Arc::new(Mutex::new(FeedbackTracker::new(Instant::now())));
        tokio::spawn(write_video(conn.clone(), rx, epoch_rx));
        tokio::spawn(read_reports(conn.clone(), tracker.clone()));
        Self {
            conn,
            commands,
            epoch_tx,
            tracker,
            session,
            epoch: None,
            next_epoch: 1,
            seq: 0,
        }
    }
}

impl VideoSender for HostVideo {
    fn start_epoch(&mut self, params: &VideoParams) -> Result<EpochId, MediaError> {
        let epoch = self.next_epoch;
        self.next_epoch += 1;
        self.epoch = Some(epoch);
        self.seq = 0;
        self.tracker.lock().unwrap().on_epoch(epoch);
        // Abort any write still blocked on the old epoch's stream.
        self.epoch_tx.send_replace(epoch);
        let preamble = EpochPreamble {
            session: self.session,
            epoch,
            codec: EpochPreamble::CODEC_H264,
            width: params.width as u16,
            height: params.height as u16,
            fps: params.fps as u16,
        };
        self.commands
            .send(WriterCmd::StartEpoch(preamble))
            .map_err(|_| MediaError::Closed)?;
        Ok(EpochId(epoch))
    }

    fn send_frame(&mut self, frame: &EncodedFrame) -> Result<(), MediaError> {
        let epoch = self
            .epoch
            .ok_or_else(|| MediaError::Backend("send_frame before start_epoch".into()))?;
        let header = FrameHeader {
            seq: self.seq,
            keyframe: frame.keyframe,
            capture_us: frame.captured_at.0,
            encode_us: frame.encode_time_us,
            len: frame.data.len() as u32,
        };
        self.seq += 1;
        let mut buf = Vec::with_capacity(FrameHeader::LEN + frame.data.len());
        header.write(&mut buf);
        buf.extend_from_slice(&frame.data);
        self.tracker
            .lock()
            .unwrap()
            .on_sent(Instant::now(), buf.len() as u64);
        self.commands
            .send(WriterCmd::Frame {
                epoch,
                bytes: buf.into(),
            })
            .map_err(|_| MediaError::Closed)
    }

    fn feedback(&self) -> FeedbackSnapshot {
        let path = selected_path(&self.conn);
        self.tracker.lock().unwrap().snapshot(Instant::now(), path)
    }
}

async fn write_video(
    conn: Connection,
    mut commands: mpsc::UnboundedReceiver<WriterCmd>,
    mut epoch_rx: watch::Receiver<u32>,
) {
    let mut stream: Option<(u32, SendStream)> = None;
    while let Some(cmd) = commands.recv().await {
        match cmd {
            WriterCmd::StartEpoch(preamble) => {
                if let Some((_, mut old)) = stream.take() {
                    let _ = old.reset(VarInt::from_u32(reset_code::SKIP_AHEAD));
                }
                let mut new = match conn.open_uni().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("opening a video stream failed: {e}");
                        return;
                    }
                };
                let mut buf = Vec::with_capacity(EpochPreamble::LEN);
                preamble.write(&mut buf);
                if new.write_all(&buf).await.is_ok() {
                    stream = Some((preamble.epoch, new));
                }
            }
            WriterCmd::Frame { epoch, bytes } => {
                let Some((current, s)) = stream.as_mut() else {
                    continue;
                };
                if *current != epoch || *epoch_rx.borrow() != epoch {
                    continue; // superseded by a newer epoch
                }
                let superseded = tokio::select! {
                    result = s.write_all(&bytes) => {
                        if let Err(e) = result {
                            tracing::debug!("video stream write failed: {e}");
                            stream = None;
                        }
                        false
                    }
                    () = epoch_changed(&mut epoch_rx, epoch) => true,
                };
                if superseded {
                    tracing::debug!(epoch, "abandoned a frame write for a newer epoch");
                }
            }
        }
    }
    if let Some((_, mut s)) = stream {
        let _ = s.finish();
    }
}

async fn epoch_changed(rx: &mut watch::Receiver<u32>, epoch: u32) {
    loop {
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
        if *rx.borrow() != epoch {
            return;
        }
    }
}

async fn read_reports(conn: Connection, tracker: Arc<Mutex<FeedbackTracker>>) {
    while let Ok(datagram) = conn.read_datagram().await {
        let Some((tag, payload)) = datagram_tag(&datagram) else {
            continue;
        };
        if tag != datagram::RECEIVER_REPORT {
            continue;
        }
        let Some(report) = decode_payload::<ReceiverReport>(payload) else {
            tracing::debug!("malformed receiver report");
            continue;
        };
        let delays: Vec<i64> = report.delays.iter().map(|d| d.one_way_us).collect();
        tracker.lock().unwrap().on_report(
            Instant::now(),
            report.epoch,
            report.epoch_bytes_received,
            report.total_bytes_received,
            &delays,
        );
    }
}

/// A complete frame as it arrives at the viewer.
#[derive(Debug, Clone)]
pub struct VideoFrameIn {
    pub epoch: u32,
    pub header: FrameHeader,
    pub data: Bytes,
    pub arrived: Micros,
}

#[derive(Default)]
struct ViewerState {
    latest_epoch: u32,
    epoch_bytes: u64,
    total_bytes: u64,
    frames: u64,
    delays: Vec<DelaySample>,
    decode_errors: u32,
}

/// Viewer side: accepts epoch streams, delivers frames in order, and sends a
/// receiver report every 50 ms. Call inside a tokio runtime.
pub struct ViewerVideo {
    pub frames: mpsc::Receiver<VideoFrameIn>,
    state: Arc<Mutex<ViewerState>>,
}

impl ViewerVideo {
    pub fn start(conn: Connection, clock: Arc<dyn Clock>) -> Self {
        let (tx, frames) = mpsc::channel(64);
        let state = Arc::new(Mutex::new(ViewerState::default()));
        tokio::spawn(accept_epochs(conn.clone(), state.clone(), tx, clock));
        tokio::spawn(send_reports(conn, state.clone()));
        Self { frames, state }
    }

    /// The newest epoch seen; frames from older ones should be dropped.
    pub fn latest_epoch(&self) -> u32 {
        self.state.lock().unwrap().latest_epoch
    }

    pub fn note_decode_error(&self) {
        self.state.lock().unwrap().decode_errors += 1;
    }
}

async fn accept_epochs(
    conn: Connection,
    state: Arc<Mutex<ViewerState>>,
    tx: mpsc::Sender<VideoFrameIn>,
    clock: Arc<dyn Clock>,
) {
    while let Ok(recv) = conn.accept_uni().await {
        tokio::spawn(read_epoch(recv, state.clone(), tx.clone(), clock.clone()));
    }
}

async fn read_epoch(
    mut recv: RecvStream,
    state: Arc<Mutex<ViewerState>>,
    tx: mpsc::Sender<VideoFrameIn>,
    clock: Arc<dyn Clock>,
) {
    let mut preamble = [0u8; EpochPreamble::LEN];
    if recv.read_exact(&mut preamble).await.is_err() {
        return;
    }
    if preamble[0] != stream_kind::VIDEO_EPOCH {
        tracing::debug!("ignoring stream of kind {:#04x}", preamble[0]);
        let _ = recv.stop(VarInt::from_u32(0));
        return;
    }
    let Some(preamble) = EpochPreamble::read(&preamble) else {
        tracing::warn!("malformed epoch preamble");
        return;
    };
    let epoch = preamble.epoch;
    {
        let mut s = state.lock().unwrap();
        if epoch < s.latest_epoch {
            let _ = recv.stop(VarInt::from_u32(reset_code::SKIP_AHEAD));
            return;
        }
        if epoch > s.latest_epoch {
            s.latest_epoch = epoch;
            s.epoch_bytes = EpochPreamble::LEN as u64;
        }
    }

    let mut header = [0u8; FrameHeader::LEN];
    loop {
        if recv.read_exact(&mut header).await.is_err() {
            return; // finished, reset by the host, or connection gone
        }
        let Some(h) = FrameHeader::read(&header) else {
            tracing::warn!("oversized frame announced; dropping the epoch");
            let _ = recv.stop(VarInt::from_u32(0));
            return;
        };
        let mut data = vec![0u8; h.len as usize];
        if recv.read_exact(&mut data).await.is_err() {
            return;
        }
        let arrived = clock.now();
        {
            let mut s = state.lock().unwrap();
            if s.latest_epoch != epoch {
                let _ = recv.stop(VarInt::from_u32(reset_code::SKIP_AHEAD));
                return;
            }
            let bytes = (FrameHeader::LEN + data.len()) as u64;
            s.epoch_bytes += bytes;
            s.total_bytes += bytes;
            s.frames += 1;
            if s.delays.len() < MAX_DELAY_SAMPLES {
                s.delays.push(DelaySample {
                    seq: h.seq,
                    // `capture_us` comes from the peer; don't let it overflow.
                    one_way_us: (arrived.0 as i64).saturating_sub(h.capture_us as i64),
                });
            }
        }
        let frame = VideoFrameIn {
            epoch,
            header: h,
            data: data.into(),
            arrived,
        };
        if tx.send(frame).await.is_err() {
            return;
        }
    }
}

async fn send_reports(conn: Connection, state: Arc<Mutex<ViewerState>>) {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let report = {
            let mut s = state.lock().unwrap();
            ReceiverReport {
                epoch: s.latest_epoch,
                epoch_bytes_received: s.epoch_bytes,
                total_bytes_received: s.total_bytes,
                frames_received: s.frames,
                delays: std::mem::take(&mut s.delays),
                decode_errors: s.decode_errors,
            }
        };
        let datagram = encode_datagram(datagram::RECEIVER_REPORT, &report);
        match conn.send_datagram(datagram.into()) {
            Ok(()) => {}
            Err(iroh::endpoint::SendDatagramError::ConnectionLost(_)) => return,
            Err(e) => tracing::debug!("receiver report not sent: {e}"),
        }
    }
}
