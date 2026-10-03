//! Networking on iroh: endpoint construction and identity now; the control
//! stream, epoch-stream video and datagram channels from M1; admission and
//! the room mesh from M4a.

pub mod endpoint;

pub use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl, SecretKey};

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("could not bind the iroh endpoint: {0}")]
    Bind(String),
}
