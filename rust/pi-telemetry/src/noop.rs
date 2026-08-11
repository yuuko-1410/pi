//! No-op telemetry, mirroring `packages/telemetry/src/noop.ts`.

use std::sync::OnceLock;

use crate::{SpanOptions, TelemetryContext, TelemetrySpanHandle};

/// Mirrors `startNoopSpan`: admits the callback synchronously with the inert
/// no-op span and propagates the result unchanged.
pub(crate) fn start_noop_span<T, E, F>(options: SpanOptions, callback: F) -> Result<T, E>
where
    F: FnOnce(&TelemetrySpanHandle) -> Result<T, E>,
{
    let _ = options;
    callback(noop_span_handle())
}

/// The shared inert span handle. Mirrors the frozen `noopTelemetrySpan`:
/// every no-op `startSpan` yields the same handle, and all recording methods
/// are no-ops.
pub fn noop_span_handle() -> &'static TelemetrySpanHandle {
    static HANDLE: OnceLock<TelemetrySpanHandle> = OnceLock::new();
    HANDLE.get_or_init(|| TelemetrySpanHandle {
        inner: crate::SpanHandleInner::Noop,
    })
}

/// Shared telemetry context used when an application does not provide one.
pub struct NoopTelemetryContext;

/// Mirrors `NOOP_TELEMETRY_CONTEXT`. A ZST; `start_span` never inspects the
/// options (the JS implementation does not read unreadable option payloads,
/// which is trivially true here) and never records.
pub const NOOP_TELEMETRY_CONTEXT: NoopTelemetryContext = NoopTelemetryContext;

impl TelemetryContext for NoopTelemetryContext {
    fn start_span<T, E, F>(&self, options: SpanOptions, callback: F) -> Result<T, E>
    where
        F: FnOnce(&TelemetrySpanHandle) -> Result<T, E>,
    {
        start_noop_span(options, callback)
    }
}
