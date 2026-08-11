//! Pi JSON-RPC client over byte transports, port of `packages/client`.
//!
//! Synchronous analog of the JS PiClient: `connect` and `request` block
//! until the server responds; a background reader thread decodes inbound
//! frames. Session lease and attachment semantics mirror the JS state
//! machine.

pub mod client;
pub mod connection;
pub mod errors;
pub mod unix;
