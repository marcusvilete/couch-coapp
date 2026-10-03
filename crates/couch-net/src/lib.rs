//! Networking on iroh: the endpoint and its media congestion controller, the
//! control stream, video over per-epoch QUIC streams with receiver reports,
//! and input. Admission and the room mesh arrive in M4a.

pub mod congestion;
pub mod control;
pub mod datagrams;
pub mod endpoint;
pub mod input;
#[cfg(feature = "netem")]
pub mod netem;
pub mod video;

pub use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl, SecretKey, endpoint::Connection};

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("could not bind the iroh endpoint: {0}")]
    Bind(String),
    #[error("connection: {0}")]
    Connection(String),
    #[error("protocol: {0}")]
    Protocol(String),
}
