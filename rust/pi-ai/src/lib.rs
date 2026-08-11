//! LLM provider abstractions for pi (Rust port of `@earendil-works/pi-ai`).
//!
//! This crate currently covers the core layer of `packages/ai`:
//! - `types`: model/message/stream types (port of `src/types.ts`)
//! - `event_stream`: the assistant message event stream (port of
//!   `src/utils/event-stream.ts`)
//! - `utils`: pure utilities (ports of `src/utils/*`)
//!
//! The provider/API layer (`src/api/*`, `src/providers/*`, `src/auth/*`),
//! model catalogs, and the remaining utils follow in later ports.

pub mod api;
pub mod event_stream;
pub mod types;
pub mod utils;

pub use event_stream::{
    create_assistant_message_event_stream, AssistantMessageEventStream, EventStream,
};
pub use types::*;
