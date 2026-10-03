//! One reader for a connection's datagrams, handing each to whoever
//! subscribed to its tag (`couch_proto::media::datagram`). QUIC has a single
//! datagram queue per connection, so two readers would steal from each other.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use bytes::Bytes;
use couch_proto::media::datagram_tag;
use iroh::endpoint::Connection;
use tokio::sync::mpsc;

/// Datagrams waiting per subscriber. They are disposable: when a subscriber
/// falls this far behind, newer ones are dropped.
const QUEUE: usize = 256;

#[derive(Clone)]
pub struct Datagrams {
    subscribers: Arc<Mutex<HashMap<u8, mpsc::Sender<Bytes>>>>,
}

impl Datagrams {
    /// Starts the reader. Call inside a tokio runtime.
    pub fn start(conn: Connection) -> Self {
        let subscribers: Arc<Mutex<HashMap<u8, mpsc::Sender<Bytes>>>> = Arc::default();
        let routes = subscribers.clone();
        tokio::spawn(async move {
            while let Ok(datagram) = conn.read_datagram().await {
                let Some((tag, _)) = datagram_tag(&datagram) else {
                    continue;
                };
                let route = routes.lock().unwrap().get(&tag).cloned();
                if let Some(tx) = route {
                    // Without the tag byte.
                    let _ = tx.try_send(datagram.slice(1..));
                }
            }
            routes.lock().unwrap().clear();
        });
        Self { subscribers }
    }

    /// Payloads of datagrams tagged `tag`, from now on. A second subscription
    /// to a tag replaces the first.
    pub fn subscribe(&self, tag: u8) -> mpsc::Receiver<Bytes> {
        let (tx, rx) = mpsc::channel(QUEUE);
        self.subscribers.lock().unwrap().insert(tag, tx);
        rx
    }
}
