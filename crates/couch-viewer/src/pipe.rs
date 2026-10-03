//! The stdin/stdout link to the main process (`couch_proto::ipc`).

use std::{
    io::Write,
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
};

use couch_proto::ipc::{self, FrameTiming, NetStats, VideoFrameHeader};

/// What the main process sends.
pub enum Incoming {
    Video(VideoFrameHeader, Vec<u8>),
    Shutdown,
}

/// What the viewer sends back.
pub enum Outgoing {
    Timing(FrameTiming),
    KeyframeRequest,
}

/// Reads stdin until the main process closes it; then the channel closes too.
/// Network stats only feed the HUD, so they go straight to `net`.
pub fn spawn_reader(
    tx: mpsc::Sender<Incoming>,
    net: Arc<Mutex<Option<NetStats>>>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("ipc-read".into())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            loop {
                let frame = match ipc::read_frame(&mut stdin) {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::error!("IPC read failed: {e}");
                        break;
                    }
                };
                let message = match frame.kind {
                    ipc::kinds::VIDEO_FRAME => match VideoFrameHeader::read(&frame.payload) {
                        Some((header, data)) => Incoming::Video(header, data.to_vec()),
                        None => {
                            tracing::warn!("malformed video frame on the IPC pipe");
                            continue;
                        }
                    },
                    ipc::kinds::SHUTDOWN => Incoming::Shutdown,
                    ipc::kinds::NET_STATS => {
                        *net.lock().unwrap() = NetStats::read(&frame.payload);
                        continue;
                    }
                    ipc::kinds::HELLO => continue,
                    other => {
                        tracing::debug!("ignoring IPC kind {other:#04x}");
                        continue;
                    }
                };
                let shutdown = matches!(message, Incoming::Shutdown);
                if tx.send(message).is_err() || shutdown {
                    break;
                }
            }
        })
        .expect("spawning the IPC reader")
}

/// Writes replies to stdout, the only code in the viewer allowed to.
pub fn spawn_writer(rx: mpsc::Receiver<Outgoing>) -> JoinHandle<()> {
    thread::Builder::new()
        .name("ipc-write".into())
        .spawn(move || {
            let mut stdout = std::io::stdout().lock();
            let mut buf = Vec::new();
            let mut payload = Vec::new();
            for message in rx {
                buf.clear();
                payload.clear();
                let kind = match message {
                    Outgoing::Timing(timing) => {
                        timing.write(&mut payload);
                        ipc::kinds::FRAME_TIMING
                    }
                    Outgoing::KeyframeRequest => ipc::kinds::KEYFRAME_REQUEST,
                };
                ipc::encode(kind, &payload, &mut buf).expect("small IPC frames");
                if stdout
                    .write_all(&buf)
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    break;
                }
            }
        })
        .expect("spawning the IPC writer")
}
