//! pi-messages API provider, port of `packages/ai/src/api/pi-messages.ts`.
//!
//! Streams pi's own message protocol to a backend: a single POST of
//! `{ model, context, options }` to `<baseUrl>/messages`, with an SSE stream
//! of serialized assistant-message events plus a terminal `done`/`error`
//! event (the Radius gateway protocol).
//!
//! Porting notes:
//! - The JS `PiMessagesResponseError` (with `code`/`diagnosticDetails`) is
//!   flattened into the error message string; `PiMessagesResponseError`
//!   instance checks in error paths therefore do not apply.
//! - Rewrite diagnostics (`pi_messages_rewrite`) are parsed but not attached
//!   to the assistant message: the Rust `AssistantMessage` has no
//!   `diagnostics` field yet (adding it is a shared change affecting all
//!   constructors; deferred).
//! - HTTP status text is unavailable from the Rust client, so error strings
//!   are `<status>: <suffix>` instead of `<status> <statusText>: <suffix>`.

use pi_protocol::Value;

use crate::event_stream::AssistantMessageEventStream;
use crate::http::client::HttpClient;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, CacheRetention, Content, Context, Model,
    SimpleStreamOptions, StopReason, StreamOptions, ThinkingLevel, ToolCall, Usage, UsageCost,
};
use crate::utils::headers::provider_headers_to_record;
use crate::utils::json::{parse_json_with_repair, parse_streaming_json};
use crate::utils::provider_env::get_provider_env_value;

#[derive(Clone, Debug, Default)]
pub struct PiMessagesOptions {
    pub stream: StreamOptions,
    pub reasoning: Option<ThinkingLevel>,
    pub tool_choice: Option<Value>,
    /// Ask the backend for debug metadata (e.g. routing response headers).
    pub debug: Option<bool>,
}

/// Impact summary of a server-side message rewrite (e.g. a gateway policy).
#[derive(Clone, Debug, PartialEq)]
pub struct PiMessagesRewriteImpact {
    pub policy_id: String,
    pub policy_version: f64,
    pub changed: bool,
    pub token_count_change: f64,
    pub message_count_change: f64,
    pub system_prompt_changed: bool,
}

/// Serialized assistant-message event as sent by a pi-messages backend.
#[derive(Clone, Debug, PartialEq)]
pub enum PiMessagesEvent {
    Start,
    TextStart { content_index: f64 },
    TextDelta { content_index: f64, delta: String },
    TextEnd { content_index: f64, content: String, content_signature: Option<String> },
    ThinkingStart { content_index: f64 },
    ThinkingDelta { content_index: f64, delta: String },
    ThinkingEnd {
        content_index: f64,
        content: String,
        content_signature: Option<String>,
        redacted: Option<bool>,
    },
    ToolCallStart { content_index: f64, id: String, tool_name: String },
    ToolCallDelta { content_index: f64, delta: String },
    ToolCallEnd { content_index: f64, tool_call: ToolCall },
    Done {
        reason: String,
        usage: Usage,
        response_id: Option<String>,
        rewrite: Option<PiMessagesRewriteImpact>,
    },
    Error {
        reason: String,
        usage: Usage,
        error_message: Option<String>,
        response_id: Option<String>,
        rewrite: Option<PiMessagesRewriteImpact>,
    },
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

fn get_obj<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a [(String, Value)]> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_map())
}

fn get_bool(entries: &[(String, Value)], key: &str) -> Option<bool> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_bool())
}

fn parse_usage(value: &Value) -> Usage {
    let Some(entries) = value.as_map() else {
        return empty_usage();
    };
    let cost = get_obj(entries, "cost").map(|cost| {
        let input = get_num(cost, "input").unwrap_or(0.0);
        let output = get_num(cost, "output").unwrap_or(0.0);
        let cache_read = get_num(cost, "cacheRead").unwrap_or(0.0);
        let cache_write = get_num(cost, "cacheWrite").unwrap_or(0.0);
        let total = get_num(cost, "total").unwrap_or(0.0);
        UsageCost {
            input,
            output,
            cache_read,
            cache_write,
            total,
        }
    });
    Usage {
        input: get_num(entries, "input").unwrap_or(0.0),
        output: get_num(entries, "output").unwrap_or(0.0),
        cache_read: get_num(entries, "cacheRead").unwrap_or(0.0),
        cache_write: get_num(entries, "cacheWrite").unwrap_or(0.0),
        cache_write_1h: get_num(entries, "cacheWrite1h"),
        reasoning: get_num(entries, "reasoning"),
        total_tokens: get_num(entries, "totalTokens").unwrap_or(0.0),
        cost: cost.unwrap_or_else(empty_cost),
    }
}

fn empty_cost() -> UsageCost {
    UsageCost {
        input: 0.0,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
        total: 0.0,
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
        cost: empty_cost(),
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as f64
}

fn parse_tool_call(value: &Value) -> ToolCall {
    let entries: Vec<(String, Value)> = value.as_map().map(|e| e.to_vec()).unwrap_or_default();
    let id = get_str(&entries, "id").unwrap_or_default();
    let name = get_str(&entries, "name").unwrap_or_default();
    let arguments = entries
        .iter()
        .find(|(k, _)| k == "arguments")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| Value::Map(Vec::new()));
    ToolCall {
        id,
        name,
        arguments,
        thought_signature: None,
        namespace: None,
    }
}

fn parse_rewrite(value: &Value) -> Option<PiMessagesRewriteImpact> {
    let entries = value.as_map()?;
    Some(PiMessagesRewriteImpact {
        policy_id: get_str(entries, "policyId")?,
        policy_version: get_num(entries, "policyVersion").unwrap_or(0.0),
        changed: get_bool(entries, "changed").unwrap_or(false),
        token_count_change: get_num(entries, "tokenCountChange").unwrap_or(0.0),
        message_count_change: get_num(entries, "messageCountChange").unwrap_or(0.0),
        system_prompt_changed: get_bool(entries, "systemPromptChanged").unwrap_or(false),
    })
}

/// Parses a serialized pi-messages event JSON payload.
pub fn parse_pi_messages_event(data: &str) -> Option<PiMessagesEvent> {
    let value: Value = parse_json_with_repair(data).ok()?;
    let entries = value.as_map()?;
    let type_ = get_str(entries, "type")?;
    let content_index = get_num(entries, "contentIndex").unwrap_or(0.0);
    let delta = get_str(entries, "delta").unwrap_or_default();

    Some(match type_.as_str() {
        "start" => PiMessagesEvent::Start,
        "text_start" => PiMessagesEvent::TextStart { content_index },
        "text_delta" => PiMessagesEvent::TextDelta { content_index, delta },
        "text_end" => PiMessagesEvent::TextEnd {
            content_index,
            content: get_str(entries, "content").unwrap_or_default(),
            content_signature: get_str(entries, "contentSignature"),
        },
        "thinking_start" => PiMessagesEvent::ThinkingStart { content_index },
        "thinking_delta" => PiMessagesEvent::ThinkingDelta { content_index, delta },
        "thinking_end" => PiMessagesEvent::ThinkingEnd {
            content_index,
            content: get_str(entries, "content").unwrap_or_default(),
            content_signature: get_str(entries, "contentSignature"),
            redacted: get_bool(entries, "redacted"),
        },
        "toolcall_start" => PiMessagesEvent::ToolCallStart {
            content_index,
            id: get_str(entries, "id").unwrap_or_default(),
            tool_name: get_str(entries, "toolName").unwrap_or_default(),
        },
        "toolcall_delta" => PiMessagesEvent::ToolCallDelta { content_index, delta },
        "toolcall_end" => PiMessagesEvent::ToolCallEnd {
            content_index,
            tool_call: get_obj(entries, "toolCall")
                .map(|tool_call| parse_tool_call(&Value::Map(tool_call.to_vec())))
                .unwrap_or_else(|| ToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: Value::Map(Vec::new()),
                    thought_signature: None,
                    namespace: None,
                }),
        },
        "done" => PiMessagesEvent::Done {
            reason: get_str(entries, "reason").unwrap_or_default(),
            usage: get_obj(entries, "usage")
                .map(|usage| parse_usage(&Value::Map(usage.to_vec())))
                .unwrap_or_else(empty_usage),
            response_id: get_str(entries, "responseId"),
            rewrite: get_obj(entries, "rewrite")
                .and_then(|rewrite| parse_rewrite(&Value::Map(rewrite.to_vec()))),
        },
        "error" => PiMessagesEvent::Error {
            reason: get_str(entries, "reason").unwrap_or_default(),
            usage: get_obj(entries, "usage")
                .map(|usage| parse_usage(&Value::Map(usage.to_vec())))
                .unwrap_or_else(empty_usage),
            error_message: get_str(entries, "errorMessage"),
            response_id: get_str(entries, "responseId"),
            rewrite: get_obj(entries, "rewrite")
                .and_then(|rewrite| parse_rewrite(&Value::Map(rewrite.to_vec()))),
        },
        _ => return None,
    })
}

/// Mirrors `readPiMessagesEvents`: accumulates the byte stream, normalizes
/// CRLF, splits on blank lines, extracts the `data:` line, and parses each
/// event (skipping `[DONE]`). A trailing unterminated block is parsed once
/// at end of stream.
fn read_pi_messages_events(reader: impl std::io::Read) -> Vec<PiMessagesEvent> {
    let mut events = Vec::new();
    let mut buffer = String::new();
    let mut reader = reader;
    let mut chunk = [0u8; 8192];
    let push_blocks = |buffer: &mut String, events: &mut Vec<PiMessagesEvent>| {
        *buffer = buffer.replace("\r\n", "\n");
        let mut split = buffer.find("\n\n");
        while let Some(pos) = split {
            let block = buffer[..pos].to_string();
            if let Some(event) = parse_pi_messages_block(&block) {
                events.push(event);
            }
            buffer.drain(..pos + 2);
            split = buffer.find("\n\n");
        }
    };
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buffer.push_str(&String::from_utf8_lossy(&chunk[..n]));
                push_blocks(&mut buffer, &mut events);
            }
            Err(_) => break,
        }
    }
    // Flush: parse a trailing unterminated block if non-blank.
    if !buffer.trim().is_empty() {
        if let Some(event) = parse_pi_messages_block(&buffer) {
            events.push(event);
        }
    }
    events
}

fn parse_pi_messages_block(raw: &str) -> Option<PiMessagesEvent> {
    let data = raw
        .split('\n')
        .find(|line| line.starts_with("data:"))
        .map(|line| line[5..].trim().to_string());
    let Some(data) = data else {
        return None;
    };
    if data == "[DONE]" {
        return None;
    }
    parse_pi_messages_event(&data)
}

/// Converts pi-messages wire events into assistant message stream events,
/// mirroring `createEventConverter`.
struct PiMessagesEventConverter {
    partial: AssistantMessage,
    tool_json: std::collections::HashMap<u64, String>,
}

impl PiMessagesEventConverter {
    fn new(model: &Model) -> Self {
        Self {
            partial: AssistantMessage {
                content: vec![],
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                response_model: None,
                response_id: None,
                usage: empty_usage(),
                stop_reason: StopReason::Pending,
                deferred: None,
                error_message: None,
                raw_stop_reason: None,
                end_turn: None,
                timestamp: now_ms(),
            },
            tool_json: std::collections::HashMap::new(),
        }
    }

    fn convert(&mut self, event: &PiMessagesEvent) -> AssistantMessageEvent {
        match event {
            PiMessagesEvent::Done {
                reason,
                usage,
                response_id,
                ..
            } => {
                self.partial.stop_reason = StopReason::parse(reason).unwrap_or(StopReason::Stop);
                self.partial.usage = usage.clone();
                self.partial.response_id = response_id.clone();
                // Rewrite diagnostics are not attached (no diagnostics field
                // on the Rust AssistantMessage yet).
                AssistantMessageEvent::Done {
                    reason: reason.clone(),
                    message: self.partial.clone(),
                }
            }
            PiMessagesEvent::Error {
                reason,
                usage,
                error_message,
                response_id,
                ..
            } => {
                self.partial.stop_reason = StopReason::parse(reason).unwrap_or(StopReason::Error);
                self.partial.usage = usage.clone();
                self.partial.error_message = error_message.clone();
                self.partial.response_id = response_id.clone();
                AssistantMessageEvent::Error {
                    reason: reason.clone(),
                    error: self.partial.clone(),
                }
            }
            PiMessagesEvent::Start => AssistantMessageEvent::Start {
                partial: self.partial.clone(),
            },
            PiMessagesEvent::TextStart { content_index } => {
                set_content(&mut self.partial, *content_index, Content::Text(crate::types::TextContent {
                    text: String::new(),
                    text_signature: None,
                }));
                AssistantMessageEvent::TextStart {
                    content_index: *content_index,
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::TextDelta { content_index, delta } => {
                if let Some(Content::Text(text)) = content_at_mut(&mut self.partial, *content_index) {
                    text.text.push_str(delta);
                }
                AssistantMessageEvent::TextDelta {
                    content_index: *content_index,
                    delta: delta.clone(),
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::TextEnd {
                content_index,
                content,
                content_signature,
            } => {
                if let Some(Content::Text(text)) = content_at_mut(&mut self.partial, *content_index) {
                    text.text = content.clone();
                    text.text_signature = content_signature.clone();
                }
                AssistantMessageEvent::TextEnd {
                    content_index: *content_index,
                    content: content.clone(),
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::ThinkingStart { content_index } => {
                set_content(
                    &mut self.partial,
                    *content_index,
                    Content::Thinking(crate::types::ThinkingContent {
                        thinking: String::new(),
                        thinking_signature: None,
                        redacted: None,
                    }),
                );
                AssistantMessageEvent::ThinkingStart {
                    content_index: *content_index,
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::ThinkingDelta { content_index, delta } => {
                if let Some(Content::Thinking(thinking)) = content_at_mut(&mut self.partial, *content_index) {
                    thinking.thinking.push_str(delta);
                }
                AssistantMessageEvent::ThinkingDelta {
                    content_index: *content_index,
                    delta: delta.clone(),
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::ThinkingEnd {
                content_index,
                content,
                content_signature,
                redacted,
            } => {
                if let Some(Content::Thinking(thinking)) = content_at_mut(&mut self.partial, *content_index) {
                    thinking.thinking = content.clone();
                    thinking.thinking_signature = content_signature.clone();
                    thinking.redacted = *redacted;
                }
                AssistantMessageEvent::ThinkingEnd {
                    content_index: *content_index,
                    content: content.clone(),
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::ToolCallStart {
                content_index,
                id,
                tool_name,
            } => {
                set_content(
                    &mut self.partial,
                    *content_index,
                    Content::ToolCall(ToolCall {
                        id: id.clone(),
                        name: tool_name.clone(),
                        arguments: Value::Map(Vec::new()),
                        thought_signature: None,
                        namespace: None,
                    }),
                );
                self.tool_json.insert(*content_index as u64, String::new());
                AssistantMessageEvent::ToolCallStart {
                    content_index: *content_index,
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::ToolCallDelta { content_index, delta } => {
                let json = format!(
                    "{}{}",
                    self.tool_json.get(&(*content_index as u64)).map(String::as_str).unwrap_or(""),
                    delta
                );
                self.tool_json.insert(*content_index as u64, json.clone());
                if let Some(Content::ToolCall(tool_call)) = content_at_mut(&mut self.partial, *content_index) {
                    tool_call.arguments = parse_streaming_json(Some(&json));
                }
                AssistantMessageEvent::ToolCallDelta {
                    content_index: *content_index,
                    delta: delta.clone(),
                    partial: self.partial.clone(),
                }
            }
            PiMessagesEvent::ToolCallEnd { content_index, tool_call } => {
                if let Some(Content::ToolCall(slot)) = content_at_mut(&mut self.partial, *content_index) {
                    // The terminal toolCall is authoritative.
                    slot.id = tool_call.id.clone();
                    slot.name = tool_call.name.clone();
                    slot.arguments = tool_call.arguments.clone();
                    slot.thought_signature = tool_call.thought_signature.clone();
                    slot.namespace = tool_call.namespace.clone();
                }
                self.tool_json.remove(&(*content_index as u64));
                AssistantMessageEvent::ToolCallEnd {
                    content_index: *content_index,
                    tool_call: tool_call.clone(),
                    partial: self.partial.clone(),
                }
            }
        }
    }
}

fn content_at_mut(message: &mut AssistantMessage, content_index: f64) -> Option<&mut Content> {
    message.content.get_mut(content_index as usize)
}

fn set_content(message: &mut AssistantMessage, content_index: f64, content: Content) {
    let index = content_index as usize;
    if message.content.len() <= index {
        message.content.resize(index + 1, Content::Text(crate::types::TextContent {
            text: String::new(),
            text_signature: None,
        }));
    }
    message.content[index] = content;
}

fn create_error_event(model: &Model, error: String) -> AssistantMessageEvent {
    AssistantMessageEvent::Error {
        reason: "error".to_string(),
        error: AssistantMessage {
            content: vec![],
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_model: None,
            response_id: None,
            usage: empty_usage(),
            stop_reason: StopReason::Error,
            deferred: None,
            error_message: Some(error),
            raw_stop_reason: None,
            end_turn: None,
            timestamp: now_ms(),
        },
    }
}

/// Resolve cache retention: backend defaults apply when unset; only the
/// legacy env opt-in is mapped.
fn resolve_cache_retention(
    cache_retention: Option<&CacheRetention>,
    env: Option<&crate::types::ProviderEnv>,
) -> Option<CacheRetention> {
    if let Some(cache_retention) = cache_retention {
        return Some(cache_retention.clone());
    }
    if get_provider_env_value("PI_CACHE_RETENTION", env).as_deref() == Some("long") {
        Some("long".to_string())
    } else {
        None
    }
}

/// Mirrors `formatPiMessagesResponseError` (status text omitted; see module
/// docs).
fn format_pi_messages_response_error(status: u16, body: &str) -> String {
    // Attempt to extract `error.message` / `error.code` from the body.
    if let Ok(parsed) = parse_json_with_repair::<Value>(body) {
        if let Some(entries) = parsed.as_map() {
            if let Some(error) = get_obj(entries, "error") {
                let message = get_str(error, "message");
                let code = get_str(error, "code");
                if let Some(message) = message {
                    let code_suffix = code.map(|c| format!(" ({c})")).unwrap_or_default();
                    return format!("{status}: {message}{code_suffix}");
                }
            }
        }
    }
    format!("{status}: {body}")
}

fn build_payload(
    model: &Model,
    context: &Context,
    options: Option<&PiMessagesOptions>,
) -> Value {
    let options_value = Value::Map(vec![
        (
            "temperature".to_string(),
            options
                .and_then(|o| o.stream.temperature)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        ),
        (
            "maxTokens".to_string(),
            options
                .and_then(|o| o.stream.max_tokens)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        ),
        (
            "reasoning".to_string(),
            options
                .and_then(|o| o.reasoning.clone())
                .map(Value::String)
                .unwrap_or(Value::Null),
        ),
        (
            "cacheRetention".to_string(),
            match resolve_cache_retention(
                options.and_then(|o| o.stream.cache_retention.as_ref()),
                options.and_then(|o| o.stream.request.env.as_ref()),
            ) {
                Some(retention) => Value::String(retention),
                None => Value::Null,
            },
        ),
        (
            "sessionId".to_string(),
            options
                .and_then(|o| o.stream.session_id.clone())
                .map(Value::String)
                .unwrap_or(Value::Null),
        ),
        (
            "toolChoice".to_string(),
            options
                .and_then(|o| o.tool_choice.clone())
                .unwrap_or(Value::Null),
        ),
    ]);
    Value::Map(vec![
        ("model".to_string(), Value::String(model.id.clone())),
        ("context".to_string(), context_to_value(context)),
        ("options".to_string(), options_value),
    ])
}

/// Serializes the context like `JSON.stringify(context)` (system prompt,
/// messages, tools).
fn context_to_value(context: &Context) -> Value {
    let mut entries: Vec<(String, Value)> = Vec::new();
    if let Some(system_prompt) = &context.system_prompt {
        entries.push(("systemPrompt".to_string(), Value::String(system_prompt.clone())));
    }
    entries.push((
        "messages".to_string(),
        Value::Array(context.messages.iter().map(message_to_value).collect()),
    ));
    if let Some(tools) = &context.tools {
        entries.push((
            "tools".to_string(),
            Value::Array(tools.iter().map(|tool| tool.to_value()).collect()),
        ));
    }
    Value::Map(entries)
}

fn message_to_value(message: &crate::types::Message) -> Value {
    match message {
        crate::types::Message::User(user) => {
            let content = match &user.content {
                crate::types::UserMessageContent::Text(text) => Value::String(text.clone()),
                crate::types::UserMessageContent::Blocks(blocks) => {
                    Value::Array(blocks.iter().map(content_to_value).collect())
                }
            };
            Value::Map(vec![
                ("role".to_string(), Value::String("user".to_string())),
                ("content".to_string(), content),
                ("timestamp".to_string(), Value::Number(user.timestamp)),
            ])
        }
        crate::types::Message::Assistant(assistant) => Value::Map(vec![
            ("role".to_string(), Value::String("assistant".to_string())),
            (
                "content".to_string(),
                Value::Array(assistant.content.iter().map(content_to_value).collect()),
            ),
            ("api".to_string(), Value::String(assistant.api.clone())),
            ("provider".to_string(), Value::String(assistant.provider.clone())),
            ("model".to_string(), Value::String(assistant.model.clone())),
            ("usage".to_string(), usage_to_value(&assistant.usage)),
            ("stopReason".to_string(), Value::String(assistant.stop_reason.as_str().to_string())),
            ("timestamp".to_string(), Value::Number(assistant.timestamp)),
        ]),
        crate::types::Message::ToolResult(tool) => Value::Map(vec![
            ("role".to_string(), Value::String("toolResult".to_string())),
            ("toolCallId".to_string(), Value::String(tool.tool_call_id.clone())),
            ("toolName".to_string(), Value::String(tool.tool_name.clone())),
            (
                "content".to_string(),
                Value::Array(tool.content.iter().map(content_to_value).collect()),
            ),
            ("isError".to_string(), Value::Bool(tool.is_error)),
            ("timestamp".to_string(), Value::Number(tool.timestamp)),
        ]),
    }
}

fn content_to_value(content: &Content) -> Value {
    match content {
        Content::Text(text) => {
            let mut entries = vec![
                ("type".to_string(), Value::String("text".to_string())),
                ("text".to_string(), Value::String(text.text.clone())),
            ];
            if let Some(signature) = &text.text_signature {
                entries.push(("textSignature".to_string(), Value::String(signature.clone())));
            }
            Value::Map(entries)
        }
        Content::Thinking(thinking) => {
            let mut entries = vec![
                ("type".to_string(), Value::String("thinking".to_string())),
                ("thinking".to_string(), Value::String(thinking.thinking.clone())),
            ];
            if let Some(signature) = &thinking.thinking_signature {
                entries.push(("thinkingSignature".to_string(), Value::String(signature.clone())));
            }
            if let Some(redacted) = thinking.redacted {
                entries.push(("redacted".to_string(), Value::Bool(redacted)));
            }
            Value::Map(entries)
        }
        Content::Image(image) => Value::Map(vec![
            ("type".to_string(), Value::String("image".to_string())),
            ("data".to_string(), Value::String(image.data.clone())),
            ("mimeType".to_string(), Value::String(image.mime_type.clone())),
        ]),
        Content::ToolCall(tool_call) => Value::Map(vec![
            ("type".to_string(), Value::String("toolCall".to_string())),
            ("id".to_string(), Value::String(tool_call.id.clone())),
            ("name".to_string(), Value::String(tool_call.name.clone())),
            ("arguments".to_string(), tool_call.arguments.clone()),
        ]),
    }
}

fn usage_to_value(usage: &Usage) -> Value {
    Value::Map(vec![
        ("input".to_string(), Value::Number(usage.input)),
        ("output".to_string(), Value::Number(usage.output)),
        ("cacheRead".to_string(), Value::Number(usage.cache_read)),
        ("cacheWrite".to_string(), Value::Number(usage.cache_write)),
        ("totalTokens".to_string(), Value::Number(usage.total_tokens)),
        (
            "cost".to_string(),
            Value::Map(vec![
                ("input".to_string(), Value::Number(usage.cost.input)),
                ("output".to_string(), Value::Number(usage.cost.output)),
                ("cacheRead".to_string(), Value::Number(usage.cost.cache_read)),
                ("cacheWrite".to_string(), Value::Number(usage.cost.cache_write)),
                ("total".to_string(), Value::Number(usage.cost.total)),
            ]),
        ),
    ])
}

/// Stream function for the pi-messages API. Spawns a worker thread that
/// performs the request and feeds the returned stream (mirroring the JS
/// async IIFE).
pub fn stream(
    model: &Model,
    context: &Context,
    options: Option<&PiMessagesOptions>,
    api_key: Option<&str>,
    client: &HttpClient,
) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let worker_stream = stream.clone();
    let model = model.clone();
    let context = context.clone();
    let options = options.cloned();
    let api_key = api_key.map(|s| s.to_string());
    let client = client.clone();

    std::thread::spawn(move || {
        let stream = worker_stream;
        let mut converter = PiMessagesEventConverter::new(&model);
        let result = (|| -> Result<(), String> {
            let api_key = api_key.ok_or_else(|| format!("No API key provided for provider \"{}\"", model.provider))?;

            let base_url = model.base_url.trim_end_matches('/');
            let mut url = format!("{base_url}/messages");
            if options.as_ref().and_then(|o| o.debug).unwrap_or(false) {
                url.push_str("?debug=1");
            }

            let payload = build_payload(&model, &context, options.as_ref());

            let mut headers: Vec<(String, String)> = vec![
                ("authorization".to_string(), format!("Bearer {api_key}")),
                ("accept".to_string(), "text/event-stream".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ];
            if let Some(options_headers) = options.as_ref().and_then(|o| o.stream.request.headers.as_ref()) {
                if let Some(record) = provider_headers_to_record(Some(options_headers)) {
                    for (key, value) in record {
                        if let Some(existing) = headers.iter_mut().find(|(k, _)| k == &key) {
                            existing.1 = value;
                        } else {
                            headers.push((key, value));
                        }
                    }
                }
            }

            let response = client
                .post_json(
                    &url,
                    &headers,
                    &payload,
                    options.as_ref().and_then(|o| o.stream.request.timeout_ms),
                )
                .map_err(|error| match error.status {
                    Some(status) => format_pi_messages_response_error(status, &error.message),
                    None => error.message,
                })?;

            stream.push(AssistantMessageEvent::Start {
                partial: converter.partial.clone(),
            });

            let events = crate::api::pi_messages::read_pi_messages_events(response.reader);
            let mut saw_terminal = false;
            for pi_event in &events {
                let event = converter.convert(pi_event);
                let is_terminal = matches!(
                    event,
                    AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
                );
                stream.push(event);
                if is_terminal {
                    saw_terminal = true;
                    break;
                }
            }

            if !saw_terminal {
                return Err(format!("{} stream ended without a terminal event", model.provider));
            }
            Ok(())
        })();

        if let Err(error) = result {
            stream.push(create_error_event(&model, error));
        }
        stream.end(None);
    });

    stream
}

/// Simple-stream variant: forwards reasoning/toolChoice/debug and delegates
/// to `stream`.
pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<&SimpleStreamOptions>,
    api_key: Option<&str>,
    client: &HttpClient,
) -> AssistantMessageEventStream {
    let extra = options.and_then(|o| o.stream.sampling_params.as_ref());
    let pi_options = PiMessagesOptions {
        stream: options.cloned().map(|o| o.stream).unwrap_or_default(),
        reasoning: options.and_then(|o| o.reasoning.clone()),
        tool_choice: extra.and_then(|params| {
            params
                .iter()
                .find(|(key, _)| key == "toolChoice")
                .map(|(_, value)| value.clone())
        }),
        debug: extra.and_then(|params| {
            params
                .iter()
                .find(|(key, _)| key == "debug")
                .and_then(|(_, value)| value.as_bool())
        }),
    };
    stream(model, context, Some(&pi_options), api_key, client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pi_message_blocks() {
        let block = "event: text_delta\ndata: {\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"hi\"}\n";
        let event = parse_pi_messages_block(block).unwrap();
        assert_eq!(
            event,
            PiMessagesEvent::TextDelta {
                content_index: 0.0,
                delta: "hi".to_string()
            }
        );
    }

    #[test]
    fn skips_done_marker() {
        let block = "data: [DONE]\n";
        assert!(parse_pi_messages_block(block).is_none());
    }

    #[test]
    fn reads_events_across_chunks() {
        let wire = "data: {\"type\":\"start\"}\n\ndata: {\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"a\"}\n\ndata: [DONE]\n\n";
        let events = read_pi_messages_events(wire.as_bytes());
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], PiMessagesEvent::Start));
        assert!(matches!(events[1], PiMessagesEvent::TextDelta { .. }));
    }

    #[test]
    fn parses_done_with_usage() {
        let data = r#"{"type":"done","reason":"stop","usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":3,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}"#;
        let event = parse_pi_messages_event(data).unwrap();
        match event {
            PiMessagesEvent::Done { reason, usage, .. } => {
                assert_eq!(reason, "stop");
                assert_eq!(usage.input, 1.0);
                assert_eq!(usage.total_tokens, 3.0);
            }
            other => panic!("expected done, got {other:?}"),
        }
    }

    #[test]
    fn converter_applies_events_in_order() {
        let model = crate::types::Model {
            id: "m".to_string(),
            name: "m".to_string(),
            api: "pi-messages".to_string(),
            provider: "p".to_string(),
            base_url: "https://x".to_string(),
            reasoning: false,
            thinking_level_map: None,
            input: vec!["text".to_string()],
            cost: crate::types::ModelCost {
                rates: crate::types::ModelCostRates {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                tiers: None,
            },
            context_window: 1000.0,
            max_tokens: 100.0,
            sampling_params: None,
            headers: None,
            compat: None,
        };
        let mut converter = PiMessagesEventConverter::new(&model);
        let events = vec![
            PiMessagesEvent::TextStart { content_index: 0.0 },
            PiMessagesEvent::TextDelta {
                content_index: 0.0,
                delta: "Hel".to_string(),
            },
            PiMessagesEvent::TextDelta {
                content_index: 0.0,
                delta: "lo".to_string(),
            },
            PiMessagesEvent::Done {
                reason: "stop".to_string(),
                usage: empty_usage(),
                response_id: Some("resp".to_string()),
                rewrite: None,
            },
        ];
        let mut converted = Vec::new();
        for event in &events {
            converted.push(converter.convert(event));
        }
        assert!(matches!(converted[0], AssistantMessageEvent::TextStart { .. }));
        let last = converted.last().unwrap();
        match last {
            AssistantMessageEvent::Done { message, .. } => {
                assert_eq!(message.content.len(), 1);
                assert!(matches!(
                    &message.content[0],
                    Content::Text(text) if text.text == "Hello"
                ));
                assert_eq!(message.response_id.as_deref(), Some("resp"));
                assert_eq!(message.stop_reason, StopReason::Stop);
            }
            other => panic!("expected done, got {other:?}"),
        }
    }
}
