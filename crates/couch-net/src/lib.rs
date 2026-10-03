//! Networking on iroh: the endpoint and its media congestion controller, the
//! control stream, and video over per-epoch QUIC streams with receiver
//! reports. Admission and the room mesh arrive in M4a.

pub mod congestion;
pub mod control;
pub mod endpoint;
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
