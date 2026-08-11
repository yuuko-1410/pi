//! In-memory telemetry recording, mirroring `packages/telemetry/src/memory.ts`.

use std::sync::Mutex;
use std::sync::Arc;

use crate::{copy_attributes, copy_status, SpanAttributes, SpanOptions, SpanStatus, TelemetryContext, TelemetrySpanHandle};

/// Mirrors `RecordedTelemetryEvent` (detached snapshot).
#[derive(Clone, Debug, PartialEq)]
pub struct RecordedTelemetryEvent {
    pub name: String,
    pub attributes: SpanAttributes,
}

/// Mirrors `RecordedTelemetrySpan` (detached snapshot).
#[derive(Clone, Debug, PartialEq)]
pub struct RecordedTelemetrySpan {
    pub id: u64,
    pub parent_id: Option<u64>,
    pub name: String,
    pub attributes: SpanAttributes,
    pub events: Vec<RecordedTelemetryEvent>,
    pub status: SpanStatus,
    pub settled: bool,
    pub end_sequence: Option<u64>,
}

/// Shared mutable recording state (single-threaded, like the JS runtime).
pub(crate) struct InMemoryTelemetryState {
    pub(crate) spans: Vec<Arc<Mutex<SpanRecord>>>,
    pub(crate) next_span_id: u64,
    pub(crate) next_end_sequence: u64,
}

pub(crate) struct SpanRecord {
    pub(crate) state: Arc<Mutex<InMemoryTelemetryState>>,
    pub(crate) id: u64,
    pub(crate) parent_id: Option<u64>,
    pub(crate) name: String,
    pub(crate) attributes: SpanAttributes,
    pub(crate) events: Vec<RecordedEvent>,
    pub(crate) status: SpanStatus,
    pub(crate) explicit_status: bool,
    pub(crate) settled: bool,
    pub(crate) end_sequence: Option<u64>,
}

pub(crate) struct RecordedEvent {
    pub(crate) name: String,
    pub(crate) attributes: SpanAttributes,
}

/// Mirrors `automaticErrorStatus`: JS records `{name, message}` when the
/// rejection value is an `Error`; the generic Rust `E` cannot be inspected,
/// so the automatic status always omits details (see lib.rs notes).
fn automatic_error_status() -> SpanStatus {
    SpanStatus::Error { error: None }
}

/// Mirrors `settleSpan`: no-op when already settled; failed spans get an
/// automatic error status unless an explicit status was recorded; the span
/// receives the next end sequence.
fn settle_span(record: &Arc<Mutex<SpanRecord>>, failed: bool) {
    let mut span = record.lock().unwrap();
    if span.settled {
        return;
    }
    if failed && !span.explicit_status {
        span.status = automatic_error_status();
    }
    span.settled = true;
    span.end_sequence = {
        let mut state = span.state.lock().unwrap();
        let sequence = state.next_end_sequence;
        state.next_end_sequence += 1;
        Some(sequence)
    };
}

/// Mirrors `createSpan`.
fn create_span(
    state: &Arc<Mutex<InMemoryTelemetryState>>,
    parent: Option<&Arc<Mutex<SpanRecord>>>,
    options: &SpanOptions,
) -> Arc<Mutex<SpanRecord>> {
    let mut state_ref = state.lock().unwrap();
    let id = state_ref.next_span_id;
    state_ref.next_span_id += 1;
    let record = Arc::new(Mutex::new(SpanRecord {
        state: state.clone(),
        id,
        parent_id: parent.map(|parent| parent.lock().unwrap().id),
        name: options.name.clone(),
        attributes: copy_attributes(&options.attributes),
        events: Vec::new(),
        status: SpanStatus::Ok,
        explicit_status: false,
        settled: false,
        end_sequence: None,
    }));
    state_ref.spans.push(record.clone());
    record
}

/// Mirrors `startInMemorySpan`.
pub(crate) fn start_recording_span<T, E, F>(
    state: &Arc<Mutex<InMemoryTelemetryState>>,
    parent: Option<&Arc<Mutex<SpanRecord>>>,
    options: SpanOptions,
    callback: F,
) -> Result<T, E>
where
    F: FnOnce(&TelemetrySpanHandle) -> Result<T, E>,
{
    // `if (parent?.settled) return NOOP_TELEMETRY_CONTEXT.startSpan(...)` is
    // checked by the caller (handle) before reaching here; keep the guard
    // here as well for direct root calls with an explicit settled parent.
    if let Some(parent) = parent {
        if parent.lock().unwrap().settled {
            return crate::noop::start_noop_span(options, callback);
        }
    }

    let record = create_span(state, parent, &options);
    let handle = TelemetrySpanHandle {
        inner: crate::SpanHandleInner::Recording(record.clone()),
    };
    match callback(&handle) {
        Ok(value) => {
            settle_span(&record, false);
            Ok(value)
        }
        Err(error) => {
            settle_span(&record, true);
            Err(error)
        }
    }
}

/// Backend-neutral reference implementation that records spans in process
/// memory. Create a fresh instance to isolate tests or recording scopes.
#[derive(Clone)]
pub struct InMemoryTelemetryContext {
    state: Arc<Mutex<InMemoryTelemetryState>>,
}

impl InMemoryTelemetryContext {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(InMemoryTelemetryState {
                spans: Vec::new(),
                next_span_id: 1,
                next_end_sequence: 1,
            })),
        }
    }

    /// Returns detached snapshots in span-start order.
    pub fn get_spans(&self) -> Vec<RecordedTelemetrySpan> {
        self.state
            .lock().unwrap()
            .spans
            .iter()
            .map(|record| {
                let span = record.lock().unwrap();
                RecordedTelemetrySpan {
                    id: span.id,
                    parent_id: span.parent_id,
                    name: span.name.clone(),
                    attributes: copy_attributes(&span.attributes),
                    events: span
                        .events
                        .iter()
                        .map(|event| RecordedTelemetryEvent {
                            name: event.name.clone(),
                            attributes: copy_attributes(&event.attributes),
                        })
                        .collect(),
                    status: copy_status(&span.status),
                    settled: span.settled,
                    end_sequence: span.end_sequence,
                }
            })
            .collect()
    }
}

impl Default for InMemoryTelemetryContext {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryContext for InMemoryTelemetryContext {
    fn start_span<T, E, F>(&self, options: SpanOptions, callback: F) -> Result<T, E>
    where
        F: FnOnce(&TelemetrySpanHandle) -> Result<T, E>,
    {
        start_recording_span(&self.state, None, options, callback)
    }
}
