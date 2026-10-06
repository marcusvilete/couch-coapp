//! The control stream: framed `couch_proto::control` messages on the first
//! bidirectional stream of a connection. The dialer opens it and speaks first
//! (QUIC only announces a stream once data is sent on it).
//!
//! Liveness: whoever owns a [`ControlSender`] sends `Alive` every
//! [`ALIVE_EVERY`](couch_proto::control::ALIVE_EVERY); the
//! [`ControlReceiver`] times out a peer that has sent `Alive` and then
//! nothing for the silent limit. QUIC's own idle timeout (15 s) stays as the
//! backstop.

use std::time::Duration;

use couch_proto::{
    PROTOCOL_MINOR,
    control::{ControlMessage, DecodeError, Decoded, Hello, SILENT_LIMIT},
    envelope,
};
use iroh::endpoint::{Connection, RecvStream, SendStream};

use crate::NetError;

/// The control stream's priority: above video epochs (0), so a seat change or
/// a keyframe request never waits behind a keyframe; below input (100).
const PRIORITY: i32 = 50;

pub struct ControlSender {
    stream: SendStream,
    buf: Vec<u8>,
}

pub struct ControlReceiver {
    stream: RecvStream,
    buf: Vec<u8>,
    /// Once the peer has sent `Alive`, silence this long ends the stream
    /// with [`NetError::TimedOut`]. A peer that never sends it (an older
    /// build) is never timed out.
    silent_limit: Duration,
    heard_alive: bool,
}

impl ControlSender {
    pub async fn send(&mut self, message: &ControlMessage) -> Result<(), NetError> {
        self.buf.clear();
        message
            .encode(&mut self.buf)
            .map_err(|e| NetError::Protocol(e.to_string()))?;
        self.stream
            .write_all(&self.buf)
            .await
            .map_err(|e| NetError::Connection(e.to_string()))
    }

    /// Ends the stream and waits, at most `wait`, until the peer has
    /// acknowledged everything sent. Closing a connection discards data not
    /// yet delivered, so call this before closing on purpose.
    pub async fn finish(&mut self, wait: std::time::Duration) {
        if self.stream.finish().is_ok() {
            let _ = tokio::time::timeout(wait, self.stream.stopped()).await;
        }
    }
}

impl ControlReceiver {
    fn new(stream: RecvStream) -> Self {
        Self {
            stream,
            buf: Vec::new(),
            silent_limit: SILENT_LIMIT,
            heard_alive: false,
        }
    }

    /// Changes how long a peer that sends `Alive` may stay silent (tests).
    pub fn set_silent_limit(&mut self, limit: Duration) {
        self.silent_limit = limit;
    }

    /// The next message; `Ok(None)` when the peer finished the stream.
    /// Unknown optional message types and `Alive` are skipped; once the peer
    /// has sent `Alive`, nothing at all for the silent limit is
    /// [`NetError::TimedOut`].
    pub async fn recv(&mut self) -> Result<Option<ControlMessage>, NetError> {
        loop {
            if let Some((frame, used)) =
                envelope::decode(&self.buf).map_err(|e| NetError::Protocol(e.to_string()))?
            {
                self.buf.drain(..used);
                match ControlMessage::from_frame(&frame).map_err(|e| match e {
                    DecodeError::UnknownRequired(kind) => NetError::Version(kind),
                    other => NetError::Protocol(other.to_string()),
                })? {
                    Decoded::Message(ControlMessage::Alive(_)) => {
                        self.heard_alive = true;
                        continue;
                    }
                    Decoded::Message(message) => return Ok(Some(message)),
                    Decoded::SkippedOptional(kind) => {
                        tracing::debug!("skipping unknown optional control message {kind:#06x}");
                        continue;
                    }
                }
            }
            let read = self.stream.read_chunk(64 * 1024);
            let chunk = if self.heard_alive {
                tokio::time::timeout(self.silent_limit, read)
                    .await
                    .map_err(|_| NetError::TimedOut)?
            } else {
                read.await
            };
            match chunk.map_err(|e| NetError::Connection(e.to_string()))? {
                Some(chunk) => self.buf.extend_from_slice(&chunk),
                None => return Ok(None),
            }
        }
    }
}

fn hello() -> ControlMessage {
    ControlMessage::Hello(Hello {
        proto_minor: PROTOCOL_MINOR,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        features: 0,
    })
}

async fn expect_hello(receiver: &mut ControlReceiver) -> Result<Hello, NetError> {
    match receiver.recv().await? {
        Some(ControlMessage::Hello(hello)) => Ok(hello),
        Some(other) => Err(NetError::Protocol(format!("expected Hello, got {other:?}"))),
        None => Err(NetError::Protocol(
            "control stream closed before Hello".into(),
        )),
    }
}

/// Dialer side: opens the control stream and exchanges Hello.
pub async fn open(conn: &Connection) -> Result<(ControlSender, ControlReceiver, Hello), NetError> {
    let (send, recv) = conn
        .open_bi()
        .await
        .map_err(|e| NetError::Connection(e.to_string()))?;
    let _ = send.set_priority(PRIORITY);
    let mut sender = ControlSender {
        stream: send,
        buf: Vec::new(),
    };
    let mut receiver = ControlReceiver::new(recv);
    sender.send(&hello()).await?;
    let theirs = expect_hello(&mut receiver).await?;
    Ok((sender, receiver, theirs))
}

/// Acceptor side: accepts the control stream and exchanges Hello.
pub async fn accept(
    conn: &Connection,
) -> Result<(ControlSender, ControlReceiver, Hello), NetError> {
    let (send, recv) = conn
        .accept_bi()
        .await
        .map_err(|e| NetError::Connection(e.to_string()))?;
    let _ = send.set_priority(PRIORITY);
    let mut sender = ControlSender {
        stream: send,
        buf: Vec::new(),
    };
    let mut receiver = ControlReceiver::new(recv);
    let theirs = expect_hello(&mut receiver).await?;
    sender.send(&hello()).await?;
    Ok((sender, receiver, theirs))
}
