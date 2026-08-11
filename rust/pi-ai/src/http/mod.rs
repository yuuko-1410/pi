//! HTTP transport helpers (SSE parsing; the HTTP client layer follows).

pub mod sse;

pub use sse::{SseEvent, SseParser};
