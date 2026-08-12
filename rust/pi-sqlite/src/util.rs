//! JSON serialization helpers for the SQLite repo layer.
//!
//! The JS repo stores entries and records as JSON payloads whose shapes
//! mirror the in-memory objects; these helpers convert between the Rust
//! types and the Value tree with those shapes.

use pi_agent_core::harness::session_types::{
    AbortRequestedRecord, Entry, LaneRecord, OperationFinishedRecord, OperationOutcome,
    OperationStartedRecord, ProvisionedEntry, QueueCancelledRecord, QueueEnqueuedRecord, RunIntent,
    SessionStopReason, StepAttemptRecord, ToolStartedRecord, UsageRecord, WriteDeferredRecord,
};
use pi_agent_core::types::AgentMessage;
use pi_ai::types::Usage;
use pi_protocol::cbor::Value;

pub type JsonValue = Value;

fn kv(key: &str, value: Value) -> (String, Value) {
    (key.to_string(), value)
}

fn str(value: &str) -> Value {
    Value::String(value.to_string())
}

fn num(value: f64) -> Value {
    Value::Number(value)
}

fn opt_str(value: Option<&str>) -> Value {
    match value {
        Some(value) => Value::String(value.to_string()),
        None => Value::Null,
    }
}

fn opt_num(value: Option<f64>) -> Value {
    match value {
        Some(value) => Value::Number(value),
        None => Value::Null,
    }
}

fn opt_value(value: Option<Value>) -> Value {
    value.unwrap_or(Value::Null)
}

pub fn json_stringify(value: &Value) -> String {
    pi_ai::utils::json::json_stringify(value)
}

/// Serialize a Usage to the JS shape.
pub fn usage_to_json(usage: &Usage) -> Value {
    Value::Map(vec![
        kv("input", num(usage.input)),
        kv("output", num(usage.output)),
        kv("cacheRead", num(usage.cache_read)),
        kv("cacheWrite", num(usage.cache_write)),
        kv("cacheWrite1h", opt_num(usage.cache_write_1h)),
        kv("reasoning", opt_num(usage.reasoning)),
        kv("totalTokens", num(usage.total_tokens)),
        kv(
            "cost",
            Value::Map(vec![
                kv("input", num(usage.cost.input)),
                kv("output", num(usage.cost.output)),
                kv("cacheRead", num(usage.cost.cache_read)),
                kv("cacheWrite", num(usage.cost.cache_write)),
                kv("total", num(usage.cost.total)),
            ]),
        ),
    ])
}

/// Parse a Usage from the JS shape.
pub fn json_to_usage(entries: &[(String, Value)]) -> Usage {
    fn number(entries: &[(String, Value)], key: &str) -> f64 {
        entries
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.as_number())
            .unwrap_or(0.0)
    }
    fn opt_number(entries: &[(String, Value)], key: &str) -> Option<f64> {
        entries
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.as_number())
    }
    let cost: Vec<(String, Value)> = entries
        .iter()
        .find(|(k, _)| k == "cost")
        .and_then(|(_, v)| v.as_map())
        .map(|map| map.to_vec())
        .unwrap_or_default();
    Usage {
        input: number(entries, "input"),
        output: number(entries, "output"),
        cache_read: number(entries, "cacheRead"),
        cache_write: number(entries, "cacheWrite"),
        cache_write_1h: opt_number(entries, "cacheWrite1h"),
        reasoning: opt_number(entries, "reasoning"),
        total_tokens: number(entries, "totalTokens"),
        cost: pi_ai::types::UsageCost {
            input: number(&cost, "input"),
            output: number(&cost, "output"),
            cache_read: number(&cost, "cacheRead"),
            cache_write: number(&cost, "cacheWrite"),
            total: number(&cost, "total"),
        },
    }
}

/// Serialize an AgentMessage (Llm messages map to their JS shapes; custom
/// messages to {role, customType, ...}).
pub fn agent_message_to_json(message: &AgentMessage) -> Value {
    match message {
        AgentMessage::Llm(pi_ai::types::Message::User(user)) => Value::Map(vec![
            kv("role", str("user")),
            kv(
                "content",
                match &user.content {
                    pi_ai::types::UserMessageContent::Text(text) => str(text),
                    pi_ai::types::UserMessageContent::Blocks(blocks) => Value::Array(
                        blocks
                            .iter()
                            .map(|block| match block {
                                pi_ai::types::Content::Text(text) => Value::Map(vec![
                                    kv("type", str("text")),
                                    kv("text", str(&text.text)),
                                ]),
                                pi_ai::types::Content::Image(image) => Value::Map(vec![
                                    kv("type", str("image")),
                                    kv("data", str(&image.data)),
                                    kv("mimeType", str(&image.mime_type)),
                                ]),
                                _other => Value::Map(vec![kv("type", str("unknown"))]),
                            })
                            .collect(),
                    ),
                },
            ),
            kv("timestamp", num(user.timestamp)),
        ]),
        AgentMessage::Llm(pi_ai::types::Message::Assistant(assistant)) => {
            let mut entries = vec![
                kv("role", str("assistant")),
                kv(
                    "content",
                    Value::Array(
                        assistant
                            .content
                            .iter()
                            .map(|block| match block {
                                pi_ai::types::Content::Text(text) => Value::Map(vec![
                                    kv("type", str("text")),
                                    kv("text", str(&text.text)),
                                ]),
                                pi_ai::types::Content::Thinking(thinking) => Value::Map(vec![
                                    kv("type", str("thinking")),
                                    kv("thinking", str(&thinking.thinking)),
                                ]),
                                pi_ai::types::Content::Image(image) => Value::Map(vec![
                                    kv("type", str("image")),
                                    kv("data", str(&image.data)),
                                    kv("mimeType", str(&image.mime_type)),
                                ]),
                                pi_ai::types::Content::ToolCall(tool_call) => Value::Map(vec![
                                    kv("type", str("toolCall")),
                                    kv("id", str(&tool_call.id)),
                                    kv("name", str(&tool_call.name)),
                                    kv("arguments", tool_call.arguments.clone()),
                                ]),
                            })
                            .collect(),
                    ),
                ),
                kv("api", str(&assistant.api)),
                kv("provider", str(&assistant.provider)),
                kv("model", str(&assistant.model)),
                kv("usage", usage_to_json(&assistant.usage)),
                kv("stopReason", str(assistant.stop_reason.as_str())),
                kv("timestamp", num(assistant.timestamp)),
            ];
            if let Some(response_model) = &assistant.response_model {
                entries.push(kv("responseModel", str(response_model)));
            }
            if let Some(error_message) = &assistant.error_message {
                entries.push(kv("errorMessage", str(error_message)));
            }
            Value::Map(entries)
        }
        AgentMessage::Llm(pi_ai::types::Message::ToolResult(tool_result)) => Value::Map(vec![
            kv("role", str("toolResult")),
            kv("toolCallId", str(&tool_result.tool_call_id)),
            kv("toolName", str(&tool_result.tool_name)),
            kv(
                "content",
                Value::Array(
                    tool_result
                        .content
                        .iter()
                        .map(|block| match block {
                            pi_ai::types::Content::Text(text) => Value::Map(vec![
                                kv("type", str("text")),
                                kv("text", str(&text.text)),
                            ]),
                            pi_ai::types::Content::Image(image) => Value::Map(vec![
                                kv("type", str("image")),
                                kv("data", str(&image.data)),
                                kv("mimeType", str(&image.mime_type)),
                            ]),
                            _other => Value::Map(vec![kv("type", str("unknown"))]),
                        })
                        .collect(),
                ),
            ),
            kv("isError", Value::Bool(tool_result.is_error)),
            kv("timestamp", num(tool_result.timestamp)),
        ]),
        AgentMessage::Custom(_custom) => Value::Map(vec![kv("role", str("custom"))]),
    }
}

pub fn agent_messages_to_json(messages: &[AgentMessage]) -> Value {
    Value::Array(messages.iter().map(agent_message_to_json).collect())
}

/// Parse an AgentMessage from the JS shape.
pub fn json_to_agent_message(value: Value) -> AgentMessage {
    let entries: Vec<(String, Value)> = value.as_map().map(|map| map.to_vec()).unwrap_or_default();
    let role = entries
        .iter()
        .find(|(k, _)| k == "role")
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("");
    match role {
        "user" => {
            let content = entries
                .iter()
                .find(|(k, _)| k == "content")
                .map(|(_, v)| v.clone())
                .unwrap_or(Value::Null);
            let content = match content {
                Value::String(text) => pi_ai::types::UserMessageContent::Text(text),
                Value::Array(blocks) => pi_ai::types::UserMessageContent::Blocks(
                    blocks.iter().map(json_content_to_ai).collect(),
                ),
                _ => pi_ai::types::UserMessageContent::Text(String::new()),
            };
            AgentMessage::Llm(pi_ai::types::Message::User(pi_ai::types::UserMessage {
                content,
                timestamp: number_of(&entries, "timestamp"),
            }))
        }
        "assistant" => {
            let content = entries
                .iter()
                .find(|(k, _)| k == "content")
                .and_then(|(_, v)| v.as_array())
                .unwrap_or_default()
                .iter()
                .map(json_content_to_ai)
                .collect();
            let stop_reason = entries
                .iter()
                .find(|(k, _)| k == "stopReason")
                .and_then(|(_, v)| v.as_str())
                .unwrap_or("stop");
            let usage = match entries
                .iter()
                .find(|(k, _)| k == "usage")
                .and_then(|(_, v)| v.as_map())
            {
                Some(usage_entries) => json_to_usage(usage_entries),
                None => empty_usage(),
            };
            AgentMessage::Llm(pi_ai::types::Message::Assistant(pi_ai::types::AssistantMessage {
                content,
                api: string_of(&entries, "api"),
                provider: string_of(&entries, "provider"),
                model: string_of(&entries, "model"),
                response_model: entries
                    .iter()
                    .find(|(k, _)| k == "responseModel")
                    .and_then(|(_, v)| v.as_str())
                    .map(|value| value.to_string()),
                response_id: None,
                usage,
                stop_reason: pi_ai::types::StopReason::parse(stop_reason)
                    .unwrap_or(pi_ai::types::StopReason::Stop),
                deferred: None,
                error_message: entries
                    .iter()
                    .find(|(k, _)| k == "errorMessage")
                    .and_then(|(_, v)| v.as_str())
                    .map(|value| value.to_string()),
                raw_stop_reason: None,
                end_turn: None,
                timestamp: number_of(&entries, "timestamp"),
            }))
        }
        "toolResult" => AgentMessage::Llm(pi_ai::types::Message::ToolResult(pi_ai::types::ToolResultMessage {
            tool_call_id: string_of(&entries, "toolCallId"),
            tool_name: string_of(&entries, "toolName"),
            content: entries
                .iter()
                .find(|(k, _)| k == "content")
                .and_then(|(_, v)| v.as_array())
                .unwrap_or_default()
                .iter()
                .map(json_content_to_ai)
                .collect(),
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: entries
                .iter()
                .find(|(k, _)| k == "isError")
                .and_then(|(_, v)| v.as_bool())
                .unwrap_or(false),
            timestamp: number_of(&entries, "timestamp"),
        })),
        _ => AgentMessage::Llm(pi_ai::types::Message::User(pi_ai::types::UserMessage {
            content: pi_ai::types::UserMessageContent::Text(String::new()),
            timestamp: 0.0,
        })),
    }
}

fn json_content_to_ai(value: &Value) -> pi_ai::types::Content {
    let entries: Vec<(String, Value)> = value.as_map().map(|map| map.to_vec()).unwrap_or_default();
    let type_ = entries
        .iter()
        .find(|(k, _)| k == "type")
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("");
    match type_ {
        "text" => pi_ai::types::Content::Text(pi_ai::types::TextContent {
            text: string_of(&entries, "text"),
            text_signature: None,
        }),
        "thinking" => pi_ai::types::Content::Thinking(pi_ai::types::ThinkingContent {
            thinking: string_of(&entries, "thinking"),
            thinking_signature: None,
            redacted: entries
                .iter()
                .find(|(k, _)| k == "redacted")
                .and_then(|(_, v)| v.as_bool()),
        }),
        "image" => pi_ai::types::Content::Image(pi_ai::types::ImageContent {
            data: string_of(&entries, "data"),
            mime_type: string_of(&entries, "mimeType"),
        }),
        "toolCall" => pi_ai::types::Content::ToolCall(pi_ai::types::ToolCall {
            id: string_of(&entries, "id"),
            name: string_of(&entries, "name"),
            arguments: entries
                .iter()
                .find(|(k, _)| k == "arguments")
                .map(|(_, v)| v.clone())
                .unwrap_or(Value::Map(Vec::new())),
            thought_signature: None,
            namespace: None,
        }),
        _ => pi_ai::types::Content::Text(pi_ai::types::TextContent {
            text: String::new(),
            text_signature: None,
        }),
    }
}

fn empty_usage() -> Usage {
    Usage {
        input: 0.0,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0.0,
        cost: pi_ai::types::UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
    }
}

fn number_of(entries: &[(String, Value)], key: &str) -> f64 {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_number())
        .unwrap_or(0.0)
}

fn string_of(entries: &[(String, Value)], key: &str) -> String {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------------------
// LaneRecord JSON
// ---------------------------------------------------------------------------

fn record_base_entries(base: &pi_agent_core::harness::session_types::RecordBase) -> Vec<(String, Value)> {
    vec![
        kv("id", str(&base.id)),
        kv("lane", str(&base.lane)),
        kv("seq", num(base.seq)),
        kv("timestamp", num(base.timestamp)),
    ]
}

fn run_intent_to_json(intent: &RunIntent) -> Value {
    match intent {
        RunIntent::Run {
            original_prompt,
            initial_messages,
            system_prompt_override,
            resume_data,
        } => Value::Map(vec![
            kv("kind", str("run")),
            kv("originalPrompt", agent_messages_to_json(original_prompt)),
            kv("initialMessages", Value::Array(initial_messages.iter().map(provisioned_entry_to_json).collect())),
            kv("systemPromptOverride", opt_str(system_prompt_override.as_deref())),
            kv(
                "resumeData",
                match resume_data {
                    Some(entries) => Value::Array(
                        entries
                            .iter()
                            .map(|(key, value)| Value::Map(vec![kv("key", str(key)), kv("value", value.clone())]))
                            .collect(),
                    ),
                    None => Value::Null,
                },
            ),
        ]),
        RunIntent::Compaction {
            custom_instructions,
            result_entry_id,
        } => Value::Map(vec![
            kv("kind", str("compaction")),
            kv("customInstructions", opt_str(custom_instructions.as_deref())),
            kv("resultEntryId", str(result_entry_id)),
        ]),
        RunIntent::Navigation {
            target_id,
            summarize,
            custom_instructions,
            label,
            summary_entry_id,
        } => Value::Map(vec![
            kv("kind", str("navigation")),
            kv("targetId", opt_str(target_id.as_deref())),
            kv("summarize", Value::Bool(*summarize)),
            kv("customInstructions", opt_str(custom_instructions.as_deref())),
            kv("label", opt_str(label.as_deref())),
            kv("summaryEntryId", opt_str(summary_entry_id.as_deref())),
        ]),
    }
}

fn provisioned_entry_to_json(entry: &ProvisionedEntry) -> Value {
    crate::repo::entry_payload(entry)
}

fn json_to_run_intent(value: &Value) -> RunIntent {
    let entries: Vec<(String, Value)> = value.as_map().map(|map| map.to_vec()).unwrap_or_default();
    let kind = entries
        .iter()
        .find(|(k, _)| k == "kind")
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("");
    match kind {
        "compaction" => RunIntent::Compaction {
            custom_instructions: entries
                .iter()
                .find(|(k, _)| k == "customInstructions")
                .and_then(|(_, v)| v.as_str())
                .map(|value| value.to_string()),
            result_entry_id: string_of(&entries, "resultEntryId"),
        },
        "navigation" => RunIntent::Navigation {
            target_id: entries
                .iter()
                .find(|(k, _)| k == "targetId")
                .and_then(|(_, v)| v.as_str())
                .map(|value| value.to_string()),
            summarize: entries
                .iter()
                .find(|(k, _)| k == "summarize")
                .and_then(|(_, v)| v.as_bool())
                .unwrap_or(false),
            custom_instructions: entries
                .iter()
                .find(|(k, _)| k == "customInstructions")
                .and_then(|(_, v)| v.as_str())
                .map(|value| value.to_string()),
            label: entries
                .iter()
                .find(|(k, _)| k == "label")
                .and_then(|(_, v)| v.as_str())
                .map(|value| value.to_string()),
            summary_entry_id: entries
                .iter()
                .find(|(k, _)| k == "summaryEntryId")
                .and_then(|(_, v)| v.as_str())
                .map(|value| value.to_string()),
        },
        _ => RunIntent::Run {
            original_prompt: entries
                .iter()
                .find(|(k, _)| k == "originalPrompt")
                .and_then(|(_, v)| v.as_array())
                .unwrap_or_default()
                .iter()
                .map(|value| json_to_agent_message(value.clone()))
                .collect(),
            initial_messages: entries
                .iter()
                .find(|(k, _)| k == "initialMessages")
                .and_then(|(_, v)| v.as_array())
                .unwrap_or_default()
                .iter()
                .map(json_to_provisioned_entry)
                .collect(),
            system_prompt_override: entries
                .iter()
                .find(|(k, _)| k == "systemPromptOverride")
                .and_then(|(_, v)| v.as_str())
                .map(|value| value.to_string()),
            resume_data: entries
                .iter()
                .find(|(k, _)| k == "resumeData")
                .and_then(|(_, v)| v.as_array())
                .map(|array| {
                    array
                        .iter()
                        .filter_map(|entry| {
                            let entries = entry.as_map()?;
                            let key = entries.iter().find(|(k, _)| k == "key")?.1.as_str()?;
                            let value = entries
                                .iter()
                                .find(|(k, _)| k == "value")
                                .map(|(_, v)| v.clone())
                                .unwrap_or(Value::Null);
                            Some((key.to_string(), value))
                        })
                        .collect()
                }),
        },
    }
}

/// Serialize a LaneRecord to its JSON payload shape.
pub fn lane_record_to_json(record: &LaneRecord) -> Value {
    let (type_, extra): (&str, Vec<(String, Value)>) = match record {
        LaneRecord::OperationStarted(record) => (
            "operation_started",
            vec![
                kv("sourceLeafId", opt_str(record.source_leaf_id.as_deref())),
                kv("intent", run_intent_to_json(&record.intent)),
            ],
        ),
        LaneRecord::AbortRequested(record) => (
            "abort_requested",
            vec![kv("runId", str(&record.run_id))],
        ),
        LaneRecord::OperationFinished(record) => (
            "operation_finished",
            vec![
                kv("runId", str(&record.run_id)),
                kv(
                    "outcome",
                    str(match record.outcome {
                        OperationOutcome::Completed => "completed",
                        OperationOutcome::Aborted => "aborted",
                        OperationOutcome::Failed => "failed",
                        OperationOutcome::Declined => "declined",
                    }),
                ),
                kv(
                    "error",
                    match &record.error {
                        Some((code, message)) => Value::Map(vec![kv("code", str(code)), kv("message", str(message))]),
                        None => Value::Null,
                    },
                ),
            ],
        ),
        LaneRecord::StepAttempt(record) => (
            "step_attempt",
            vec![
                kv("runId", str(&record.run_id)),
                kv("step", str(&record.step)),
                kv("attempt", num(record.attempt)),
                kv("resultEntryId", str(&record.result_entry_id)),
                kv("compactionReason", opt_str(record.compaction_reason.as_deref())),
            ],
        ),
        LaneRecord::ToolStarted(record) => (
            "tool_started",
            vec![
                kv("runId", str(&record.run_id)),
                kv("assistantEntryId", str(&record.assistant_entry_id)),
                kv("toolIndex", num(record.tool_index)),
                kv("toolCallId", str(&record.tool_call_id)),
                kv("toolName", str(&record.tool_name)),
                kv(
                    "effectiveArgs",
                    Value::Array(
                        record
                            .effective_args
                            .iter()
                            .map(|(key, value)| Value::Map(vec![kv("key", str(key)), kv("value", value.clone())]))
                            .collect(),
                    ),
                ),
                kv("resultEntryId", str(&record.result_entry_id)),
                kv("replay", str(&record.replay)),
            ],
        ),
        LaneRecord::QueueEnqueued(record) => (
            "queue_enqueued",
            vec![
                kv("queue", str(&record.queue)),
                kv("runId", opt_str(record.run_id.as_deref())),
                kv("target", provisioned_entry_to_json(&record.target)),
            ],
        ),
        LaneRecord::QueueCancelled(record) => (
            "queue_cancelled",
            vec![
                kv("runId", opt_str(record.run_id.as_deref())),
                kv("entryId", str(&record.entry_id)),
            ],
        ),
        LaneRecord::WriteDeferred(record) => (
            "write_deferred",
            vec![
                kv("runId", str(&record.run_id)),
                kv("target", provisioned_entry_to_json(&record.target)),
            ],
        ),
        LaneRecord::Usage(record) => (
            "usage",
            vec![
                kv("usage", usage_to_json(&record.usage)),
                kv("cause", str(&record.cause)),
                kv("runId", opt_str(record.run_id.as_deref())),
                kv("entryId", opt_str(record.entry_id.as_deref())),
                kv("attempt", opt_num(record.attempt)),
                kv(
                    "stopReason",
                    match &record.stop_reason {
                        Some(reason) => str(reason.as_str()),
                        None => Value::Null,
                    },
                ),
                kv("toolCallId", opt_str(record.tool_call_id.as_deref())),
                kv("details", opt_value(record.details.clone())),
            ],
        ),
    };
    let mut entries = vec![kv("type", str(type_))];
    entries.extend(record_base_entries(record_base_of(record)));
    entries.extend(extra);
    Value::Map(entries)
}

fn record_base_of(record: &LaneRecord) -> &pi_agent_core::harness::session_types::RecordBase {
    match record {
        LaneRecord::OperationStarted(record) => &record.base,
        LaneRecord::AbortRequested(record) => &record.base,
        LaneRecord::OperationFinished(record) => &record.base,
        LaneRecord::StepAttempt(record) => &record.base,
        LaneRecord::ToolStarted(record) => &record.base,
        LaneRecord::QueueEnqueued(record) => &record.base,
        LaneRecord::QueueCancelled(record) => &record.base,
        LaneRecord::WriteDeferred(record) => &record.base,
        LaneRecord::Usage(record) => &record.base,
    }
}

fn base_from_json(entries: &[(String, Value)]) -> pi_agent_core::harness::session_types::RecordBase {
    pi_agent_core::harness::session_types::RecordBase {
        id: string_of(entries, "id"),
        seq: number_of(entries, "seq"),
        lane: string_of(entries, "lane"),
        timestamp: number_of(entries, "timestamp"),
    }
}

fn json_to_provisioned_entry(value: &Value) -> ProvisionedEntry {
    // ponytail: provisioned entries round-trip through the entry payload
    // decode path; the base fields are defaults since storage re-assigns
    // them.
    let entries: Vec<(String, Value)> = value.as_map().map(|map| map.to_vec()).unwrap_or_default();
    let type_ = string_of(&entries, "type");
    let base = pi_agent_core::harness::session_types::EntryBase {
        type_: type_.clone(),
        id: string_of(&entries, "id"),
        seq: 0.0,
        parent_id: None,
        timestamp: 0.0,
    };
    match type_.as_str() {
        "message" => Entry::Message(pi_agent_core::harness::session_types::MessageEntry {
            base,
            message: json_to_agent_message(
                entries
                    .iter()
                    .find(|(k, _)| k == "message")
                    .map(|(_, v)| v.clone())
                    .unwrap_or(Value::Null),
            ),
            terminate: None,
        }),
        "custom" => Entry::Custom(pi_agent_core::harness::session_types::CustomEntry {
            base,
            custom_type: string_of(&entries, "customType"),
            data: entries
                .iter()
                .find(|(k, _)| k == "data")
                .map(|(_, v)| v.clone()),
        }),
        "model_change" => Entry::ModelChange(pi_agent_core::harness::session_types::ModelChangeEntry {
            base,
            provider: string_of(&entries, "provider"),
            model_id: string_of(&entries, "modelId"),
        }),
        "thinking_level_change" => {
            Entry::ThinkingLevelChange(pi_agent_core::harness::session_types::ThinkingLevelEntry {
                base,
                thinking_level: string_of(&entries, "thinkingLevel"),
            })
        }
        "active_tools_change" => Entry::ActiveToolsChange(
            pi_agent_core::harness::session_types::ActiveToolsEntry {
                base,
                active_tool_names: entries
                    .iter()
                    .find(|(k, _)| k == "activeToolNames")
                    .and_then(|(_, v)| v.as_array())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|value| value.as_str().map(|value| value.to_string()))
                    .collect(),
            },
        ),
        _ => Entry::Custom(pi_agent_core::harness::session_types::CustomEntry {
            base,
            custom_type: String::new(),
            data: None,
        }),
    }
}

/// Parse a LaneRecord from its JSON payload.
pub fn json_to_lane_record(value: Value) -> Result<LaneRecord, String> {
    let entries: Vec<(String, Value)> = value.as_map().map(|map| map.to_vec()).unwrap_or_default();
    let type_ = string_of(&entries, "type");
    let base = base_from_json(&entries);
    let outcome_of = |entries: &[(String, Value)]| -> OperationOutcome {
        match string_of(entries, "outcome").as_str() {
            "aborted" => OperationOutcome::Aborted,
            "failed" => OperationOutcome::Failed,
            "declined" => OperationOutcome::Declined,
            _ => OperationOutcome::Completed,
        }
    };
    let error_of = |entries: &[(String, Value)]| -> Option<(String, String)> {
        entries
            .iter()
            .find(|(k, _)| k == "error")
            .and_then(|(_, v)| v.as_map())
            .map(|map| (string_of(map, "code"), string_of(map, "message")))
    };
    let stop_reason_of = |entries: &[(String, Value)]| -> Option<SessionStopReason> {
        entries
            .iter()
            .find(|(k, _)| k == "stopReason")
            .and_then(|(_, v)| v.as_str())
            .map(|reason| reason.to_string())
    };
    let record = match type_.as_str() {
        "operation_started" => LaneRecord::OperationStarted(OperationStartedRecord {
            base,
            source_leaf_id: entries
                .iter()
                .find(|(k, _)| k == "sourceLeafId")
                .and_then(|(_, v)| v.as_str())
                .map(|value| value.to_string()),
            intent: entries
                .iter()
                .find(|(k, _)| k == "intent")
                .map(|(_, v)| json_to_run_intent(v))
                .unwrap_or(RunIntent::Run {
                    original_prompt: vec![],
                    initial_messages: vec![],
                    system_prompt_override: None,
                    resume_data: None,
                }),
        }),
        "abort_requested" => LaneRecord::AbortRequested(AbortRequestedRecord {
            base,
            run_id: string_of(&entries, "runId"),
        }),
        "operation_finished" => LaneRecord::OperationFinished(OperationFinishedRecord {
            base,
            run_id: string_of(&entries, "runId"),
            outcome: outcome_of(&entries),
            error: error_of(&entries),
        }),
        "step_attempt" => LaneRecord::StepAttempt(StepAttemptRecord {
            base,
            run_id: string_of(&entries, "runId"),
            step: string_of(&entries, "step"),
            attempt: number_of(&entries, "attempt"),
            result_entry_id: string_of(&entries, "resultEntryId"),
            compaction_reason: entries
                .iter()
                .find(|(k, _)| k == "compactionReason")
                .and_then(|(_, v)| v.as_str())
                .map(|value| value.to_string()),
        }),
        "tool_started" => LaneRecord::ToolStarted(ToolStartedRecord {
            base,
            run_id: string_of(&entries, "runId"),
            assistant_entry_id: string_of(&entries, "assistantEntryId"),
            tool_index: number_of(&entries, "toolIndex"),
            tool_call_id: string_of(&entries, "toolCallId"),
            tool_name: string_of(&entries, "toolName"),
            effective_args: entries
                .iter()
                .find(|(k, _)| k == "effectiveArgs")
                .and_then(|(_, v)| v.as_array())
                .map(|array| array.to_vec())
                .unwrap_or_default()
                .iter()
                .filter_map(|entry| {
                    let entries = entry.as_map()?;
                    let key = entries.iter().find(|(k, _)| k == "key")?.1.as_str()?;
                    let value = entries
                        .iter()
                        .find(|(k, _)| k == "value")
                        .map(|(_, v)| v.clone())
                        .unwrap_or(Value::Null);
                    Some((key.to_string(), value))
                })
                .collect(),
            result_entry_id: string_of(&entries, "resultEntryId"),
            replay: string_of(&entries, "replay"),
        }),
        "queue_enqueued" => LaneRecord::QueueEnqueued(QueueEnqueuedRecord {
            base,
            queue: string_of(&entries, "queue"),
            run_id: entries
                .iter()
                .find(|(k, _)| k == "runId")
                .and_then(|(_, v)| v.as_str())
                .map(|value| value.to_string()),
            target: entries
                .iter()
                .find(|(k, _)| k == "target")
                .map(|(_, v)| json_to_provisioned_entry(v))
                .unwrap_or(Entry::Custom(pi_agent_core::harness::session_types::CustomEntry {
                    base: pi_agent_core::harness::session_types::EntryBase {
                        type_: "custom".to_string(),
                        id: String::new(),
                        seq: 0.0,
                        parent_id: None,
                        timestamp: 0.0,
                    },
                    custom_type: String::new(),
                    data: None,
                })),
        }),
        "queue_cancelled" => LaneRecord::QueueCancelled(QueueCancelledRecord {
            base,
            run_id: entries
                .iter()
                .find(|(k, _)| k == "runId")
                .and_then(|(_, v)| v.as_str())
                .map(|value| value.to_string()),
            entry_id: string_of(&entries, "entryId"),
        }),
        "write_deferred" => LaneRecord::WriteDeferred(WriteDeferredRecord {
            base,
            run_id: string_of(&entries, "runId"),
            target: entries
                .iter()
                .find(|(k, _)| k == "target")
                .map(|(_, v)| json_to_provisioned_entry(v))
                .unwrap_or(Entry::Custom(pi_agent_core::harness::session_types::CustomEntry {
                    base: pi_agent_core::harness::session_types::EntryBase {
                        type_: "custom".to_string(),
                        id: String::new(),
                        seq: 0.0,
                        parent_id: None,
                        timestamp: 0.0,
                    },
                    custom_type: String::new(),
                    data: None,
                })),
        }),
        "usage" => LaneRecord::Usage(UsageRecord {
            base,
            usage: match entries
                .iter()
                .find(|(k, _)| k == "usage")
                .and_then(|(_, v)| v.as_map())
            {
                Some(usage_entries) => json_to_usage(usage_entries),
                None => empty_usage(),
            },
            cause: string_of(&entries, "cause"),
            run_id: entries
                .iter()
                .find(|(k, _)| k == "runId")
                .and_then(|(_, v)| v.as_str())
                .map(|value| value.to_string()),
            entry_id: entries
                .iter()
                .find(|(k, _)| k == "entryId")
                .and_then(|(_, v)| v.as_str())
                .map(|value| value.to_string()),
            attempt: entries
                .iter()
                .find(|(k, _)| k == "attempt")
                .and_then(|(_, v)| v.as_number()),
            stop_reason: stop_reason_of(&entries),
            tool_call_id: entries
                .iter()
                .find(|(k, _)| k == "toolCallId")
                .and_then(|(_, v)| v.as_str())
                .map(|value| value.to_string()),
            details: entries
                .iter()
                .find(|(k, _)| k == "details")
                .map(|(_, v)| v.clone()),
        }),
        _ => return Err(format!("Unknown record type: {type_}")),
    };
    Ok(record)
}
