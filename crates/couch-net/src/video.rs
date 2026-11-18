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
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use bytes::Bytes;
use couch_media::{
    Codec, EncodedFrame, EpochId, FeedbackSnapshot, MediaError, Micros, VideoParams, VideoSender,
    clock::Clock,
    ratecontrol::{FeedbackTracker, PathSample, SendDelays},
};
use couch_proto::media::{
    DelaySample, EpochPreamble, FrameHeader, FrameRef, MAX_DELAY_SAMPLES, ReceiverReport, datagram,
    decode_payload, encode_datagram, reset_code, stream_kind,
};
use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt};
use tokio::sync::{Notify, mpsc, watch};

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

/// Where each frame of the current epoch ends in its stream, to tell from
/// the viewer's reports (`epoch_bytes_received`) which frames it received
/// whole (M12d-2).
#[derive(Default)]
struct Receipts {
    epoch: u32,
    /// Each frame's sequence number and the byte its stream had after it,
    /// for the frames not yet known to be received.
    ends: VecDeque<(u32, u64)>,
    written: u64,
    newest: Option<FrameRef>,
}

/// Frames whose receipt is still awaited, at most: 8 s at 120 fps. Past it,
/// the oldest are forgotten (a viewer that silent is paused anyway).
const AWAITED: usize = 1024;

impl Receipts {
    fn epoch(&mut self, epoch: u32) {
        self.epoch = epoch;
        self.ends.clear();
        self.written = EpochPreamble::LEN as u64;
    }

    fn sent(&mut self, seq: u32, bytes: u64) {
        self.written += bytes;
        self.ends.push_back((seq, self.written));
        if self.ends.len() > AWAITED {
            self.ends.pop_front();
        }
    }

    /// The viewer reports `bytes` of `epoch`'s stream: every frame ending
    /// within them is received.
    fn report(&mut self, epoch: u32, bytes: u64) {
        if epoch != self.epoch {
            return;
        }
        while let Some(&(seq, end)) = self.ends.front() {
            if end > bytes {
                break;
            }
            self.newest = Some(FrameRef { epoch, seq });
            self.ends.pop_front();
        }
    }
}

enum WriterCmd {
    StartEpoch(EpochPreamble),
    Frame {
        epoch: u32,
        bytes: Bytes,
    },
    /// Reset the current epoch's stream with this code and stop.
    End(u32),
    /// Reset the current epoch's stream with this code; a new epoch may
    /// follow.
    EndEpoch(u32),
}

/// Host side of one viewer's video.
pub struct HostVideo {
    conn: Connection,
    commands: mpsc::UnboundedSender<WriterCmd>,
    epoch_tx: watch::Sender<u32>,
    tracker: Arc<Mutex<FeedbackTracker>>,
    /// Each frame's time on the host, taken off the viewer's delays.
    send_delays: Arc<Mutex<SendDelays>>,
    receipts: Arc<Mutex<Receipts>>,
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
        let receipts = Arc::new(Mutex::new(Receipts::default()));
        tokio::spawn(write_video(conn.clone(), rx, epoch_rx.clone()));
        tokio::spawn(read_reports(
            datagrams.subscribe(datagram::RECEIVER_REPORT),
            tracker.clone(),
            send_delays.clone(),
            receipts.clone(),
            epoch_rx,
        ));
        Self {
            conn,
            commands,
            epoch_tx,
            tracker,
            send_delays,
            receipts,
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

    /// Ends the current epoch with `code` (`reset_code::STREAM_END`: the host
    /// stopped streaming, the session goes on, M19) but keeps sending: the
    /// next [`VideoSender::start_epoch`] counts on, so the viewer, which
    /// drops older epochs, takes it. Nothing without a current epoch.
    pub fn end_epoch(&mut self, code: u32) {
        if self.epoch.take().is_none() {
            return;
        }
        let _ = self.commands.send(WriterCmd::EndEpoch(code));
        // Abort a write still blocked on the epoch's stream.
        self.epoch_tx.send_replace(0);
    }
}

impl VideoSender for HostVideo {
    fn start_epoch(
        &mut self,
        params: &VideoParams,
        continues: Option<FrameRef>,
    ) -> Result<EpochId, MediaError> {
        let epoch = self.next_epoch;
        self.next_epoch += 1;
        self.epoch = Some(epoch);
        self.seq = 0;
        self.tracker.lock().unwrap().on_epoch(epoch);
        self.receipts.lock().unwrap().epoch(epoch);
        // Abort any write still blocked on the old epoch's stream.
        self.epoch_tx.send_replace(epoch);
        let preamble = EpochPreamble {
            session: self.session,
            epoch,
            codec: params.codec.wire(),
            width: params.width as u16,
            height: params.height as u16,
            fps: params.fps as u16,
            continues,
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
        self.receipts
            .lock()
            .unwrap()
            .sent(header.seq, buf.len() as u64);
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

    fn next_frame(&self) -> Option<FrameRef> {
        self.epoch.map(|epoch| FrameRef {
            epoch,
            seq: self.seq,
        })
    }

    fn received(&self) -> Option<FrameRef> {
        self.receipts.lock().unwrap().newest
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
            WriterCmd::EndEpoch(code) => {
                if let Some((_, mut s)) = stream.take() {
                    let _ = s.reset(VarInt::from_u32(code));
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

/// Ends when the reports stop or the [`HostVideo`] is dropped (its epoch
/// sender goes).
async fn read_reports(
    mut reports: mpsc::Receiver<Bytes>,
    tracker: Arc<Mutex<FeedbackTracker>>,
    send_delays: Arc<Mutex<SendDelays>>,
    receipts: Arc<Mutex<Receipts>>,
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
        receipts
            .lock()
            .unwrap()
            .report(report.epoch, report.epoch_bytes_received);
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
    /// The epoch's codec, as its preamble named it (`couch_proto::codec`).
    pub codec: u8,
    pub header: FrameHeader,
    pub data: Bytes,
    pub arrived: Micros,
    /// On an epoch's first frame, the frame the epoch continues from
    /// (M12d-2, `EpochPreamble::continues`).
    pub continues: Option<FrameRef>,
}

#[derive(Default)]
struct ViewerState {
    latest_epoch: u32,
    epoch_bytes: u64,
    total_bytes: u64,
    frames: u64,
    delays: Vec<DelaySample>,
    decode_errors: u32,
    /// The frame each recent epoch continues from (M12d-2), by epoch: the
    /// chain back from the latest says which older frames are still wanted.
    continued: BTreeMap<u32, FrameRef>,
    /// Epochs whose stream is still being read.
    reading: Vec<u32>,
}

/// Recent continuing epochs remembered.
const CONTINUED_KEPT: usize = 32;

impl ViewerState {
    /// Whether a frame of `epoch` is still wanted: of the latest epoch, or of
    /// an older one up to the frame the latest continues from, following the
    /// chain back (an epoch continuing from one that continued too).
    fn wanted(&self, epoch: u32, seq: u32) -> bool {
        if epoch >= self.latest_epoch {
            return true;
        }
        let mut at = self.latest_epoch;
        while let Some(from) = self.continued.get(&at) {
            if from.epoch == epoch {
                return seq <= from.seq;
            }
            if from.epoch < epoch {
                return false;
            }
            at = from.epoch;
        }
        false
    }

    /// The latest epoch is `epoch`, continuing from `continues` if it does.
    fn begin(&mut self, epoch: u32, continues: Option<FrameRef>) {
        self.latest_epoch = epoch;
        if let Some(from) = continues.filter(|f| f.epoch < epoch) {
            self.continued.insert(epoch, from);
            while self.continued.len() > CONTINUED_KEPT {
                self.continued.pop_first();
            }
        }
    }
}

/// How long a continuing epoch's first frame waits for an older epoch's
/// reader to hand over its last frames, which it may continue from.
const OLDER_READERS: Duration = Duration::from_millis(200);

/// Viewer side: takes the epoch streams `streams` hands over, delivers
/// frames in order, and sends a receiver report every 50 ms. Its tasks end
/// when it is dropped. Call inside a tokio runtime.
pub struct ViewerVideo {
    pub frames: mpsc::Receiver<VideoFrameIn>,
    state: Arc<Mutex<ViewerState>>,
}

/// Woken whenever an epoch's reader ends.
#[derive(Clone, Default)]
struct Readers {
    ended: Arc<Notify>,
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
            Readers::default(),
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

    /// Whether a frame of `epoch` that arrived is still to be decoded: of the
    /// latest epoch, or of an older one up to the frame the latest continues
    /// from (M12d-2).
    pub fn wanted(&self, epoch: u32, seq: u32) -> bool {
        self.state.lock().unwrap().wanted(epoch, seq)
    }

    pub fn note_decode_error(&self) {
        self.state.lock().unwrap().decode_errors += 1;
    }
}

async fn accept_epochs(
    mut epochs: mpsc::Receiver<RecvStream>,
    session: Option<u64>,
    state: Arc<Mutex<ViewerState>>,
    readers: Readers,
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
            readers.clone(),
            tx.clone(),
            clock.clone(),
        ));
    }
}

/// An epoch's reader, counted in [`ViewerState::reading`] until it ends.
struct Reading {
    state: Arc<Mutex<ViewerState>>,
    readers: Readers,
    epoch: u32,
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap()
            .reading
            .retain(|&e| e != self.epoch);
        self.readers.ended.notify_waiters();
    }
}

/// Waits, at most [`OLDER_READERS`], until no epoch older than `epoch` is
/// still being read: their last frames, which a continuation may refer to,
/// go to the decoder before its first (M12d-2). The host resets an old epoch
/// before it opens the next, so they end at once.
async fn older_readers_done(state: &Mutex<ViewerState>, readers: &Readers, epoch: u32) {
    let deadline = tokio::time::Instant::now() + OLDER_READERS;
    loop {
        let ended = readers.ended.notified();
        if !state.lock().unwrap().reading.iter().any(|&e| e < epoch) {
            return;
        }
        if tokio::time::timeout_at(deadline, ended).await.is_err() {
            tracing::debug!(epoch, "an older epoch's reader still runs; going on");
            return;
        }
    }
}

async fn read_epoch(
    mut recv: RecvStream,
    session: Option<u64>,
    state: Arc<Mutex<ViewerState>>,
    readers: Readers,
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
    if Codec::from_wire(preamble.codec).is_none() {
        tracing::warn!(codec = preamble.codec, "an epoch in an unknown codec");
        let _ = recv.stop(VarInt::from_u32(0));
        return;
    }
    if session.is_some_and(|s| s != preamble.session) {
        tracing::debug!(
            session = preamble.session,
            "refusing an epoch of another session"
        );
        let _ = recv.stop(VarInt::from_u32(reset_code::SESSION_END));
        return;
    }
    let epoch = preamble.epoch;
    let _reading = {
        let mut s = state.lock().unwrap();
        if epoch < s.latest_epoch {
            let _ = recv.stop(VarInt::from_u32(reset_code::SKIP_AHEAD));
            return;
        }
        if epoch > s.latest_epoch {
            s.begin(epoch, preamble.continues);
            s.epoch_bytes = EpochPreamble::LEN as u64;
            // Samples are reported with the latest epoch: the abandoned
            // one's go, so the host matches each to the right frame.
            s.delays.clear();
        }
        s.reading.push(epoch);
        Reading {
            state: state.clone(),
            readers: readers.clone(),
            epoch,
        }
    };

    let mut header = [0u8; FrameHeader::LEN];
    let mut first = true;
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
            let bytes = (FrameHeader::LEN + data.len()) as u64;
            if s.latest_epoch != epoch {
                // A newer epoch began. Frames up to the one it continues
                // from still go to the decoder; the rest are dropped.
                if !s.wanted(epoch, h.seq) {
                    let _ = recv.stop(VarInt::from_u32(reset_code::SKIP_AHEAD));
                    return;
                }
                s.total_bytes += bytes;
                s.frames += 1;
            } else {
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
        }
        let continues = if std::mem::take(&mut first) {
            if preamble.continues.is_some() {
                older_readers_done(&state, &readers, epoch).await;
            }
            preamble.continues
        } else {
            None
        };
        let frame = VideoFrameIn {
            epoch,
            codec: preamble.codec,
            header: h,
            data: data.into(),
            arrived,
            continues,
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

#[cfg(test)]
mod tests {
    use couch_media::Codec;
    use couch_proto::codec;

    use super::{FrameRef, ViewerState};

    /// M12d-2: an epoch continuing from one that continued too still wants
    /// the frames both continue from (the review's finding).
    #[test]
    fn a_chain_of_continuations_keeps_what_each_needs() {
        let mut s = ViewerState::default();
        s.begin(1, None);
        s.begin(2, Some(FrameRef { epoch: 1, seq: 20 }));
        s.begin(3, Some(FrameRef { epoch: 2, seq: 3 }));
        assert!(s.wanted(3, 0) && s.wanted(2, 3) && s.wanted(1, 20));
        assert!(!s.wanted(2, 4) && !s.wanted(1, 21));
        // A keyframe epoch ends the chain.
        s.begin(4, None);
        assert!(!s.wanted(3, 0) && !s.wanted(1, 20));
        // So does one continuing from a newer epoch than asked about.
        s.begin(5, Some(FrameRef { epoch: 4, seq: 9 }));
        assert!(s.wanted(4, 9) && !s.wanted(3, 0));
    }

    #[test]
    fn codec_numbers_agree_with_the_wire() {
        assert_eq!(Codec::H264.wire(), codec::H264);
        assert_eq!(Codec::Hevc.wire(), codec::HEVC);
        assert_eq!(Codec::H264.bit(), codec::DECODES_H264);
        assert_eq!(Codec::Hevc.bit(), codec::DECODES_HEVC);
        for c in [Codec::H264, Codec::Hevc] {
            assert_eq!(Codec::from_wire(c.wire()), Some(c));
        }
        assert_eq!(Codec::from_wire(0), None);
        assert_eq!(Codec::from_wire(3), None);
    }
}
