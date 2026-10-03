//! Room, session and seat state machines.
//!
//! Sans-IO: transitions take `(state, input, now)` and return effects for the
//! caller to carry out. Nothing here does networking, spawns threads or reads
//! a clock, so behaviour is deterministic and property-testable. `couch-engine`
//! drives these machines. Admission and room membership arrive in M4a, the
//! session and control transitions in M5.

pub mod session;
