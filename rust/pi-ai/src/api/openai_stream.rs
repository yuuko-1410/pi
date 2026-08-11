//! OpenAI Responses stream events and processing.
//!
//! Port of the stream-processing half of
//! `packages/ai/src/api/openai-responses-shared.ts`: wire events parsed from
//! SSE JSON, and `processResponsesStream` (the output-slot state machine that
//! converts OpenAI stream events into assistant message stream events).

use pi_protocol::Value;

use crate::api::constrained_sampling::GrammarToolInputJsonBuffer;
use crate::api::openai_responses_shared::{
    encode_text_signature_v1,
};
use crate::event_stream::AssistantMessageEventStream;
use crate::types::{
    AssistantMessage, Content, Model, StopReason, TextContent, ThinkingContent, ToolCall, Usage, UsageCost,
};
use crate::utils::json::{json_stringify, parse_json_with_repair, parse_streaming_json};

// ---------------------------------------------------------------------------
// Wire event types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct ResponseUsage {
    pub input_tokens: f64,
    pub output_tokens: f64,
    pub total_tokens: f64,
    pub cached_tokens: f64,
    pub cache_write_tokens: f64,
    pub reasoning_tokens: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResponseReasoningItem {
    pub id: Option<String>,
    pub summary: Vec<String>,
    pub content: Vec<String>,
    pub encrypted_content: Option<String>,
    /// Full original JSON for signature storage.
    pub raw: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResponseMessageItem {
    pub id: Option<String>,
    pub phase: Option<String>,
    /// (text, refusal) pairs in order.
    pub content: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResponseFunctionCallItem {
    pub id: Option<String>,
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    pub namespace: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResponseCustomToolCallItem {
    pub id: Option<String>,
    pub call_id: String,
    pub name: String,
    pub input: String,
    pub namespace: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResponseOutputItem {
    Reasoning(ResponseReasoningItem),
    Message(ResponseMessageItem),
    FunctionCall(ResponseFunctionCallItem),
    CustomToolCall(ResponseCustomToolCallItem),
}

impl ResponseOutputItem {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Reasoning(_) => "reasoning",
            Self::Message(_) => "message",
            Self::FunctionCall(_) => "function_call",
            Self::CustomToolCall(_) => "custom_tool_call",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResponseObject {
    pub id: Option<String>,
    pub status: Option<String>,
    pub usage: Option<ResponseUsage>,
    pub output: Vec<ResponseOutputItem>,
    pub service_tier: Option<String>,
    pub error: Option<(String, String)>,
    pub incomplete_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResponseStreamEvent {
    ResponseCreated { response: ResponseObject },
    ResponseOutputItemAdded { output_index: f64, item: ResponseOutputItem },
    ResponseReasoningSummaryTextDelta { output_index: f64, delta: String },
    ResponseReasoningSummaryPartDone { output_index: f64 },
    ResponseReasoningTextDelta { output_index: f64, delta: String },
    ResponseOutputTextDelta { output_index: f64, delta: String },
    ResponseRefusalDelta { output_index: f64, delta: String },
    ResponseFunctionCallArgumentsDelta { output_index: f64, delta: String },
    ResponseFunctionCallArgumentsDone { output_index: f64, arguments: String },
    ResponseCustomToolCallInputDelta { output_index: f64, delta: String },
    ResponseCustomToolCallInputDone { output_index: f64, input: String },
    ResponseOutputItemDone { output_index: f64, item: ResponseOutputItem },
    ResponseCompleted { response: ResponseObject },
    ResponseIncomplete { response: ResponseObject },
    ResponseFailed { response: ResponseObject },
    StreamError { code: Option<String>, message: Option<String> },
}

fn get_str(entries: &[(String, Value)], key: &str) -> Option<String> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_str())
        .map(|s| s.to_string())
}

fn get_num(entries: &[(String, Value)], key: &str) -> Option<f64> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_number())
}

fn get_array<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a [Value]> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_array())
}

fn get_obj<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a [(String, Value)]> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_map())
}

fn parse_text_blocks(items: &[Value]) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for item in items {
        if let Some(entries) = item.as_map() {
            let text = get_str(entries, "text").unwrap_or_default();
            let refusal = get_str(entries, "refusal").unwrap_or_default();
            if !text.is_empty() || !refusal.is_empty() {
                result.push((text, refusal));
            }
        }
    }
    result
}

fn parse_reasoning_summary(items: &[Value]) -> Vec<String> {
    let mut result = Vec::new();
    for item in items {
        if let Some(entries) = item.as_map() {
            if let Some(text) = get_str(entries, "text") {
                result.push(text);
            }
        }
    }
    result
}

fn parse_output_item(value: &Value) -> Option<ResponseOutputItem> {
    let entries = value.as_map()?;
    let type_ = get_str(entries, "type")?;
    match type_.as_str() {
        "reasoning" => Some(ResponseOutputItem::Reasoning(ResponseReasoningItem {
            id: get_str(entries, "id"),
            summary: get_array(entries, "summary")
                .map(|items| parse_reasoning_summary(items))
                .unwrap_or_default(),
            content: get_array(entries, "content")
                .map(|items| parse_reasoning_summary(items))
                .unwrap_or_default(),
            encrypted_content: get_str(entries, "encrypted_content"),
            raw: value.clone(),
        })),
        "message" => Some(ResponseOutputItem::Message(ResponseMessageItem {
            id: get_str(entries, "id"),
            phase: get_str(entries, "phase"),
            content: get_array(entries, "content")
                .map(|items| parse_text_blocks(items))
                .unwrap_or_default(),
        })),
        "function_call" => Some(ResponseOutputItem::FunctionCall(ResponseFunctionCallItem {
            id: get_str(entries, "id"),
            call_id: get_str(entries, "call_id").unwrap_or_default(),
            name: get_str(entries, "name").unwrap_or_default(),
            arguments: get_str(entries, "arguments").unwrap_or_default(),
            namespace: get_str(entries, "namespace"),
        })),
        "custom_tool_call" => Some(ResponseOutputItem::CustomToolCall(
            ResponseCustomToolCallItem {
                id: get_str(entries, "id"),
                call_id: get_str(entries, "call_id").unwrap_or_default(),
                name: get_str(entries, "name").unwrap_or_default(),
                input: get_str(entries, "input").unwrap_or_default(),
                namespace: get_str(entries, "namespace"),
            },
        )),
        _ => None,
    }
}

fn parse_response(value: &Value) -> ResponseObject {
    let entries: Vec<(String, Value)> = value
        .as_map()
        .map(|entries| entries.to_vec())
        .unwrap_or_default();
    let usage = get_obj(&entries, "usage").map(|usage_entries| {
        let input_details = get_obj(usage_entries, "input_tokens_details");
        let output_details = get_obj(usage_entries, "output_tokens_details");
        ResponseUsage {
            input_tokens: get_num(usage_entries, "input_tokens").unwrap_or(0.0),
            output_tokens: get_num(usage_entries, "output_tokens").unwrap_or(0.0),
            total_tokens: get_num(usage_entries, "total_tokens").unwrap_or(0.0),
            cached_tokens: input_details
                .and_then(|d| get_num(d, "cached_tokens"))
                .unwrap_or(0.0),
            cache_write_tokens: input_details
                .and_then(|d| get_num(d, "cache_write_tokens"))
                .unwrap_or(0.0),
            reasoning_tokens: output_details
                .and_then(|d| get_num(d, "reasoning_tokens"))
                .unwrap_or(0.0),
        }
    });
    let error = get_obj(&entries, "error").map(|error_entries| {
        (
            get_str(error_entries, "code").unwrap_or_default(),
            get_str(error_entries, "message").unwrap_or_default(),
        )
    });
    let incomplete_reason = get_obj(&entries, "incomplete_details")
        .and_then(|details| get_str(details, "reason"))
        .filter(|reason| !reason.is_empty());
    ResponseObject {
        id: get_str(&entries, "id"),
        status: get_str(&entries, "status"),
        usage,
        output: get_array(&entries, "output")
            .map(|items| items.iter().filter_map(parse_output_item).collect())
            .unwrap_or_default(),
        service_tier: get_str(&entries, "service_tier"),
        error,
        incomplete_reason,
    }
}

/// Parses a single SSE `data` payload into a stream event.
pub fn parse_stream_event(data: &str) -> Option<ResponseStreamEvent> {
    let value: Value = parse_json_with_repair(data).ok()?;
    let entries = value.as_map()?;
    let type_ = get_str(entries, "type")?;
    let output_index = get_num(entries, "output_index").unwrap_or(0.0);
    let delta = get_str(entries, "delta").unwrap_or_default();
    let response = get_obj(entries, "response").map(|response| {
        let entries: Vec<(String, Value)> = response.to_vec();
        parse_response_from_entries(&entries)
    });
    let item = get_obj(entries, "item").map(|item| {
        let entries: Vec<(String, Value)> = item.to_vec();
        parse_output_item_from_entries(&entries)
    }).flatten();
    let arguments = get_str(entries, "arguments").unwrap_or_default();
    let input = get_str(entries, "input").unwrap_or_default();
    let code = get_str(entries, "code");
    let message = get_str(entries, "message");

    Some(match type_.as_str() {
        "response.created" => ResponseStreamEvent::ResponseCreated {
            response: response.expect("response.created carries a response"),
        },
        "response.output_item.added" => ResponseStreamEvent::ResponseOutputItemAdded {
            output_index,
            item: item.expect("output_item.added carries an item"),
        },
        "response.reasoning_summary_text.delta" => {
            ResponseStreamEvent::ResponseReasoningSummaryTextDelta { output_index, delta }
        }
        "response.reasoning_summary_part.done" => {
            ResponseStreamEvent::ResponseReasoningSummaryPartDone { output_index }
        }
        "response.reasoning_text.delta" => ResponseStreamEvent::ResponseReasoningTextDelta {
            output_index,
            delta,
        },
        "response.output_text.delta" => ResponseStreamEvent::ResponseOutputTextDelta { output_index, delta },
        "response.refusal.delta" => ResponseStreamEvent::ResponseRefusalDelta { output_index, delta },
        "response.function_call_arguments.delta" => {
            ResponseStreamEvent::ResponseFunctionCallArgumentsDelta { output_index, delta }
        }
        "response.function_call_arguments.done" => ResponseStreamEvent::ResponseFunctionCallArgumentsDone {
            output_index,
            arguments,
        },
        "response.custom_tool_call_input.delta" => {
            ResponseStreamEvent::ResponseCustomToolCallInputDelta { output_index, delta }
        }
        "response.custom_tool_call_input.done" => ResponseStreamEvent::ResponseCustomToolCallInputDone {
            output_index,
            input,
        },
        "response.output_item.done" => ResponseStreamEvent::ResponseOutputItemDone {
            output_index,
            item: item.expect("output_item.done carries an item"),
        },
        "response.completed" => ResponseStreamEvent::ResponseCompleted {
            response: response.expect("response.completed carries a response"),
        },
        "response.incomplete" => ResponseStreamEvent::ResponseIncomplete {
            response: response.expect("response.incomplete carries a response"),
        },
        "response.failed" => ResponseStreamEvent::ResponseFailed {
            response: response.expect("response.failed carries a response"),
        },
        "error" => ResponseStreamEvent::StreamError { code, message },
        _ => return None,
    })
}

fn parse_response_from_entries(entries: &[(String, Value)]) -> ResponseObject {
    parse_response(&Value::Map(entries.to_vec()))
}

fn parse_output_item_from_entries(entries: &[(String, Value)]) -> Option<ResponseOutputItem> {
    parse_output_item(&Value::Map(entries.to_vec()))
}

// ---------------------------------------------------------------------------
// processResponsesStream
// ---------------------------------------------------------------------------

struct ToolCallScratch {
    partial_json: Option<String>,
    custom_input: Option<CustomInputState>,
}

struct CustomInputState {
    property: String,
    json_buffer: GrammarToolInputJsonBuffer,
}

/// Output slots reference blocks by index into `output.content` (like the JS
/// implementation, which shares object references); tool-call scratch state
/// (streaming JSON/custom input) lives here.
enum ResponsesOutputSlot {
    Thinking { content_index: usize },
    Text { content_index: usize },
    ToolCall { content_index: usize, scratch: ToolCallScratch },
}

#[derive(Clone, Debug, Default)]
pub struct OpenAIResponsesStreamOptions {
    pub service_tier: Option<String>,
    pub grammar_tool_input_properties: Option<Vec<(String, String)>>,
}

fn map_stop_reason(status: Option<&str>, incomplete_reason: Option<&str>) -> (StopReason, Option<String>) {
    let Some(status) = status else {
        return (StopReason::Stop, None);
    };
    match status {
        "completed" => (StopReason::Stop, None),
        "incomplete" => {
            if incomplete_reason == Some("max_output_tokens") {
                return (StopReason::Length, None);
            }
            (
                StopReason::Error,
                Some(match incomplete_reason {
                    Some(reason) => format!("Response incomplete: {reason}"),
                    None => "Response incomplete without a provider reason".to_string(),
                }),
            )
        }
        "failed" | "cancelled" => (StopReason::Error, None),
        "in_progress" | "queued" => (StopReason::Stop, None),
        other => {
            // JS throws for unhandled statuses.
            // Treat unknown statuses as stop to keep the stream usable.
            let _ = other;
            (StopReason::Stop, None)
        }
    }
}

fn apply_message_phase_stop_reason(output: &mut AssistantMessage, item: &ResponseOutputItem) {
    if let ResponseOutputItem::Message(message) = item {
        if message.phase.as_deref() == Some("final_answer") {
            output.stop_reason = StopReason::Stop;
        }
    }
}

fn push_tool_call_delta(
    stream: &AssistantMessageEventStream,
    output: &AssistantMessage,
    slot: &ResponsesOutputSlot,
    delta: &str,
) {
    if let ResponsesOutputSlot::ToolCall { content_index, .. } = slot {
        stream.push(crate::types::AssistantMessageEvent::ToolCallDelta {
            content_index: *content_index as f64,
            delta: delta.to_string(),
            partial: output.clone(),
        });
    }
}

fn finalize_response(
    output: &mut AssistantMessage,
    saw_terminal_response_event: &mut bool,
    reasoning_blocks_by_id: &mut std::collections::HashMap<String, ThinkingContent>,
    response: &ResponseObject,
) {
    *saw_terminal_response_event = true;
    // Backfill reasoning signatures from the terminal response (Azure).
    for item in &response.output {
        if let ResponseOutputItem::Reasoning(reasoning) = item {
            if reasoning.encrypted_content.is_none() {
                continue;
            }
            if let Some(id) = &reasoning.id {
                if let Some(block) = reasoning_blocks_by_id.get(id) {
                    if let Some(signature) = &block.thinking_signature {
                        if let Ok(stored) = parse_json_with_repair::<Value>(signature) {
                            if let Value::Map(mut stored_entries) = stored {
                                if !stored_entries.iter().any(|(k, _)| k == "encrypted_content") {
                                    stored_entries.push((
                                        "encrypted_content".to_string(),
                                        Value::String(reasoning.encrypted_content.clone().unwrap_or_default()),
                                    ));
                                    if let Some(block) = reasoning_blocks_by_id.get_mut(id) {
                                        block.thinking_signature = Some(json_stringify(&Value::Map(stored_entries)));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(id) = &response.id {
        output.response_id = Some(id.clone());
    }
    if let Some(usage) = &response.usage {
        output.usage = Usage {
            // OpenAI includes cached and cache-write tokens in input_tokens,
            // so subtract both.
            input: (usage.input_tokens - usage.cached_tokens - usage.cache_write_tokens).max(0.0),
            output: usage.output_tokens,
            cache_read: usage.cached_tokens,
            cache_write: usage.cache_write_tokens,
            cache_write_1h: None,
            reasoning: Some(usage.reasoning_tokens),
            total_tokens: usage.total_tokens,
            cost: UsageCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
        };
    }
    // calculateCost(model, usage) is ported with the models layer; costs
    // remain zero until then.

    // Map status to stop reason.
    let status = response.status.clone();
    let incomplete_reason = response.incomplete_reason.clone();
    output.raw_stop_reason = Some(match (&incomplete_reason, &status) {
        (Some(reason), Some(status)) => format!("{status}.{reason}"),
        (_, Some(status)) => status.clone(),
        _ => status.clone().unwrap_or_default(),
    });
    let (stop_reason, error_message) = map_stop_reason(status.as_deref(), incomplete_reason.as_deref());
    output.stop_reason = stop_reason;
    output.error_message = error_message;
    if output.content.iter().any(|block| matches!(block, Content::ToolCall(_)))
        && output.stop_reason == StopReason::Stop
    {
        output.stop_reason = StopReason::ToolUse;
    }
}

/// Processes an OpenAI Responses event stream into assistant stream events,
/// mirroring `processResponsesStream`. The `output` message is mutated in
/// place; the `stream` receives partial events as they are produced.
pub fn process_responses_stream<I>(
    events: I,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    _model: &Model,
    options: Option<&OpenAIResponsesStreamOptions>,
) -> Result<(), String>
where
    I: IntoIterator<Item = ResponseStreamEvent>,
{
    let mut saw_terminal_response_event = false;
    let mut output_slots: std::collections::HashMap<u64, ResponsesOutputSlot> = std::collections::HashMap::new();
    let mut reasoning_blocks_by_id: std::collections::HashMap<String, ThinkingContent> =
        std::collections::HashMap::new();

    for event in events {
        match event {
            ResponseStreamEvent::ResponseCreated { response } => {
                output.response_id = response.id;
            }
            ResponseStreamEvent::ResponseOutputItemAdded { output_index, item } => {
                create_slot(output, stream, &mut output_slots, output_index, &item, options);
            }
            ResponseStreamEvent::ResponseReasoningSummaryTextDelta { output_index, delta } => {
                if let Some(ResponsesOutputSlot::Thinking { content_index }) =
                    output_slots.get(&(output_index as u64))
                {
                    if let Content::Thinking(block) = &mut output.content[*content_index] {
                        block.thinking.push_str(&delta);
                        stream.push(crate::types::AssistantMessageEvent::ThinkingDelta {
                            content_index: *content_index as f64,
                            delta,
                            partial: output.clone(),
                        });
                    }
                }
            }
            ResponseStreamEvent::ResponseReasoningSummaryPartDone { output_index } => {
                if let Some(ResponsesOutputSlot::Thinking { content_index }) =
                    output_slots.get(&(output_index as u64))
                {
                    if let Content::Thinking(block) = &mut output.content[*content_index] {
                        block.thinking.push_str("\n\n");
                        stream.push(crate::types::AssistantMessageEvent::ThinkingDelta {
                            content_index: *content_index as f64,
                            delta: "\n\n".to_string(),
                            partial: output.clone(),
                        });
                    }
                }
            }
            ResponseStreamEvent::ResponseReasoningTextDelta { output_index, delta } => {
                if let Some(ResponsesOutputSlot::Thinking { content_index }) =
                    output_slots.get(&(output_index as u64))
                {
                    if let Content::Thinking(block) = &mut output.content[*content_index] {
                        block.thinking.push_str(&delta);
                        stream.push(crate::types::AssistantMessageEvent::ThinkingDelta {
                            content_index: *content_index as f64,
                            delta,
                            partial: output.clone(),
                        });
                    }
                }
            }
            ResponseStreamEvent::ResponseOutputTextDelta { output_index, delta } => {
                if let Some(ResponsesOutputSlot::Text { content_index }) =
                    output_slots.get(&(output_index as u64))
                {
                    if let Content::Text(block) = &mut output.content[*content_index] {
                        block.text.push_str(&delta);
                        stream.push(crate::types::AssistantMessageEvent::TextDelta {
                            content_index: *content_index as f64,
                            delta,
                            partial: output.clone(),
                        });
                    }
                }
            }
            ResponseStreamEvent::ResponseRefusalDelta { output_index, delta } => {
                if let Some(ResponsesOutputSlot::Text { content_index }) =
                    output_slots.get(&(output_index as u64))
                {
                    if let Content::Text(block) = &mut output.content[*content_index] {
                        block.text.push_str(&delta);
                        stream.push(crate::types::AssistantMessageEvent::TextDelta {
                            content_index: *content_index as f64,
                            delta,
                            partial: output.clone(),
                        });
                    }
                }
            }
            ResponseStreamEvent::ResponseFunctionCallArgumentsDelta { output_index, delta } => {
                if let Some(ResponsesOutputSlot::ToolCall { content_index, scratch }) =
                    output_slots.get_mut(&(output_index as u64))
                {
                    if let Some(partial_json) = &mut scratch.partial_json {
                        partial_json.push_str(&delta);
                        if let Content::ToolCall(tool_call) = &mut output.content[*content_index] {
                            tool_call.arguments = parse_streaming_json(Some(partial_json));
                        }
                        push_tool_call_delta(
                            stream,
                            output,
                            &ResponsesOutputSlot::ToolCall {
                                content_index: *content_index,
                                scratch: ToolCallScratch {
                                    partial_json: None,
                                    custom_input: None,
                                },
                            },
                            &delta,
                        );
                    }
                }
            }
            ResponseStreamEvent::ResponseFunctionCallArgumentsDone { output_index, arguments } => {
                if let Some(ResponsesOutputSlot::ToolCall { content_index, scratch }) =
                    output_slots.get_mut(&(output_index as u64))
                {
                    let Some(previous_partial_json) = &scratch.partial_json else {
                        continue;
                    };
                    let previous = previous_partial_json.clone();
                    scratch.partial_json = Some(arguments.clone());
                    if let Content::ToolCall(tool_call) = &mut output.content[*content_index] {
                        tool_call.arguments = parse_streaming_json(Some(&arguments));
                    }

                    if arguments.starts_with(&previous) {
                        let delta = &arguments[previous.len()..];
                        if !delta.is_empty() {
                            push_tool_call_delta(
                                stream,
                                output,
                                &ResponsesOutputSlot::ToolCall {
                                    content_index: *content_index,
                                    scratch: ToolCallScratch {
                                        partial_json: None,
                                        custom_input: None,
                                    },
                                },
                                delta,
                            );
                        }
                    }
                }
            }
            ResponseStreamEvent::ResponseCustomToolCallInputDelta { output_index, delta } => {
                if let Some(ResponsesOutputSlot::ToolCall { content_index, scratch }) =
                    output_slots.get_mut(&(output_index as u64))
                {
                    let Some(custom_input) = &mut scratch.custom_input else {
                        continue;
                    };
                    let current = match &output.content[*content_index] {
                        Content::ToolCall(tool_call) => match &tool_call.arguments {
                            Value::Map(entries) => entries
                                .iter()
                                .find(|(key, _)| key == &custom_input.property)
                                .and_then(|(_, value)| value.as_str())
                                .unwrap_or("")
                                .to_string(),
                            _ => String::new(),
                        },
                        _ => String::new(),
                    };
                    if let Ok(Some(delta_out)) = crate::api::constrained_sampling::append_grammar_tool_input_json_delta(
                        &mut custom_input.json_buffer,
                        &custom_input.property,
                        &format!("{current}{delta}"),
                        false,
                    ) {
                        if let Content::ToolCall(tool_call) = &mut output.content[*content_index] {
                            tool_call.arguments =
                                Value::Map(vec![(custom_input.property.clone(), Value::String(format!("{current}{delta}")))]);
                        }
                        push_tool_call_delta(
                            stream,
                            output,
                            &ResponsesOutputSlot::ToolCall {
                                content_index: *content_index,
                                scratch: ToolCallScratch {
                                    partial_json: None,
                                    custom_input: None,
                                },
                            },
                            &delta_out,
                        );
                    }
                }
            }
            ResponseStreamEvent::ResponseCustomToolCallInputDone { output_index, input } => {
                if let Some(ResponsesOutputSlot::ToolCall { content_index, scratch }) =
                    output_slots.get_mut(&(output_index as u64))
                {
                    let Some(custom_input) = &mut scratch.custom_input else {
                        continue;
                    };
                    if let Ok(Some(delta_out)) = crate::api::constrained_sampling::append_grammar_tool_input_json_delta(
                        &mut custom_input.json_buffer,
                        &custom_input.property,
                        &input,
                        true,
                    ) {
                        if let Content::ToolCall(tool_call) = &mut output.content[*content_index] {
                            tool_call.arguments =
                                Value::Map(vec![(custom_input.property.clone(), Value::String(input.clone()))]);
                        }
                        push_tool_call_delta(
                            stream,
                            output,
                            &ResponsesOutputSlot::ToolCall {
                                content_index: *content_index,
                                scratch: ToolCallScratch {
                                    partial_json: None,
                                    custom_input: None,
                                },
                            },
                            &delta_out,
                        );
                    }
                }
            }
            ResponseStreamEvent::ResponseOutputItemDone { output_index, item } => {
                apply_message_phase_stop_reason(output, &item);
                if output_slots.get(&(output_index as u64)).is_none() {
                    create_slot(output, stream, &mut output_slots, output_index, &item, options);
                }

                match &item {
                    ResponseOutputItem::Reasoning(reasoning) => {
                        if let Some(ResponsesOutputSlot::Thinking { content_index }) =
                            output_slots.remove(&(output_index as u64))
                        {
                            let summary_text = reasoning.summary.join("\n\n");
                            let content_text = reasoning.content.join("\n\n");
                            let mut thinking = String::new();
                            if let Content::Thinking(block) = &mut output.content[content_index] {
                                block.thinking = if !summary_text.is_empty() {
                                    summary_text
                                } else if !content_text.is_empty() {
                                    content_text
                                } else {
                                    block.thinking.clone()
                                };
                                block.thinking_signature = Some(json_stringify(&reasoning.raw));
                                thinking = block.thinking.clone();
                                if let Some(id) = &reasoning.id {
                                    reasoning_blocks_by_id.insert(id.clone(), block.clone());
                                }
                            }
                            stream.push(crate::types::AssistantMessageEvent::ThinkingEnd {
                                content_index: content_index as f64,
                                content: thinking,
                                partial: output.clone(),
                            });
                        }
                    }
                    ResponseOutputItem::Message(message) => {
                        if let Some(ResponsesOutputSlot::Text { content_index }) =
                            output_slots.remove(&(output_index as u64))
                        {
                            let mut final_text = String::new();
                            if let Content::Text(block) = &mut output.content[content_index] {
                                block.text = message
                                    .content
                                    .iter()
                                    .map(|(text, refusal)| {
                                        if !text.is_empty() {
                                            text.clone()
                                        } else {
                                            refusal.clone()
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("");
                                block.text_signature = Some(encode_text_signature_v1(
                                    message.id.as_deref().unwrap_or(""),
                                    message.phase.as_deref(),
                                ));
                                final_text = block.text.clone();
                            }
                            stream.push(crate::types::AssistantMessageEvent::TextEnd {
                                content_index: content_index as f64,
                                content: final_text,
                                partial: output.clone(),
                            });
                        }
                    }
                    ResponseOutputItem::FunctionCall(function_call) => {
                        if let Some(ResponsesOutputSlot::ToolCall { content_index, scratch }) =
                            output_slots.remove(&(output_index as u64))
                        {
                            if scratch.partial_json.is_some() {
                                if let Content::ToolCall(tool_call) = &mut output.content[content_index] {
                                    let args = if !function_call.arguments.is_empty() {
                                        function_call.arguments.clone()
                                    } else {
                                        scratch.partial_json.clone().unwrap_or_else(|| "{}".to_string())
                                    };
                                    tool_call.arguments = parse_streaming_json(Some(&args));
                                    if function_call.namespace.is_some() {
                                        tool_call.namespace = function_call.namespace.clone();
                                    }
                                    let finalized = tool_call.clone();
                                    stream.push(crate::types::AssistantMessageEvent::ToolCallEnd {
                                        content_index: content_index as f64,
                                        tool_call: finalized,
                                        partial: output.clone(),
                                    });
                                }
                            }
                        }
                    }
                    ResponseOutputItem::CustomToolCall(custom_tool_call) => {
                        if let Some(ResponsesOutputSlot::ToolCall { content_index, scratch }) =
                            output_slots.remove(&(output_index as u64))
                        {
                            if scratch.custom_input.is_some() {
                                let input = if !custom_tool_call.input.is_empty() {
                                    custom_tool_call.input.clone()
                                } else {
                                    match &output.content[content_index] {
                                        Content::ToolCall(tool_call) => match &tool_call.arguments {
                                            Value::Map(entries) => entries
                                                .iter()
                                                .find(|(key, _)| key == "input")
                                                .and_then(|(_, value)| value.as_str())
                                                .unwrap_or("")
                                                .to_string(),
                                            _ => String::new(),
                                        },
                                        _ => String::new(),
                                    }
                                };
                                let mut scratch = scratch;
                                if let Some(custom_input) = &mut scratch.custom_input {
                                    if let Ok(Some(delta_out)) =
                                        crate::api::constrained_sampling::append_grammar_tool_input_json_delta(
                                            &mut custom_input.json_buffer,
                                            &custom_input.property,
                                            &input,
                                            true,
                                        )
                                    {
                                        if let Content::ToolCall(tool_call) = &mut output.content[content_index] {
                                            tool_call.arguments = Value::Map(vec![(
                                                custom_input.property.clone(),
                                                Value::String(input.clone()),
                                            )]);
                                        }
                                        push_tool_call_delta(
                                            stream,
                                            output,
                                            &ResponsesOutputSlot::ToolCall {
                                                content_index,
                                                scratch: ToolCallScratch {
                                                    partial_json: None,
                                                    custom_input: None,
                                                },
                                            },
                                            &delta_out,
                                        );
                                    }
                                }
                                if let Content::ToolCall(tool_call) = &mut output.content[content_index] {
                                    if custom_tool_call.namespace.is_some() {
                                        tool_call.namespace = custom_tool_call.namespace.clone();
                                    }
                                    let finalized = tool_call.clone();
                                    stream.push(crate::types::AssistantMessageEvent::ToolCallEnd {
                                        content_index: content_index as f64,
                                        tool_call: finalized,
                                        partial: output.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
            ResponseStreamEvent::ResponseCompleted { response } => {
                finalize_response(
                    output,
                    &mut saw_terminal_response_event,
                    &mut reasoning_blocks_by_id,
                    &response,
                );
            }
            ResponseStreamEvent::ResponseIncomplete { response } => {
                finalize_response(
                    output,
                    &mut saw_terminal_response_event,
                    &mut reasoning_blocks_by_id,
                    &response,
                );
            }
            ResponseStreamEvent::StreamError { code, message } => {
                return Err(format!(
                    "Error Code {}: {}",
                    code.unwrap_or_default(),
                    message.unwrap_or_default()
                ));
            }
            ResponseStreamEvent::ResponseFailed { response } => {
                output.raw_stop_reason = response.status.clone();
                let msg = match &response.error {
                    Some((code, message)) => format!("{code}: {message}"),
                    None => match &response.incomplete_reason {
                        Some(reason) => format!("incomplete: {reason}"),
                        None => "Unknown error (no error details in response)".to_string(),
                    },
                };
                return Err(msg);
            }
        }
    }

    if !saw_terminal_response_event {
        return Err("OpenAI Responses stream ended before a terminal response event".to_string());
    }
    Ok(())
}



fn create_slot(
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    output_slots: &mut std::collections::HashMap<u64, ResponsesOutputSlot>,
    output_index: f64,
    item: &ResponseOutputItem,
    options: Option<&OpenAIResponsesStreamOptions>,
) {
    match item {
        ResponseOutputItem::Reasoning(_) => {
            output.content.push(Content::Thinking(ThinkingContent {
                thinking: String::new(),
                thinking_signature: None,
                redacted: None,
            }));
            let content_index = output.content.len() - 1;
            output_slots.insert(output_index as u64, ResponsesOutputSlot::Thinking { content_index });
            stream.push(crate::types::AssistantMessageEvent::ThinkingStart {
                content_index: content_index as f64,
                partial: output.clone(),
            });
        }
        ResponseOutputItem::Message(_) => {
            output.content.push(Content::Text(TextContent {
                text: String::new(),
                text_signature: None,
            }));
            let content_index = output.content.len() - 1;
            output_slots.insert(output_index as u64, ResponsesOutputSlot::Text { content_index });
            stream.push(crate::types::AssistantMessageEvent::TextStart {
                content_index: content_index as f64,
                partial: output.clone(),
            });
        }
        ResponseOutputItem::FunctionCall(function_call) => {
            output.content.push(Content::ToolCall(ToolCall {
                id: format!(
                    "{}|{}",
                    function_call.call_id,
                    function_call.id.as_deref().unwrap_or("")
                ),
                name: function_call.name.clone(),
                arguments: Value::Map(Vec::new()),
                thought_signature: None,
                namespace: function_call.namespace.clone(),
            }));
            let content_index = output.content.len() - 1;
            output_slots.insert(
                output_index as u64,
                ResponsesOutputSlot::ToolCall {
                    content_index,
                    scratch: ToolCallScratch {
                        partial_json: Some(function_call.arguments.clone()),
                        custom_input: None,
                    },
                },
            );
            stream.push(crate::types::AssistantMessageEvent::ToolCallStart {
                content_index: content_index as f64,
                partial: output.clone(),
            });
        }
        ResponseOutputItem::CustomToolCall(custom_tool_call) => {
            let input_property = options
                .and_then(|o| o.grammar_tool_input_properties.as_ref())
                .and_then(|properties| {
                    properties
                        .iter()
                        .find(|(name, _)| name == &custom_tool_call.name)
                        .map(|(_, property)| property.clone())
                })
                .unwrap_or_else(|| "input".to_string());
            let input = custom_tool_call.input.clone();
            output.content.push(Content::ToolCall(ToolCall {
                id: format!(
                    "{}|{}",
                    custom_tool_call.call_id,
                    custom_tool_call.id.as_deref().unwrap_or("")
                ),
                name: custom_tool_call.name.clone(),
                arguments: Value::Map(vec![(input_property.clone(), Value::String(input))]),
                thought_signature: None,
                namespace: custom_tool_call.namespace.clone(),
            }));
            let content_index = output.content.len() - 1;
            output_slots.insert(
                output_index as u64,
                ResponsesOutputSlot::ToolCall {
                    content_index,
                    scratch: ToolCallScratch {
                        partial_json: None,
                        custom_input: Some(CustomInputState {
                            property: input_property,
                            json_buffer: GrammarToolInputJsonBuffer::default(),
                        }),
                    },
                },
            );
            stream.push(crate::types::AssistantMessageEvent::ToolCallStart {
                content_index: content_index as f64,
                partial: output.clone(),
            });
        }
    }
}
