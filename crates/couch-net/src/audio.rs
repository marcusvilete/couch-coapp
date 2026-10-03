//! Audio over iroh (docs/protocol.md, "Channels"): one datagram per 10 ms
//! Opus packet, host to viewer. Datagrams never wait behind video, and a lost
//! one costs a concealed frame rather than a stall.

use couch_proto::media::{AudioDatagram, datagram, encode_datagram};
use iroh::endpoint::{Connection, SendDatagramError};
use tokio::sync::mpsc;

use crate::datagrams::Datagrams;

/// Host side: sends one packet. Never blocks; `false` once the connection
/// is gone.
pub fn send_audio(conn: &Connection, packet: &AudioDatagram) -> bool {
    match conn.send_datagram(encode_datagram(datagram::AUDIO, packet).into()) {
        Ok(()) => true,
        Err(SendDatagramError::ConnectionLost(_)) => false,
        Err(e) => {
            tracing::debug!("audio datagram not sent: {e}");
            true
        }
    }
}

/// Viewer side: decoded audio datagrams, malformed ones skipped. Call inside
/// a tokio runtime.
pub fn receive_audio(datagrams: &Datagrams) -> mpsc::Receiver<AudioDatagram> {
    let mut raw = datagrams.subscribe(datagram::AUDIO);
    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(async move {
        while let Some(payload) = raw.recv().await {
            match AudioDatagram::decode(&payload) {
                // Disposable: drop rather than wait behind a slow consumer.
                Some(packet) => {
                    if let Err(mpsc::error::TrySendError::Closed(_)) = tx.try_send(packet) {
                        return;
                    }
                }
                None => tracing::debug!("malformed audio datagram"),
            }
        }
    });
    rx
}
