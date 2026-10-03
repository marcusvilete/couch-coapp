//! The control stream: framed `couch_proto::control` messages on the first
//! bidirectional stream of a connection. The dialer opens it and speaks first
//! (QUIC only announces a stream once data is sent on it).

use couch_proto::{
    PROTOCOL_MINOR,
    control::{ControlMessage, Decoded, Hello},
    envelope,
};
use iroh::endpoint::{Connection, RecvStream, SendStream};

use crate::NetError;

pub struct ControlSender {
    stream: SendStream,
    buf: Vec<u8>,
}

pub struct ControlReceiver {
    stream: RecvStream,
    buf: Vec<u8>,
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
}

impl ControlReceiver {
    /// The next message; `Ok(None)` when the peer finished the stream.
    /// Unknown optional message types are skipped.
    pub async fn recv(&mut self) -> Result<Option<ControlMessage>, NetError> {
        loop {
            if let Some((frame, used)) =
                envelope::decode(&self.buf).map_err(|e| NetError::Protocol(e.to_string()))?
            {
                self.buf.drain(..used);
                match ControlMessage::from_frame(&frame)
                    .map_err(|e| NetError::Protocol(e.to_string()))?
                {
                    Decoded::Message(message) => return Ok(Some(message)),
                    Decoded::SkippedOptional(kind) => {
                        tracing::debug!("skipping unknown optional control message {kind:#06x}");
                        continue;
                    }
                }
            }
            match self
                .stream
                .read_chunk(64 * 1024)
                .await
                .map_err(|e| NetError::Connection(e.to_string()))?
            {
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
    let mut sender = ControlSender {
        stream: send,
        buf: Vec::new(),
    };
    let mut receiver = ControlReceiver {
        stream: recv,
        buf: Vec::new(),
    };
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
    let mut sender = ControlSender {
        stream: send,
        buf: Vec::new(),
    };
    let mut receiver = ControlReceiver {
        stream: recv,
        buf: Vec::new(),
    };
    let theirs = expect_hello(&mut receiver).await?;
    sender.send(&hello()).await?;
    Ok((sender, receiver, theirs))
}
