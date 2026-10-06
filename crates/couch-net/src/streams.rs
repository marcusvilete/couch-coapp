//! One acceptor for a connection's unidirectional streams, handing each to
//! whoever subscribed to its kind (`couch_proto::media::stream_kind`). QUIC
//! has a single accept queue per connection, so two acceptors would take each
//! other's streams. Video and input share a room connection: one member's
//! input stream and the other's video epochs can arrive on the same one.
//!
//! The kind byte is read here; a subscriber gets the stream positioned just
//! after it.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use iroh::endpoint::{Connection, RecvStream, VarInt};
use tokio::sync::mpsc;

/// Streams waiting per subscriber. Video opens one per epoch, input one per
/// connection, so this is never reached by a well-behaved peer.
const QUEUE: usize = 16;
/// A stream must name its kind this soon. QUIC already caps how many a peer
/// can have open (64), so this only frees the slots sooner.
const KIND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct Streams {
    subscribers: Arc<Mutex<HashMap<u8, mpsc::Sender<RecvStream>>>>,
}

impl Streams {
    /// Starts the acceptor. Call inside a tokio runtime.
    pub fn start(conn: Connection) -> Self {
        let subscribers: Arc<Mutex<HashMap<u8, mpsc::Sender<RecvStream>>>> = Arc::default();
        let routes = subscribers.clone();
        tokio::spawn(async move {
            while let Ok(recv) = conn.accept_uni().await {
                tokio::spawn(route(recv, routes.clone()));
            }
            routes.lock().unwrap().clear();
        });
        Self { subscribers }
    }

    /// Streams of `kind` from now on, positioned after their kind byte. A
    /// second subscription to a kind replaces the first. Streams of a kind
    /// nobody subscribed to (or whose subscriber went away) are stopped.
    pub fn subscribe(&self, kind: u8) -> mpsc::Receiver<RecvStream> {
        let (tx, rx) = mpsc::channel(QUEUE);
        self.subscribers.lock().unwrap().insert(kind, tx);
        rx
    }
}

async fn route(mut recv: RecvStream, routes: Arc<Mutex<HashMap<u8, mpsc::Sender<RecvStream>>>>) {
    let mut kind = [0u8; 1];
    match tokio::time::timeout(KIND_TIMEOUT, recv.read_exact(&mut kind)).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => return,
        Err(_) => {
            let _ = recv.stop(VarInt::from_u32(0));
            return;
        }
    }
    let route = routes.lock().unwrap().get(&kind[0]).cloned();
    let Some(tx) = route else {
        tracing::debug!("ignoring stream of kind {:#04x}", kind[0]);
        let _ = recv.stop(VarInt::from_u32(0));
        return;
    };
    if let Err(e) = tx.try_send(recv) {
        let mut recv = match e {
            mpsc::error::TrySendError::Full(recv) | mpsc::error::TrySendError::Closed(recv) => recv,
        };
        tracing::debug!("no taker for a stream of kind {:#04x}", kind[0]);
        let _ = recv.stop(VarInt::from_u32(0));
    }
}
