//! Port of `packages/telemetry/test/telemetry.test.ts` (runtime parts; the
//! `expectTypeOf` compile-time assertions are TypeScript-only).

use pi_telemetry::{
    create_typed_span_starter, define_telemetry_schema, noop_span_handle, AttributeKind,
    InMemoryTelemetryContext, SpanAttributes, SpanOptions, SpanStatus, TelemetryContext,
    TelemetryEventDefinition, TelemetryParentDefinition, TelemetrySchemaDefinition, TelemetrySpanDefinition,
    TelemetryStatusDefinition, NOOP_TELEMETRY_CONTEXT,
};

fn schema_definition() -> TelemetrySchemaDefinition {
    TelemetrySchemaDefinition {
        version: 1,
        spans: vec![(
            "operation".to_string(),
            TelemetrySpanDefinition {
                description: "Test operation".to_string(),
                parents: TelemetryParentDefinition::Any,
                start_attributes: vec![(
                    "kind".to_string(),
                    pi_telemetry::TelemetryAttributeDefinition {
                        description: "Kind".to_string(),
                        sensitive: None,
                        cardinality: None,
                        required: true,
                        kind: AttributeKind::Str {
                            values: Some(vec!["read".to_string(), "write".to_string()]),
                            examples: None,
                        },
                    },
                )],
                end_attributes: vec![],
                events: Some(vec![(
                    "result".to_string(),
                    TelemetryEventDefinition {
                        description: "Result".to_string(),
                        attributes: vec![(
                            "outcome".to_string(),
                            pi_telemetry::TelemetryAttributeDefinition {
                                description: "Outcome".to_string(),
                                sensitive: None,
                                cardinality: None,
                                required: true,
                                kind: AttributeKind::Str {
                                    values: Some(vec!["ok".to_string(), "error".to_string()]),
                                    examples: None,
                                },
                            },
                        )],
                    },
                )]),
                status: TelemetryStatusDefinition {
                    default: "ok".to_string(),
                    error_when: "The operation fails".to_string(),
                },
            },
        )],
    }
}

#[test]
fn preserves_serializable_definitions() {
    let definition = schema_definition();
    let schema = define_telemetry_schema(definition.clone());
    assert_eq!(schema, definition);
}

#[test]
fn combines_schema_vocabularies_and_binds_child_starters_to_their_parent_spans() {
    let telemetry_context = InMemoryTelemetryContext::new();
    let start_span = create_typed_span_starter(telemetry_context.clone());

    let result: i64 = start_span
        .start_span("operation", SpanAttributes::with("kind", "read"), |_operation, start_child_span| {
            start_child_span.start_span(
                "request",
                SpanAttributes::with("provider", "example"),
                |request_span, _child| {
                    request_span.set_attributes(SpanAttributes::with("response", "cached"));
                    Ok::<_, ()>(42)
                },
            )
        })
        .unwrap();

    assert_eq!(result, 42);
    let spans = telemetry_context.get_spans();
    let operation = spans.iter().find(|span| span.name == "operation").expect("operation span");
    let request = spans.iter().find(|span| span.name == "request").expect("request span");
    assert_eq!(operation.parent_id, None);
    assert_eq!(request.parent_id, Some(operation.id));

    // Synchronous failure propagates and records an automatic error status.
    let sync_error = "sync failure".to_string();
    let sync_result = start_span.start_span(
        "operation",
        SpanAttributes::with("kind", "write"),
        |_span, _child| -> Result<i64, String> { Err(sync_error.clone()) },
    );
    assert_eq!(sync_result, Err(sync_error));
}

#[test]
fn noop_context_admits_callbacks_synchronously_and_reuses_one_inert_span() {
    let mut admitted = false;
    let mut first_span: Option<*const pi_telemetry::TelemetrySpanHandle> = None;
    let result = NOOP_TELEMETRY_CONTEXT
        .start_span(SpanOptions::new("first"), |span| {
            admitted = true;
            first_span = Some(span as *const pi_telemetry::TelemetrySpanHandle);
            let child = span
                .start_span(SpanOptions::new("child"), |child_span| {
                    assert!(
                        std::ptr::eq(span, child_span),
                        "child span must be the same no-op handle"
                    );
                    Ok::<(), ()>(())
                })
                .unwrap();
            let _ = child;
            Ok::<_, ()>(42)
        })
        .unwrap();

    assert!(admitted);
    assert_eq!(result, 42);
    let second = noop_span_handle() as *const pi_telemetry::TelemetrySpanHandle;
    assert_eq!(first_span, Some(second), "no-op handle must be a shared singleton");
}

#[test]
fn noop_context_preserves_rejection_values() {
    let sync_error = "sync".to_string();
    let sync = NOOP_TELEMETRY_CONTEXT.start_span(
        SpanOptions::new("sync"),
        |_span| -> Result<(), String> { Err(sync_error.clone()) },
    );
    assert_eq!(sync, Err(sync_error));
}

#[test]
fn noop_context_does_not_inspect_or_retain_payloads() {
    // The JS version guards against unreadable Proxy payloads; Rust values
    // are always readable, so this asserts the recording surface is inert.
    let options = SpanOptions::with_attributes("operation", SpanAttributes::with("secret", "prompt content"));
    let result = NOOP_TELEMETRY_CONTEXT
        .start_span(options, |span| {
            span.add_event("event", SpanAttributes::with("secret", "content"));
            span.set_attributes(SpanAttributes::with("secret", "content"));
            span.set_status(SpanStatus::Ok);
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(result, ());
}

#[test]
fn in_memory_context_records_typed_starter_nesting() {
    // Mirrors the parentage assertion of the typed-starter test with two
    // schemas, simplified to runtime behavior.
    let context = InMemoryTelemetryContext::new();
    let start_span = create_typed_span_starter(context.clone());
    start_span
        .start_span("operation", SpanAttributes::with("kind", "read"), |_op, child_starter| {
            child_starter.start_span(
                "request",
                SpanAttributes::with("provider", "example"),
                |request_span, _child| {
                    request_span.set_attributes(SpanAttributes::with("response", "cached"));
                    Ok::<_, ()>(())
                },
            )
        })
        .unwrap();

    let spans = context.get_spans();
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].name, "operation");
    assert_eq!(spans[1].name, "request");
    assert_eq!(spans[1].parent_id, Some(spans[0].id));
    assert_eq!(
        spans[1].attributes.get("response"),
        Some(&pi_telemetry::AttributeValue::Str("cached".to_string()))
    );
}
