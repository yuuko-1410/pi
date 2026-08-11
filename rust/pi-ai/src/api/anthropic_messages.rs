//! Anthropic Messages API provider, port of
//! `packages/ai/src/api/anthropic-messages.ts`.
//!
//! Request assembly (system prompt, messages, tools, thinking modes, cache
//! control), custom SSE decoding with raw-line retention (for parse-error
//! messages), streaming event processing (thinking/redacted_thinking/tool_use
//! blocks, usage incl. ephemeral_1h split and thinking tokens), stop-reason
//! mapping, and Claude Code identity headers for OAuth tokens.

use pi_protocol::Value;

use crate::api::github_copilot_headers::{
    build_copilot_dynamic_headers, has_copilot_vision_input, CopilotDynamicHeadersParams,
};
use crate::api::simple_options::{adjust_max_tokens_for_thinking, build_base_options, clamp_max_tokens_to_context};
use crate::api::transform_messages::transform_messages;
use crate::event_stream::AssistantMessageEventStream;
use crate::http::client::HttpClient;
use crate::models::calculate_cost;
use crate::types::{
    AnthropicMessagesCompat, AssistantMessage, CacheRetention, Content, Context, Model, ProviderEnv,
    ProviderHeaders, SimpleStreamOptions, StopReason, StreamOptions, Tool, Usage, UsageCost,
};
use crate::utils::deferred_tools::split_deferred_tools;
use crate::utils::error_body::{format_provider_error, NormalizedProviderError};
use crate::utils::json::{parse_json_with_repair, parse_streaming_json};
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{
    retry_provider_request, ProviderError, ProviderRetryFailure, ProviderRetryOptions,
};
use crate::utils::sanitize::sanitize_surrogates;

const FINE_GRAINED_TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";
const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";
const ANTHROPIC_VERSION: &str = "2023-06-01";
// Stealth mode: mimic Claude Code's tool naming exactly.
const CLAUDE_CODE_VERSION: &str = "2.1.75";

// Claude Code 2.x tool names (canonical casing).
const CLAUDE_CODE_TOOLS: [&str; 17] = [
    "Read",
    "Write",
    "Edit",
    "Bash",
    "Grep",
    "Glob",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
    "KillShell",
    "NotebookEdit",
    "Skill",
    "Task",
    "TaskOutput",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
];

const ANTHROPIC_MESSAGE_EVENTS: [&str; 6] = [
    "message_start",
    "message_delta",
    "message_stop",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
];

/// Convert tool name to CC canonical casing if it matches (case-insensitive).
fn to_claude_code_name(name: &str) -> String {
    let lower = name.to_lowercase();
    CLAUDE_CODE_TOOLS
        .iter()
        .find(|candidate| candidate.to_lowercase() == lower)
        .map(|candidate| candidate.to_string())
        .unwrap_or_else(|| name.to_string())
}

fn from_claude_code_name(name: &str, tools: Option<&[Tool]>) -> String {
    if let Some(tools) = tools {
        if !tools.is_empty() {
            let lower = name.to_lowercase();
            if let Some(matched) = tools.iter().find(|tool| tool.name.to_lowercase() == lower) {
                return matched.name.clone();
            }
        }
    }
    name.to_string()
}

/// Resolve cache retention preference. Defaults to "short" and uses
/// PI_CACHE_RETENTION for backward compatibility.
fn resolve_cache_retention(cache_retention: Option<&CacheRetention>, env: Option<&ProviderEnv>) -> CacheRetention {
    if let Some(cache_retention) = cache_retention {
        return cache_retention.clone();
    }
    if get_provider_env_value("PI_CACHE_RETENTION", env).as_deref() == Some("long") {
        return "long".to_string();
    }
    "short".to_string()
}

#[derive(Clone, Debug)]
pub struct CacheControl {
    pub ttl: Option<String>,
}

impl CacheControl {
    fn to_value(&self) -> Value {
        let mut entries = vec![("type".to_string(), Value::String("ephemeral".to_string()))];
        if let Some(ttl) = &self.ttl {
            entries.push(("ttl".to_string(), Value::String(ttl.clone())));
        }
        Value::Map(entries)
    }
}

fn get_anthropic_compat(model: &Model) -> RequiredAnthropicCompat {
    RequiredAnthropicCompat::new(model)
}

fn get_cache_control(
    model: &Model,
    cache_retention: Option<&CacheRetention>,
    env: Option<&ProviderEnv>,
) -> (CacheRetention, Option<CacheControl>) {
    let retention = resolve_cache_retention(cache_retention, env);
    if retention == "none" {
        return (retention, None);
    }
    let ttl = if retention == "long" && get_anthropic_compat(model).supports_long_cache_retention {
        Some("1h".to_string())
    } else {
        None
    };
    (retention, Some(CacheControl { ttl }))
}

#[derive(Clone, Debug)]
pub struct RequiredAnthropicCompat {
    pub supports_eager_tool_input_streaming: bool,
    pub supports_long_cache_retention: bool,
    pub send_session_affinity_headers: bool,
    pub supports_cache_control_on_tools: bool,
    pub supports_temperature: bool,
    pub allow_empty_signature: bool,
    pub supports_strict_tools: bool,
    pub supports_tool_references: bool,
}

impl RequiredAnthropicCompat {
    pub fn new(model: &Model) -> Self {
        let compat = match &model.compat {
            Some(crate::types::ModelCompat::AnthropicMessages(compat)) => compat.clone(),
            _ => AnthropicMessagesCompat::default(),
        };
        Self {
            supports_eager_tool_input_streaming: compat.supports_eager_tool_input_streaming.unwrap_or(true),
            supports_long_cache_retention: compat.supports_long_cache_retention.unwrap_or(true),
            send_session_affinity_headers: compat.send_session_affinity_headers.unwrap_or(false),
            supports_cache_control_on_tools: compat.supports_cache_control_on_tools.unwrap_or(true),
            supports_temperature: compat.supports_temperature.unwrap_or(true),
            allow_empty_signature: compat.allow_empty_signature.unwrap_or(false),
            supports_strict_tools: compat.supports_strict_tools.unwrap_or(false),
            supports_tool_references: compat
                .supports_tool_references
                .unwrap_or_else(|| default_supports_tool_references(model)),
        }
    }
}

/// Default for `supportsToolReferences`: first-party Anthropic models except
/// Haiku and models that predate tool search (Claude 3.x, Opus/Sonnet 4.0,
/// Opus 4.1).
fn default_supports_tool_references(model: &Model) -> bool {
    if model.provider != "anthropic" || model.id.contains("haiku") {
        return false;
    }
    // /^claude-(opus|sonnet|fable)-(\d+)(?:-(\d+))?(?:-|$)/
    let prefix = match model.id.strip_prefix("claude-") {
        Some(rest) => rest,
        None => return false,
    };
    let family = match prefix.split('-').next() {
        Some("opus") | Some("sonnet") | Some("fable") => prefix.split('-').next().unwrap().to_string(),
        _ => return false,
    };
    let rest = &prefix[family.len() + 1..];
    let mut parts = rest.split('-');
    let major = match parts.next().and_then(|part| part.parse::<u32>().ok()) {
        Some(major) => major,
        None => return false,
    };
    let minor = match parts.next() {
        Some(part) if part.len() < 8 => part.parse::<u32>().unwrap_or(0),
        _ => 0,
    };
    major > 4 || (major == 4 && minor >= 5)
}

pub type AnthropicEffort = String;
pub type AnthropicThinkingDisplay = String;

/// Anthropic Messages-specific stream options.
#[derive(Clone, Debug, Default)]
pub struct AnthropicOptions {
    pub stream: StreamOptions,
    /// Enable extended thinking (adaptive for capable models, budget-based
    /// otherwise). Default: omitted.
    pub thinking_enabled: Option<bool>,
    /// Token budget for extended thinking (older models only). Default: 1024.
    pub thinking_budget_tokens: Option<f64>,
    /// Effort level for adaptive thinking models.
    pub effort: Option<String>,
    /// Thinking display: "summarized" (default) or "omitted".
    pub thinking_display: Option<String>,
    /// Whether to request the interleaved thinking beta header for
    /// non-adaptive thinking models. Default: true.
    pub interleaved_thinking: Option<bool>,
    /// Anthropic tool choice behavior.
    pub tool_choice: Option<AnthropicToolChoice>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AnthropicToolChoice {
    Auto,
    Any,
    None,
    Tool { name: String },
}

impl AnthropicToolChoice {
    fn to_value(&self) -> Value {
        match self {
            Self::Auto => Value::Map(vec![("type".to_string(), Value::String("auto".to_string()))]),
            Self::Any => Value::Map(vec![("type".to_string(), Value::String("any".to_string()))]),
            Self::None => Value::Map(vec![("type".to_string(), Value::String("none".to_string()))]),
            Self::Tool { name } => Value::Map(vec![
                ("type".to_string(), Value::String("tool".to_string())),
                ("name".to_string(), Value::String(name.clone())),
            ]),
        }
    }
}

// ---------------------------------------------------------------------------
// SSE decoding (custom decoder with raw-line retention, mirroring the JS
// implementation in anthropic-messages.ts)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct AnthropicSseEvent {
    pub event: Option<String>,
    pub data: String,
    pub raw: Vec<String>,
}

struct SseDecoderState {
    event: Option<String>,
    data: Vec<String>,
    raw: Vec<String>,
}

fn flush_sse_event(state: &mut SseDecoderState) -> Option<AnthropicSseEvent> {
    if state.event.is_none() && state.data.is_empty() {
        return None;
    }
    let event = AnthropicSseEvent {
        event: state.event.take(),
        data: state.data.join("\n"),
        raw: std::mem::take(&mut state.raw),
    };
    state.data.clear();
    Some(event)
}

fn decode_sse_line(line: &str, state: &mut SseDecoderState) -> Option<AnthropicSseEvent> {
    if line.is_empty() {
        return flush_sse_event(state);
    }
    state.raw.push(line.to_string());
    if line.starts_with(':') {
        return None;
    }
    let (field_name, value) = match line.find(':') {
        Some(delimiter) => {
            let field_name = &line[..delimiter];
            let mut value = &line[delimiter + 1..];
            if value.starts_with(' ') {
                value = &value[1..];
            }
            (field_name, value)
        }
        None => (line, ""),
    };
    match field_name {
        "event" => state.event = Some(value.to_string()),
        "data" => state.data.push(value.to_string()),
        _ => {}
    }
    None
}

/// Incremental SSE decoder for Anthropic responses, retaining raw lines.
pub struct AnthropicSseDecoder {
    buffer: Vec<u8>,
    state: SseDecoderState,
}

impl Default for AnthropicSseDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicSseDecoder {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            state: SseDecoderState {
                event: None,
                data: Vec::new(),
                raw: Vec::new(),
            },
        }
    }

    /// Feeds bytes and returns any complete events.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<AnthropicSseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        loop {
            let Some(line_break) = next_line_break_index(&self.buffer) else {
                break;
            };
            let line: Vec<u8> = self.buffer.drain(..=line_break).collect();
            let line_text = String::from_utf8_lossy(&line);
            let line_text = line_text.trim_end_matches(['\r', '\n']);
            if let Some(event) = decode_sse_line(line_text, &mut self.state) {
                events.push(event);
            }
        }
        events
    }

    /// End of stream: flush any trailing event.
    pub fn end(&mut self) -> Option<AnthropicSseEvent> {
        let trailing = flush_sse_event(&mut self.state);
        self.buffer.clear();
        trailing
    }
}

fn next_line_break_index(buffer: &[u8]) -> Option<usize> {
    buffer.iter().position(|b| *b == b'\n' || *b == b'\r')
}

// ---------------------------------------------------------------------------
// Parsed stream events
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct AnthropicUsage {
    pub input_tokens: f64,
    pub output_tokens: f64,
    pub cache_read_input_tokens: f64,
    pub cache_creation_input_tokens: f64,
    pub ephemeral_1h_input_tokens: f64,
    pub thinking_tokens: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct AnthropicMessageInfo {
    pub id: String,
    pub usage: Option<AnthropicUsage>,
}

#[derive(Clone, Debug)]
pub enum AnthropicContentBlockStart {
    Text { text: String },
    Thinking { thinking: String, signature: String },
    RedactedThinking { data: String },
    ToolUse { id: String, name: String, input: Value },
}

#[derive(Clone, Debug)]
pub enum AnthropicContentBlockDelta {
    TextDelta { text: String },
    ThinkingDelta { thinking: String },
    InputJsonDelta { partial_json: String },
    SignatureDelta { signature: String },
}

#[derive(Clone, Debug)]
pub enum AnthropicStreamEvent {
    MessageStart { message: AnthropicMessageInfo },
    MessageDelta { stop_reason: Option<String>, stop_details: Option<String>, usage: Option<AnthropicUsage> },
    MessageStop,
    ContentBlockStart { index: f64, content_block: AnthropicContentBlockStart },
    ContentBlockDelta { index: f64, delta: AnthropicContentBlockDelta },
    ContentBlockStop { index: f64 },
}

fn get_str<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a str> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_str())
}

fn get_num(entries: &[(String, Value)], key: &str) -> Option<f64> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_number())
}

fn get_obj<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a [(String, Value)]> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_map())
}

fn parse_usage(entries: &[(String, Value)]) -> AnthropicUsage {
    let cache_creation = get_obj(entries, "cache_creation");
    let output_details = get_obj(entries, "output_tokens_details");
    AnthropicUsage {
        input_tokens: get_num(entries, "input_tokens").unwrap_or(0.0),
        output_tokens: get_num(entries, "output_tokens").unwrap_or(0.0),
        cache_read_input_tokens: get_num(entries, "cache_read_input_tokens").unwrap_or(0.0),
        cache_creation_input_tokens: get_num(entries, "cache_creation_input_tokens").unwrap_or(0.0),
        ephemeral_1h_input_tokens: cache_creation
            .and_then(|creation| get_num(creation, "ephemeral_1h_input_tokens"))
            .unwrap_or(0.0),
        thinking_tokens: output_details.and_then(|details| get_num(details, "thinking_tokens")),
    }
}

fn parse_content_block_start(value: &Value) -> Option<AnthropicContentBlockStart> {
    let entries = value.as_map()?;
    let type_ = get_str(entries, "type")?;
    match type_ {
        "text" => Some(AnthropicContentBlockStart::Text {
            text: get_str(entries, "text").unwrap_or("").to_string(),
        }),
        "thinking" => Some(AnthropicContentBlockStart::Thinking {
            thinking: get_str(entries, "thinking").unwrap_or("").to_string(),
            signature: get_str(entries, "signature").unwrap_or("").to_string(),
        }),
        "redacted_thinking" => Some(AnthropicContentBlockStart::RedactedThinking {
            data: get_str(entries, "data").unwrap_or("").to_string(),
        }),
        "tool_use" => Some(AnthropicContentBlockStart::ToolUse {
            id: get_str(entries, "id").unwrap_or("").to_string(),
            name: get_str(entries, "name").unwrap_or("").to_string(),
            input: entries
                .iter()
                .find(|(k, _)| k == "input")
                .map(|(_, v)| v.clone())
                .unwrap_or(Value::Map(Vec::new())),
        }),
        _ => None,
    }
}

fn parse_content_block_delta(value: &Value) -> Option<AnthropicContentBlockDelta> {
    let entries = value.as_map()?;
    let type_ = get_str(entries, "type")?;
    match type_ {
        "text_delta" => Some(AnthropicContentBlockDelta::TextDelta {
            text: get_str(entries, "text").unwrap_or("").to_string(),
        }),
        "thinking_delta" => Some(AnthropicContentBlockDelta::ThinkingDelta {
            thinking: get_str(entries, "thinking").unwrap_or("").to_string(),
        }),
        "input_json_delta" => Some(AnthropicContentBlockDelta::InputJsonDelta {
            partial_json: get_str(entries, "partial_json").unwrap_or("").to_string(),
        }),
        "signature_delta" => Some(AnthropicContentBlockDelta::SignatureDelta {
            signature: get_str(entries, "signature").unwrap_or("").to_string(),
        }),
        _ => None,
    }
}

/// Parses a single SSE event into a typed stream event. Returns Ok(None) for
/// events outside the Anthropic message-event set, Err for parse failures.
pub fn parse_anthropic_stream_event(sse: &AnthropicSseEvent) -> Result<Option<AnthropicStreamEvent>, String> {
    let event_name = sse.event.clone().unwrap_or_default();
    if event_name == "error" {
        return Err(sse.data.clone());
    }
    if !ANTHROPIC_MESSAGE_EVENTS.contains(&event_name.as_str()) {
        return Ok(None);
    }

    let value: Value = parse_json_with_repair(&sse.data)
        .map_err(|error| format_parse_error(sse, &event_name, &error.to_string()))?;
    let entries: Vec<(String, Value)> = value.as_map().map(|e| e.to_vec()).unwrap_or_default();
    let type_ = get_str(&entries, "type").unwrap_or("").to_string();

    let parsed = match type_.as_str() {
        "message_start" => {
            let message = get_obj(&entries, "message").ok_or_else(|| {
                format_parse_error(sse, &event_name, "message_start missing message")
            })?;
            AnthropicStreamEvent::MessageStart {
                message: AnthropicMessageInfo {
                    id: get_str(message, "id").unwrap_or("").to_string(),
                    usage: get_obj(message, "usage").map(parse_usage),
                },
            }
        }
        "message_delta" => {
            let delta = get_obj(&entries, "delta");
            let stop_reason = delta.and_then(|d| get_str(d, "stop_reason")).map(|s| s.to_string());
            let stop_details = delta.and_then(|d| get_str(d, "explanation")).map(|s| s.to_string());
            AnthropicStreamEvent::MessageDelta {
                stop_reason,
                stop_details,
                usage: get_obj(&entries, "usage").map(parse_usage),
            }
        }
        "message_stop" => AnthropicStreamEvent::MessageStop,
        "content_block_start" => {
            let index = get_num(&entries, "index").unwrap_or(0.0);
            let content_block = get_obj(&entries, "content_block")
                .map(|block| Value::Map(block.to_vec()))
                .and_then(|block| parse_content_block_start(&block))
                .ok_or_else(|| format_parse_error(sse, &event_name, "content_block_start missing block"))?;
            AnthropicStreamEvent::ContentBlockStart { index, content_block }
        }
        "content_block_delta" => {
            let index = get_num(&entries, "index").unwrap_or(0.0);
            let delta = get_obj(&entries, "delta")
                .map(|delta| Value::Map(delta.to_vec()))
                .and_then(|delta| parse_content_block_delta(&delta))
                .ok_or_else(|| format_parse_error(sse, &event_name, "content_block_delta missing delta"))?;
            AnthropicStreamEvent::ContentBlockDelta { index, delta }
        }
        "content_block_stop" => AnthropicStreamEvent::ContentBlockStop {
            index: get_num(&entries, "index").unwrap_or(0.0),
        },
        _ => return Ok(None),
    };
    Ok(Some(parsed))
}

fn format_parse_error(sse: &AnthropicSseEvent, event_name: &str, message: &str) -> String {
    format!(
        "Could not parse Anthropic SSE event {event_name}: {message}; data={}; raw={}",
        sse.data,
        sse.raw.join("\\n")
    )
}

// ---------------------------------------------------------------------------
// Request construction
// ---------------------------------------------------------------------------

/// Mirrors `normalizeToolCallId`.
fn normalize_tool_call_id(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .take(64)
        .collect()
}

enum ConvertedContent {
    Text(String),
    Blocks(Vec<Value>),
}

/// Mirrors `convertContentBlocks`.
fn convert_content_blocks(content: &[Content]) -> ConvertedContent {
    let has_images = content.iter().any(|block| matches!(block, Content::Image(_)));
    if !has_images {
        let text = content
            .iter()
            .filter_map(|block| match block {
                Content::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        return ConvertedContent::Text(sanitize_surrogates(&text));
    }

    let mut blocks: Vec<Value> = content
        .iter()
        .map(|block| match block {
            Content::Text(text) => Value::Map(vec![
                ("type".to_string(), Value::String("text".to_string())),
                ("text".to_string(), Value::String(sanitize_surrogates(&text.text))),
            ]),
            Content::Image(image) => Value::Map(vec![
                ("type".to_string(), Value::String("image".to_string())),
                (
                    "source".to_string(),
                    Value::Map(vec![
                        ("type".to_string(), Value::String("base64".to_string())),
                        ("media_type".to_string(), Value::String(image.mime_type.clone())),
                        ("data".to_string(), Value::String(image.data.clone())),
                    ]),
                ),
            ]),
            _ => Value::Map(Vec::new()),
        })
        .collect();

    let has_text = blocks.iter().any(|block| match block {
        Value::Map(entries) => get_str(entries, "type") == Some("text"),
        _ => false,
    });
    if !has_text {
        blocks.insert(
            0,
            Value::Map(vec![
                ("type".to_string(), Value::String("text".to_string())),
                ("text".to_string(), Value::String("(see attached image)".to_string())),
            ]),
        );
    }
    ConvertedContent::Blocks(blocks)
}

/// Mirrors `convertToolResult`: builds the tool_result block and displaced
/// sibling content for tool references.
fn convert_tool_result(
    msg: &crate::types::ToolResultMessage,
    is_oauth_token: bool,
    deferred_tool_names: &std::collections::HashSet<String>,
    loaded_tool_names: &mut std::collections::HashSet<String>,
    normalize_tool_name: &dyn Fn(&str) -> String,
) -> (Value, Vec<Value>) {
    let mut references: Vec<Value> = Vec::new();
    if let Some(added_tool_names) = &msg.added_tool_names {
        for name in added_tool_names {
            let normalized_name = normalize_tool_name(name);
            if !deferred_tool_names.contains(&normalized_name) || loaded_tool_names.contains(&normalized_name) {
                continue;
            }
            loaded_tool_names.insert(normalized_name);
            references.push(Value::Map(vec![
                ("type".to_string(), Value::String("tool_reference".to_string())),
                (
                    "tool_name".to_string(),
                    Value::String(if is_oauth_token {
                        to_claude_code_name(name)
                    } else {
                        name.clone()
                    }),
                ),
            ]));
        }
    }
    let has_references = !references.is_empty();
    let converted_content = convert_content_blocks(&msg.content);
    let tool_result_content = if has_references {
        Value::Array(references)
    } else {
        match &converted_content {
            ConvertedContent::Text(text) => Value::String(text.clone()),
            ConvertedContent::Blocks(blocks) => Value::Array(blocks.clone()),
        }
    };
    let tool_result = Value::Map(vec![
        ("type".to_string(), Value::String("tool_result".to_string())),
        ("tool_use_id".to_string(), Value::String(msg.tool_call_id.clone())),
        ("content".to_string(), tool_result_content),
        ("is_error".to_string(), Value::Bool(msg.is_error)),
    ]);
    let sibling_content = if !has_references {
        Vec::new()
    } else {
        match &converted_content {
            ConvertedContent::Text(text) => vec![Value::Map(vec![
                ("type".to_string(), Value::String("text".to_string())),
                ("text".to_string(), Value::String(text.clone())),
            ])],
            ConvertedContent::Blocks(blocks) => blocks.clone(),
        }
    };
    (tool_result, sibling_content)
}

fn convert_messages(
    transformed_messages: &[crate::types::Message],
    is_oauth_token: bool,
    cache_control: Option<&CacheControl>,
    allow_empty_signature: bool,
    deferred_tool_names: &std::collections::HashSet<String>,
    normalize_tool_name: &dyn Fn(&str) -> String,
) -> Vec<Value> {
    let mut params: Vec<Value> = Vec::new();
    let mut loaded_tool_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut i = 0;
    while i < transformed_messages.len() {
        let msg = &transformed_messages[i];
        match msg {
            crate::types::Message::User(user) => {
                match &user.content {
                    crate::types::UserMessageContent::Text(text) => {
                        if !text.trim().is_empty() {
                            params.push(Value::Map(vec![
                                ("role".to_string(), Value::String("user".to_string())),
                                ("content".to_string(), Value::String(sanitize_surrogates(text))),
                            ]));
                        }
                    }
                    crate::types::UserMessageContent::Blocks(blocks) => {
                        let content_blocks: Vec<Value> = blocks
                            .iter()
                            .filter_map(|item| match item {
                                Content::Text(text) => Some(Value::Map(vec![
                                    ("type".to_string(), Value::String("text".to_string())),
                                    ("text".to_string(), Value::String(sanitize_surrogates(&text.text))),
                                ])),
                                Content::Image(image) => Some(Value::Map(vec![
                                    ("type".to_string(), Value::String("image".to_string())),
                                    (
                                        "source".to_string(),
                                        Value::Map(vec![
                                            ("type".to_string(), Value::String("base64".to_string())),
                                            ("media_type".to_string(), Value::String(image.mime_type.clone())),
                                            ("data".to_string(), Value::String(image.data.clone())),
                                        ]),
                                    ),
                                ])),
                                _ => None,
                            })
                            .filter(|block| match block {
                                Value::Map(entries) => {
                                    if get_str(entries, "type") == Some("text") {
                                        get_str(entries, "text").is_some_and(|text| !text.trim().is_empty())
                                    } else {
                                        true
                                    }
                                }
                                _ => true,
                            })
                            .collect();
                        if content_blocks.is_empty() {
                            i += 1;
                            continue;
                        }
                        params.push(Value::Map(vec![
                            ("role".to_string(), Value::String("user".to_string())),
                            ("content".to_string(), Value::Array(content_blocks)),
                        ]));
                    }
                }
            }
            crate::types::Message::Assistant(assistant) => {
                let mut blocks: Vec<Value> = Vec::new();
                for block in &assistant.content {
                    match block {
                        Content::Text(text) => {
                            if text.text.trim().is_empty() {
                                continue;
                            }
                            blocks.push(Value::Map(vec![
                                ("type".to_string(), Value::String("text".to_string())),
                                ("text".to_string(), Value::String(sanitize_surrogates(&text.text))),
                            ]));
                        }
                        Content::Thinking(thinking) => {
                            if thinking.redacted == Some(true) {
                                // Redacted thinking: pass the opaque payload back.
                                blocks.push(Value::Map(vec![
                                    ("type".to_string(), Value::String("redacted_thinking".to_string())),
                                    ("data".to_string(), Value::String(thinking.thinking_signature.clone().unwrap_or_default())),
                                ]));
                                continue;
                            }
                            let has_signature = thinking
                                .thinking_signature
                                .as_deref()
                                .is_some_and(|signature| !signature.trim().is_empty());
                            if thinking.thinking.trim().is_empty() && !has_signature {
                                continue;
                            }
                            if !has_signature {
                                // Convert to plain text unless the model accepts
                                // empty signatures.
                                blocks.push(if allow_empty_signature {
                                    Value::Map(vec![
                                        ("type".to_string(), Value::String("thinking".to_string())),
                                        ("thinking".to_string(), Value::String(sanitize_surrogates(&thinking.thinking))),
                                        ("signature".to_string(), Value::String(String::new())),
                                    ])
                                } else {
                                    Value::Map(vec![
                                        ("type".to_string(), Value::String("text".to_string())),
                                        ("text".to_string(), Value::String(sanitize_surrogates(&thinking.thinking))),
                                    ])
                                });
                            } else {
                                blocks.push(Value::Map(vec![
                                    ("type".to_string(), Value::String("thinking".to_string())),
                                    ("thinking".to_string(), Value::String(sanitize_surrogates(&thinking.thinking))),
                                    (
                                        "signature".to_string(),
                                        Value::String(thinking.thinking_signature.clone().unwrap_or_default()),
                                    ),
                                ]));
                            }
                        }
                        Content::ToolCall(tool_call) => {
                            blocks.push(Value::Map(vec![
                                ("type".to_string(), Value::String("tool_use".to_string())),
                                ("id".to_string(), Value::String(tool_call.id.clone())),
                                (
                                    "name".to_string(),
                                    Value::String(if is_oauth_token {
                                        to_claude_code_name(&tool_call.name)
                                    } else {
                                        tool_call.name.clone()
                                    }),
                                ),
                                ("input".to_string(), tool_call.arguments.clone()),
                            ]));
                        }
                        Content::Image(_) => {}
                    }
                }
                if blocks.is_empty() {
                    i += 1;
                    continue;
                }
                params.push(Value::Map(vec![
                    ("role".to_string(), Value::String("assistant".to_string())),
                    ("content".to_string(), Value::Array(blocks)),
                ]));
            }
            crate::types::Message::ToolResult(_) => {
                // Collect all consecutive toolResult messages (needed for the
                // z.ai Anthropic endpoint).
                let mut tool_results: Vec<Value> = Vec::new();
                let mut sibling_content: Vec<Value> = Vec::new();
                let mut j = i;
                while j < transformed_messages.len() {
                    if let crate::types::Message::ToolResult(tool_result) = &transformed_messages[j] {
                        let converted = convert_tool_result(
                            tool_result,
                            is_oauth_token,
                            deferred_tool_names,
                            &mut loaded_tool_names,
                            normalize_tool_name,
                        );
                        tool_results.push(converted.0);
                        sibling_content.extend(converted.1);
                        j += 1;
                    } else {
                        break;
                    }
                }
                // Skip the messages already processed.
                i = j - 1;

                // Displaced reference-bearing results follow every tool_result block.
                let mut content = tool_results;
                content.extend(sibling_content);
                params.push(Value::Map(vec![
                    ("role".to_string(), Value::String("user".to_string())),
                    ("content".to_string(), Value::Array(content)),
                ]));
            }
        }
        i += 1;
    }

    // Add cache_control to the last user message to cache conversation history.
    if let Some(cache_control) = cache_control {
        if let Some(last_message) = params.last_mut() {
            if let Value::Map(entries) = last_message {
                if get_str(entries, "role") == Some("user") {
                    if let Some(Value::Array(content)) = entries.iter_mut().find(|(k, _)| k == "content").map(|(_, v)| v) {
                        if let Some(last_block) = content.last_mut() {
                            if let Value::Map(block_entries) = last_block {
                                let block_type = get_str(block_entries, "type").map(|s| s.to_string());
                                if matches!(block_type.as_deref(), Some("text") | Some("image") | Some("tool_result")) {
                                    block_entries.push((
                                        "cache_control".to_string(),
                                        cache_control.to_value(),
                                    ));
                                }
                            }
                        }
                    } else if let Some(Value::String(text)) = entries.iter_mut().find(|(k, _)| k == "content").map(|(_, v)| v) {
                        // String content becomes a block with cache_control.
                        let block = Value::Map(vec![
                            ("type".to_string(), Value::String("text".to_string())),
                            ("text".to_string(), Value::String(text.clone())),
                            ("cache_control".to_string(), cache_control.to_value()),
                        ]);
                        entries.push(("content".to_string(), Value::Array(vec![block])));
                    }
                }
            }
        }
    }

    params
}

fn legacy_input_schema(schema: &crate::types::JsonSchemaObject) -> Value {
    let mut entries = vec![("type".to_string(), Value::String("object".to_string()))];
    let properties = schema
        .properties
        .as_ref()
        .map(|properties| {
            Value::Map(
                properties
                    .iter()
                    .map(|(key, sub)| (key.clone(), sub.to_value()))
                    .collect(),
            )
        })
        .unwrap_or(Value::Map(Vec::new()));
    let required = schema
        .required
        .as_ref()
        .map(|required| {
            Value::Array(required.iter().map(|name| Value::String(name.clone())).collect())
        })
        .unwrap_or(Value::Array(Vec::new()));
    entries.push(("properties".to_string(), properties));
    entries.push(("required".to_string(), required));
    Value::Map(entries)
}

fn input_schema_for_tool(strict: bool, schema: &crate::types::JsonSchemaObject) -> Value {
    let legacy = legacy_input_schema(schema);
    if !strict {
        return legacy;
    }
    // strict === true: spread tool.parameters then override with legacy fields.
    let mut entries = match schema.to_value() {
        Value::Map(entries) => entries,
        _ => Vec::new(),
    };
    if let Value::Map(legacy_entries) = &legacy {
        for (key, value) in legacy_entries {
            if let Some(existing) = entries.iter_mut().find(|(k, _)| k == key) {
                existing.1 = value.clone();
            } else {
                entries.push((key.clone(), value.clone()));
            }
        }
    }
    Value::Map(entries)
}

fn should_use_fine_grained_tool_streaming_beta(model: &Model, context: &Context) -> bool {
    context.tools.as_ref().is_some_and(|tools| !tools.is_empty())
        && !get_anthropic_compat(model).supports_eager_tool_input_streaming
}

fn convert_tools(
    tools: &[Tool],
    is_oauth_token: bool,
    supports_eager_tool_input_streaming: bool,
    supports_strict_tools: bool,
    cache_control: Option<&CacheControl>,
    defer_loading: bool,
) -> Vec<Value> {
    tools
        .iter()
        .enumerate()
        .map(|(index, tool)| {
            let strict = resolve_json_schema_strict_sampling(tool, supports_strict_tools).unwrap_or(None);
            let input_schema = input_schema_for_tool(strict == Some(true), &tool.parameters);

            let mut entries = vec![
                (
                    "name".to_string(),
                    Value::String(if is_oauth_token {
                        to_claude_code_name(&tool.name)
                    } else {
                        tool.name.clone()
                    }),
                ),
                ("description".to_string(), Value::String(tool.description.clone())),
                ("input_schema".to_string(), input_schema),
            ];
            if supports_eager_tool_input_streaming {
                entries.push(("eager_input_streaming".to_string(), Value::Bool(true)));
            }
            if strict == Some(true) {
                entries.push(("strict".to_string(), Value::Bool(true)));
            }
            if defer_loading {
                entries.push(("defer_loading".to_string(), Value::Bool(true)));
            }
            if let Some(cache_control) = cache_control {
                if index == tools.len() - 1 {
                    entries.push(("cache_control".to_string(), cache_control.to_value()));
                }
            }
            Value::Map(entries)
        })
        .collect()
}

fn map_stop_reason(reason: &str, stop_details: Option<&str>) -> (StopReason, Option<String>) {
    match reason {
        "end_turn" => (StopReason::Stop, None),
        "max_tokens" => (StopReason::Length, None),
        "tool_use" => (StopReason::ToolUse, None),
        "refusal" => (
            StopReason::Error,
            Some(
                stop_details
                    .map(|explanation| explanation.to_string())
                    .unwrap_or_else(|| "The model refused to complete the request".to_string()),
            ),
        ),
        "pause_turn" => (StopReason::Stop, None),
        "stop_sequence" => (StopReason::Stop, None),
        "sensitive" => (StopReason::Error, Some("Provider stopped with: sensitive".to_string())),
        other => panic!("Unhandled stop reason: {other}"),
    }
}

/// Maps a ThinkingLevel to Anthropic effort levels for adaptive thinking.
fn map_thinking_level_to_effort(model: &Model, level: Option<&str>) -> String {
    if let Some(level) = level {
        if let Some(mapped) = model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.iter().find(|(key, _)| key == level))
            .and_then(|(_, value)| value.clone())
        {
            return mapped;
        }
        return match level {
            "minimal" | "low" => "low".to_string(),
            "medium" => "medium".to_string(),
            _ => "high".to_string(),
        };
    }
    "high".to_string()
}

fn has_header(headers: Option<&ProviderHeaders>, name: &str) -> bool {
    let Some(headers) = headers else {
        return false;
    };
    headers.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case(name)
            && value.as_deref().is_some_and(|value| !value.trim().is_empty())
    })
}

fn assert_request_auth(provider: &str, api_key: Option<&str>, headers: Option<&ProviderHeaders>) -> Result<(), String> {
    if api_key.is_some_and(|key| !key.is_empty()) {
        return Ok(());
    }
    if has_header(headers, "authorization")
        || has_header(headers, "x-api-key")
        || has_header(headers, "cf-aig-authorization")
    {
        return Ok(());
    }
    Err(format!("No API key for provider: {provider}"))
}

fn is_oauth_token(api_key: Option<&str>) -> bool {
    api_key.is_some_and(|key| key.contains("sk-ant-oat"))
}

/// Builds the request headers for the three auth modes (Copilot Bearer,
/// OAuth Bearer with Claude Code identity, API key).
#[allow(clippy::too_many_arguments)]
fn build_anthropic_headers(
    model: &Model,
    api_key: Option<&str>,
    is_oauth: bool,
    is_copilot: bool,
    beta_features: &[&str],
    dynamic_headers: Option<&[(String, String)]>,
    options_headers: Option<&ProviderHeaders>,
    session_id: Option<&str>,
) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = vec![
        ("accept".to_string(), "application/json".to_string()),
        ("anthropic-dangerous-direct-browser-access".to_string(), "true".to_string()),
        ("anthropic-version".to_string(), ANTHROPIC_VERSION.to_string()),
    ];

    if is_copilot {
        headers.push(("Authorization".to_string(), format!("Bearer {}", api_key.unwrap_or(""))));
    } else if is_oauth {
        headers.push(("Authorization".to_string(), format!("Bearer {}", api_key.unwrap_or(""))));
        let mut beta = vec!["claude-code-20250219".to_string(), "oauth-2025-04-20".to_string()];
        beta.extend(beta_features.iter().map(|feature| feature.to_string()));
        headers.push(("anthropic-beta".to_string(), beta.join(",")));
        headers.push(("user-agent".to_string(), format!("claude-cli/{CLAUDE_CODE_VERSION}")));
        headers.push(("x-app".to_string(), "cli".to_string()));
    } else if !beta_features.is_empty() {
        headers.push((
            "anthropic-beta".to_string(),
            beta_features.join(","),
        ));
        if let Some(api_key) = api_key {
            headers.push(("x-api-key".to_string(), api_key.to_string()));
        }
    } else if let Some(api_key) = api_key {
        headers.push(("x-api-key".to_string(), api_key.to_string()));
    }

    // Session affinity for API-key mode.
    if !is_oauth && !is_copilot {
        if let Some(session_id) = session_id {
            if get_anthropic_compat(model).send_session_affinity_headers {
                headers.push(("x-session-affinity".to_string(), session_id.to_string()));
            }
        }
    }

    // Merge model headers, dynamic headers, then options headers (last wins).
    let model_headers: Vec<(String, String)> = model.headers.clone().unwrap_or_default();
    for (key, value) in model_headers {
        if let Some(existing) = headers.iter_mut().find(|(k, _)| k == &key) {
            existing.1 = value;
        } else {
            headers.push((key, value));
        }
    }
    if let Some(dynamic_headers) = dynamic_headers {
        for (key, value) in dynamic_headers {
            if let Some(existing) = headers.iter_mut().find(|(k, _)| k == key) {
                existing.1 = value.clone();
            } else {
                headers.push((key.clone(), value.clone()));
            }
        }
    }
    if let Some(options_headers) = options_headers {
        for (key, value) in options_headers {
            if let Some(value) = value {
                if let Some(existing) = headers.iter_mut().find(|(k, _)| k == key) {
                    existing.1 = value.clone();
                } else {
                    headers.push((key.clone(), value.clone()));
                }
            }
        }
    }
    headers
}

/// Builds the Messages request body.
fn build_params(
    model: &Model,
    context: &Context,
    is_oauth_token: bool,
    options: Option<&AnthropicOptions>,
) -> Value {
    let (cache_control, cache_control_value) = {
        let (retention, cache_control) = get_cache_control(
            model,
            options.and_then(|o| o.stream.cache_retention.as_ref()),
            options.and_then(|o| o.stream.request.env.as_ref()),
        );
        let _ = retention;
        let value = cache_control.as_ref().map(|control| control.to_value());
        (cache_control, value)
    };
    let compat = get_anthropic_compat(model);
    let transformed_messages = transform_messages(context.messages.clone(), model, Some(&|id, _, _| normalize_tool_call_id(id)));
    let normalize_tool_name: fn(&str) -> String = if is_oauth_token {
        |name: &str| to_claude_code_name(name)
    } else {
        |name: &str| name.to_string()
    };
    let transformed_context = Context {
        messages: transformed_messages.clone(),
        ..context.clone()
    };
    let tool_placement = split_deferred_tools(
        &transformed_context,
        compat.supports_tool_references,
        Some(Box::new(normalize_tool_name)),
    );
    let mut immediate_tools = tool_placement.immediate;
    let mut deferred_tools: Vec<Tool> = tool_placement.deferred.into_iter().map(|(_, tool)| tool).collect();
    if immediate_tools.is_empty() && !deferred_tools.is_empty() {
        immediate_tools = std::mem::take(&mut deferred_tools);
    }
    let deferred_tool_names: std::collections::HashSet<String> = deferred_tools
        .iter()
        .map(|tool| normalize_tool_name(&tool.name))
        .collect();

    let mut entries: Vec<(String, Value)> = vec![
        ("model".to_string(), Value::String(model.id.clone())),
        (
            "messages".to_string(),
            Value::Array(convert_messages(
                &transformed_messages,
                is_oauth_token,
                cache_control.as_ref(),
                compat.allow_empty_signature,
                &deferred_tool_names,
                &normalize_tool_name,
            )),
        ),
        (
            "max_tokens".to_string(),
            Value::Number(options.and_then(|o| o.stream.max_tokens).unwrap_or(model.max_tokens)),
        ),
        ("stream".to_string(), Value::Bool(true)),
    ];

    // System prompt with optional cache control; OAuth requires Claude Code
    // identity.
    let mut system_blocks: Vec<Value> = Vec::new();
    if is_oauth_token {
        let mut identity = vec![
            ("type".to_string(), Value::String("text".to_string())),
            (
                "text".to_string(),
                Value::String("You are Claude Code, Anthropic's official CLI for Claude.".to_string()),
            ),
        ];
        if let Some(value) = &cache_control_value {
            identity.push(("cache_control".to_string(), value.clone()));
        }
        system_blocks.push(Value::Map(identity));
    }
    if let Some(system_prompt) = &context.system_prompt {
        let mut block = vec![
            ("type".to_string(), Value::String("text".to_string())),
            ("text".to_string(), Value::String(sanitize_surrogates(system_prompt))),
        ];
        if let Some(value) = &cache_control_value {
            block.push(("cache_control".to_string(), value.clone()));
        }
        system_blocks.push(Value::Map(block));
    }
    if !system_blocks.is_empty() {
        entries.push(("system".to_string(), Value::Array(system_blocks)));
    }

    // Temperature is incompatible with extended thinking and unsupported on
    // Claude Opus 4.7+.
    if let Some(temperature) = options.and_then(|o| o.stream.temperature) {
        if options.and_then(|o| o.thinking_enabled) != Some(true) && compat.supports_temperature {
            entries.push(("temperature".to_string(), Value::Number(temperature)));
        }
    }

    if !immediate_tools.is_empty() || !deferred_tools.is_empty() {
        let mut tools = convert_tools(
            &immediate_tools,
            is_oauth_token,
            compat.supports_eager_tool_input_streaming,
            compat.supports_strict_tools,
            if compat.supports_cache_control_on_tools {
                cache_control.as_ref()
            } else {
                None
            },
            false,
        );
        tools.extend(convert_tools(
            &deferred_tools,
            is_oauth_token,
            compat.supports_eager_tool_input_streaming,
            compat.supports_strict_tools,
            None,
            true,
        ));
        entries.push(("tools".to_string(), Value::Array(tools)));
    }

    // Configure thinking mode: adaptive, budget-based, or explicitly disabled.
    if model.reasoning {
        if options.and_then(|o| o.thinking_enabled) == Some(true) {
            let display = options
                .and_then(|o| o.thinking_display.clone())
                .unwrap_or_else(|| "summarized".to_string());
            if model.compat.as_ref().is_some_and(|compat| {
                matches!(compat, crate::types::ModelCompat::AnthropicMessages(compat) if compat.force_adaptive_thinking == Some(true))
            }) {
                // Adaptive thinking: Claude decides when and how much to think.
                let thinking = vec![
                    ("type".to_string(), Value::String("adaptive".to_string())),
                    ("display".to_string(), Value::String(display)),
                ];
                entries.push(("thinking".to_string(), Value::Map(thinking)));
                if let Some(effort) = options.and_then(|o| o.effort.clone()) {
                    entries.push((
                        "output_config".to_string(),
                        Value::Map(vec![("effort".to_string(), Value::String(effort))]),
                    ));
                }
            } else {
                // Budget-based thinking for older models.
                let budget = options.and_then(|o| o.thinking_budget_tokens).unwrap_or(1024.0);
                let thinking = Value::Map(vec![
                    ("type".to_string(), Value::String("enabled".to_string())),
                    ("budget_tokens".to_string(), Value::Number(budget)),
                    ("display".to_string(), Value::String(display)),
                ]);
                entries.push(("thinking".to_string(), thinking));
            }
        } else if options.and_then(|o| o.thinking_enabled) == Some(false) {
            let off_is_null = model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.iter().find(|(key, _)| key == "off"))
                .is_some_and(|(_, value)| value.is_none());
            if !off_is_null {
                entries.push((
                    "thinking".to_string(),
                    Value::Map(vec![("type".to_string(), Value::String("disabled".to_string()))]),
                ));
            }
        }
    }

    // Metadata: extract user_id.
    if let Some(metadata) = options.and_then(|o| o.stream.metadata.as_ref()) {
        if let Some(Value::String(user_id)) = metadata.iter().find(|(key, _)| key == "user_id").map(|(_, v)| v) {
            entries.push((
                "metadata".to_string(),
                Value::Map(vec![("user_id".to_string(), Value::String(user_id.clone()))]),
            ));
        }
    }

    if let Some(tool_choice) = options.and_then(|o| o.tool_choice.clone()) {
        entries.push(("tool_choice".to_string(), tool_choice.to_value()));
    }

    Value::Map(entries)
}

// ---------------------------------------------------------------------------
// Stream processing
// ---------------------------------------------------------------------------

fn empty_usage() -> Usage {
    Usage {
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
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as f64
}

fn output_message(model: &Model) -> AssistantMessage {
    AssistantMessage {
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
    }
}

fn format_anthropic_error(error: &ProviderError) -> String {
    let normalized = NormalizedProviderError::new(error.message.clone(), error.status, None);
    format_provider_error(&normalized, Some("Anthropic API error"))
}

fn apply_usage(output: &mut AssistantMessage, usage: &AnthropicUsage) {
    output.usage.input = usage.input_tokens;
    output.usage.output = usage.output_tokens;
    output.usage.cache_read = usage.cache_read_input_tokens;
    output.usage.cache_write = usage.cache_creation_input_tokens;
    output.usage.cache_write_1h = Some(usage.ephemeral_1h_input_tokens);
    if let Some(thinking_tokens) = usage.thinking_tokens {
        output.usage.reasoning = Some(thinking_tokens);
    }
    output.usage.total_tokens =
        output.usage.input + output.usage.output + output.usage.cache_read + output.usage.cache_write;
}

fn update_usage_from_delta(output: &mut AssistantMessage, usage: &AnthropicUsage) {
    // Only update fields that are present; preserves input_tokens from
    // message_start when proxies omit it in message_delta.
    if usage.input_tokens != 0.0 {
        output.usage.input = usage.input_tokens;
    }
    if usage.output_tokens != 0.0 {
        output.usage.output = usage.output_tokens;
    }
    if usage.cache_read_input_tokens != 0.0 {
        output.usage.cache_read = usage.cache_read_input_tokens;
    }
    if usage.cache_creation_input_tokens != 0.0 {
        output.usage.cache_write = usage.cache_creation_input_tokens;
    }
    if let Some(thinking_tokens) = usage.thinking_tokens {
        output.usage.reasoning = Some(thinking_tokens);
    }
    output.usage.total_tokens =
        output.usage.input + output.usage.output + output.usage.cache_read + output.usage.cache_write;
}

/// Processes Anthropic stream events into assistant stream events, mirroring
/// the JS `for await` loop in `stream`.
pub fn process_anthropic_stream<I>(
    events: I,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
    is_oauth_token: bool,
    context_tools: Option<&[Tool]>,
) -> Result<(), String>
where
    I: IntoIterator<Item = AnthropicStreamEvent>,
{
    let mut saw_message_start = false;
    let mut saw_message_end = false;
    // Anthropic block index -> output.content index.
    let mut block_indices: Vec<(f64, usize)> = Vec::new();
    // Output content index -> streaming JSON scratch buffer for tool calls.
    let mut partial_json: std::collections::HashMap<usize, String> = std::collections::HashMap::new();

    for event in events {
        match event {
            AnthropicStreamEvent::MessageStart { message } => {
                saw_message_start = true;
                output.response_id = Some(message.id);
                if let Some(usage) = &message.usage {
                    apply_usage(output, usage);
                    calculate_cost(model, &mut output.usage);
                }
            }
            AnthropicStreamEvent::ContentBlockStart { index, content_block } => {
                match content_block {
                    AnthropicContentBlockStart::Text { text } => {
                        output.content.push(Content::Text(crate::types::TextContent {
                            text,
                            text_signature: None,
                        }));
                        let content_index = output.content.len() - 1;
                        block_indices.push((index, content_index));
                        stream.push(crate::types::AssistantMessageEvent::TextStart {
                            content_index: content_index as f64,
                            partial: output.clone(),
                        });
                    }
                    AnthropicContentBlockStart::Thinking { thinking, signature } => {
                        output.content.push(Content::Thinking(crate::types::ThinkingContent {
                            thinking,
                            thinking_signature: Some(signature),
                            redacted: None,
                        }));
                        let content_index = output.content.len() - 1;
                        block_indices.push((index, content_index));
                        stream.push(crate::types::AssistantMessageEvent::ThinkingStart {
                            content_index: content_index as f64,
                            partial: output.clone(),
                        });
                    }
                    AnthropicContentBlockStart::RedactedThinking { data } => {
                        output.content.push(Content::Thinking(crate::types::ThinkingContent {
                            thinking: "[Reasoning redacted]".to_string(),
                            thinking_signature: Some(data),
                            redacted: Some(true),
                        }));
                        let content_index = output.content.len() - 1;
                        block_indices.push((index, content_index));
                        stream.push(crate::types::AssistantMessageEvent::ThinkingStart {
                            content_index: content_index as f64,
                            partial: output.clone(),
                        });
                    }
                    AnthropicContentBlockStart::ToolUse { id, name, input } => {
                        output.content.push(Content::ToolCall(crate::types::ToolCall {
                            id,
                            name: if is_oauth_token {
                                from_claude_code_name(&name, context_tools)
                            } else {
                                name
                            },
                            arguments: input,
                            thought_signature: None,
                            namespace: None,
                        }));
                        let content_index = output.content.len() - 1;
                        block_indices.push((index, content_index));
                        stream.push(crate::types::AssistantMessageEvent::ToolCallStart {
                            content_index: content_index as f64,
                            partial: output.clone(),
                        });
                    }
                }
            }
            AnthropicStreamEvent::ContentBlockDelta { index, delta } => {
                let Some(content_index) = block_indices.iter().find(|(i, _)| *i == index).map(|(_, c)| *c) else {
                    continue;
                };
                match delta {
                    AnthropicContentBlockDelta::TextDelta { text } => {
                        if let Content::Text(block) = &mut output.content[content_index] {
                            block.text.push_str(&text);
                            stream.push(crate::types::AssistantMessageEvent::TextDelta {
                                content_index: content_index as f64,
                                delta: text,
                                partial: output.clone(),
                            });
                        }
                    }
                    AnthropicContentBlockDelta::ThinkingDelta { thinking } => {
                        if let Content::Thinking(block) = &mut output.content[content_index] {
                            block.thinking.push_str(&thinking);
                            stream.push(crate::types::AssistantMessageEvent::ThinkingDelta {
                                content_index: content_index as f64,
                                delta: thinking,
                                partial: output.clone(),
                            });
                        }
                    }
                    AnthropicContentBlockDelta::InputJsonDelta { partial_json: delta } => {
                        if let Content::ToolCall(tool_call) = &mut output.content[content_index] {
                            let scratch = partial_json.entry(content_index).or_default();
                            scratch.push_str(&delta);
                            tool_call.arguments = parse_streaming_json(Some(scratch));
                            stream.push(crate::types::AssistantMessageEvent::ToolCallDelta {
                                content_index: content_index as f64,
                                delta,
                                partial: output.clone(),
                            });
                        }
                    }
                    AnthropicContentBlockDelta::SignatureDelta { signature } => {
                        if let Content::Thinking(block) = &mut output.content[content_index] {
                            let current = block.thinking_signature.take().unwrap_or_default();
                            block.thinking_signature = Some(format!("{current}{signature}"));
                        }
                    }
                }
            }
            AnthropicStreamEvent::ContentBlockStop { index } => {
                let Some(content_index) = block_indices.iter().find(|(i, _)| *i == index).map(|(_, c)| *c) else {
                    continue;
                };
                match &mut output.content[content_index] {
                    Content::Text(block) => {
                        stream.push(crate::types::AssistantMessageEvent::TextEnd {
                            content_index: content_index as f64,
                            content: block.text.clone(),
                            partial: output.clone(),
                        });
                    }
                    Content::Thinking(block) => {
                        stream.push(crate::types::AssistantMessageEvent::ThinkingEnd {
                            content_index: content_index as f64,
                            content: block.thinking.clone(),
                            partial: output.clone(),
                        });
                    }
                    Content::ToolCall(tool_call) => {
                        let scratch = partial_json.remove(&content_index).unwrap_or_default();
                        tool_call.arguments = parse_streaming_json(Some(&scratch));
                        let finalized = tool_call.clone();
                        stream.push(crate::types::AssistantMessageEvent::ToolCallEnd {
                            content_index: content_index as f64,
                            tool_call: finalized,
                            partial: output.clone(),
                        });
                    }
                    Content::Image(_) => {}
                }
            }
            AnthropicStreamEvent::MessageDelta { stop_reason, stop_details, usage } => {
                if let Some(stop_reason) = stop_reason {
                    output.raw_stop_reason = Some(stop_reason.clone());
                    let (mapped, error_message) = map_stop_reason(&stop_reason, stop_details.as_deref());
                    output.stop_reason = mapped;
                    if let Some(error_message) = error_message {
                        output.error_message = Some(error_message);
                    }
                }
                if let Some(usage) = &usage {
                    update_usage_from_delta(output, usage);
                    calculate_cost(model, &mut output.usage);
                }
            }
            AnthropicStreamEvent::MessageStop => {
                saw_message_end = true;
            }
        }
    }

    if saw_message_start && !saw_message_end {
        return Err("Anthropic stream ended before message_stop".to_string());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// stream / streamSimple
// ---------------------------------------------------------------------------

/// Stream function for the Anthropic Messages API. Spawns a worker thread
/// that performs the request and feeds the returned stream (mirroring the JS
/// async IIFE).
pub fn stream(
    model: &Model,
    context: &Context,
    options: Option<&AnthropicOptions>,
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
        let mut output = output_message(&model);
        let result = (|| -> Result<(), String> {
            assert_request_auth(
                &model.provider,
                api_key.as_deref(),
                options.as_ref().and_then(|o| o.stream.request.headers.as_ref()),
            )?;
            let is_oauth = is_oauth_token(api_key.as_deref());
            let is_copilot = model.provider == "github-copilot";

            let mut dynamic_headers: Option<Vec<(String, String)>> = None;
            if is_copilot {
                let has_images = has_copilot_vision_input(&context.messages);
                dynamic_headers = Some(build_copilot_dynamic_headers(CopilotDynamicHeadersParams {
                    messages: &context.messages,
                    has_images,
                }));
            }

            let cache_retention = resolve_cache_retention(
                options.as_ref().and_then(|o| o.stream.cache_retention.as_ref()),
                options.as_ref().and_then(|o| o.stream.request.env.as_ref()),
            );
            let cache_session_id = if cache_retention == "none" {
                None
            } else {
                options.as_ref().and_then(|o| o.stream.session_id.clone())
            };

            let mut beta_features: Vec<&str> = Vec::new();
            if should_use_fine_grained_tool_streaming_beta(&model, &context) {
                beta_features.push(FINE_GRAINED_TOOL_STREAMING_BETA);
            }
            let needs_interleaved_beta = options
                .as_ref()
                .and_then(|o| o.interleaved_thinking)
                .unwrap_or(true)
                && model
                    .compat
                    .as_ref()
                    .is_some_and(|compat| {
                        !matches!(compat, crate::types::ModelCompat::AnthropicMessages(compat) if compat.force_adaptive_thinking == Some(true))
                    });
            if needs_interleaved_beta {
                beta_features.push(INTERLEAVED_THINKING_BETA);
            }

            let headers = build_anthropic_headers(
                &model,
                api_key.as_deref(),
                is_oauth,
                is_copilot,
                &beta_features,
                dynamic_headers.as_deref(),
                options.as_ref().and_then(|o| o.stream.request.headers.as_ref()),
                cache_session_id.as_deref(),
            );

            let params = build_params(&model, &context, is_oauth, options.as_ref());

            let url = format!("{}/v1/messages", model.base_url.trim_end_matches('/'));
            let response = retry_provider_request(
                || {
                    client
                        .post_json(
                            &url,
                            &headers,
                            &params,
                            options.as_ref().and_then(|o| o.stream.request.timeout_ms),
                        )
                        .map_err(|error| ProviderError::new(error.status, error.headers.clone(), error.message.clone()))
                },
                ProviderRetryOptions {
                    max_retries: options.as_ref().and_then(|o| o.stream.request.max_retries),
                    max_retry_delay_ms: options
                        .as_ref()
                        .and_then(|o| o.stream.request.max_retry_delay_ms)
                        .map(|v| v as f64),
                    token: None,
                },
            )
            .map_err(|failure| match failure {
                ProviderRetryFailure::Error(error) => format_anthropic_error(&error),
                ProviderRetryFailure::Aborted => "Request was aborted".to_string(),
            })?;

            stream.push(crate::types::AssistantMessageEvent::Start {
                partial: output.clone(),
            });

            let mut events: Vec<AnthropicStreamEvent> = Vec::new();
            let mut parse_error: Option<String> = None;
            crate::http::client::read_sse_stream(response.reader, |sse| {
                let sse = AnthropicSseEvent {
                    event: sse.event.clone(),
                    data: sse.data.clone(),
                    raw: Vec::new(),
                };
                match parse_anthropic_stream_event(&sse) {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => {}
                    Err(error) => parse_error = Some(error),
                }
            });
            if let Some(error) = parse_error {
                return Err(error);
            }

            process_anthropic_stream(events, &mut output, &stream, &model, is_oauth, context.tools.as_deref())?;

            if output.stop_reason == StopReason::Pending {
                return Err("Anthropic stream ended without a stop reason".to_string());
            }
            if output.stop_reason == StopReason::Aborted || output.stop_reason == StopReason::Error {
                return Err(output
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "An unknown error occurred".to_string()));
            }

            stream.push(crate::types::AssistantMessageEvent::Done {
                reason: output.stop_reason.as_str().to_string(),
                message: output.clone(),
            });
            stream.end(None);
            Ok(())
        })();

        if let Err(error) = result {
            output.stop_reason = StopReason::Error;
            output.error_message = Some(error);
            stream.push(crate::types::AssistantMessageEvent::Error {
                reason: "error".to_string(),
                error: output,
            });
            stream.end(None);
        }
    });

    stream
}

/// Simple-stream variant: maps a reasoning level to thinking mode.
pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<&SimpleStreamOptions>,
    api_key: Option<&str>,
    client: &HttpClient,
) -> AssistantMessageEventStream {
    let _ = assert_request_auth(
        &model.provider,
        api_key,
        options.and_then(|o| o.stream.request.headers.as_ref()),
    );

    let base = build_base_options(model, context, options, api_key);
    if options.and_then(|o| o.reasoning.as_deref()).is_none() {
        return stream(
            model,
            context,
            Some(&AnthropicOptions {
                stream: base,
                thinking_enabled: Some(false),
                ..AnthropicOptions::default()
            }),
            api_key,
            client,
        );
    }

    let reasoning = options.and_then(|o| o.reasoning.clone()).expect("checked above");
    // For models with adaptive thinking: use an effort level.
    let force_adaptive = model.compat.as_ref().is_some_and(|compat| {
        matches!(compat, crate::types::ModelCompat::AnthropicMessages(compat) if compat.force_adaptive_thinking == Some(true))
    });
    if force_adaptive {
        let effort = map_thinking_level_to_effort(model, Some(&reasoning));
        return stream(
            model,
            context,
            Some(&AnthropicOptions {
                stream: base,
                thinking_enabled: Some(true),
                effort: Some(effort),
                ..AnthropicOptions::default()
            }),
            api_key,
            client,
        );
    }

    // Budget-based thinking for older models.
    let adjusted = adjust_max_tokens_for_thinking(
        base.max_tokens,
        model.max_tokens,
        &reasoning,
        options.and_then(|o| o.thinking_budgets.as_ref()),
    );
    let max_tokens = clamp_max_tokens_to_context(model, context, adjusted.0);
    let thinking_budget = adjusted.1.min((max_tokens - 1024.0).max(0.0));

    stream(
        model,
        context,
        Some(&AnthropicOptions {
            stream: StreamOptions {
                max_tokens: Some(max_tokens),
                ..base
            },
            thinking_enabled: Some(true),
            thinking_budget_tokens: Some(thinking_budget),
            ..AnthropicOptions::default()
        }),
        api_key,
        client,
    )
}

// Import resolveJsonSchemaStrictSampling from constrained-sampling.
use crate::api::constrained_sampling::resolve_json_schema_strict_sampling;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        Content, ModelCompat, ModelCost, ModelCostRates, TextContent, ToolResultMessage,
        UserMessage, UserMessageContent,
    };

    fn model(provider: &str, id: &str, reasoning: bool) -> Model {
        Model {
            id: id.to_string(),
            name: id.to_string(),
            api: "anthropic-messages".to_string(),
            provider: provider.to_string(),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning,
            thinking_level_map: None,
            input: vec!["text".to_string()],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 3.0,
                    output: 15.0,
                    cache_read: 0.3,
                    cache_write: 3.0,
                },
                tiers: None,
            },
            context_window: 200_000.0,
            max_tokens: 8192.0,
            sampling_params: None,
            headers: None,
            compat: None,
        }
    }

    fn user_text(text: &str) -> crate::types::Message {
        crate::types::Message::User(UserMessage {
            content: UserMessageContent::Text(text.to_string()),
            timestamp: 1.0,
        })
    }

    fn assistant_text(text: &str) -> crate::types::Message {
        crate::types::Message::Assistant(AssistantMessage {
            content: vec![Content::Text(TextContent {
                text: text.to_string(),
                text_signature: None,
            })],
            api: "anthropic-messages".to_string(),
            provider: "anthropic".to_string(),
            model: "claude-sonnet".to_string(),
            response_model: None,
            response_id: None,
            usage: empty_usage(),
            stop_reason: StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 2.0,
        })
    }

    fn tool_result() -> crate::types::Message {
        crate::types::Message::ToolResult(ToolResultMessage {
            tool_call_id: "call-1".to_string(),
            tool_name: "read".to_string(),
            content: vec![Content::Text(TextContent {
                text: "done".to_string(),
                text_signature: None,
            })],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 3.0,
        })
    }

    #[test]
    fn normalizes_tool_call_ids() {
        assert_eq!(normalize_tool_call_id("call_1"), "call_1");
        assert_eq!(normalize_tool_call_id("weird|id!"), "weird_id_");
        let long = "x".repeat(100);
        assert_eq!(normalize_tool_call_id(&long).chars().count(), 64);
    }

    #[test]
    fn maps_stop_reasons() {
        assert_eq!(map_stop_reason("end_turn", None).0, StopReason::Stop);
        assert_eq!(map_stop_reason("max_tokens", None).0, StopReason::Length);
        assert_eq!(map_stop_reason("tool_use", None).0, StopReason::ToolUse);
        assert_eq!(map_stop_reason("pause_turn", None).0, StopReason::Stop);
        assert_eq!(map_stop_reason("stop_sequence", None).0, StopReason::Stop);
        let refusal = map_stop_reason("refusal", None);
        assert_eq!(refusal.0, StopReason::Error);
        assert!(refusal.1.unwrap().contains("refused"));
        let refusal = map_stop_reason("refusal", Some("because"));
        assert_eq!(refusal.1.unwrap(), "because");
        let sensitive = map_stop_reason("sensitive", None);
        assert_eq!(sensitive.0, StopReason::Error);
        assert_eq!(sensitive.1.unwrap(), "Provider stopped with: sensitive");
    }

    #[test]
    fn converts_claude_code_tool_names() {
        assert_eq!(to_claude_code_name("bash"), "Bash");
        assert_eq!(to_claude_code_name("Bash"), "Bash");
        assert_eq!(to_claude_code_name("custom_tool"), "custom_tool");
        let tools = vec![Tool {
            name: "MyTool".to_string(),
            description: "d".to_string(),
            parameters: crate::types::JsonSchemaObject::default(),
            constrained_sampling: None,
        }];
        assert_eq!(from_claude_code_name("mytool", Some(&tools)), "MyTool");
        assert_eq!(from_claude_code_name("read", Some(&tools)), "read");
    }

    #[test]
    fn detects_oauth_tokens() {
        assert!(is_oauth_token(Some("sk-ant-oat123")));
        assert!(!is_oauth_token(Some("sk-ant-api03-abc")));
        assert!(!is_oauth_token(None));
    }

    #[test]
    fn defaults_tool_references() {
        let m = |id: &str| model("anthropic", id, false);
        assert!(default_supports_tool_references(&m("claude-opus-4-5")));
        assert!(!default_supports_tool_references(&m("claude-opus-4-1")));
        assert!(!default_supports_tool_references(&m("claude-opus-4")));
        assert!(!default_supports_tool_references(&m("claude-3-5-sonnet")));
        assert!(!default_supports_tool_references(&m("claude-sonnet-4-5-haiku")));
        assert!(default_supports_tool_references(&m("claude-sonnet-4-5")));
        assert!(!default_supports_tool_references(&model("other", "claude-opus-4-5", false)));
    }

    #[test]
    fn converts_content_blocks() {
        let text_only = vec![Content::Text(TextContent {
            text: "hi".to_string(),
            text_signature: None,
        })];
        match convert_content_blocks(&text_only) {
            ConvertedContent::Text(text) => assert_eq!(text, "hi"),
            _ => panic!("expected text"),
        }

        let with_image = vec![
            Content::Text(TextContent {
                text: "hi".to_string(),
                text_signature: None,
            }),
            Content::Image(crate::types::ImageContent {
                data: "abc".to_string(),
                mime_type: "image/png".to_string(),
            }),
        ];
        match convert_content_blocks(&with_image) {
            ConvertedContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
                assert!(matches!(&blocks[1], Value::Map(_)));
            }
            _ => panic!("expected blocks"),
        }

        let image_only = vec![Content::Image(crate::types::ImageContent {
            data: "abc".to_string(),
            mime_type: "image/jpeg".to_string(),
        })];
        match convert_content_blocks(&image_only) {
            ConvertedContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
                assert_eq!(get_str(blocks[0].as_map().unwrap(), "text"), Some("(see attached image)"));
            }
            _ => panic!("expected blocks"),
        }
    }

    #[test]
    fn converts_messages_with_tool_results_and_cache_control() {
        let messages = vec![
            user_text("hello"),
            assistant_text("hi"),
            tool_result(),
            user_text("  "),
        ];
        let cache_control = Some(CacheControl { ttl: None });
        let converted = convert_messages(&messages, false, cache_control.as_ref(), false, &std::collections::HashSet::new(), &|name| name.to_string());
        assert_eq!(converted.len(), 3); // blank user skipped
        let last = converted.last().unwrap();
        let entries = last.as_map().unwrap();
        // cache_control on the final user message block (tool_result content).
        assert!(json_contains_cache_control(last), "last message should carry cache_control");
        let _ = entries;
    }

    fn json_contains_cache_control(value: &Value) -> bool {
        match value {
            Value::Map(entries) => {
                if entries.iter().any(|(k, _)| k == "cache_control") {
                    return true;
                }
                entries
                    .iter()
                    .any(|(_, v)| json_contains_cache_control(v))
            }
            Value::Array(items) => items.iter().any(json_contains_cache_control),
            _ => false,
        }
    }

    #[test]
    fn converts_tools_with_strict_schemas() {
        let tools = vec![Tool {
            name: "add".to_string(),
            description: "Adds".to_string(),
            parameters: crate::types::JsonSchemaObject {
                type_: Some(vec!["object".to_string()]),
                properties: Some(vec![(
                    "a".to_string(),
                    crate::types::JsonSchemaObject {
                        type_: Some(vec!["number".to_string()]),
                        ..crate::types::JsonSchemaObject::default()
                    },
                )]),
                required: Some(vec!["a".to_string()]),
                ..crate::types::JsonSchemaObject::default()
            },
            constrained_sampling: None,
        }];
        let tools_with_strict = vec![Tool {
            constrained_sampling: Some(crate::types::ConstrainedSampling::Config(
                crate::types::ConstrainedSamplingConfig::JsonSchema {
                    strict: "require".to_string(),
                },
            )),
            ..tools[0].clone()
        }];
        let converted = convert_tools(&tools_with_strict, false, true, true, None, false);
        assert_eq!(converted.len(), 1);
        let entries = converted[0].as_map().unwrap();
        assert_eq!(get_str(entries, "name"), Some("add"));
        assert!(entries.iter().any(|(k, v)| k == "eager_input_streaming" && matches!(v, Value::Bool(true))));
        assert!(entries.iter().any(|(k, v)| k == "strict" && matches!(v, Value::Bool(true))));
        // input_schema keeps properties/required.
        let input_schema = entries.iter().find(|(k, _)| k == "input_schema").map(|(_, v)| v).unwrap();
        assert!(json_contains_key(input_schema, "properties"));
        assert!(json_contains_key(input_schema, "required"));
    }

    fn json_contains_key(value: &Value, key: &str) -> bool {
        match value {
            Value::Map(entries) => entries.iter().any(|(k, _)| k == key),
            Value::Array(items) => items.iter().any(|v| json_contains_key(v, key)),
            _ => false,
        }
    }

    #[test]
    fn decodes_anthropic_sse_events() {
        let wire = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":5}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let mut decoder = AnthropicSseDecoder::new();
        let events = decoder.push(wire.as_bytes());
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].event.as_deref(), Some("message_start"));
        assert_eq!(events[3].event.as_deref(), Some("message_stop"));

        let parsed: Vec<_> = events
            .iter()
            .filter_map(|sse| parse_anthropic_stream_event(sse).unwrap())
            .collect();
        assert_eq!(parsed.len(), 4);
        assert!(matches!(parsed[0], AnthropicStreamEvent::MessageStart { .. }));
        assert!(matches!(parsed[1], AnthropicStreamEvent::ContentBlockStart { .. }));
        assert!(matches!(parsed[2], AnthropicStreamEvent::ContentBlockDelta { .. }));
        assert!(matches!(parsed[3], AnthropicStreamEvent::MessageStop));
    }

    #[test]
    fn rejects_parse_errors_with_raw() {
        let mut decoder = AnthropicSseDecoder::new();
        let events = decoder.push(b"event: content_block_start\ndata: {bad json\n\n");
        let sse = &events[0];
        let error = parse_anthropic_stream_event(sse).unwrap_err();
        assert!(error.contains("Could not parse Anthropic SSE event"), "{error}");
        assert!(error.contains("data={bad json"), "{error}");
    }

    #[test]
    fn processes_a_text_stream_end_to_end() {
        let wire = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let mut decoder = AnthropicSseDecoder::new();
        let sse_events = decoder.push(wire.as_bytes());
        let parsed: Vec<AnthropicStreamEvent> = sse_events
            .iter()
            .filter_map(|sse| parse_anthropic_stream_event(sse).unwrap())
            .collect();

        let m = model("anthropic", "claude-sonnet-4-5", false);
        let mut output = output_message(&m);
        let stream = AssistantMessageEventStream::new();
        process_anthropic_stream(parsed, &mut output, &stream, &m, false, None).unwrap();

        assert_eq!(output.response_id.as_deref(), Some("msg_1"));
        assert_eq!(output.stop_reason, StopReason::Stop);
        assert_eq!(output.raw_stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(output.content.len(), 1);
        match &output.content[0] {
            Content::Text(block) => assert_eq!(block.text, "Hello world"),
            other => panic!("expected text, got {other:?}"),
        }
        assert_eq!(output.usage.input, 5.0);
        assert_eq!(output.usage.output, 2.0);
        assert_eq!(output.usage.total_tokens, 7.0);
    }

    #[test]
    fn processes_tool_call_with_thinking() {
        let wire = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_2\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"sig_1\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"let me\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\" think\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"/tm\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"p/file\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let mut decoder = AnthropicSseDecoder::new();
        let sse_events = decoder.push(wire.as_bytes());
        let parsed: Vec<AnthropicStreamEvent> = sse_events
            .iter()
            .filter_map(|sse| parse_anthropic_stream_event(sse).unwrap())
            .collect();

        let m = model("anthropic", "claude-sonnet-4-5", false);
        let mut output = output_message(&m);
        let stream = AssistantMessageEventStream::new();
        process_anthropic_stream(parsed, &mut output, &stream, &m, false, None).unwrap();

        assert_eq!(output.stop_reason, StopReason::ToolUse);
        assert_eq!(output.content.len(), 2);
        match &output.content[0] {
            Content::Thinking(block) => {
                assert_eq!(block.thinking, "let me think");
                assert_eq!(block.thinking_signature.as_deref(), Some("sig_1"));
            }
            other => panic!("expected thinking, got {other:?}"),
        }
        match &output.content[1] {
            Content::ToolCall(tool_call) => {
                assert_eq!(tool_call.id, "toolu_1");
                assert_eq!(tool_call.name, "read");
                assert_eq!(
                    tool_call.arguments,
                    Value::Map(vec![("path".to_string(), Value::String("/tmp/file".to_string()))])
                );
            }
            other => panic!("expected tool call, got {other:?}"),
        }
    }

    #[test]
    fn build_params_includes_thinking_and_tools() {
        let mut m = model("anthropic", "claude-sonnet-4-5", true);
        m.compat = Some(ModelCompat::AnthropicMessages(crate::types::AnthropicMessagesCompat {
            force_adaptive_thinking: Some(true),
            ..crate::types::AnthropicMessagesCompat::default()
        }));
        let context = Context {
            system_prompt: Some("be brief".to_string()),
            messages: vec![user_text("hi")],
            tools: Some(vec![Tool {
                name: "read".to_string(),
                description: "Read a file".to_string(),
                parameters: crate::types::JsonSchemaObject {
                    type_: Some(vec!["object".to_string()]),
                    properties: Some(vec![(
                        "path".to_string(),
                        crate::types::JsonSchemaObject {
                            type_: Some(vec!["string".to_string()]),
                            ..crate::types::JsonSchemaObject::default()
                        },
                    )]),
                    required: Some(vec!["path".to_string()]),
                    ..crate::types::JsonSchemaObject::default()
                },
                constrained_sampling: None,
            }]),
        };
        let options = AnthropicOptions {
            thinking_enabled: Some(true),
            effort: Some("high".to_string()),
            ..AnthropicOptions::default()
        };
        let params = build_params(&m, &context, false, Some(&options));
        let entries = params.as_map().unwrap();
        assert!(json_contains_key(&params, "system"));
        assert!(json_contains_key(&params, "tools"));
        let thinking = entries.iter().find(|(k, _)| k == "thinking").map(|(_, v)| v).unwrap();
        assert!(json_contains_key(thinking, "display"));
        assert!(json_contains_key(&params, "output_config"));
    }

    #[test]
    fn redacted_thinking_round_trips() {
        let wire = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"encrypted_payload\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let mut decoder = AnthropicSseDecoder::new();
        let sse_events = decoder.push(wire.as_bytes());
        let parsed: Vec<AnthropicStreamEvent> = sse_events
            .iter()
            .filter_map(|sse| parse_anthropic_stream_event(sse).unwrap())
            .collect();
        let m = model("anthropic", "claude-sonnet-4-5", false);
        let mut output = output_message(&m);
        let stream = AssistantMessageEventStream::new();
        process_anthropic_stream(parsed, &mut output, &stream, &m, false, None).unwrap();
        match &output.content[0] {
            Content::Thinking(block) => {
                assert_eq!(block.redacted, Some(true));
                assert_eq!(block.thinking, "[Reasoning redacted]");
                assert_eq!(block.thinking_signature.as_deref(), Some("encrypted_payload"));
            }
            other => panic!("expected thinking, got {other:?}"),
        }
    }

    #[test]
    fn claude_code_name_obfuscation_on_oauth() {
        let m = model("anthropic", "claude-sonnet-4-5", false);
        let context = Context {
            messages: vec![user_text("hi")],
            tools: Some(vec![Tool {
                name: "Bash".to_string(),
                description: "Run a command".to_string(),
                parameters: crate::types::JsonSchemaObject::default(),
                constrained_sampling: None,
            }]),
            ..Context::default()
        };
        let params = build_params(&m, &context, true, None);
        // OAuth: tools are obfuscated to CC casing (Bash stays Bash), system
        // carries the Claude Code identity.
        assert!(json_contains_key(&params, "system"));
        let system = params
            .as_map()
            .unwrap()
            .iter()
            .find(|(k, _)| k == "system")
            .map(|(_, v)| v)
            .unwrap();
        let system_text = json_find_text(system, "You are Claude Code");
        assert!(system_text, "OAuth system prompt must include Claude Code identity");
    }

    fn json_find_text(value: &Value, needle: &str) -> bool {
        match value {
            Value::Map(entries) => entries.iter().any(|(k, v)| {
                if k == "text" {
                    v.as_str().is_some_and(|text| text.contains(needle))
                } else {
                    json_find_text(v, needle)
                }
            }),
            Value::Array(items) => items.iter().any(|v| json_find_text(v, needle)),
            _ => false,
        }
    }
}
