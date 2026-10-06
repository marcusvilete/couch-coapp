//! Room and session state machines.
//!
//! Sans-IO: transitions take `(state, input, now)` and return effects for the
//! caller to carry out. Nothing here does networking, spawns threads or reads
//! a clock, so behaviour is deterministic and property-testable. `couch-engine`
//! drives these machines.
//!
//! - [`room`]: membership, the owner's policy, and the mesh of connections.
//! - [`limits`]: admission rate limits.
//! - [`signing`]: room ids and the signatures on records and policies.
//! - [`session`]: a session as its host runs it: viewers, requests for
//!   control, the panic key's pause. Who holds a seat is decided by
//!   `couch_input::router`, which this mirrors.

pub mod limits;
pub mod room;
pub mod session;
pub mod signing;

pub use ed25519_dalek::SigningKey;
