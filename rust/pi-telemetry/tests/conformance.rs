//! Port of the `InMemoryTelemetryContext` conformance cases from
//! `packages/telemetry/test/conformance.test.ts`.
//!
//! Skipped cases (JS-only, unrepresentable or meaningless in Rust):
//! - "ignores failed attribute calls atomically" / "ignores failed status
//!   calls atomically" / "suppresses unreadable telemetry payload failures":
//!   Proxy-based unreadable payloads; Rust values cannot fail to read.
//! - async rejection variants: Rust callbacks are synchronous; `Err` covers
//!   both sync throws and rejected promises.
//! - "records nested and concurrent child relationships": concurrency is a
//!   Promise artifact; the sequential equivalent records the same order and
//!   end sequences (second child settles before first child settles before
//!   the parent).

use pi_telemetry::{InMemoryTelemetryContext, SpanAttributes, SpanOptions, SpanStatus, TelemetryContext};

fn find_span(spans: &[pi_telemetry::RecordedTelemetrySpan], name: &str) -> pi_telemetry::RecordedTelemetrySpan {
    spans
        .iter()
        .find(|candidate| candidate.name == name)
        .unwrap_or_else(|| panic!("Expected recorded span {name}"))
        .clone()
}

#[test]
fn admits_once_synchronously_and_preserves_the_result() {
    let context = InMemoryTelemetryContext::new();
    let mut admitted = false;
    let mut calls = 0;
    let expected = 42;
    let result = context
        .start_span(SpanOptions::new("success"), |_span| {
            admitted = true;
            calls += 1;
            Ok::<_, ()>(expected)
        })
        .unwrap();

    assert!(admitted);
    assert_eq!(calls, 1);
    assert_eq!(result, expected);
    let spans = context.get_spans();
    assert_eq!(find_span(&spans, "success").status, SpanStatus::Ok);
    assert!(find_span(&spans, "success").settled);
}

#[test]
fn preserves_rejection_values_and_marks_spans_as_error() {
    let context = InMemoryTelemetryContext::new();

    let sync_error: Box<dyn std::error::Error> = "sync".into();
    let sync: Result<i64, Box<dyn std::error::Error>> = context.start_span(
        SpanOptions::new("sync-error"),
        |_span| Err(sync_error.to_string().into()),
    );
    assert!(sync.is_err());

    let undefined_error: Option<()> = None;
    let undefined: Result<i64, Option<()>> =
        context.start_span(SpanOptions::new("undefined-error"), |_span| Err(undefined_error));
    assert_eq!(undefined, Err(None));

    let spans = context.get_spans();
    for name in ["sync-error", "undefined-error"] {
        assert_eq!(find_span(&spans, name).status, SpanStatus::Error { error: None });
    }
}

#[test]
fn uses_last_explicit_status_without_automatic_overwrite() {
    let context = InMemoryTelemetryContext::new();

    context
        .start_span(SpanOptions::new("last-status"), |span| {
            span.set_status(SpanStatus::Error {
                error: Some(pi_telemetry::ErrorInfo {
                    name: "Expected".to_string(),
                    message: "first".to_string(),
                }),
            });
            span.set_status(SpanStatus::Ok);
            Ok::<_, ()>(())
        })
        .unwrap();

    let thrown: Box<dyn std::error::Error> = "after explicit status".into();
    let _ = context.start_span::<(), Box<dyn std::error::Error>, _>(
        SpanOptions::new("explicit-before-throw"),
        |span| {
            span.set_status(SpanStatus::Ok);
            Err(thrown.to_string().into())
        },
    );

    context
        .start_span(SpanOptions::new("expected-failure"), |span| {
            span.set_status(SpanStatus::Error {
                error: Some(pi_telemetry::ErrorInfo {
                    name: "Expected".to_string(),
                    message: "returned failure".to_string(),
                }),
            });
            Ok::<_, ()>(false)
        })
        .unwrap();

    let spans = context.get_spans();
    assert_eq!(find_span(&spans, "last-status").status, SpanStatus::Ok);
    assert_eq!(find_span(&spans, "explicit-before-throw").status, SpanStatus::Ok);
    assert_eq!(
        find_span(&spans, "expected-failure").status,
        SpanStatus::Error {
            error: Some(pi_telemetry::ErrorInfo {
                name: "Expected".to_string(),
                message: "returned failure".to_string(),
            }),
        }
    );
}

#[test]
fn merges_attributes_and_records_ordered_events() {
    let context = InMemoryTelemetryContext::new();
    let options = SpanOptions::with_attributes(
        "recording",
        vec![
            ("start".to_string(), "value".into()),
            ("overwrite".to_string(), "start".into()),
        ]
        .into(),
    );
    context
        .start_span(options, |span| {
            span.set_attributes(
                vec![
                    ("count".to_string(), 1.0.into()),
                    ("overwrite".to_string(), "middle".into()),
                ]
                .into(),
            );
            span.set_attributes(
                vec![
                    ("overwrite".to_string(), "end".into()),
                    ("kept".to_string(), "yes".into()),
                ]
                .into(),
            );
            span.add_event("first", SpanAttributes::with("index", 1.0));
            span.add_event("second", SpanAttributes::with("index", 2.0));
            Ok::<_, ()>(())
        })
        .unwrap();

    let span = find_span(&context.get_spans(), "recording");
    assert_eq!(
        span.attributes.0,
        vec![
            ("start".to_string(), "value".into()),
            ("overwrite".to_string(), "end".into()),
            ("count".to_string(), 1.0.into()),
            ("kept".to_string(), "yes".to_string().into()),
        ]
    );
    assert_eq!(
        span.events,
        vec![
            pi_telemetry::RecordedTelemetryEvent {
                name: "first".to_string(),
                attributes: SpanAttributes::with("index", 1.0),
            },
            pi_telemetry::RecordedTelemetryEvent {
                name: "second".to_string(),
                attributes: SpanAttributes::with("index", 2.0),
            },
        ]
    );
}

#[test]
fn makes_calls_after_settlement_inert() {
    let context = InMemoryTelemetryContext::new();
    let mut settled_span: Option<pi_telemetry::TelemetrySpanHandle> = None;
    context
        .start_span(
            SpanOptions::with_attributes("settled", SpanAttributes::with("value", "initial")),
            |span| {
                settled_span = Some(span.clone());
                Ok::<_, ()>(())
            },
        )
        .unwrap();
    let captured_span = settled_span.expect("callback span");

    captured_span.set_attributes(SpanAttributes::with("value", "late"));
    captured_span.add_event("late", SpanAttributes::with("value", true));
    captured_span.set_status(SpanStatus::Error { error: None });
    let mut child_admitted = false;
    let child_result: i64 = captured_span
        .start_span(SpanOptions::new("late-child"), |_span| {
            child_admitted = true;
            Ok::<_, ()>(7)
        })
        .unwrap();
    assert!(child_admitted);
    assert_eq!(child_result, 7);

    let spans = context.get_spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].attributes.0, vec![("value".to_string(), "initial".into())]);
    assert_eq!(spans[0].events, vec![]);
    assert_eq!(spans[0].status, SpanStatus::Ok);
}

#[test]
fn records_nested_child_relationships_with_end_sequences() {
    let context = InMemoryTelemetryContext::new();
    context
        .start_span(SpanOptions::new("parent"), |parent| {
            parent
                .start_span(SpanOptions::new("second-child"), |_span| Ok::<_, ()>("done"))
                .unwrap();
            parent
                .start_span(SpanOptions::new("first-child"), |_span| Ok::<_, ()>(()))
                .unwrap();
            Ok::<_, ()>(())
        })
        .unwrap();

    let spans = context.get_spans();
    let parent = find_span(&spans, "parent");
    let first = find_span(&spans, "first-child");
    let second = find_span(&spans, "second-child");
    assert_eq!(parent.parent_id, None);
    assert_eq!(first.parent_id, Some(parent.id));
    assert_eq!(second.parent_id, Some(parent.id));
    let (second_seq, first_seq, parent_seq) = (
        second.end_sequence.expect("second settled"),
        first.end_sequence.expect("first settled"),
        parent.end_sequence.expect("parent settled"),
    );
    assert!(second_seq < first_seq);
    assert!(first_seq < parent_seq);
}

#[test]
fn returns_detached_snapshots_without_exposing_mutable_recording_state() {
    let context = InMemoryTelemetryContext::new();
    let mut open_settled: Option<bool> = None;
    let mut open_end_sequence: Option<Option<u64>> = None;
    context
        .start_span(
            SpanOptions::with_attributes("snapshot", SpanAttributes::with("tags", vec!["initial".to_string()])),
            |span| {
                span.add_event("event", SpanAttributes::with("value", 1.0));
                let open = &context.get_spans()[0];
                open_settled = Some(open.settled);
                open_end_sequence = Some(open.end_sequence);
                Ok::<_, ()>(())
            },
        )
        .unwrap();

    assert_eq!(open_settled, Some(false));
    assert_eq!(open_end_sequence, Some(None));
    let first = &context.get_spans()[0];
    assert!(first.settled);
    assert_eq!(first.end_sequence, Some(1));

    // Mutate the detached snapshot; the internal state must not change.
    let mut spans = context.get_spans();
    spans[0].attributes.0[0].1 = pi_telemetry::AttributeValue::StrArray(vec!["mutated".to_string()]);
    spans[0].events[0].attributes.0[0].1 = pi_telemetry::AttributeValue::Number(2.0);

    let second = &context.get_spans()[0];
    assert_eq!(
        second.attributes.0,
        vec![("tags".to_string(), pi_telemetry::AttributeValue::StrArray(vec!["initial".to_string()]))]
    );
    assert_eq!(
        second.events,
        vec![pi_telemetry::RecordedTelemetryEvent {
            name: "event".to_string(),
            attributes: SpanAttributes::with("value", 1.0),
        }]
    );
}
