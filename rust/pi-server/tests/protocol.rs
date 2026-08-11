//! Protocol conversion layer tests.

use pi_ai::types::{AssistantMessage, Content, StopReason, ToolCall, ToolResultMessage, UserMessage, UserMessageContent};
use pi_protocol::schemas::{AssistantStatus, ToolStatus};
use pi_server::protocol::*;

fn usage() -> pi_ai::types::Usage {
    pi_ai::types::Usage {
        input: 10.0,
        output: 5.0,
        cache_read: 0.0,
        cache_write: 0.0,
        cache_write_1h: None,
        reasoning: Some(2.0),
        total_tokens: 17.0,
        cost: pi_ai::types::UsageCost {
            input: 0.1,
            output: 0.2,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.3,
        },
    }
}

fn user_message() -> UserMessage {
    UserMessage {
        content: UserMessageContent::Text("hello".to_string()),
        timestamp: 1000.0,
    }
}

#[test]
fn user_message_conversion() {
    let item = to_protocol_user_message(&user_message(), "u1").unwrap();
    assert_eq!(item.id, "u1");
    assert_eq!(item.content.len(), 1);
    assert_eq!(item.timestamp, 1000.0);
}

#[test]
fn usage_clamps_non_finite() {
    let mut u = usage();
    u.input = f64::NAN;
    u.cost.total = -5.0;
    let mapped = to_protocol_usage(Some(&u)).unwrap();
    assert_eq!(mapped.input, 0.0);
    assert_eq!(mapped.cost.total, 0.0);
    assert_eq!(mapped.reasoning, Some(2.0));
    assert_eq!(to_protocol_usage(None), None);
}

#[test]
fn assistant_message_statuses() {
    let mut message = AssistantMessage {
        content: vec![Content::Text(pi_ai::types::TextContent {
            text: "hi".to_string(),
            text_signature: None,
        })],
        api: "api".to_string(),
        provider: "p".to_string(),
        model: "m".to_string(),
        response_model: None,
        response_id: None,
        usage: usage(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 2000.0,
    };
    let item = to_protocol_assistant_message(&message, "a1").unwrap();
    assert!(matches!(item.status, AssistantStatus::Complete { ref stop_reason } if stop_reason == "stop"));
    assert_eq!(item.model.provider, "p");
    assert_eq!(item.usage.as_ref().unwrap().input, 10.0);

    message.stop_reason = StopReason::Pending;
    let item = to_protocol_assistant_message(&message, "a2").unwrap();
    assert!(matches!(item.status, AssistantStatus::Streaming));

    message.stop_reason = StopReason::Deferred;
    assert!(to_protocol_assistant_message(&message, "a3").is_err());

    message.stop_reason = StopReason::Error;
    message.error_message = Some("".to_string());
    assert!(to_protocol_assistant_message(&message, "a4").is_err());
    message.error_message = Some("boom".to_string());
    let item = to_protocol_assistant_message(&message, "a5").unwrap();
    assert!(matches!(item.status, AssistantStatus::Error { .. }));
}

#[test]
fn tool_result_matching() {
    let call = ToolCall {
        id: "t1".to_string(),
        name: "read".to_string(),
        arguments: pi_protocol::cbor::Value::Map(vec![(
            "path".to_string(),
            pi_protocol::cbor::Value::String("a.ts".to_string()),
        )]),
        thought_signature: None,
        namespace: None,
    };
    let message = ToolResultMessage {
        tool_call_id: "t1".to_string(),
        tool_name: "read".to_string(),
        content: vec![Content::Text(pi_ai::types::TextContent {
            text: "content".to_string(),
            text_signature: None,
        })],
        is_error: false,
        timestamp: 3000.0,
        usage: None,
        details: None,
        added_tool_names: None,
    };
    let item = to_protocol_tool_result_message(&message, &call, "r1").unwrap();
    assert_eq!(item.tool_call_id, "t1");
    assert!(matches!(item.status, ToolStatus::Complete));

    let mut wrong = message.clone();
    wrong.tool_call_id = "t2".to_string();
    assert!(to_protocol_tool_result_message(&wrong, &call, "r2").is_err());
    let mut wrong = message.clone();
    wrong.tool_name = "write".to_string();
    assert!(to_protocol_tool_result_message(&wrong, &call, "r3").is_err());

    let mut error_result = message.clone();
    error_result.is_error = true;
    let item = to_protocol_tool_result_message(&error_result, &call, "r4").unwrap();
    assert!(matches!(item.status, ToolStatus::Error));
}

#[test]
fn model_metadata() {
    let model = pi_ai::types::Model {
        id: "m1".to_string(),
        name: "Model One".to_string(),
        api: "openai-responses".to_string(),
        provider: "openai".to_string(),
        base_url: "https://api.openai.com/v1".to_string(),
        reasoning: true,
        thinking_level_map: None,
        input: vec!["text".to_string(), "image".to_string()],
        cost: pi_ai::types::ModelCost {
            rates: pi_ai::types::ModelCostRates {
                input: 1.0,
                output: 2.0,
                cache_read: 0.5,
                cache_write: 1.5,
            },
            tiers: None,
        },
        context_window: 128000.0,
        max_tokens: 4096.0,
        sampling_params: None,
        headers: None,
        compat: None,
    };
    let metadata = to_protocol_model_metadata(&model, true).unwrap();
    assert_eq!(metadata.id, "m1");
    assert_eq!(metadata.context_window, 128000.0);
    assert_eq!(metadata.cost.input, 1.0);
    assert!(metadata.authenticated);
    assert!(metadata.supported_thinking_levels.contains(&"high".to_string()));
}

#[test]
fn identifiers_are_validated() {
    assert!(to_protocol_user_message(&user_message(), "").is_err());
    let mut message = user_message();
    message.timestamp = -1.0;
    assert!(to_protocol_user_message(&message, "u1").is_err());
    let mut message = user_message();
    message.timestamp = 1.5;
    assert!(to_protocol_user_message(&message, "u1").is_err());
}
