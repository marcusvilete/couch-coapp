//! Video over iroh: one unidirectional QUIC stream per epoch (ADR 0003), and
//! receiver reports back as datagrams.
//!
//! The host side is a [`VideoSender`]: `send_frame` never waits on the
//! network; a writer task drains a queue onto the current epoch's stream.
//! Starting an epoch resets the old stream at once, dropping whatever it still
//! had queued: that is how the catch-up rule sheds latency.
//!
//! Both sides stop their tasks when dropped: a room connection outlives the
//! sessions that use it.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use bytes::Bytes;
use couch_media::{
    EncodedFrame, EpochId, FeedbackSnapshot, MediaError, Micros, VideoParams, VideoSender,
    clock::Clock,
    ratecontrol::{FeedbackTracker, PathSample, SendDelays},
};
use couch_proto::media::{
    DelaySample, EpochPreamble, FrameHeader, MAX_DELAY_SAMPLES, ReceiverReport, datagram,
    decode_payload, encode_datagram, reset_code, stream_kind,
};
use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt};
use tokio::sync::{mpsc, watch};

use crate::{datagrams::Datagrams, streams::Streams};

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
    Frame {
        epoch: u32,
        bytes: Bytes,
    },
    /// Reset the current epoch's stream with this code and stop.
    End(u32),
}

/// Host side of one viewer's video.
pub struct HostVideo {
    conn: Connection,
    commands: mpsc::UnboundedSender<WriterCmd>,
    epoch_tx: watch::Sender<u32>,
    tracker: Arc<Mutex<FeedbackTracker>>,
    /// Each frame's time on the host, taken off the viewer's delays.
    send_delays: Arc<Mutex<SendDelays>>,
    /// The clock frames' capture times are on.
    clock: Arc<dyn Clock>,
    session: u64,
    epoch: Option<u32>,
    next_epoch: u32,
    seq: u32,
}

impl HostVideo {
    /// Starts the writer and report-reader tasks. `clock` is the one frames'
    /// capture times are on. Call inside a tokio runtime.
    pub fn start(
        conn: Connection,
        session: u64,
        datagrams: &Datagrams,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let (commands, rx) = mpsc::unbounded_channel();
        let (epoch_tx, epoch_rx) = watch::channel(0);
        let tracker = Arc::new(Mutex::new(FeedbackTracker::new(Instant::now())));
        let send_delays = Arc::new(Mutex::new(SendDelays::new()));
        tokio::spawn(write_video(conn.clone(), rx, epoch_rx.clone()));
        tokio::spawn(read_reports(
            datagrams.subscribe(datagram::RECEIVER_REPORT),
            tracker.clone(),
            send_delays.clone(),
            epoch_rx,
        ));
        Self {
            conn,
            commands,
            epoch_tx,
            tracker,
            send_delays,
            clock,
            session,
            epoch: None,
            next_epoch: 1,
            seq: 0,
        }
    }

    /// Stops sending: the current epoch's stream is reset with `code`
    /// (`reset_code::SESSION_END` or `VIEWER_REMOVED`), so the viewer knows
    /// the stream is over rather than stalled. Dropping without this
    /// finishes the stream instead.
    pub fn end(self, code: u32) {
        let _ = self.commands.send(WriterCmd::End(code));
        // Abort a write still blocked on the stream.
        self.epoch_tx.send_replace(u32::MAX);
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
        let host_us = (self.clock.now().0 as i64).saturating_sub(frame.captured_at.0 as i64);
        self.send_delays
            .lock()
            .unwrap()
            .sent(epoch, header.seq, host_us);
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
            WriterCmd::End(code) => {
                if let Some((_, mut s)) = stream.take() {
                    let _ = s.reset(VarInt::from_u32(code));
                }
                return;
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

/// Ends when the reports stop or the [`HostVideo`] is dropped (its epoch
/// sender goes).
async fn read_reports(
    mut reports: mpsc::Receiver<Bytes>,
    tracker: Arc<Mutex<FeedbackTracker>>,
    send_delays: Arc<Mutex<SendDelays>>,
    mut epoch_rx: watch::Receiver<u32>,
) {
    loop {
        let payload = tokio::select! {
            payload = reports.recv() => match payload {
                Some(payload) => payload,
                None => return,
            },
            changed = epoch_rx.changed() => match changed {
                Ok(()) => continue,
                Err(_) => return,
            },
        };
        let Some(report) = decode_payload::<ReceiverReport>(&payload) else {
            tracing::debug!("malformed receiver report");
            continue;
        };
        // The network's part only: a busy host is not a congested path.
        let delays: Vec<i64> = {
            let sent = send_delays.lock().unwrap();
            report
                .delays
                .iter()
                .map(|d| sent.network_part(report.epoch, d.seq, d.one_way_us))
                .collect()
        };
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

/// Viewer side: takes the epoch streams `streams` hands over, delivers
/// frames in order, and sends a receiver report every 50 ms. Its tasks end
/// when it is dropped. Call inside a tokio runtime.
pub struct ViewerVideo {
    pub frames: mpsc::Receiver<VideoFrameIn>,
    state: Arc<Mutex<ViewerState>>,
}

impl ViewerVideo {
    /// With `session`, epochs that name another session are refused: what
    /// is left of an earlier session on the same connection.
    pub fn start(
        conn: Connection,
        streams: &Streams,
        session: Option<u64>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let (tx, frames) = mpsc::channel(64);
        let state = Arc::new(Mutex::new(ViewerState::default()));
        let epochs = streams.subscribe(stream_kind::VIDEO_EPOCH);
        tokio::spawn(accept_epochs(
            epochs,
            session,
            state.clone(),
            tx.clone(),
            clock,
        ));
        tokio::spawn(send_reports(conn, state.clone(), tx));
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
    mut epochs: mpsc::Receiver<RecvStream>,
    session: Option<u64>,
    state: Arc<Mutex<ViewerState>>,
    tx: mpsc::Sender<VideoFrameIn>,
    clock: Arc<dyn Clock>,
) {
    loop {
        let recv = tokio::select! {
            recv = epochs.recv() => match recv {
                Some(recv) => recv,
                None => return,
            },
            // The `ViewerVideo` was dropped.
            () = tx.closed() => return,
        };
        tokio::spawn(read_epoch(
            recv,
            session,
            state.clone(),
            tx.clone(),
            clock.clone(),
        ));
    }
}

async fn read_epoch(
    mut recv: RecvStream,
    session: Option<u64>,
    state: Arc<Mutex<ViewerState>>,
    tx: mpsc::Sender<VideoFrameIn>,
    clock: Arc<dyn Clock>,
) {
    // `Streams` has read the kind byte already.
    let mut preamble = [0u8; EpochPreamble::LEN];
    preamble[0] = stream_kind::VIDEO_EPOCH;
    if recv.read_exact(&mut preamble[1..]).await.is_err() {
        return;
    }
    let Some(preamble) = EpochPreamble::read(&preamble) else {
        tracing::warn!("malformed epoch preamble");
        let _ = recv.stop(VarInt::from_u32(0));
        return;
    };
    if session.is_some_and(|s| s != preamble.session) {
        tracing::debug!(
            session = preamble.session,
            "refusing an epoch of another session"
        );
        let _ = recv.stop(VarInt::from_u32(reset_code::SESSION_END));
        return;
    }
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
            // Samples are reported with the latest epoch: the abandoned
            // one's go, so the host matches each to the right frame.
            s.delays.clear();
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

/// Ends when the connection does, or the `ViewerVideo` is dropped (`frames`
/// closes).
async fn send_reports(
    conn: Connection,
    state: Arc<Mutex<ViewerState>>,
    frames: mpsc::Sender<VideoFrameIn>,
) {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            () = frames.closed() => return,
        }
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
