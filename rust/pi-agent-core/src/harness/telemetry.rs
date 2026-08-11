//! Agent-owned telemetry schemas, port of `packages/agent/src/harness/telemetry.ts`.
//!
//! The JS module defines two schema constants and typed span starter
//! wrappers; Rust replaces the type vocabulary with plain data structures
//! and the wrappers with synchronous helpers (pi-telemetry is synchronous).

use pi_telemetry::{
    AttributeKind, Cardinality, TelemetryAttributeDefinition, TelemetryParentDefinition,
    TelemetrySchemaDefinition, TelemetrySpanDefinition, TelemetryStatusDefinition,
};
use pi_telemetry::{InMemoryTelemetryContext, SpanAttributes, SpanOptions, TelemetryContext, TelemetrySpanHandle};

fn attr(
    description: &str,
    kind: AttributeKind,
    required: bool,
    cardinality: Option<Cardinality>,
) -> TelemetryAttributeDefinition {
    TelemetryAttributeDefinition {
        description: description.to_string(),
        sensitive: None,
        cardinality,
        required,
        kind,
    }
}

fn str_attr(description: &str, required: bool) -> TelemetryAttributeDefinition {
    attr(description, AttributeKind::Str { values: None, examples: None }, required, None)
}

fn str_values_attr(
    description: &str,
    required: bool,
    values: &[&str],
) -> TelemetryAttributeDefinition {
    attr(
        description,
        AttributeKind::Str {
            values: Some(values.iter().map(|v| v.to_string()).collect()),
            examples: None,
        },
        required,
        None,
    )
}

fn high_attr(description: &str, required: bool) -> TelemetryAttributeDefinition {
    attr(
        description,
        AttributeKind::Str { values: None, examples: None },
        required,
        Some(Cardinality::High),
    )
}

fn low_attr(description: &str, required: bool) -> TelemetryAttributeDefinition {
    attr(
        description,
        AttributeKind::Str { values: None, examples: None },
        required,
        Some(Cardinality::Low),
    )
}

fn num_attr(description: &str, required: bool) -> TelemetryAttributeDefinition {
    attr(description, AttributeKind::Number { values: None, examples: None }, required, None)
}

fn bool_attr(description: &str, required: bool) -> TelemetryAttributeDefinition {
    attr(description, AttributeKind::Bool { values: None, examples: None }, required, None)
}

/// `pi.ai.request` span schema: one logical request to an AI provider.
pub fn ai_telemetry_schema() -> TelemetrySchemaDefinition {
    TelemetrySchemaDefinition {
        version: 1,
        spans: vec![(
            "pi.ai.request".to_string(),
            TelemetrySpanDefinition {
                description: "One logical request to an AI provider".to_string(),
                parents: TelemetryParentDefinition::Any,
                start_attributes: vec![
                    (
                        "pi.ai.operation".to_string(),
                        str_values_attr(
                            "Logical provider operation",
                            true,
                            &["stream", "fetch_deferred", "cancel_deferred", "generate_images"],
                        ),
                    ),
                    ("pi.ai.provider".to_string(), str_attr("Selected provider id", true)),
                    ("pi.ai.model".to_string(), str_attr("Requested model id", true)),
                    ("pi.ai.api".to_string(), str_attr("Provider API id", true)),
                    ("pi.ai.streaming".to_string(), bool_attr("Whether this operation returns a stream", true)),
                    (
                        "pi.ai.deferred".to_string(),
                        bool_attr("Whether the operation requests or participates in deferred execution", false),
                    ),
                ],
                end_attributes: vec![
                    ("pi.ai.response.model".to_string(), str_attr("Concrete response model", false)),
                    (
                        "pi.ai.response.id".to_string(),
                        attr(
                            "Provider response id",
                            AttributeKind::Str { values: None, examples: None },
                            false,
                            Some(Cardinality::High),
                        ),
                    ),
                    (
                        "pi.ai.response.stop_reason".to_string(),
                        str_values_attr(
                            "Normalized terminal response reason",
                            false,
                            &["stop", "length", "tool_use", "error", "aborted", "deferred"],
                        ),
                    ),
                    ("pi.ai.http.status_code".to_string(), num_attr("Final HTTP status", false)),
                    ("pi.ai.usage.input_tokens".to_string(), num_attr("Reported input tokens", false)),
                    ("pi.ai.usage.output_tokens".to_string(), num_attr("Reported output tokens", false)),
                    ("pi.ai.usage.cache_read_tokens".to_string(), num_attr("Reported cache-read tokens", false)),
                    ("pi.ai.usage.cache_write_tokens".to_string(), num_attr("Reported cache-write tokens", false)),
                    ("pi.ai.usage.reasoning_tokens".to_string(), num_attr("Reported reasoning tokens", false)),
                    ("pi.ai.usage.total_tokens".to_string(), num_attr("Reported total tokens", false)),
                    ("pi.ai.usage.cost".to_string(), num_attr("Reported total cost", false)),
                    ("pi.ai.stream.chunk_count".to_string(), num_attr("Streamed update chunk count", false)),
                    (
                        "pi.ai.stream.time_to_first_chunk_ms".to_string(),
                        num_attr("Elapsed milliseconds to first update chunk", false),
                    ),
                    (
                        "pi.ai.error.type".to_string(),
                        attr(
                            "Provider or transport error class",
                            AttributeKind::Str { values: None, examples: None },
                            false,
                            Some(Cardinality::Low),
                        ),
                    ),
                ],
                events: None,
                status: TelemetryStatusDefinition {
                    default: "ok".to_string(),
                    error_when: "The operation throws or returns an error result".to_string(),
                },
            },
        )],
    }
}

const HOOK_NAMES: &[&str] = &[
    "before_run",
    "before_resume",
    "before_run_end",
    "transform_context",
    "before_request",
    "before_payload",
    "after_response",
    "before_tool",
    "after_tool",
    "before_compaction",
    "before_navigation",
];

const EVENT_TYPES: &[&str] = &[
    "run_start",
    "run_resume",
    "run_suspend",
    "run_abort",
    "run_end",
    "fault",
    "handler_error",
    "turn_start",
    "turn_end",
    "retry_scheduled",
    "retry_start",
    "retry_end",
    "message_start",
    "message_update",
    "message_end",
    "tool_start",
    "tool_update",
    "tool_end",
    "entry_added",
    "write_pending",
    "queue_update",
    "fact_update",
    "config_update",
    "compaction_start",
    "compaction_end",
    "navigation_start",
    "navigation_end",
    "lane_created",
    "usage",
];

fn operation_start_attributes() -> Vec<(String, TelemetryAttributeDefinition)> {
    vec![
        ("pi.session.id".to_string(), high_attr("Session id", true)),
        ("pi.lane.name".to_string(), high_attr("Lane name", true)),
        ("pi.operation.id".to_string(), high_attr("Durable operation id", true)),
        (
            "pi.operation.recovery".to_string(),
            bool_attr("Whether this invocation resumes durable work", true),
        ),
    ]
}

fn operation_error_attributes() -> Vec<(String, TelemetryAttributeDefinition)> {
    vec![
        ("pi.error.code".to_string(), low_attr("Stable operation error code", false)),
        ("pi.error.type".to_string(), low_attr("Low-cardinality operation error class", false)),
    ]
}

/// Harness telemetry schema: run/compaction/navigation/checkpoint/turn/step/
/// tool/hook/sleep/event_handler/session.write spans.
pub fn harness_telemetry_schema() -> TelemetrySchemaDefinition {
    let mut start = operation_start_attributes();
    start.push((
        "pi.operation.kind".to_string(),
        str_values_attr("Run operation kind", true, &["run"]),
    ));
    let mut end = vec![(
        "pi.operation.outcome".to_string(),
        str_values_attr("Run invocation outcome", false, &["completed", "aborted", "failed", "suspended"]),
    )];
    end.extend(operation_error_attributes());

    let mut compaction_start = operation_start_attributes();
    compaction_start.push((
        "pi.operation.kind".to_string(),
        str_values_attr("Compaction operation kind", true, &["compaction"]),
    ));
    let mut compaction_end = vec![(
        "pi.operation.outcome".to_string(),
        str_values_attr("Compaction invocation outcome", false, &["completed", "declined", "aborted", "failed"]),
    )];
    compaction_end.extend(operation_error_attributes());

    let mut navigation_start = operation_start_attributes();
    navigation_start.push((
        "pi.operation.kind".to_string(),
        str_values_attr("Navigation operation kind", true, &["navigation"]),
    ));
    let mut navigation_end = vec![(
        "pi.operation.outcome".to_string(),
        str_values_attr("Navigation invocation outcome", false, &["completed", "declined", "aborted", "failed"]),
    )];
    navigation_end.extend(operation_error_attributes());

    let mut step_start = vec![
        ("pi.lane.name".to_string(), high_attr("Lane name", true)),
        ("pi.operation.id".to_string(), high_attr("Durable operation id", true)),
        (
            "pi.step.kind".to_string(),
            str_values_attr("Retryable step kind", true, &["assistant", "compaction", "branch_summary"]),
        ),
        ("pi.step.attempt".to_string(), num_attr("One-based durable attempt number", true)),
        (
            "pi.compaction.reason".to_string(),
            str_values_attr("Compaction trigger", false, &["manual", "threshold", "overflow"]),
        ),
    ];

    let mut tool_start = vec![
        ("pi.lane.name".to_string(), high_attr("Lane name", true)),
        ("pi.operation.id".to_string(), high_attr("Durable operation id", true)),
        ("pi.turn.id".to_string(), high_attr("Invocation-local live turn id", false)),
        ("pi.tool.name".to_string(), str_attr("Tool name", true)),
        ("pi.tool.call_id".to_string(), high_attr("Tool call id", true)),
        ("pi.tool.replay".to_string(), str_values_attr("Declared replay policy", true, &["never", "safe"])),
        ("pi.tool.recovery".to_string(), bool_attr("Whether this is recovery execution", true)),
    ];

    let mut hook_start = vec![
        ("pi.lane.name".to_string(), high_attr("Lane name", true)),
        ("pi.operation.id".to_string(), high_attr("Durable operation id when accepted", false)),
        (
            "pi.hook.name".to_string(),
            str_values_attr("Hook name", true, HOOK_NAMES),
        ),
        ("pi.hook.registration_id".to_string(), str_attr("Stable hook registration id", false)),
    ];

    let mut event_handler_start = vec![(
        "pi.event.type".to_string(),
        str_values_attr("Delivered harness event type", true, EVENT_TYPES),
    )];
    event_handler_start.push(("pi.lane.name".to_string(), high_attr("Lane name for lane-scoped events", false)));

    let mut session_write_start = vec![
        ("pi.lane.name".to_string(), high_attr("Lane name", true)),
        ("pi.operation.id".to_string(), high_attr("Durable operation id when accepted", false)),
        (
            "pi.session.mutation".to_string(),
            str_values_attr("Session mutation kind", true, &["entry", "record", "lane", "fact"]),
        ),
        ("pi.session.item_type".to_string(), str_attr("Entry, record, lane, or fact subtype", false)),
    ];

    TelemetrySchemaDefinition {
        version: 1,
        spans: vec![
            (
                "pi.harness.run".to_string(),
                TelemetrySpanDefinition {
                    description: "One admitted in-process run invocation".to_string(),
                    parents: TelemetryParentDefinition::RootOrExternal,
                    start_attributes: start,
                    end_attributes: end,
                    events: None,
                    status: TelemetryStatusDefinition {
                        default: "ok".to_string(),
                        error_when: "The run fails or throws".to_string(),
                    },
                },
            ),
            (
                "pi.harness.compaction".to_string(),
                TelemetrySpanDefinition {
                    description: "One admitted in-process manual compaction invocation".to_string(),
                    parents: TelemetryParentDefinition::RootOrExternal,
                    start_attributes: compaction_start,
                    end_attributes: compaction_end,
                    events: None,
                    status: TelemetryStatusDefinition {
                        default: "ok".to_string(),
                        error_when: "The compaction fails or throws".to_string(),
                    },
                },
            ),
            (
                "pi.harness.navigation".to_string(),
                TelemetrySpanDefinition {
                    description: "One admitted in-process navigation invocation".to_string(),
                    parents: TelemetryParentDefinition::RootOrExternal,
                    start_attributes: navigation_start,
                    end_attributes: navigation_end,
                    events: None,
                    status: TelemetryStatusDefinition {
                        default: "ok".to_string(),
                        error_when: "The navigation fails or throws".to_string(),
                    },
                },
            ),
            (
                "pi.harness.checkpoint".to_string(),
                TelemetrySpanDefinition {
                    description: "One run checkpoint".to_string(),
                    parents: TelemetryParentDefinition::Spans(vec!["pi.harness.run".to_string()]),
                    start_attributes: vec![
                        ("pi.lane.name".to_string(), high_attr("Lane name", true)),
                        ("pi.operation.id".to_string(), high_attr("Durable operation id", true)),
                        (
                            "pi.checkpoint.kind".to_string(),
                            str_values_attr("Checkpoint purpose", true, &["normal", "failure_drain", "abort_reconcile"]),
                        ),
                    ],
                    end_attributes: vec![],
                    events: None,
                    status: TelemetryStatusDefinition {
                        default: "ok".to_string(),
                        error_when: "Checkpoint work throws".to_string(),
                    },
                },
            ),
            (
                "pi.harness.turn".to_string(),
                TelemetrySpanDefinition {
                    description: "One assistant response and its tool batch".to_string(),
                    parents: TelemetryParentDefinition::Spans(vec!["pi.harness.run".to_string()]),
                    start_attributes: vec![
                        ("pi.lane.name".to_string(), high_attr("Lane name", true)),
                        ("pi.operation.id".to_string(), high_attr("Durable operation id", true)),
                        ("pi.turn.id".to_string(), high_attr("Invocation-local turn id", true)),
                    ],
                    end_attributes: vec![],
                    events: None,
                    status: TelemetryStatusDefinition {
                        default: "ok".to_string(),
                        error_when: "Turn work throws".to_string(),
                    },
                },
            ),
            (
                "pi.harness.step".to_string(),
                TelemetrySpanDefinition {
                    description: "One durable retry attempt".to_string(),
                    parents: TelemetryParentDefinition::Spans(vec![
                        "pi.harness.turn".to_string(),
                        "pi.harness.checkpoint".to_string(),
                        "pi.harness.compaction".to_string(),
                        "pi.harness.navigation".to_string(),
                    ]),
                    start_attributes: std::mem::take(&mut step_start),
                    end_attributes: vec![(
                        "pi.step.outcome".to_string(),
                        str_values_attr(
                            "Attempt outcome",
                            false,
                            &["succeeded", "retry", "failed", "aborted", "deferred", "overflow"],
                        ),
                    )],
                    events: None,
                    status: TelemetryStatusDefinition {
                        default: "ok".to_string(),
                        error_when: "The attempt retries, fails, or throws".to_string(),
                    },
                },
            ),
            (
                "pi.harness.tool".to_string(),
                TelemetrySpanDefinition {
                    description: "One raw phase-2 tool execution".to_string(),
                    parents: TelemetryParentDefinition::Spans(vec![
                        "pi.harness.turn".to_string(),
                        "pi.harness.run".to_string(),
                    ]),
                    start_attributes: std::mem::take(&mut tool_start),
                    end_attributes: vec![(
                        "pi.tool.is_error".to_string(),
                        bool_attr("Whether raw phase-2 execution returned an error", false),
                    )],
                    events: None,
                    status: TelemetryStatusDefinition {
                        default: "ok".to_string(),
                        error_when: "Raw phase-2 execution returns an error".to_string(),
                    },
                },
            ),
            (
                "pi.harness.hook".to_string(),
                TelemetrySpanDefinition {
                    description: "One registered hook handler invocation".to_string(),
                    parents: TelemetryParentDefinition::Any,
                    start_attributes: std::mem::take(&mut hook_start),
                    end_attributes: vec![(
                        "pi.hook.outcome".to_string(),
                        str_values_attr("Handler outcome", false, &["completed", "skipped", "blocked", "failed"]),
                    )],
                    events: None,
                    status: TelemetryStatusDefinition {
                        default: "ok".to_string(),
                        error_when: "The handler throws".to_string(),
                    },
                },
            ),
            (
                "pi.harness.sleep".to_string(),
                TelemetrySpanDefinition {
                    description: "One retry delay".to_string(),
                    parents: TelemetryParentDefinition::Spans(vec![
                        "pi.harness.step".to_string(),
                        "pi.harness.run".to_string(),
                    ]),
                    start_attributes: vec![
                        ("pi.operation.id".to_string(), high_attr("Durable operation id", true)),
                        ("pi.sleep.delay_ms".to_string(), num_attr("Requested delay in milliseconds", true)),
                    ],
                    end_attributes: vec![(
                        "pi.sleep.outcome".to_string(),
                        str_values_attr("Delay outcome", false, &["elapsed", "aborted"]),
                    )],
                    events: None,
                    status: TelemetryStatusDefinition {
                        default: "ok".to_string(),
                        error_when: "Sleep work throws".to_string(),
                    },
                },
            ),
            (
                "pi.harness.event_handler".to_string(),
                TelemetrySpanDefinition {
                    description: "One passive event listener invocation".to_string(),
                    parents: TelemetryParentDefinition::Any,
                    start_attributes: event_handler_start,
                    end_attributes: vec![],
                    events: None,
                    status: TelemetryStatusDefinition {
                        default: "ok".to_string(),
                        error_when: "The listener throws".to_string(),
                    },
                },
            ),
            (
                "pi.session.write".to_string(),
                TelemetrySpanDefinition {
                    description: "One committed session mutation".to_string(),
                    parents: TelemetryParentDefinition::Any,
                    start_attributes: std::mem::take(&mut session_write_start),
                    end_attributes: vec![(
                        "pi.session.seq".to_string(),
                        num_attr("Committed session sequence when exposed", false),
                    )],
                    events: None,
                    status: TelemetryStatusDefinition {
                        default: "ok".to_string(),
                        error_when: "Storage rejects the mutation".to_string(),
                    },
                },
            ),
        ],
    }
}

/// Combined typed span vocabulary for agent-owned AI-request and harness
/// telemetry.
pub fn agent_telemetry_schemas() -> Vec<TelemetrySchemaDefinition> {
    vec![ai_telemetry_schema(), harness_telemetry_schema()]
}

/// Synchronous analog of `startAiSpan`.
pub fn start_ai_span<T, E, F>(
    context: &InMemoryTelemetryContext,
    name: &str,
    attributes: SpanAttributes,
    callback: F,
) -> Result<T, E>
where
    F: FnOnce(&TelemetrySpanHandle) -> Result<T, E>,
{
    context.start_span(SpanOptions::with_attributes(name, attributes), callback)
}

/// Synchronous analog of `startHarnessSpan`.
pub fn start_harness_span<T, E, F>(
    context: &InMemoryTelemetryContext,
    name: &str,
    attributes: SpanAttributes,
    callback: F,
) -> Result<T, E>
where
    F: FnOnce(&TelemetrySpanHandle) -> Result<T, E>,
{
    context.start_span(SpanOptions::with_attributes(name, attributes), callback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_schema_has_request_span() {
        let schema = ai_telemetry_schema();
        assert_eq!(schema.version, 1);
        assert_eq!(schema.spans.len(), 1);
        let (name, span) = &schema.spans[0];
        assert_eq!(name, "pi.ai.request");
        let start_names: Vec<&str> = span.start_attributes.iter().map(|(k, _)| k.as_str()).collect();
        assert!(start_names.contains(&"pi.ai.operation"));
        assert!(start_names.contains(&"pi.ai.deferred"));
        // Required flags: operation is required, deferred is not.
        let operation = span
            .start_attributes
            .iter()
            .find(|(k, _)| k == "pi.ai.operation")
            .unwrap();
        assert!(operation.1.required);
        let deferred = span
            .start_attributes
            .iter()
            .find(|(k, _)| k == "pi.ai.deferred")
            .unwrap();
        assert!(!deferred.1.required);
    }

    #[test]
    fn harness_schema_has_all_spans() {
        let schema = harness_telemetry_schema();
        let names: Vec<&str> = schema.spans.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "pi.harness.run",
                "pi.harness.compaction",
                "pi.harness.navigation",
                "pi.harness.checkpoint",
                "pi.harness.turn",
                "pi.harness.step",
                "pi.harness.tool",
                "pi.harness.hook",
                "pi.harness.sleep",
                "pi.harness.event_handler",
                "pi.session.write",
            ]
        );
        // Step span keeps values and required flags.
        let step = &schema.spans.iter().find(|(name, _)| name == "pi.harness.step").unwrap().1;
        let kind = &step.start_attributes.iter().find(|(k, _)| k == "pi.step.kind").unwrap();
        assert!(kind.1.required);
        let AttributeKind::Str { values, .. } = &kind.1.kind else {
            panic!("expected string attribute");
        };
        assert!(values.as_ref().unwrap().contains(&"compaction".to_string()));
    }

    #[test]
    fn schemas_span_a_span() {
        use pi_telemetry::InMemoryTelemetryContext;
        let context = InMemoryTelemetryContext::new();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let captured_clone = captured.clone();
        let outcome = start_harness_span(
            &context,
            "pi.harness.run",
            SpanAttributes(vec![
                ("pi.session.id".to_string(), "s1".into()),
                ("pi.lane.name".to_string(), "main".into()),
                ("pi.operation.id".to_string(), "op1".into()),
                ("pi.operation.kind".to_string(), "run".into()),
                ("pi.operation.recovery".to_string(), false.into()),
            ]),
            |_span| {
                *captured_clone.lock().unwrap() = Some("ran".to_string());
                Ok::<_, ()>(42)
            },
        );
        assert_eq!(outcome, Ok(42));
        assert_eq!(captured.lock().unwrap().as_deref(), Some("ran"));
    }
}
