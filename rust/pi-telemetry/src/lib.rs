//! Telemetry contracts for pi.
//!
//! Rust port of `@earendil-works/pi-telemetry` (`packages/telemetry`).
//!
//! Language mapping notes:
//! - The JS package's heavy conditional-type vocabulary (exact attribute
//!   inference, per-schema span typing) is compile-time only; Rust replaces
//!   it with plain data types plus the identity `define_telemetry_schema`.
//! - `start_span` callbacks are synchronous and return `Result<T, E>`
//!   instead of `T | Promise<T>`. Recording semantics are identical: the
//!   span settles when the callback returns, failures produce an automatic
//!   error status unless an explicit status was set.
//! - JS inspects `error instanceof Error` to record `{name, message}` in
//!   the automatic error status; Rust cannot inspect a generic `E`, so the
//!   automatic status is always `Error { error: None }`. Explicit
//!   `set_status` calls are unaffected. The conformance contract only
//!   requires `status.status == "error"` for failures.
//! - JS Proxy-based "unreadable payload" passivity tests are unrepresentable
//!   (no runtime property access failures in Rust); recording is trivially
//!   infallible here.

mod memory;
mod noop;
mod schema;

pub use memory::{InMemoryTelemetryContext, RecordedTelemetryEvent, RecordedTelemetrySpan};
pub use noop::{noop_span_handle, NOOP_TELEMETRY_CONTEXT};
pub use schema::{
    create_typed_span_starter, define_telemetry_schema, AttributeKind, Cardinality, TelemetryAttributeDefinition,
    TelemetryEventDefinition, TelemetryParentDefinition, TelemetrySchemaDefinition, TelemetrySpanDefinition,
    TelemetryStatusDefinition, TypedSpanStarter,
};

use std::sync::Mutex;
use std::sync::Arc;

/// One recorded attribute value: string, number, boolean, or an array of one
/// of those. Arrays are copied on record, like the JS implementation.
#[derive(Clone, Debug, PartialEq)]
pub enum AttributeValue {
    Str(String),
    Number(f64),
    Bool(bool),
    StrArray(Vec<String>),
    NumberArray(Vec<f64>),
    BoolArray(Vec<bool>),
}

impl AttributeValue {
    fn copy(value: &AttributeValue) -> AttributeValue {
        value.clone()
    }
}

impl From<&str> for AttributeValue {
    fn from(value: &str) -> Self {
        AttributeValue::Str(value.to_string())
    }
}

impl From<String> for AttributeValue {
    fn from(value: String) -> Self {
        AttributeValue::Str(value)
    }
}

impl From<f64> for AttributeValue {
    fn from(value: f64) -> Self {
        AttributeValue::Number(value)
    }
}

impl From<bool> for AttributeValue {
    fn from(value: bool) -> Self {
        AttributeValue::Bool(value)
    }
}

impl From<Vec<String>> for AttributeValue {
    fn from(value: Vec<String>) -> Self {
        AttributeValue::StrArray(value)
    }
}

impl From<Vec<f64>> for AttributeValue {
    fn from(value: Vec<f64>) -> Self {
        AttributeValue::NumberArray(value)
    }
}

impl From<Vec<bool>> for AttributeValue {
    fn from(value: Vec<bool>) -> Self {
        AttributeValue::BoolArray(value)
    }
}

/// Ordered span attributes. Mirrors the JS plain object: insertion order is
/// preserved, undefined values are dropped (Rust callers simply omit them).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpanAttributes(pub Vec<(String, AttributeValue)>);

impl SpanAttributes {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn with(name: impl Into<String>, value: impl Into<AttributeValue>) -> Self {
        Self(vec![(name.into(), value.into())])
    }

    pub fn get(&self, name: &str) -> Option<&AttributeValue> {
        self.0.iter().find(|(key, _)| key == name).map(|(_, value)| value)
    }
}

impl From<Vec<(String, AttributeValue)>> for SpanAttributes {
    fn from(value: Vec<(String, AttributeValue)>) -> Self {
        SpanAttributes(value)
    }
}

/// Mirrors `copyAttributes`: copies every entry; array values are deep-copied.
fn copy_attributes(attributes: &SpanAttributes) -> SpanAttributes {
    SpanAttributes(
        attributes
            .0
            .iter()
            .map(|(name, value)| (name.clone(), AttributeValue::copy(value)))
            .collect(),
    )
}

/// Mirrors `mergeAttributes`: starts from a copy of the current attributes,
/// then overwrites/inserts entries (existing keys keep their position, like
/// JS object assignment).
fn merge_attributes(current: &SpanAttributes, attributes: &SpanAttributes) -> SpanAttributes {
    let mut merged = copy_attributes(current);
    for (name, value) in &attributes.0 {
        if let Some(existing) = merged.0.iter_mut().find(|(key, _)| key == name) {
            existing.1 = value.clone();
        } else {
            merged.0.push((name.clone(), value.clone()));
        }
    }
    merged
}

#[derive(Clone, Debug, PartialEq)]
pub struct ErrorInfo {
    pub name: String,
    pub message: String,
}

/// Mirrors `SpanStatus`.
#[derive(Clone, Debug, PartialEq)]
pub enum SpanStatus {
    Ok,
    Error { error: Option<ErrorInfo> },
}

fn copy_status(status: &SpanStatus) -> SpanStatus {
    status.clone()
}

/// Mirrors `SpanOptions`.
#[derive(Clone, Debug, PartialEq)]
pub struct SpanOptions {
    pub name: String,
    pub attributes: SpanAttributes,
}

impl SpanOptions {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: SpanAttributes::new(),
        }
    }

    pub fn with_attributes(name: impl Into<String>, attributes: SpanAttributes) -> Self {
        Self {
            name: name.into(),
            attributes,
        }
    }
}

/// Mirrors the JS `TelemetrySpan` handle: `startSpan`, `addEvent`,
/// `setAttributes`, `setStatus`. Concrete type (not a trait object) so the
/// generic `start_span` stays usable; a handle is either a recording span or
/// the shared no-op span.
#[derive(Clone)]
pub struct TelemetrySpanHandle {
    inner: SpanHandleInner,
}

#[derive(Clone)]
pub(crate) enum SpanHandleInner {
    Recording(Arc<Mutex<memory::SpanRecord>>),
    Noop,
}

impl TelemetrySpanHandle {
    pub fn start_span<T, E, F>(&self, options: SpanOptions, callback: F) -> Result<T, E>
    where
        F: FnOnce(&TelemetrySpanHandle) -> Result<T, E>,
    {
        match &self.inner {
            SpanHandleInner::Noop => noop::start_noop_span(options, callback),
            SpanHandleInner::Recording(record) => {
                // Guard scope is a single statement: holding the record lock
                // across the call would deadlock on the re-entrant lock inside
                // start_recording_span (same thread, std Mutex).
                let settled = record.lock().unwrap().settled;
                if settled {
                    // Mirrors `if (parent?.settled) return NOOP...`.
                    noop::start_noop_span(options, callback)
                } else {
                    let state = record.lock().unwrap().state.clone();
                    memory::start_recording_span(&state, Some(record), options, callback)
                }
            }
        }
    }

    pub fn add_event(&self, name: &str, attributes: SpanAttributes) {
        if let SpanHandleInner::Recording(record) = &self.inner {
            let mut span = record.lock().unwrap();
            if span.settled {
                return;
            }
            span.events.push(memory::RecordedEvent {
                name: name.to_string(),
                attributes: copy_attributes(&attributes),
            });
        }
    }

    pub fn set_attributes(&self, attributes: SpanAttributes) {
        if let SpanHandleInner::Recording(record) = &self.inner {
            let mut span = record.lock().unwrap();
            if span.settled {
                return;
            }
            span.attributes = merge_attributes(&span.attributes, &attributes);
        }
    }

    pub fn set_status(&self, status: SpanStatus) {
        if let SpanHandleInner::Recording(record) = &self.inner {
            let mut span = record.lock().unwrap();
            if span.settled {
                return;
            }
            span.status = copy_status(&status);
            span.explicit_status = true;
        }
    }
}

/// Mirrors the JS `TelemetryContext` interface. `start_span` admits the
/// callback synchronously, records the span, and settles it when the
/// callback returns (Ok or Err).
pub trait TelemetryContext {
    fn start_span<T, E, F>(&self, options: SpanOptions, callback: F) -> Result<T, E>
    where
        F: FnOnce(&TelemetrySpanHandle) -> Result<T, E>;
}
