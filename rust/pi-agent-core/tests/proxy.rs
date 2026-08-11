//! Proxy stream tests: event parsing and partial reconstruction.

use pi_agent_core::proxy::{parse_proxy_event, process_proxy_event, ProxyAssistantMessageEvent};
use pi_ai::types::{
    AssistantMessage, AssistantMessageEvent, StopReason, Usage, UsageCost,
};

fn empty_partial() -> AssistantMessage {
    AssistantMessage {
        content: vec![],
        api: "test".to_string(),
        provider: "test".to_string(),
        model: "m".to_string(),
        response_model: None,
        response_id: None,
        usage: Usage {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0.0,
            cost: UsageCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
        },
        stop_reason: StopReason::Pending,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1.0,
    }
}

#[test]
fn parses_proxy_events_from_json() {
    let event = parse_proxy_event(r#"{"type":"start"}"#).unwrap();
    assert_eq!(event, ProxyAssistantMessageEvent::Start);

    let event = parse_proxy_event(r#"{"type":"text_delta","contentIndex":0,"delta":"hello"}"#).unwrap();
    assert_eq!(
        event,
        ProxyAssistantMessageEvent::TextDelta {
            content_index: 0.0,
            delta: "hello".to_string(),
        }
    );

    let event = parse_proxy_event(r#"{"type":"done","reason":"stop","usage":{"input":1,"output":2,"totalTokens":3,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}"#)
        .unwrap();
    assert!(matches!(event, ProxyAssistantMessageEvent::Done { .. }));
}

#[test]
fn reconstructs_text_content_from_deltas() {
    let mut partial = empty_partial();
    let start = process_proxy_event(
        &ProxyAssistantMessageEvent::TextStart { content_index: 0.0 },
        &mut partial,
    )
    .unwrap();
    assert!(matches!(start, AssistantMessageEvent::TextStart { .. }));

    process_proxy_event(
        &ProxyAssistantMessageEvent::TextDelta {
            content_index: 0.0,
            delta: "Hello".to_string(),
        },
        &mut partial,
    )
    .unwrap();
    process_proxy_event(
        &ProxyAssistantMessageEvent::TextDelta {
            content_index: 0.0,
            delta: " world".to_string(),
        },
        &mut partial,
    )
    .unwrap();

    let end = process_proxy_event(
        &ProxyAssistantMessageEvent::TextEnd {
            content_index: 0.0,
            content_signature: Some("sig".to_string()),
        },
        &mut partial,
    )
    .unwrap();
    assert!(matches!(end, AssistantMessageEvent::TextEnd { .. }));
    let pi_ai::types::Content::Text(text) = &partial.content[0] else {
        panic!("expected text");
    };
    assert_eq!(text.text, "Hello world");
    assert_eq!(text.text_signature.as_deref(), Some("sig"));
}

#[test]
fn done_event_finalizes_stop_reason_and_usage() {
    let mut partial = empty_partial();
    let event = process_proxy_event(
        &ProxyAssistantMessageEvent::Done {
            reason: "toolUse".to_string(),
            usage: Usage {
                input: 10.0,
                output: 5.0,
                cache_read: 2.0,
                cache_write: 0.0,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: 17.0,
                cost: UsageCost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    total: 0.0,
                },
            },
        },
        &mut partial,
    )
    .unwrap();
    assert!(matches!(event, AssistantMessageEvent::Done { .. }));
    assert_eq!(partial.stop_reason, StopReason::ToolUse);
    assert_eq!(partial.usage.input, 10.0);
    assert_eq!(partial.usage.total_tokens, 17.0);
}
