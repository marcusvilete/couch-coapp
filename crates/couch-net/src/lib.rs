//! Networking on iroh: the endpoint and its media congestion controller, the
//! control stream and the password check that admits a connection into a
//! room, video over per-epoch QUIC streams with receiver reports, audio, and
//! input. The room mesh itself is `couch_room::room`, driven by the engine.
//!
//! Each connection has one reader for its datagrams ([`datagrams`]) and one
//! acceptor for its unidirectional streams ([`streams`]); video, audio and
//! input subscribe to them by tag or kind.

pub mod addr;
pub mod admission;
pub mod audio;
pub mod congestion;
pub mod control;
pub mod datagrams;
pub mod endpoint;
pub mod input;
#[cfg(feature = "netem")]
pub mod netem;
pub mod streams;
pub mod video;

pub use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayUrl, SecretKey,
    endpoint::{Connection, Incoming, WeakConnectionHandle},
};

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("could not bind the iroh endpoint: {0}")]
    Bind(String),
    #[error("connection: {0}")]
    Connection(String),
    #[error("protocol: {0}")]
    Protocol(String),
    /// A message type we must understand and don't: the peer is newer.
    #[error("unknown message type {0:#06x} that must be understood")]
    Version(u16),
    /// A peer that sends `Alive` went silent for the silent limit.
    #[error("the peer went silent")]
    TimedOut,
}
