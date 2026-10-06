//! Input over iroh (docs/protocol.md, "Input"): keys and mouse buttons on one
//! unidirectional stream that outranks video, gamepad state and mouse motion
//! as datagrams, rumble back as datagrams.

use couch_input::{forward::Outgoing, router::Input};
use couch_proto::{
    envelope,
    input::{InputMessage, MouseDatagram, PadDatagram, RumbleDatagram},
    media::{datagram, decode_payload, encode_datagram, stream_kind},
};
use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt};
use tokio::sync::mpsc;

use crate::{datagrams::Datagrams, streams::Streams};

/// The input stream's priority. Video streams have 0, so a key press never
/// waits behind a keyframe.
const PRIORITY: i32 = 100;

/// Viewer side: sends what the forwarder produces. Never blocks the caller.
pub struct InputSender {
    tx: mpsc::UnboundedSender<Outgoing>,
}

impl InputSender {
    /// Starts the writer. Call inside a tokio runtime.
    pub fn start(conn: Connection) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(write_input(conn, rx));
        Self { tx }
    }

    pub fn send(&self, message: Outgoing) {
        let _ = self.tx.send(message);
    }
}

async fn write_input(conn: Connection, mut rx: mpsc::UnboundedReceiver<Outgoing>) {
    let mut stream: Option<SendStream> = None;
    let mut buf = Vec::new();
    while let Some(message) = rx.recv().await {
        let (tag, payload) = match message {
            Outgoing::Stream(m) => {
                if stream.is_none() {
                    stream = open_stream(&conn).await;
                }
                let Some(s) = stream.as_mut() else {
                    continue;
                };
                buf.clear();
                if let Err(e) = m.encode(&mut buf) {
                    tracing::warn!("input message not encoded: {e}");
                    continue;
                }
                if let Err(e) = s.write_all(&buf).await {
                    tracing::debug!("input stream write failed: {e}");
                    stream = None;
                }
                continue;
            }
            Outgoing::Pad(d) => (datagram::GAMEPAD, encode_datagram(datagram::GAMEPAD, &d)),
            Outgoing::Mouse(d) => (
                datagram::MOUSE_MOTION,
                encode_datagram(datagram::MOUSE_MOTION, &d),
            ),
        };
        match conn.send_datagram(payload.into()) {
            Ok(()) => {}
            Err(iroh::endpoint::SendDatagramError::ConnectionLost(_)) => return,
            Err(e) => tracing::debug!("input datagram {tag:#04x} not sent: {e}"),
        }
    }
}

async fn open_stream(conn: &Connection) -> Option<SendStream> {
    let mut s = match conn.open_uni().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("opening the input stream failed: {e}");
            return None;
        }
    };
    let _ = s.set_priority(PRIORITY);
    s.write_all(&[stream_kind::INPUT]).await.ok()?;
    Some(s)
}

/// Host side: everything a viewer sends, merged into one queue. Its tasks
/// end when it is dropped.
pub struct InputReceiver {
    pub inputs: mpsc::Receiver<Input>,
}

impl InputReceiver {
    /// Starts taking the viewer's input stream from `streams` and its input
    /// datagrams from `datagrams`. Call inside a tokio runtime.
    pub fn start(datagrams: &Datagrams, streams: &Streams) -> Self {
        let (tx, inputs) = mpsc::channel(1024);
        let mut pads = datagrams.subscribe(datagram::GAMEPAD);
        let mut mice = datagrams.subscribe(datagram::MOUSE_MOTION);
        {
            let tx = tx.clone();
            tokio::spawn(async move {
                loop {
                    let input = tokio::select! {
                        Some(p) = pads.recv() => decode_payload::<PadDatagram>(&p).map(Input::Pad),
                        Some(m) = mice.recv() => decode_payload::<MouseDatagram>(&m).map(Input::Mouse),
                        () = tx.closed() => return,
                        else => return,
                    };
                    match input {
                        Some(input) => {
                            // Datagrams are disposable: drop rather than wait.
                            let _ = tx.try_send(input);
                        }
                        None => tracing::debug!("malformed input datagram"),
                    }
                }
            });
        }
        let mut incoming = streams.subscribe(stream_kind::INPUT);
        tokio::spawn(async move {
            loop {
                let recv = tokio::select! {
                    recv = incoming.recv() => match recv {
                        Some(recv) => recv,
                        None => return,
                    },
                    () = tx.closed() => return,
                };
                tokio::spawn(read_stream(recv, tx.clone()));
            }
        });
        Self { inputs }
    }
}

/// Reads one input stream, whose kind byte `Streams` has read already.
async fn read_stream(mut recv: RecvStream, tx: mpsc::Sender<Input>) {
    let mut buf = Vec::new();
    loop {
        match envelope::decode(&buf) {
            Ok(Some((frame, used))) => {
                buf.drain(..used);
                match InputMessage::from_frame(&frame) {
                    Ok(Some(message)) => {
                        let input = match message {
                            InputMessage::Key(m) => Input::Key(m),
                            InputMessage::MouseButton(m) => Input::MouseButton(m),
                            InputMessage::Held(m) => Input::Held(m),
                        };
                        // Stream input is never dropped: a lost key-up would
                        // leave a key held until the next sync.
                        if tx.send(input).await.is_err() {
                            return;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!("bad input message: {e}");
                        let _ = recv.stop(VarInt::from_u32(0));
                        return;
                    }
                }
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("bad input frame: {e}");
                let _ = recv.stop(VarInt::from_u32(0));
                return;
            }
        }
        match recv.read_chunk(64 * 1024).await {
            Ok(Some(chunk)) => buf.extend_from_slice(&chunk),
            _ => return,
        }
    }
}

/// Host side: sends rumble to a seat's holder.
pub fn send_rumble(conn: &Connection, rumble: RumbleDatagram) {
    let datagram = encode_datagram(datagram::RUMBLE, &rumble);
    if let Err(e) = conn.send_datagram(datagram.into()) {
        tracing::debug!("rumble not sent: {e}");
    }
}
