//! Telemetry schema definitions and typed span starter, mirroring the
//! runtime surface of `packages/telemetry/src/index.ts`.
//!
//! The JS `defineTelemetrySchema`/`createTypedSpanStarter` types exist for
//! compile-time attribute inference; at runtime they are an identity and a
//! binding wrapper respectively. Rust keeps those runtime behaviors and
//! replaces the type vocabulary with plain data structures.

use crate::{InMemoryTelemetryContext, SpanAttributes, SpanOptions, TelemetryContext, TelemetrySpanHandle};

#[derive(Clone, Debug, PartialEq)]
pub enum Cardinality {
    Low,
    High,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AttributeKind {
    Str {
        values: Option<Vec<String>>,
        examples: Option<Vec<String>>,
    },
    Number {
        values: Option<Vec<f64>>,
        examples: Option<Vec<f64>>,
    },
    Bool {
        values: Option<Vec<bool>>,
        examples: Option<Vec<bool>>,
    },
    StrArray {
        element_values: Option<Vec<String>>,
        examples: Option<Vec<Vec<String>>>,
    },
    NumberArray {
        element_values: Option<Vec<f64>>,
        examples: Option<Vec<Vec<f64>>>,
    },
    BoolArray {
        element_values: Option<Vec<bool>>,
        examples: Option<Vec<Vec<bool>>>,
    },
}

/// Mirrors `TelemetryAttributeDefinition` (with `required` folded in, as JS
/// adds it only on start/event definitions).
#[derive(Clone, Debug, PartialEq)]
pub struct TelemetryAttributeDefinition {
    pub description: String,
    pub sensitive: Option<bool>,
    pub cardinality: Option<Cardinality>,
    pub required: bool,
    pub kind: AttributeKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TelemetryEventDefinition {
    pub description: String,
    pub attributes: Vec<(String, TelemetryAttributeDefinition)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TelemetryParentDefinition {
    Any,
    RootOrExternal,
    Spans(Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TelemetrySpanDefinition {
    pub description: String,
    pub parents: TelemetryParentDefinition,
    pub start_attributes: Vec<(String, TelemetryAttributeDefinition)>,
    pub end_attributes: Vec<(String, TelemetryAttributeDefinition)>,
    pub events: Option<Vec<(String, TelemetryEventDefinition)>>,
    pub status: TelemetryStatusDefinition,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TelemetryStatusDefinition {
    pub default: String,
    pub error_when: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TelemetrySchemaDefinition {
    pub version: u64,
    pub spans: Vec<(String, TelemetrySpanDefinition)>,
}

/// Typed identity helper for serializable telemetry schema data. The JS
/// version exists for compile-time inference; Rust types already carry the
/// shape, so this is the identity function.
pub fn define_telemetry_schema<T>(schema: T) -> T {
    schema
}

/// Target for a typed span starter: a root context or a parent span.
enum StarterTarget {
    Root(InMemoryTelemetryContext),
    Span(TelemetrySpanHandle),
}

/// Mirrors the runtime behavior of `createTypedSpanStarter`: a wrapper whose
/// `start_span` binds an explicit parent context and passes a child starter
/// bound to the newly created span into the callback.
pub struct TypedSpanStarter {
    target: StarterTarget,
}

impl TypedSpanStarter {
    pub fn new(context: InMemoryTelemetryContext) -> Self {
        Self {
            target: StarterTarget::Root(context),
        }
    }

    pub fn start_span<T, E, F>(
        &self,
        name: &str,
        attributes: SpanAttributes,
        callback: F,
    ) -> Result<T, E>
    where
        F: FnOnce(&TelemetrySpanHandle, &TypedSpanStarter) -> Result<T, E>,
    {
        let options = SpanOptions::with_attributes(name, attributes);
        match &self.target {
            StarterTarget::Root(context) => context.start_span(options, |span| {
                let child_starter = TypedSpanStarter {
                    target: StarterTarget::Span(span.clone()),
                };
                callback(span, &child_starter)
            }),
            StarterTarget::Span(span) => span.start_span(options, |child| {
                let child_starter = TypedSpanStarter {
                    target: StarterTarget::Span(child.clone()),
                };
                callback(child, &child_starter)
            }),
        }
    }
}

/// Mirrors `createTypedSpanStarter`. The JS signature takes schema values
/// purely for compile-time vocabulary inference; Rust has no equivalent
/// literal-type machinery, so schemas are not required (see lib.rs notes).
pub fn create_typed_span_starter(context: InMemoryTelemetryContext) -> TypedSpanStarter {
    TypedSpanStarter::new(context)
}
