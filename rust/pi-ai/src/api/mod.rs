//! API-layer helpers (ports of `packages/ai/src/api/*` pure-logic modules).
//!
//! The HTTP transport layer (SSE streaming, request dispatch, per-provider
//! adapters) follows; these modules are pure conversions used by it.

pub mod constrained_sampling;
pub mod openai_responses_shared;
pub mod prompt_cache;
pub mod simple_options;
pub mod transform_messages;
