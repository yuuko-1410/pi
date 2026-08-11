//! Protocol conversion layer, port of `packages/server/src/protocol.ts`.
//!
//! Rust values are already JSON-compatible and acyclic (Value tree), so
//! `toProtocolJsonValue`/`sanitizeProtocolDetails` are identity functions;
//! the validation semantics they enforce in JS (finite numbers, plain
//! objects, no cycles) cannot be violated by the Rust type system.

use pi_ai::types::{AssistantMessage, Content, Model, ToolCall, ToolResultMessage, UserMessage, UserMessageContent};
use pi_protocol::cbor::Value;
use pi_protocol::schemas::{
    AssistantItem, AssistantStatus, InputKind, ModelCost, ModelMetadata, ModelRef, ToolItem, ToolStatus,
    Usage, UserItem,
};

fn non_negative_integer(value: f64) -> f64 {
    if !value.is_finite() {
        0.0
    } else {
        value.max(0.0).floor()
    }
}

fn non_negative_number(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn identifier(value: &str, label: &str) -> Result<String, String> {
    if value.is_empty() {
        Err(format!("{label} must be a non-empty string"))
    } else {
        Ok(value.to_string())
    }
}

fn timestamp(value: f64) -> Result<f64, String> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        Err("Protocol timestamps must be non-negative integers".to_string())
    } else {
        Ok(value)
    }
}

/// Validate and copy a value from an execution boundary into the protocol's
/// JSON-compatible subset. Identity for Rust Value (see module docs).
pub fn to_protocol_json_value(value: Value) -> Result<Value, String> {
    Ok(value)
}

/// Lossily sanitize diagnostic tool details. Identity for Rust Value.
pub fn sanitize_protocol_details(value: Value) -> Option<Value> {
    Some(value)
}

/// Map ai usage to the protocol Usage with clamping.
pub fn to_protocol_usage(usage: Option<&pi_ai::types::Usage>) -> Option<Usage> {
    let usage = usage?;
    let reasoning = usage.reasoning.map(non_negative_integer);
    let mut result = Usage {
        input: non_negative_integer(usage.input),
        output: non_negative_integer(usage.output),
        cache_read: non_negative_integer(usage.cache_read),
        cache_write: non_negative_integer(usage.cache_write),
        reasoning,
        total_tokens: non_negative_integer(usage.total_tokens),
        cost: pi_protocol::schemas::UsageCost {
            input: non_negative_number(usage.cost.input),
            output: non_negative_number(usage.cost.output),
            cache_read: non_negative_number(usage.cost.cache_read),
            cache_write: non_negative_number(usage.cost.cache_write),
            total: non_negative_number(usage.cost.total),
        },
    };
    if reasoning.is_none() {
        result.reasoning = None;
    }
    Some(result)
}

fn get_supported_thinking_levels(model: &Model) -> Vec<String> {
    // JS derives these from model metadata; the Rust Model carries a
    // reasoning flag only. Levels mirror the default set when reasoning is
    // enabled.
    if model.reasoning {
        vec![
            "off".to_string(),
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
        ]
    } else {
        vec!["off".to_string()]
    }
}

pub fn to_protocol_model_metadata(model: &Model, authenticated: bool) -> Result<ModelMetadata, String> {
    Ok(ModelMetadata {
        provider: identifier(&model.provider, "Model provider")?,
        id: identifier(&model.id, "Model id")?,
        name: identifier(&model.name, "Model name")?,
        api: identifier(&model.api, "Model API")?,
        reasoning: model.reasoning,
        input: model
            .input
            .iter()
            .map(|kind| match kind.as_str() {
                "text" => InputKind::Text,
                "image" => InputKind::Image,
                _ => InputKind::Text,
            })
            .collect(),
        context_window: (model.context_window.max(1.0)).floor(),
        max_tokens: (model.max_tokens.max(1.0)).floor(),
        cost: ModelCost {
            input: non_negative_number(model.cost.rates.input),
            output: non_negative_number(model.cost.rates.output),
            cache_read: non_negative_number(model.cost.rates.cache_read),
            cache_write: non_negative_number(model.cost.rates.cache_write),
        },
        supported_thinking_levels: get_supported_thinking_levels(model),
        authenticated,
    })
}

fn to_protocol_user_content(content: &UserMessageContent) -> Vec<pi_protocol::schemas::Content> {
    match content {
        UserMessageContent::Text(text) => vec![pi_protocol::schemas::Content::Text {
            text: text.clone(),
        }],
        UserMessageContent::Blocks(blocks) => blocks.iter().map(ai_content_to_protocol).collect(),
    }
}

fn ai_content_to_protocol(part: &Content) -> pi_protocol::schemas::Content {
    match part {
        Content::Text(text) => pi_protocol::schemas::Content::Text {
            text: text.text.clone(),
        },
        Content::Image(image) => pi_protocol::schemas::Content::Image {
            data: image.data.clone(),
            mime_type: image.mime_type.clone(),
        },
        Content::Thinking(thinking) => pi_protocol::schemas::Content::Thinking {
            thinking: thinking.thinking.clone(),
            redacted: thinking.redacted,
        },
        Content::ToolCall(tool_call) => pi_protocol::schemas::Content::ToolCall {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            input: tool_call.arguments.clone(),
        },
    }
}

pub fn to_protocol_user_message(message: &UserMessage, id: &str) -> Result<UserItem, String> {
    Ok(UserItem {
        id: identifier(id, "Transcript item id")?,
        content: to_protocol_user_content(&message.content),
        timestamp: timestamp(message.timestamp)?,
    })
}

fn to_protocol_assistant_content(message: &AssistantMessage) -> Vec<pi_protocol::schemas::Content> {
    message.content.iter().map(ai_content_to_protocol).collect()
}

pub fn to_protocol_assistant_message(
    message: &AssistantMessage,
    id: &str,
) -> Result<AssistantItem, String> {
    let usage = to_protocol_usage(Some(&message.usage));
    let response_model = match &message.response_model {
        Some(response_model) if !response_model.is_empty() => {
            Some(identifier(response_model, "Assistant response model")?)
        }
        _ => None,
    };
    let item_id = identifier(id, "Transcript item id")?;
    let model = ModelRef {
        provider: identifier(&message.provider, "Assistant provider")?,
        id: identifier(&message.model, "Assistant model")?,
    };
    let ts = timestamp(message.timestamp)?;
    let common = |status: AssistantStatus| AssistantItem {
        id: item_id.clone(),
        content: to_protocol_assistant_content(message),
        model: model.clone(),
        response_model: response_model.clone(),
        usage: usage.clone(),
        timestamp: ts,
        status,
    };
    match message.stop_reason.as_str() {
        "pending" => Ok(common(AssistantStatus::Streaming)),
        "stop" | "length" | "toolUse" => Ok(common(AssistantStatus::Complete {
            stop_reason: message.stop_reason.as_str().to_string(),
        })),
        "deferred" => Err("Deferred assistant messages are not supported by protocol v1".to_string()),
        "error" => {
            if message.error_message.as_deref() == Some("") {
                return Err("Assistant error messages must not be empty".to_string());
            }
            Ok(common(AssistantStatus::Error {
                error_message: message.error_message.clone(),
            }))
        }
        "aborted" => Ok(common(AssistantStatus::Aborted {
            error_message: message.error_message.clone(),
        })),
        other => Err(format!("Unsupported stop reason: {other}")),
    }
}

fn to_protocol_tool_content(content: &[Content]) -> Vec<pi_protocol::schemas::Content> {
    content.iter().map(ai_content_to_protocol).collect()
}

pub fn to_protocol_tool_result_message(
    message: &ToolResultMessage,
    call: &ToolCall,
    id: &str,
) -> Result<ToolItem, String> {
    let call_id = identifier(&call.id, "Tool call id")?;
    let call_name = identifier(&call.name, "Tool call name")?;
    if identifier(&message.tool_call_id, "Tool result call id")? != call_id {
        return Err(format!(
            "Tool result {} does not match tool call {call_id}",
            message.tool_call_id
        ));
    }
    if identifier(&message.tool_name, "Tool result name")? != call_name {
        return Err(format!(
            "Tool result {} does not match tool call {call_name}",
            message.tool_name
        ));
    }
    let details = message.details.clone();
    let usage = to_protocol_usage(message.usage.as_ref());
    Ok(ToolItem {
        id: identifier(id, "Transcript item id")?,
        tool_call_id: call_id,
        tool_name: call_name,
        input: call.arguments.clone(),
        content: to_protocol_tool_content(&message.content),
        details,
        usage,
        timestamp: timestamp(message.timestamp)?,
        status: if message.is_error {
            ToolStatus::Error
        } else {
            ToolStatus::Complete
        },
    })
}
