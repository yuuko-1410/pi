//! Mistral Conversations API provider, port of
//! `packages/ai/src/api/mistral-conversations.ts`.
//!
//! Mistral uses an OpenAI-compatible chat-completions stream with a custom
//! event boundary set (looser than standard SSE) and conversations-specific
//! fields (prompt caching via x-affinity, cached-token usage details). This
//! adapter mirrors the JS implementation exactly: event framing, camelCase
//! -> snake_case wire remapping, tool call ID normalization (9-char
//! alphanumeric), streaming argument accumulation, and stop-reason mapping.

use std::io::Read;

use pi_protocol::Value;

use crate::api::constrained_sampling::resolve_json_schema_strict_sampling;
use crate::api::simple_options::build_base_options;
use crate::api::transform_messages::transform_messages;
use crate::event_stream::AssistantMessageEventStream;
use crate::http::client::HttpClient;
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::types::{
    AssistantMessage, Content, Context, Message, Model, SimpleStreamOptions, StopReason, StreamOptions,
    TextContent, ThinkingContent, Tool, ToolCall, Usage, UsageCost, UserMessageContent,
};
use crate::utils::hash::short_hash;
use crate::utils::json::{json_stringify, parse_streaming_json};

const MISTRAL_TOOL_CALL_ID_LENGTH: usize = 9;
const MAX_MISTRAL_ERROR_BODY_CHARS: usize = 4000;
const DEFAULT_MISTRAL_TIMEOUT_MS: u64 = 60_000;

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum MistralToolChoice {
    Auto,
    None,
    Any,
    Required,
    Function { name: String },
}

impl MistralToolChoice {
    fn to_value(&self) -> Value {
        match self {
            Self::Auto => Value::String("auto".to_string()),
            Self::None => Value::String("none".to_string()),
            Self::Any => Value::String("any".to_string()),
            Self::Required => Value::String("required".to_string()),
            Self::Function { name } => Value::Map(vec![
                ("type".to_string(), Value::String("function".to_string())),
                (
                    "function".to_string(),
                    Value::Map(vec![("name".to_string(), Value::String(name.clone()))]),
                ),
            ]),
        }
    }
}

/// Provider-specific options for the Mistral API.
#[derive(Clone, Debug, Default)]
pub struct MistralOptions {
    pub stream: StreamOptions,
    pub tool_choice: Option<MistralToolChoice>,
    pub prompt_mode: Option<String>,
    pub reasoning_effort: Option<String>,
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum MistralContentChunk {
    Text { text: String },
    Image { image_url: String },
    Thinking { thinking: Vec<String> },
}

#[derive(Clone, Debug)]
enum MistralContentItems {
    Text(String),
    Chunks(Vec<MistralContentChunk>),
}

#[derive(Clone, Debug)]
struct MistralToolCallDelta {
    id: Option<String>,
    index: Option<f64>,
    name: String,
    arguments: String,
}

#[derive(Clone, Debug)]
struct MistralDelta {
    content: Option<MistralContentItems>,
    tool_calls: Vec<MistralToolCallDelta>,
}

#[derive(Clone, Debug)]
struct MistralChoice {
    finish_reason: Option<String>,
    delta: MistralDelta,
}

#[derive(Clone, Debug)]
struct MistralUsage {
    prompt_tokens: f64,
    completion_tokens: f64,
    total_tokens: f64,
    cached_tokens: f64,
}

#[derive(Clone, Debug)]
struct MistralCompletionEvent {
    id: Option<String>,
    usage: Option<MistralUsage>,
    choices: Vec<MistralChoice>,
}

// ---------------------------------------------------------------------------
// Value helpers
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Stream event framing (Mistral's custom boundaries)
// ---------------------------------------------------------------------------

const MISTRAL_BOUNDARIES: [&[u8]; 8] = [
    b"\r\n\r\n", b"\r\n\r", b"\r\n\n", b"\r\r\n", b"\n\r\n", b"\r\r", b"\n\r", b"\n\n",
];

/// Mirrors `findMistralEventBoundary`: finds the earliest occurrence of any
/// boundary sequence, returning (index, length).
fn find_mistral_event_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for boundary in MISTRAL_BOUNDARIES {
        if boundary.len() > buffer.len() {
            continue;
        }
        if let Some(index) = find_subslice(buffer, boundary) {
            if best.map_or(true, |(best_index, _)| index < best_index) {
                best = Some((index, boundary.len()));
            }
        }
    }
    best
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[derive(Debug)]
enum MistralEvent {
    Done,
    Data(MistralCompletionEvent),
}

/// Mirrors `parseMistralEvent`: extracts `data:` lines, returns None when
/// there is no data, Done for `[DONE]`, and parses the JSON payload.
fn parse_mistral_event(raw: &str) -> Result<Option<MistralEvent>, String> {
    let data = raw
        .split(['\r', '\n'])
        .filter(|line| line.starts_with("data:"))
        .map(|line| line[5..].trim_start())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    if data.is_empty() {
        return Ok(None);
    }
    if data == "[DONE]" {
        return Ok(Some(MistralEvent::Done));
    }

    let parsed: Value = crate::utils::json::parse_json_with_repair(&data)
        .map_err(|_| "Invalid Mistral streaming event".to_string())?;
    let Some(entries) = parsed.as_map() else {
        return Err("Invalid Mistral streaming event".to_string());
    };
    // JS validates `parsed.choices` is an array.
    let Some(choices) = entries.iter().find(|(k, _)| k == "choices").and_then(|(_, v)| v.as_array()) else {
        return Err("Invalid Mistral streaming event".to_string());
    };
    let _ = choices;
    Ok(Some(MistralEvent::Data(parse_completion_event(entries))))
}

fn parse_completion_event(entries: &[(String, Value)]) -> MistralCompletionEvent {
    let id = get_str(entries, "id");
    let usage = get_obj(entries, "usage").map(parse_usage);
    let choices = entries
        .iter()
        .find(|(k, _)| k == "choices")
        .and_then(|(_, v)| v.as_array())
        .map(|items| items.iter().filter_map(parse_choice).collect());
    MistralCompletionEvent {
        id,
        usage,
        choices: choices.unwrap_or_default(),
    }
}

fn parse_choice(value: &Value) -> Option<MistralChoice> {
    let entries = value.as_map()?;
    let finish_reason = get_str(entries, "finish_reason");
    let delta = get_obj(entries, "delta")?;

    // delta.content is either a string or an array of chunks; detect by key
    // presence (a string content has no `as_map`).
    let content = if delta.iter().any(|(k, _)| k == "content") {
        if let Some(text) = get_str(delta, "content") {
            Some(MistralContentItems::Text(text))
        } else {
            let chunks = delta
                .iter()
                .find(|(k, _)| k == "content")
                .and_then(|(_, v)| v.as_array())
                .map(|items| items.iter().filter_map(parse_content_chunk).collect())
                .unwrap_or_default();
            Some(MistralContentItems::Chunks(chunks))
        }
    } else {
        None
    };

    let tool_calls = delta
        .iter()
        .find(|(k, _)| k == "tool_calls")
        .and_then(|(_, v)| v.as_array())
        .map(|items| items.iter().filter_map(parse_tool_call_delta).collect())
        .unwrap_or_default();
    Some(MistralChoice {
        finish_reason,
        delta: MistralDelta { content, tool_calls },
    })
}

fn parse_content_chunk(value: &Value) -> Option<MistralContentChunk> {
    let entries = value.as_map()?;
    let type_ = get_str(entries, "type")?;
    match type_.as_str() {
        "thinking" => {
            let thinking = entries
                .iter()
                .find(|(k, _)| k == "thinking")
                .and_then(|(_, v)| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_map().and_then(|e| get_str(e, "text")))
                        .collect()
                })
                .unwrap_or_default();
            Some(MistralContentChunk::Thinking { thinking })
        }
        "text" => Some(MistralContentChunk::Text {
            text: get_str(entries, "text").unwrap_or_default(),
        }),
        "image_url" => Some(MistralContentChunk::Image {
            image_url: get_str(entries, "image_url").unwrap_or_default(),
        }),
        _ => None,
    }
}

fn parse_tool_call_delta(value: &Value) -> Option<MistralToolCallDelta> {
    let entries = value.as_map()?;
    let id = get_str(entries, "id");
    let index = get_num(entries, "index");
    let function_entries = get_obj(entries, "function")?;
    let name = get_str(function_entries, "name").unwrap_or_default();
    let arguments = match get_obj(function_entries, "arguments") {
        None => get_str(function_entries, "arguments").unwrap_or_default(),
        Some(_) => {
            let raw = function_entries
                .iter()
                .find(|(k, _)| k == "arguments")
                .map(|(_, v)| v);
            match raw {
                Some(value) => json_stringify(value),
                None => String::new(),
            }
        }
    };
    Some(MistralToolCallDelta {
        id,
        index,
        name,
        arguments,
    })
}

fn parse_usage(entries: &[(String, Value)]) -> MistralUsage {
    let prompt_tokens = get_num(entries, "prompt_tokens").unwrap_or(0.0);
    let cached_tokens = extract_cached_tokens(entries);
    MistralUsage {
        prompt_tokens,
        completion_tokens: get_num(entries, "completion_tokens").unwrap_or(0.0),
        total_tokens: get_num(entries, "total_tokens").unwrap_or(0.0),
        cached_tokens: cached_tokens.min(prompt_tokens).max(0.0),
    }
}

/// Mirrors `getMistralCachedPromptTokens`: probes the many field shapes
/// Mistral has used for cached token counts.
fn extract_cached_tokens(entries: &[(String, Value)]) -> f64 {
    let candidates: [Option<f64>; 6] = [
        get_obj(entries, "promptTokensDetails").and_then(|e| get_num(e, "cachedTokens")),
        get_obj(entries, "prompt_tokens_details").and_then(|e| get_num(e, "cached_tokens")),
        get_obj(entries, "promptTokenDetails").and_then(|e| get_num(e, "cachedTokens")),
        get_obj(entries, "prompt_token_details").and_then(|e| get_num(e, "cached_tokens")),
        get_num(entries, "numCachedTokens"),
        get_num(entries, "num_cached_tokens"),
    ];
    for candidate in candidates {
        if let Some(value) = candidate {
            if value.is_finite() {
                return value;
            }
        }
    }
    0.0
}

// ---------------------------------------------------------------------------
// Tool call ID normalization
// ---------------------------------------------------------------------------

fn derive_mistral_tool_call_id(id: &str, attempt: u64) -> String {
    let normalized: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if attempt == 0 && normalized.chars().count() == MISTRAL_TOOL_CALL_ID_LENGTH {
        return normalized;
    }
    let seed = if attempt == 0 {
        if normalized.is_empty() { id.to_string() } else { normalized }
    } else {
        let base = if normalized.is_empty() { id } else { &normalized };
        format!("{base}:{attempt}")
    };
    short_hash(&seed)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(MISTRAL_TOOL_CALL_ID_LENGTH)
        .collect()
}

struct MistralToolCallIdNormalizer {
    id_map: std::collections::HashMap<String, String>,
    reverse_map: std::collections::HashMap<String, String>,
}

impl MistralToolCallIdNormalizer {
    fn new() -> Self {
        Self {
            id_map: std::collections::HashMap::new(),
            reverse_map: std::collections::HashMap::new(),
        }
    }

    fn normalize(&mut self, id: &str) -> String {
        if let Some(existing) = self.id_map.get(id) {
            return existing.clone();
        }

        let mut attempt = 0u64;
        loop {
            let candidate = derive_mistral_tool_call_id(id, attempt);
            let owner = self.reverse_map.get(&candidate).cloned();
            match owner {
                None => {
                    self.id_map.insert(id.to_string(), candidate.clone());
                    self.reverse_map.insert(candidate.clone(), id.to_string());
                    return candidate;
                }
                Some(owner) if owner == id => {
                    self.id_map.insert(id.to_string(), candidate.clone());
                    self.reverse_map.insert(candidate.clone(), id.to_string());
                    return candidate;
                }
                Some(_) => attempt += 1,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Error formatting
// ---------------------------------------------------------------------------

fn truncate_error_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}... [truncated {} chars]", text.chars().count() - max_chars)
}

fn format_mistral_error(error: &crate::utils::provider_retry::ProviderError) -> String {
    let status_code = error.status;
    let body_text = error.message.trim();
    match status_code {
        Some(status_code) if !body_text.is_empty() => format!(
            "Mistral API error ({status_code}): {}",
            truncate_error_text(body_text, MAX_MISTRAL_ERROR_BODY_CHARS)
        ),
        Some(status_code) => format!("Mistral API error ({status_code}): {}", error.message),
        None => error.message.clone(),
    }
}

// ---------------------------------------------------------------------------
// Message and tool conversion
// ---------------------------------------------------------------------------

fn build_tool_result_text(text: &str, has_images: bool, supports_images: bool, is_error: bool) -> String {
    let trimmed = text.trim();
    let error_prefix = if is_error { "[tool error] " } else { "" };

    if !trimmed.is_empty() {
        let image_suffix = if has_images && !supports_images {
            "\n[tool image omitted: model does not support images]"
        } else {
            ""
        };
        return format!("{error_prefix}{trimmed}{image_suffix}");
    }

    if has_images {
        if supports_images {
            return if is_error {
                "[tool error] (see attached image)".to_string()
            } else {
                "(see attached image)".to_string()
            };
        }
        return if is_error {
            "[tool error] (image omitted: model does not support images)".to_string()
        } else {
            "(image omitted: model does not support images)".to_string()
        };
    }

    if is_error {
        "[tool error] (no tool output)".to_string()
    } else {
        "(no tool output)".to_string()
    }
}

fn sanitize(text: &str) -> String {
    crate::utils::sanitize::sanitize_surrogates(text)
}

fn to_chat_messages(messages: &[Message], supports_images: bool) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();

    for msg in messages {
        match msg {
            Message::User(user) => match &user.content {
                UserMessageContent::Text(text) => {
                    result.push(Value::Map(vec![
                        ("role".to_string(), Value::String("user".to_string())),
                        ("content".to_string(), Value::String(sanitize(text))),
                    ]));
                }
                UserMessageContent::Blocks(blocks) => {
                    let had_images = blocks.iter().any(|item| matches!(item, Content::Image(_)));
                    let content: Vec<Value> = blocks
                        .iter()
                        .filter(|item| matches!(item, Content::Text(_)) || supports_images)
                        .map(|item| match item {
                            Content::Text(text) => Value::Map(vec![
                                ("type".to_string(), Value::String("text".to_string())),
                                ("text".to_string(), Value::String(sanitize(&text.text))),
                            ]),
                            Content::Image(image) => Value::Map(vec![
                                ("type".to_string(), Value::String("image_url".to_string())),
                                (
                                    "image_url".to_string(),
                                    Value::String(format!("data:{};base64,{}", image.mime_type, image.data)),
                                ),
                            ]),
                            _ => unreachable!("filtered to text/image"),
                        })
                        .collect();
                    if !content.is_empty() {
                        result.push(Value::Map(vec![
                            ("role".to_string(), Value::String("user".to_string())),
                            ("content".to_string(), Value::Array(content)),
                        ]));
                    } else if had_images && !supports_images {
                        result.push(Value::Map(vec![
                            ("role".to_string(), Value::String("user".to_string())),
                            (
                                "content".to_string(),
                                Value::String("(image omitted: model does not support images)".to_string()),
                            ),
                        ]));
                    }
                }
            },
            Message::Assistant(assistant) => {
                let mut content_parts: Vec<Value> = Vec::new();
                let mut tool_calls: Vec<Value> = Vec::new();

                for block in &assistant.content {
                    match block {
                        Content::Text(text) => {
                            if !text.text.trim().is_empty() {
                                content_parts.push(Value::Map(vec![
                                    ("type".to_string(), Value::String("text".to_string())),
                                    ("text".to_string(), Value::String(sanitize(&text.text))),
                                ]));
                            }
                        }
                        Content::Thinking(thinking) => {
                            if !thinking.thinking.trim().is_empty() {
                                content_parts.push(Value::Map(vec![
                                    ("type".to_string(), Value::String("thinking".to_string())),
                                    (
                                        "thinking".to_string(),
                                        Value::Array(vec![Value::Map(vec![
                                            ("type".to_string(), Value::String("text".to_string())),
                                            ("text".to_string(), Value::String(sanitize(&thinking.thinking))),
                                        ])]),
                                    ),
                                ]));
                            }
                        }
                        Content::ToolCall(tool_call) => {
                            tool_calls.push(Value::Map(vec![
                                ("id".to_string(), Value::String(tool_call.id.clone())),
                                ("type".to_string(), Value::String("function".to_string())),
                                (
                                    "function".to_string(),
                                    Value::Map(vec![
                                        ("name".to_string(), Value::String(tool_call.name.clone())),
                                        ("arguments".to_string(), Value::String(json_stringify(&tool_call.arguments))),
                                    ]),
                                ),
                                ("index".to_string(), Value::Number(0.0)),
                            ]));
                        }
                        Content::Image(_) => {}
                    }
                }

                let mut message_entries: Vec<(String, Value)> = vec![
                    ("role".to_string(), Value::String("assistant".to_string())),
                    ("prefix".to_string(), Value::Bool(false)),
                ];
                if !content_parts.is_empty() {
                    message_entries.push(("content".to_string(), Value::Array(content_parts.clone())));
                }
                if !tool_calls.is_empty() {
                    message_entries.push(("tool_calls".to_string(), Value::Array(tool_calls.clone())));
                }
                if !content_parts.is_empty() || !tool_calls.is_empty() {
                    result.push(Value::Map(message_entries));
                }
            }
            Message::ToolResult(tool_result) => {
                let text_result: Vec<String> = tool_result
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        Content::Text(text) => Some(sanitize(&text.text)),
                        _ => None,
                    })
                    .collect();
                let text_result = text_result.join("\n");
                let has_images = tool_result.content.iter().any(|part| matches!(part, Content::Image(_)));
                let tool_text = build_tool_result_text(&text_result, has_images, supports_images, tool_result.is_error);
                let mut tool_content: Vec<Value> = vec![Value::Map(vec![
                    ("type".to_string(), Value::String("text".to_string())),
                    ("text".to_string(), Value::String(tool_text)),
                ])];
                if supports_images {
                    for part in &tool_result.content {
                        if let Content::Image(image) = part {
                            tool_content.push(Value::Map(vec![
                                ("type".to_string(), Value::String("image_url".to_string())),
                                (
                                    "image_url".to_string(),
                                    Value::String(format!("data:{};base64,{}", image.mime_type, image.data)),
                                ),
                            ]));
                        }
                    }
                }
                result.push(Value::Map(vec![
                    ("role".to_string(), Value::String("tool".to_string())),
                    ("tool_call_id".to_string(), Value::String(tool_result.tool_call_id.clone())),
                    ("name".to_string(), Value::String(tool_result.tool_name.clone())),
                    ("content".to_string(), Value::Array(tool_content)),
                ]));
            }
        }
    }

    result
}

fn to_function_tools(tools: &[Tool]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let strict = resolve_json_schema_strict_sampling(tool, true)
                .ok()
                .flatten()
                .unwrap_or(false);
            Value::Map(vec![
                ("type".to_string(), Value::String("function".to_string())),
                (
                    "function".to_string(),
                    Value::Map(vec![
                        ("name".to_string(), Value::String(tool.name.clone())),
                        ("description".to_string(), Value::String(tool.description.clone())),
                        ("parameters".to_string(), tool.parameters.to_value()),
                        ("strict".to_string(), Value::Bool(strict)),
                    ]),
                ),
            ])
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Payload building
// ---------------------------------------------------------------------------

fn should_use_prompt_caching(options: Option<&MistralOptions>) -> bool {
    match options {
        Some(options) => {
            options.stream.cache_retention.as_deref() != Some("none") && options.stream.session_id.is_some()
        }
        None => false,
    }
}

fn has_header_override(headers: Option<&crate::types::ProviderHeaders>, target: &str) -> bool {
    headers.is_some_and(|headers| headers.iter().any(|(name, _)| name.eq_ignore_ascii_case(target)))
}

fn has_simple_header_override(headers: Option<&Vec<(String, String)>>, target: &str) -> bool {
    headers.is_some_and(|headers| headers.iter().any(|(name, _)| name.eq_ignore_ascii_case(target)))
}

fn apply_header_merge_simple(headers: &mut Vec<(String, String)>, overrides: Option<&Vec<(String, String)>>) {
    if let Some(overrides) = overrides {
        for (name, value) in overrides {
            if let Some(existing) = headers.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
                existing.1 = value.clone();
            } else {
                headers.push((name.clone(), value.clone()));
            }
        }
    }
}

fn apply_header_merge(headers: &mut Vec<(String, String)>, overrides: Option<&crate::types::ProviderHeaders>) {
    if let Some(overrides) = overrides {
        for (name, value) in overrides {
            if let Some(value) = value {
                if let Some(existing) = headers.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
                    existing.1 = value.clone();
                } else {
                    headers.push((name.clone(), value.clone()));
                }
            } else if let Some(index) = headers.iter().position(|(k, _)| k.eq_ignore_ascii_case(name)) {
                headers.remove(index);
            }
        }
    }
}

fn build_mistral_headers(
    model: &Model,
    api_key: &str,
    options: Option<&MistralOptions>,
    session_id: Option<&str>,
) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = vec![
        ("accept".to_string(), "text/event-stream".to_string()),
        ("authorization".to_string(), format!("Bearer {api_key}")),
        ("content-type".to_string(), "application/json".to_string()),
    ];

    apply_header_merge_simple(&mut headers, model.headers.as_ref());
    apply_header_merge(&mut headers, options.and_then(|o| o.stream.request.headers.as_ref()));

    let has_explicit_affinity = has_simple_header_override(model.headers.as_ref(), "x-affinity")
        || has_header_override(options.and_then(|o| o.stream.request.headers.as_ref()), "x-affinity");
    if should_use_prompt_caching(options) && !has_explicit_affinity {
        if let Some(session_id) = session_id {
            if let Some(existing) = headers.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case("x-affinity")) {
                existing.1 = session_id.to_string();
            } else {
                headers.push(("x-affinity".to_string(), session_id.to_string()));
            }
        }
    }

    headers
}

fn build_chat_payload(model: &Model, context: &Context, messages: Vec<Message>, options: Option<&MistralOptions>) -> Value {
    let supports_images = model.input.iter().any(|kind| kind == "image");
    let mut chat_messages = to_chat_messages(&messages, supports_images);

    if let Some(system_prompt) = &context.system_prompt {
        chat_messages.insert(
            0,
            Value::Map(vec![
                ("role".to_string(), Value::String("system".to_string())),
                ("content".to_string(), Value::String(sanitize(system_prompt))),
            ]),
        );
    }

    let mut entries: Vec<(String, Value)> = vec![
        ("model".to_string(), Value::String(model.id.clone())),
        ("stream".to_string(), Value::Bool(true)),
        ("messages".to_string(), Value::Array(chat_messages)),
    ];

    if let Some(tools) = &context.tools {
        if !tools.is_empty() {
            entries.push(("tools".to_string(), Value::Array(to_function_tools(tools))));
        }
    }
    if let Some(temperature) = options.and_then(|o| o.stream.temperature) {
        entries.push(("temperature".to_string(), Value::Number(temperature)));
    }
    if let Some(max_tokens) = options.and_then(|o| o.stream.max_tokens) {
        entries.push(("max_tokens".to_string(), Value::Number(max_tokens)));
    }
    if let Some(tool_choice) = options.and_then(|o| o.tool_choice.clone()) {
        entries.push(("tool_choice".to_string(), tool_choice.to_value()));
    }
    if let Some(prompt_mode) = options.and_then(|o| o.prompt_mode.clone()) {
        entries.push(("prompt_mode".to_string(), Value::String(prompt_mode)));
    }
    if let Some(reasoning_effort) = options.and_then(|o| o.reasoning_effort.clone()) {
        entries.push(("reasoning_effort".to_string(), Value::String(reasoning_effort)));
    }
    if should_use_prompt_caching(options) {
        if let Some(session_id) = options.and_then(|o| o.stream.session_id.clone()) {
            entries.push(("prompt_cache_key".to_string(), Value::String(session_id)));
        }
    }

    Value::Map(entries)
}

// ---------------------------------------------------------------------------
// Stream consumption
// ---------------------------------------------------------------------------

fn map_chat_stop_reason(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" => (StopReason::Stop, None),
        "length" | "model_length" => (StopReason::Length, None),
        "tool_calls" => (StopReason::ToolUse, None),
        "error" => (StopReason::Error, Some("Provider stopped with: error".to_string())),
        other => (StopReason::Error, Some(format!("Provider stopped with: {other}"))),
    }
}

/// Mirrors `consumeChatStream`: converts Mistral completion events into
/// assistant message stream events.
fn consume_chat_stream(
    model: &Model,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    events: Vec<MistralCompletionEvent>,
) {
    enum CurrentBlock {
        Text(TextContent),
        Thinking(ThinkingContent),
    }

    let mut current_block: Option<CurrentBlock> = None;
    let mut tool_blocks_by_key: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut partial_args_by_key: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    let finish_current_block = |stream: &AssistantMessageEventStream,
                                output: &AssistantMessage,
                                block: Option<&CurrentBlock>| {
        let Some(block) = block else { return };
        let content_index = output.content.len() - 1;
        match block {
            CurrentBlock::Text(text) => {
                stream.push(crate::types::AssistantMessageEvent::TextEnd {
                    content_index: content_index as f64,
                    content: text.text.clone(),
                    partial: output.clone(),
                });
            }
            CurrentBlock::Thinking(thinking) => {
                stream.push(crate::types::AssistantMessageEvent::ThinkingEnd {
                    content_index: content_index as f64,
                    content: thinking.thinking.clone(),
                    partial: output.clone(),
                });
            }
        }
    };

    for event in events {
        let chunk = event;
        // Keep the first non-empty response id.
        if output.response_id.is_none() {
            output.response_id = chunk.id;
        }

        if let Some(usage) = &chunk.usage {
            output.usage.input = (usage.prompt_tokens - usage.cached_tokens).max(0.0);
            output.usage.output = usage.completion_tokens;
            output.usage.cache_read = usage.cached_tokens;
            output.usage.cache_write = 0.0;
            output.usage.total_tokens = if usage.total_tokens != 0.0 {
                usage.total_tokens
            } else {
                output.usage.input + output.usage.output + output.usage.cache_read + output.usage.cache_write
            };
            calculate_cost(model, &mut output.usage);
        }

        let Some(choice) = chunk.choices.first() else {
            continue;
        };

        if let Some(finish_reason) = &choice.finish_reason {
            output.raw_stop_reason = Some(finish_reason.clone());
            let (stop_reason, error_message) = map_chat_stop_reason(finish_reason);
            output.stop_reason = stop_reason;
            if let Some(error_message) = error_message {
                output.error_message = Some(error_message);
            }
        }

        if let Some(content) = &choice.delta.content {
            let content_items = match content {
                MistralContentItems::Text(text) => vec![MistralContentChunk::Text { text: text.clone() }],
                MistralContentItems::Chunks(chunks) => chunks.clone(),
            };
            for item in content_items {
                match item {
                    MistralContentChunk::Text { text } => {
                        let text_delta = sanitize(&text);
                        if !matches!(current_block, Some(CurrentBlock::Text(_))) {
                            finish_current_block(stream, output, current_block.as_ref());
                            output.content.push(Content::Text(TextContent {
                                text: String::new(),
                                text_signature: None,
                            }));
                            current_block = Some(CurrentBlock::Text(TextContent {
                                text: String::new(),
                                text_signature: None,
                            }));
                            stream.push(crate::types::AssistantMessageEvent::TextStart {
                                content_index: (output.content.len() - 1) as f64,
                                partial: output.clone(),
                            });
                        }
                        if let Some(CurrentBlock::Text(text)) = &mut current_block {
                            text.text.push_str(&text_delta);
                        }
                        if let Some(Content::Text(block)) = output.content.last_mut() {
                            if let Some(CurrentBlock::Text(text)) = &current_block {
                                block.text = text.text.clone();
                            }
                        }
                        stream.push(crate::types::AssistantMessageEvent::TextDelta {
                            content_index: (output.content.len() - 1) as f64,
                            delta: text_delta,
                            partial: output.clone(),
                        });
                    }
                    MistralContentChunk::Thinking { thinking } => {
                        let delta_text = thinking.join("");
                        let thinking_delta = sanitize(&delta_text);
                        if thinking_delta.is_empty() {
                            continue;
                        }
                        if !matches!(current_block, Some(CurrentBlock::Thinking(_))) {
                            finish_current_block(stream, output, current_block.as_ref());
                            output.content.push(Content::Thinking(ThinkingContent {
                                thinking: String::new(),
                                thinking_signature: None,
                                redacted: None,
                            }));
                            current_block = Some(CurrentBlock::Thinking(ThinkingContent {
                                thinking: String::new(),
                                thinking_signature: None,
                                redacted: None,
                            }));
                            stream.push(crate::types::AssistantMessageEvent::ThinkingStart {
                                content_index: (output.content.len() - 1) as f64,
                                partial: output.clone(),
                            });
                        }
                        if let Some(CurrentBlock::Thinking(thinking)) = &mut current_block {
                            thinking.thinking.push_str(&thinking_delta);
                        }
                        if let Some(Content::Thinking(block)) = output.content.last_mut() {
                            if let Some(CurrentBlock::Thinking(thinking)) = &current_block {
                                block.thinking = thinking.thinking.clone();
                            }
                        }
                        stream.push(crate::types::AssistantMessageEvent::ThinkingDelta {
                            content_index: (output.content.len() - 1) as f64,
                            delta: thinking_delta,
                            partial: output.clone(),
                        });
                    }
                    MistralContentChunk::Image { image_url } => {
                        // Image chunks in streamed content are not emitted by
                        // Mistral; kept for type completeness.
                        let _ = image_url;
                    }
                }
            }
        }

        for tool_call in &choice.delta.tool_calls {
            if current_block.is_some() {
                finish_current_block(stream, output, current_block.as_ref());
                current_block = None;
            }
            let call_id = match &tool_call.id {
                Some(id) if id != "null" => id.clone(),
                _ => derive_mistral_tool_call_id(&format!("toolcall:{}", tool_call.index.unwrap_or(0.0)), 0),
            };
            let key = format!("{call_id}:{}", tool_call.index.unwrap_or(0.0));
            let existing_index = tool_blocks_by_key.get(&key).copied();

            let block_index = match existing_index {
                Some(index) => index,
                None => {
                    let tool_block = ToolCall {
                        id: call_id.clone(),
                        name: tool_call.name.clone(),
                        arguments: Value::Map(Vec::new()),
                        thought_signature: None,
                        namespace: None,
                    };
                    output.content.push(Content::ToolCall(tool_block));
                    let index = output.content.len() - 1;
                    tool_blocks_by_key.insert(key.clone(), index);
                    stream.push(crate::types::AssistantMessageEvent::ToolCallStart {
                        content_index: index as f64,
                        partial: output.clone(),
                    });
                    index
                }
            };

            let args_delta = tool_call.arguments.clone();
            let accumulated = {
                let entry = partial_args_by_key.entry(key.clone()).or_default();
                entry.push_str(&args_delta);
                entry.clone()
            };
            let parsed_arguments = parse_streaming_json(Some(&accumulated));
            if let Some(Content::ToolCall(block)) = output.content.get_mut(block_index) {
                block.arguments = parsed_arguments;
            }
            stream.push(crate::types::AssistantMessageEvent::ToolCallDelta {
                content_index: block_index as f64,
                delta: args_delta,
                partial: output.clone(),
            });
        }
    }

    finish_current_block(stream, output, current_block.as_ref());
    let tool_indexes: Vec<usize> = tool_blocks_by_key.values().copied().collect();
    for index in tool_indexes {
        let Content::ToolCall(block) = &mut output.content[index] else {
            continue;
        };
        let finalized = block.clone();
        stream.push(crate::types::AssistantMessageEvent::ToolCallEnd {
            content_index: index as f64,
            tool_call: finalized,
            partial: output.clone(),
        });
    }
    let _ = partial_args_by_key;
}

// ---------------------------------------------------------------------------
// Stream reading (Mistral custom boundaries, byte-level)
// ---------------------------------------------------------------------------

/// Reads a byte stream, splitting on Mistral's custom event boundaries and
/// invoking `on_event` with each raw event payload.
fn read_mistral_events(
    mut reader: impl Read,
    mut on_event: impl FnMut(Result<Option<MistralEvent>, String>),
) {
    let mut buffer: Vec<u8> = Vec::new();
    let mut scratch = [0u8; 8192];
    loop {
        match reader.read(&mut scratch) {
            Ok(0) => break,
            Ok(n) => {
                buffer.extend_from_slice(&scratch[..n]);
                drain_events(&mut buffer, &mut on_event);
            }
            Err(_) => break,
        }
    }
    // Tail: process any remaining buffered event.
    drain_events(&mut buffer, &mut on_event);
    let tail = String::from_utf8_lossy(&buffer).to_string();
    if !tail.trim().is_empty() {
        if let Ok(Some(event)) = parse_mistral_event(&tail) {
            match event {
                MistralEvent::Done => on_event(Ok(Some(MistralEvent::Done))),
                MistralEvent::Data(data) => on_event(Ok(Some(MistralEvent::Data(data)))),
            }
        }
    }
}

fn drain_events(buffer: &mut Vec<u8>, on_event: &mut impl FnMut(Result<Option<MistralEvent>, String>)) {
    loop {
        let Some((index, length)) = find_mistral_event_boundary(buffer) else {
            break;
        };
        let raw_bytes: Vec<u8> = buffer.drain(..index).collect();
        let raw = String::from_utf8_lossy(&raw_bytes).to_string();
        // Consume the boundary itself.
        buffer.drain(..length);
        on_event(parse_mistral_event(&raw));
    }
}

// ---------------------------------------------------------------------------
// Public stream entry points
// ---------------------------------------------------------------------------

/// Stream responses from the native Mistral Chat Completions endpoint.
/// Spawns a worker thread (mirroring the JS async IIFE) and feeds the
/// returned stream.
pub fn stream(
    model: &Model,
    context: &Context,
    options: Option<&MistralOptions>,
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
        let mut output = create_output(&model);
        let result = (|| -> Result<(), String> {
            let api_key = api_key.ok_or_else(|| format!("No API key for provider: {}", model.provider))?;

            let normalizer = std::cell::RefCell::new(MistralToolCallIdNormalizer::new());
            let transformed_messages = transform_messages(
                context.messages.clone(),
                &model,
                Some(&|id: &str, _model: &Model, _source: &AssistantMessage| {
                    normalizer.borrow_mut().normalize(id)
                }),
            );

            let payload = build_chat_payload(&model, &context, transformed_messages, options.as_ref());
            let session_id = options.as_ref().and_then(|o| o.stream.session_id.clone());
            let headers = build_mistral_headers(&model, &api_key, options.as_ref(), session_id.as_deref());
            let base_url = model.base_url.trim_end_matches('/');
            let url = format!("{base_url}/v1/chat/completions");

            let timeout_ms = options
                .as_ref()
                .and_then(|o| o.stream.request.timeout_ms)
                .unwrap_or(DEFAULT_MISTRAL_TIMEOUT_MS);

            let response = client
                .post_json(&url, &headers, &payload, Some(timeout_ms))
                .map_err(|error| format_mistral_error(&error))?;

            stream.push(crate::types::AssistantMessageEvent::Start {
                partial: output.clone(),
            });

            // Read the stream and collect events (mirrors the async
            // generator in JS; events are processed in order).
            let mut events: Vec<MistralCompletionEvent> = Vec::new();
            let mut done = false;
            read_mistral_events(response.reader, &mut |event_result| {
                if done {
                    return;
                }
                match event_result {
                    Ok(Some(MistralEvent::Done)) => done = true,
                    Ok(Some(MistralEvent::Data(event))) => events.push(event),
                    Ok(None) => {}
                    Err(_) => {}
                }
            });
            let _ = done;

            consume_chat_stream(&model, &mut output, &stream, events);

            if output.stop_reason == StopReason::Pending {
                return Err("Mistral stream ended without a finish reason".to_string());
            }
            if output.stop_reason == StopReason::Aborted || output.stop_reason == StopReason::Error {
                return Err(output.error_message.clone().unwrap_or_else(|| "An unknown error occurred".to_string()));
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

/// Maps provider-agnostic `SimpleStreamOptions` to Mistral options.
pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<&SimpleStreamOptions>,
    api_key: Option<&str>,
    client: &HttpClient,
) -> AssistantMessageEventStream {
    if api_key.is_none() {
        // Mirrors the JS throw for a missing API key.
        let stream = AssistantMessageEventStream::new();
        let mut output = create_output(model);
        output.stop_reason = StopReason::Error;
        output.error_message = Some(format!("No API key for provider: {}", model.provider));
        stream.push(crate::types::AssistantMessageEvent::Error {
            reason: "error".to_string(),
            error: output,
        });
        stream.end(None);
        return stream;
    }

    let base = build_base_options(model, context, options, api_key);
    let clamped_reasoning = options
        .and_then(|o| o.reasoning.as_deref())
        .map(|level| clamp_thinking_level(model, level));
    let reasoning = match clamped_reasoning.as_deref() {
        Some("off") => None,
        Some(level) => Some(level.to_string()),
        None => None,
    };
    let should_use_reasoning = model.reasoning && reasoning.is_some();

    let mistral_options = MistralOptions {
        stream: base,
        prompt_mode: if should_use_reasoning && uses_prompt_mode_reasoning(model) {
            Some("reasoning".to_string())
        } else {
            None
        },
        reasoning_effort: if should_use_reasoning && uses_reasoning_effort(model) {
            map_reasoning_effort(model, reasoning.as_deref().unwrap_or("high"))
        } else {
            None
        },
        ..MistralOptions::default()
    };
    stream(model, context, Some(&mistral_options), api_key, client)
}

fn create_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: vec![],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
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
        timestamp: now_ms(),
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as f64
}

fn uses_reasoning_effort(model: &Model) -> bool {
    matches!(
        model.id.as_str(),
        "mistral-small-2603" | "mistral-small-latest" | "mistral-medium-3.5"
    )
}

fn uses_prompt_mode_reasoning(model: &Model) -> bool {
    model.reasoning && !uses_reasoning_effort(model)
}

fn map_reasoning_effort(model: &Model, level: &str) -> Option<String> {
    let mapped = model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.iter().find(|(key, _)| key == level))
        .and_then(|(_, value)| value.clone());
    Some(mapped.unwrap_or_else(|| "high".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_data_lines_and_done() {
        // [DONE] marker.
        let done = parse_mistral_event("data: [DONE]").unwrap();
        assert!(matches!(done, Some(MistralEvent::Done)));

        // No data -> None.
        assert!(matches!(parse_mistral_event("event: x").unwrap(), None));

        // Standard event with choices.
        let raw = "data: {\"id\":\"chunk-1\",\"choices\":[{\"finish_reason\":null,\"delta\":{\"content\":\"Hello\"}}]}";
        let parsed = parse_mistral_event(raw).unwrap();
        match parsed {
            Some(MistralEvent::Data(event)) => {
                assert_eq!(event.id.as_deref(), Some("chunk-1"));
                assert_eq!(event.choices.len(), 1);
                let choice = &event.choices[0];
                assert_eq!(choice.finish_reason, None);
                match &choice.delta.content {
                    Some(MistralContentItems::Text(text)) => assert_eq!(text, "Hello"),
                    _ => panic!("expected text content"),
                }
            }
            _ => panic!("expected data event"),
        }
    }

    #[test]
    fn rejects_invalid_events() {
        // choices must be an array.
        let error = parse_mistral_event("data: {\"choices\": {}}").unwrap_err();
        assert_eq!(error, "Invalid Mistral streaming event");
        // Non-object payload.
        assert!(parse_mistral_event("data: 42").is_err());
    }

    #[test]
    fn extracts_cached_tokens_from_all_field_shapes() {
        let cases: Vec<(Vec<(&str, f64)>, f64)> = vec![
            (vec![("prompt_tokens", 100.0)], 0.0),
            (vec![("prompt_tokens", 100.0), ("prompt_tokens_details", 0.0)], 0.0),
        ];
        for (fields, expected) in cases {
            let entries: Vec<(String, Value)> = fields
                .iter()
                .map(|(k, v)| (k.to_string(), Value::Number(*v)))
                .collect();
            let cached = extract_cached_tokens(&entries);
            assert_eq!(cached, expected);
        }

        // Nested shapes.
        let entries = vec![
            ("prompt_tokens".to_string(), Value::Number(100.0)),
            (
                "prompt_tokens_details".to_string(),
                Value::Map(vec![("cached_tokens".to_string(), Value::Number(40.0))]),
            ),
        ];
        assert_eq!(extract_cached_tokens(&entries), 40.0);

        let entries = vec![
            ("promptTokensDetails".to_string(), Value::Map(vec![("cachedTokens".to_string(), Value::Number(25.0))])),
        ];
        assert_eq!(extract_cached_tokens(&entries), 25.0);

        let entries = vec![("num_cached_tokens".to_string(), Value::Number(30.0))];
        assert_eq!(extract_cached_tokens(&entries), 30.0);
    }

    #[test]
    fn usage_is_clamped_to_prompt_tokens() {
        let entries = vec![
            ("prompt_tokens".to_string(), Value::Number(10.0)),
            (
                "prompt_tokens_details".to_string(),
                Value::Map(vec![("cached_tokens".to_string(), Value::Number(500.0))]),
            ),
        ];
        let usage = parse_usage(&entries);
        assert_eq!(usage.cached_tokens, 10.0, "cached clamps to prompt");
    }

    #[test]
    fn finds_mistral_event_boundaries() {
        // Standard double newline.
        let buffer = b"data: x\n\ndata: y";
        let (index, length) = find_mistral_event_boundary(buffer).unwrap();
        assert_eq!(&buffer[index..index + length], b"\n\n");

        // CRLF variant.
        let buffer = b"data: x\r\n\r\ndata: y";
        let (index, length) = find_mistral_event_boundary(buffer).unwrap();
        assert_eq!(&buffer[index..index + length], b"\r\n\r\n");

        // No boundary yet.
        assert!(find_mistral_event_boundary(b"data: partial").is_none());
    }

    #[test]
    fn derives_9_char_tool_call_ids() {
        // Already normalized alphanumeric id of length 9 is kept.
        let id = derive_mistral_tool_call_id("abc123xyz", 0);
        assert_eq!(id, "abc123xyz");
        assert_eq!(id.chars().count(), MISTRAL_TOOL_CALL_ID_LENGTH);

        // Other ids are hashed to 9 alphanumeric chars.
        let hashed = derive_mistral_tool_call_id("call_1|with|specials", 0);
        assert_eq!(hashed.chars().count(), MISTRAL_TOOL_CALL_ID_LENGTH);
        assert!(hashed.chars().all(|c| c.is_ascii_alphanumeric()));

        // Different attempts produce different ids (collision escape).
        let a = derive_mistral_tool_call_id("same-seed", 0);
        let b = derive_mistral_tool_call_id("same-seed", 1);
        assert_ne!(a, b);
    }

    #[test]
    fn normalizer_is_bijective() {
        let mut normalizer = MistralToolCallIdNormalizer::new();
        let a1 = normalizer.normalize("call-a");
        let a2 = normalizer.normalize("call-a");
        assert_eq!(a1, a2, "same input maps to same output");
        let b1 = normalizer.normalize("call-b");
        assert_ne!(a1, b1, "different inputs map to different outputs");
    }

    #[test]
    fn maps_stop_reasons() {
        assert_eq!(map_chat_stop_reason("stop"), (StopReason::Stop, None));
        assert_eq!(map_chat_stop_reason("length"), (StopReason::Length, None));
        assert_eq!(map_chat_stop_reason("model_length"), (StopReason::Length, None));
        assert_eq!(map_chat_stop_reason("tool_calls"), (StopReason::ToolUse, None));
        assert_eq!(
            map_chat_stop_reason("error"),
            (StopReason::Error, Some("Provider stopped with: error".to_string()))
        );
        let (reason, message) = map_chat_stop_reason("weird");
        assert_eq!(reason, StopReason::Error);
        assert_eq!(message, Some("Provider stopped with: weird".to_string()));
    }

    #[test]
    fn builds_tool_result_text() {
        assert_eq!(build_tool_result_text("done", false, false, false), "done");
        assert_eq!(build_tool_result_text("", false, false, true), "[tool error] (no tool output)");
        assert_eq!(build_tool_result_text("", true, false, false), "(image omitted: model does not support images)");
        assert_eq!(build_tool_result_text("", true, true, false), "(see attached image)");
        assert_eq!(
            build_tool_result_text("text", true, false, true),
            "[tool error] text\n[tool image omitted: model does not support images]"
        );
    }

    fn test_model() -> Model {
        Model {
            id: "mistral-medium".to_string(),
            name: "mistral-medium".to_string(),
            api: "mistral-conversations".to_string(),
            provider: "mistral".to_string(),
            base_url: "https://api.mistral.ai".to_string(),
            reasoning: true,
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
            context_window: 32_000.0,
            max_tokens: 2048.0,
            sampling_params: None,
            headers: None,
            compat: None,
        }
    }

    #[test]
    fn consumes_a_text_and_tool_call_stream() {
        // Simulate the events a Mistral stream would produce.
        let events = vec![
            MistralCompletionEvent {
                id: Some("resp-1".to_string()),
                usage: Some(MistralUsage {
                    prompt_tokens: 100.0,
                    completion_tokens: 5.0,
                    total_tokens: 105.0,
                    cached_tokens: 40.0,
                }),
                choices: vec![MistralChoice {
                    finish_reason: None,
                    delta: MistralDelta {
                        content: Some(MistralContentItems::Text("Hello ".to_string())),
                        tool_calls: vec![],
                    },
                }],
            },
            MistralCompletionEvent {
                id: None,
                usage: None,
                choices: vec![MistralChoice {
                    finish_reason: None,
                    delta: MistralDelta {
                        content: Some(MistralContentItems::Text("world".to_string())),
                        tool_calls: vec![],
                    },
                }],
            },
            MistralCompletionEvent {
                id: None,
                usage: None,
                choices: vec![MistralChoice {
                    finish_reason: Some("stop".to_string()),
                    delta: MistralDelta {
                        content: None,
                        tool_calls: vec![],
                    },
                }],
            },
        ];

        let model = test_model();
        let mut output = create_output(&model);
        let stream = AssistantMessageEventStream::new();
        consume_chat_stream(&model, &mut output, &stream, events);

        assert_eq!(output.response_id.as_deref(), Some("resp-1"));
        assert_eq!(output.stop_reason, StopReason::Stop);
        assert_eq!(output.raw_stop_reason.as_deref(), Some("stop"));
        assert_eq!(output.usage.input, 60.0);
        assert_eq!(output.usage.cache_read, 40.0);
        assert_eq!(output.usage.output, 5.0);
        assert_eq!(output.content.len(), 1);
        match &output.content[0] {
            Content::Text(text) => assert_eq!(text.text, "Hello world"),
            other => panic!("expected text block, got {other:?}"),
        }
    }

    #[test]
    fn consumes_a_tool_call_stream() {
        let events = vec![MistralCompletionEvent {
            id: None,
            usage: None,
            choices: vec![MistralChoice {
                finish_reason: None,
                delta: MistralDelta {
                    content: None,
                    tool_calls: vec![MistralToolCallDelta {
                        id: Some("call_123".to_string()),
                        index: Some(0.0),
                        name: "read".to_string(),
                        arguments: "{\"path\":\"".to_string(),
                    }],
                },
            }],
        }];

        let model = test_model();
        let mut output = create_output(&model);
        let stream = AssistantMessageEventStream::new();
        consume_chat_stream(&model, &mut output, &stream, events);

        assert_eq!(output.content.len(), 1);
        match &output.content[0] {
            Content::ToolCall(tool_call) => {
                assert_eq!(tool_call.id, "call_123");
                assert_eq!(tool_call.name, "read");
                assert_eq!(tool_call.arguments, Value::Map(vec![("path".to_string(), Value::String("".to_string()))]));
            }
            other => panic!("expected tool call, got {other:?}"),
        }
    }
}
