//! Agent loop and session types for pi.
//!
//! Rust port of `@earendil-works/pi-agent-core` (`packages/agent`). The
//! type layer is ported first; the agent loop and harness follow.

pub mod agent_loop;
pub mod types;

pub use types::*;
