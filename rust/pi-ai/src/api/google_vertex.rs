//! Google Vertex AI provider, port of `packages/ai/src/api/google-vertex.ts`.
//!
//! Requests go to the Vertex AI generateContent endpoint over SSE. Two auth
//! modes mirror the `@google/genai` SDK:
//! - API key: `x-goog-api-key` header (full support);
//! - Application Default Credentials: the JS SDK uses google-auth-library
//!   (OAuth token exchange with JWT signing). A zero-dependency Rust port
//!   cannot sign JWTs, so this adapter reads a pre-fetched token from the
//!   `GOOGLE_ACCESS_TOKEN` provider env when no API key is set. When only
//!   `GOOGLE_APPLICATION_CREDENTIALS` is present, the request fails with a
//!   clear error (wire in an external token provider to close that gap).

use pi_protocol::Value;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::api::google_shared::{
    convert_messages, convert_tools, is_thinking_part, map_stop_reason,
    resolve_google_function_calling_mode, retain_thought_signature, supports_google_strict_tool_sampling,
};
use crate::api::simple_options::build_base_options;
use crate::event_stream::AssistantMessageEventStream;
use crate::http::client::HttpClient;
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::types::{
    AssistantMessage, Content, Context, Model, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
    ThinkingContent, ThinkingBudgets, ToolCall, Usage, UsageCost,
};
use crate::utils::error_body::{format_provider_error, NormalizedProviderError};
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{retry_provider_request, ProviderError, ProviderRetryOptions};

const API_VERSION: &str = "v1";
const GCP_VERTEX_CREDENTIALS_MARKER: &str = "gcp-vertex-credentials";

pub const THINKING_LEVEL_MAP: [(&str, &str); 5] = [
    ("THINKING_LEVEL_UNSPECIFIED", "THINKING_LEVEL_UNSPECIFIED"),
    ("MINIMAL", "MINIMAL"),
    ("LOW", "LOW"),
    ("MEDIUM", "MEDIUM"),
    ("HIGH", "HIGH"),
];

/// Counter for generating unique tool call IDs.
static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Vertex-specific stream options.
#[derive(Clone, Debug, Default)]
pub struct GoogleVertexOptions {
    pub stream: StreamOptions,
    pub tool_choice: Option<String>,
    pub thinking: Option<VertexThinking>,
    pub project: Option<String>,
    pub location: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VertexThinking {
    pub enabled: bool,
    /// -1 for dynamic, 0 to disable
    pub budget_tokens: Option<f64>,
    pub level: Option<String>,
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as f64
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
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
    }
}

fn output_message(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: vec![],
        api: "google-vertex".to_string(),
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

// ---------------------------------------------------------------------------
// Options resolution
// ---------------------------------------------------------------------------

fn is_placeholder_api_key(api_key: &str) -> bool {
    api_key.starts_with('<') && api_key.ends_with('>')
}

fn resolve_api_key(options: Option<&GoogleVertexOptions>) -> Option<String> {
    let api_key = options.and_then(|o| o.stream.request.api_key.as_deref())?.trim();
    if api_key.is_empty()
        || api_key == GCP_VERTEX_CREDENTIALS_MARKER
        || is_placeholder_api_key(api_key)
    {
        return None;
    }
    Some(api_key.to_string())
}

fn resolve_project(options: Option<&GoogleVertexOptions>) -> Result<String, String> {
    let project = options
        .and_then(|o| o.project.clone())
        .or_else(|| {
            get_provider_env_value("GOOGLE_CLOUD_PROJECT", options.and_then(|o| o.stream.request.env.as_ref()))
        })
        .or_else(|| get_provider_env_value("GCLOUD_PROJECT", options.and_then(|o| o.stream.request.env.as_ref())));
    project.ok_or_else(|| {
        "Vertex AI requires a project ID. Set GOOGLE_CLOUD_PROJECT/GCLOUD_PROJECT or pass project in options."
            .to_string()
    })
}

fn resolve_location(options: Option<&GoogleVertexOptions>) -> Result<String, String> {
    let location = options
        .and_then(|o| o.location.clone())
        .or_else(|| get_provider_env_value("GOOGLE_CLOUD_LOCATION", options.and_then(|o| o.stream.request.env.as_ref())));
    location.ok_or_else(|| {
        "Vertex AI requires a location. Set GOOGLE_CLOUD_LOCATION or pass location in options.".to_string()
    })
}

fn resolve_custom_base_url(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() || trimmed.contains("{location}") {
        return None;
    }
    Some(trimmed.to_string())
}

fn base_url_includes_api_version(base_url: &str) -> bool {
    // Mirrors the JS regex on URL pathname segments: /^v\d+(?:beta\d*)?$/
    let path = match base_url.split_once("://") {
        Some((_, rest)) => match rest.split_once('/') {
            Some((_, path)) => path,
            None => "",
        },
        None => base_url,
    };
    path.split('/').any(|part| {
        let part = part.trim_end_matches('/');
        if !part.starts_with('v') {
            return false;
        }
        let rest = &part[1..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return false;
        }
        let remainder = &rest[digits.len()..];
        remainder == "beta"
            || remainder.is_empty()
            || (remainder.starts_with("beta")
                && remainder[4..].chars().all(|c| c.is_ascii_digit()))
    })
}

// ---------------------------------------------------------------------------
// URL construction (mirrors the @google/genai SDK)
// ---------------------------------------------------------------------------

/// Multi-regional Vertex locations use the rep.googleapis.com host.
fn is_multi_regional_location(location: &str) -> bool {
    matches!(
        location,
        "us" | "eu" | "europe" | "asia" | "global"
    )
}

fn construct_url(
    model_id: &str,
    project: Option<&str>,
    location: Option<&str>,
    api_key_mode: bool,
    custom_base_url: Option<&str>,
) -> String {
    let api_version = API_VERSION;

    let base_url = match custom_base_url {
        Some(custom) => custom.trim_end_matches('/').to_string(),
        None if api_key_mode => "https://aiplatform.googleapis.com".to_string(),
        None => {
            let location = location.expect("location resolved before URL construction");
            if is_multi_regional_location(location) {
                format!("https://aiplatform.{location}.rep.googleapis.com")
            } else {
                format!("https://{location}-aiplatform.googleapis.com")
            }
        }
    };

    let version_segment = if base_url_includes_api_version(&base_url) {
        ""
    } else {
        api_version
    };

    // Model path transformation (tModel, Vertex mode).
    let model_path = if model_id.starts_with("publishers/")
        || model_id.starts_with("projects/")
        || model_id.starts_with("models/")
    {
        model_id.to_string()
    } else if model_id.contains('/') {
        let parts: Vec<&str> = model_id.splitn(2, '/').collect();
        format!("publishers/{}/models/{}", parts[0], parts[1])
    } else {
        format!("publishers/google/models/{model_id}")
    };

    let path = format!("{model_path}:streamGenerateContent?alt=sse");

    let mut segments: Vec<String> = vec![base_url];
    if !version_segment.is_empty() {
        segments.push(version_segment.to_string());
    }
    // Project/location prefix only for the default (non-custom-baseUrl) mode.
    if custom_base_url.is_none() && !api_key_mode {
        if let (Some(project), Some(location)) = (project, location) {
            segments.push(format!("projects/{project}/locations/{location}"));
        }
    }
    segments.push(path);
    segments.join("/")
}

// ---------------------------------------------------------------------------
// Request assembly
// ---------------------------------------------------------------------------

fn format_vertex_error(error: &ProviderError) -> String {
    let normalized = NormalizedProviderError::new(error.message.clone(), error.status, None);
    format_provider_error(&normalized, None)
}

fn is_gemini3_pro_model(model_id: &str) -> bool {
    let lower = model_id.to_ascii_lowercase();
    lower.contains("gemini-3-pro")
}

fn is_gemini3_flash_model(model_id: &str) -> bool {
    let lower = model_id.to_ascii_lowercase();
    lower.contains("gemini-3-flash") || lower == "gemini-flash-latest" || lower == "gemini-flash-lite-latest"
}

/// Google docs: Gemini 3.1 Pro cannot disable thinking, and Gemini 3
/// Flash / Flash-Lite do not support full thinking-off either. For Gemini 3
/// models use the lowest supported thinkingLevel without includeThoughts.
fn get_disabled_thinking_config(model_id: &str) -> Value {
    if is_gemini3_pro_model(model_id) {
        return Value::Map(vec![("thinkingLevel".to_string(), Value::String("LOW".to_string()))]);
    }
    if is_gemini3_flash_model(model_id) {
        return Value::Map(vec![("thinkingLevel".to_string(), Value::String("MINIMAL".to_string()))]);
    }
    // Gemini 2.x supports disabling via thinkingBudget = 0.
    Value::Map(vec![("thinkingBudget".to_string(), Value::Number(0.0))])
}

fn get_gemini3_thinking_level(effort: &str, model_id: &str) -> String {
    if is_gemini3_pro_model(model_id) {
        return match effort {
            "minimal" | "low" => "LOW".to_string(),
            _ => "HIGH".to_string(),
        };
    }
    match effort {
        "minimal" => "MINIMAL".to_string(),
        "low" => "LOW".to_string(),
        "medium" => "MEDIUM".to_string(),
        _ => "HIGH".to_string(),
    }
}

fn get_google_budget(model_id: &str, effort: &str, custom_budgets: Option<&ThinkingBudgets>) -> f64 {
    let custom = match effort {
        "minimal" => custom_budgets.and_then(|b| b.minimal),
        "low" => custom_budgets.and_then(|b| b.low),
        "medium" => custom_budgets.and_then(|b| b.medium),
        _ => custom_budgets.and_then(|b| b.high),
    };
    if let Some(custom) = custom {
        return custom;
    }

    if model_id.contains("2.5-pro") {
        return match effort {
            "minimal" => 128.0,
            "low" => 2048.0,
            "medium" => 8192.0,
            _ => 32768.0,
        };
    }

    if model_id.contains("2.5-flash") {
        return match effort {
            "minimal" => 128.0,
            "low" => 2048.0,
            "medium" => 8192.0,
            _ => 24576.0,
        };
    }

    -1.0
}

/// Assembles the Vertex GenerateContent request body.
fn build_params(
    model: &Model,
    context: &Context,
    options: Option<&GoogleVertexOptions>,
) -> Result<Value, String> {
    let contents = convert_messages(model, context);

    let mut generation_config: Vec<(String, Value)> = Vec::new();
    if let Some(temperature) = options.and_then(|o| o.stream.temperature) {
        generation_config.push(("temperature".to_string(), Value::Number(temperature)));
    }
    if let Some(max_tokens) = options.and_then(|o| o.stream.max_tokens) {
        generation_config.push(("maxOutputTokens".to_string(), Value::Number(max_tokens)));
    }

    let function_calling_mode = match context.tools.as_deref() {
        Some(tools) if !tools.is_empty() => resolve_google_function_calling_mode(
            tools,
            options.and_then(|o| o.tool_choice.as_deref()),
            supports_google_strict_tool_sampling(&model.id),
        ),
        _ => None,
    };

    if let Some(thinking) = options.and_then(|o| o.thinking.as_ref()) {
        if thinking.enabled && model.reasoning {
            let mut thinking_entries = vec![("includeThoughts".to_string(), Value::Bool(true))];
            if let Some(level) = &thinking.level {
                let mapped = THINKING_LEVEL_MAP
                    .iter()
                    .find(|(key, _)| key == level)
                    .map(|(_, value)| *value)
                    .unwrap_or(level.as_str());
                thinking_entries.push(("thinkingLevel".to_string(), Value::String(mapped.to_string())));
            } else if let Some(budget) = thinking.budget_tokens {
                thinking_entries.push(("thinkingBudget".to_string(), Value::Number(budget)));
            }
            generation_config.push((
                "thinkingConfig".to_string(),
                Value::Map(thinking_entries),
            ));
        } else if model.reasoning && !thinking.enabled {
            generation_config.push((
                "thinkingConfig".to_string(),
                get_disabled_thinking_config(&model.id),
            ));
        }
    }

    let mut body_entries: Vec<(String, Value)> = vec![(
        "contents".to_string(),
        Value::Array(contents.iter().map(|content| content.to_value()).collect()),
    )];
    if !generation_config.is_empty() {
        body_entries.push(("generationConfig".to_string(), Value::Map(generation_config)));
    }
    if let Some(system_prompt) = &context.system_prompt {
        body_entries.push((
            "systemInstruction".to_string(),
            Value::Map(vec![(
                "parts".to_string(),
                Value::Array(vec![Value::Map(vec![(
                    "text".to_string(),
                    Value::String(crate::utils::sanitize::sanitize_surrogates(system_prompt)),
                )])]),
            )]),
        ));
    }
    if let Some(tools) = context.tools.as_deref() {
        if !tools.is_empty() {
            if let Some(converted) = convert_tools(tools, false) {
                body_entries.push(("tools".to_string(), converted));
            }
        }
    }
    if let Some(mode) = function_calling_mode {
        body_entries.push((
            "toolConfig".to_string(),
            Value::Map(vec![(
                "functionCallingConfig".to_string(),
                Value::Map(vec![("mode".to_string(), Value::String(mode))]),
            )]),
        ));
    }

    Ok(Value::Map(body_entries))
}

// ---------------------------------------------------------------------------
// Stream processing
// ---------------------------------------------------------------------------

struct CurrentBlock {
    is_thinking: bool,
    text: String,
    signature: Option<String>,
}

/// Pushes the end event for a finished block and returns the new block.
fn emit_block_end(
    stream: &AssistantMessageEventStream,
    output: &mut AssistantMessage,
    block: Option<&CurrentBlock>,
    content_index: usize,
) {
    if let Some(block) = block {
        if block.is_thinking {
            stream.push(crate::types::AssistantMessageEvent::ThinkingEnd {
                content_index: content_index as f64,
                content: block.text.clone(),
                partial: output.clone(),
            });
        } else {
            stream.push(crate::types::AssistantMessageEvent::TextEnd {
                content_index: content_index as f64,
                content: block.text.clone(),
                partial: output.clone(),
            });
        }
    }
}

/// Parses usage metadata from a chunk into the output usage.
fn apply_usage_metadata(model: &Model, output: &mut AssistantMessage, chunk: &Value) {
    let Some(usage_metadata) = chunk.as_map().and_then(|entries| {
        entries
            .iter()
            .find(|(key, _)| key == "usageMetadata")
            .map(|(_, value)| value)
    }) else {
        return;
    };
    let get = |key: &str| -> f64 {
        usage_metadata
            .as_map()
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|(k, _)| k == key)
                    .and_then(|(_, v)| v.as_number())
            })
            .unwrap_or(0.0)
    };
    let prompt = get("promptTokenCount");
    let cached = get("cachedContentTokenCount");
    let candidates = get("candidatesTokenCount");
    let thoughts = get("thoughtsTokenCount");
    output.usage = Usage {
        input: (prompt - cached).max(0.0),
        output: candidates + thoughts,
        cache_read: cached,
        cache_write: 0.0,
        cache_write_1h: None,
        reasoning: Some(thoughts),
        total_tokens: get("totalTokenCount"),
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
    };
    calculate_cost(model, &mut output.usage);
}

/// Handles one streamed chunk (GenerateContentResponse JSON).
fn process_chunk(
    stream: &AssistantMessageEventStream,
    output: &mut AssistantMessage,
    current_block: &mut Option<CurrentBlock>,
    model: &Model,
    chunk: &Value,
) {
    let Some(chunk_entries) = chunk.as_map() else {
        return;
    };

    // output.responseId ||= chunk.responseId
    if output.response_id.is_none() {
        if let Some(response_id) = chunk_entries
            .iter()
            .find(|(key, _)| key == "responseId")
            .and_then(|(_, value)| value.as_str())
        {
            output.response_id = Some(response_id.to_string());
        }
    }

    let candidate = chunk_entries
        .iter()
        .find(|(key, _)| key == "candidates")
        .and_then(|(_, value)| value.as_array())
        .and_then(|candidates| candidates.first());

    if let Some(candidate) = candidate {
        let parts = candidate
            .as_map()
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|(key, _)| key == "content")
                    .and_then(|(_, value)| value.as_map())
                    .and_then(|content| {
                        content
                            .iter()
                            .find(|(key, _)| key == "parts")
                            .and_then(|(_, value)| value.as_array())
                    })
            })
            .map(|parts| parts.to_vec());

        if let Some(parts) = parts {
            for part in parts {
                // text handling
                let part_text = part
                    .as_map()
                    .and_then(|entries| {
                        entries
                            .iter()
                            .find(|(key, _)| key == "text")
                            .and_then(|(_, value)| value.as_str())
                    })
                    .map(|s| s.to_string());

                if let Some(text) = part_text {
                    let is_thinking = is_thinking_part(&part);
                    let part_signature = part
                        .as_map()
                        .and_then(|entries| {
                            entries
                                .iter()
                                .find(|(key, _)| key == "thoughtSignature")
                                .and_then(|(_, value)| value.as_str())
                        })
                        .map(|s| s.to_string());

                    let needs_new_block = match current_block {
                        None => true,
                        Some(block) => block.is_thinking != is_thinking,
                    };
                    if needs_new_block {
                        if let Some(block) = current_block.take() {
                            emit_block_end(stream, output, Some(&block), output.content.len() - 1);
                        }
                        let new_block = CurrentBlock {
                            is_thinking,
                            text: String::new(),
                            signature: None,
                        };
                        output.content.push(if is_thinking {
                            Content::Thinking(ThinkingContent {
                                thinking: String::new(),
                                thinking_signature: None,
                                redacted: None,
                            })
                        } else {
                            Content::Text(TextContent {
                                text: String::new(),
                                text_signature: None,
                            })
                        });
                        let content_index = output.content.len() - 1;
                        if is_thinking {
                            stream.push(crate::types::AssistantMessageEvent::ThinkingStart {
                                content_index: content_index as f64,
                                partial: output.clone(),
                            });
                        } else {
                            stream.push(crate::types::AssistantMessageEvent::TextStart {
                                content_index: content_index as f64,
                                partial: output.clone(),
                            });
                        }
                        *current_block = Some(new_block);
                    }

                    let block = current_block.as_mut().expect("block created above");
                    block.text.push_str(&text);
                    block.signature = retain_thought_signature(block.signature.clone(), part_signature.as_deref());
                    // Persist the signature on the output content block.
                    let content_index = output.content.len() - 1;
                    match &mut output.content[content_index] {
                        Content::Thinking(thinking) => {
                            thinking.thinking = block.text.clone();
                            thinking.thinking_signature = block.signature.clone();
                        }
                        Content::Text(text_block) => {
                            text_block.text = block.text.clone();
                            text_block.text_signature = block.signature.clone();
                        }
                        _ => {}
                    }
                    if block.is_thinking {
                        stream.push(crate::types::AssistantMessageEvent::ThinkingDelta {
                            content_index: content_index as f64,
                            delta: text.clone(),
                            partial: output.clone(),
                        });
                    } else {
                        stream.push(crate::types::AssistantMessageEvent::TextDelta {
                            content_index: content_index as f64,
                            delta: text.clone(),
                            partial: output.clone(),
                        });
                    }
                }

                // functionCall handling
                if let Some(function_call) = part.as_map().and_then(|entries| {
                    entries
                        .iter()
                        .find(|(key, _)| key == "functionCall")
                        .map(|(_, value)| value)
                }) {
                    if let Some(block) = current_block.take() {
                        emit_block_end(stream, output, Some(&block), output.content.len() - 1);
                    }

                    let function_call_entries: Vec<(String, Value)> = function_call.as_map().map(|e| e.to_vec()).unwrap_or_default();
                    let name = function_call_entries
                        .iter()
                        .find(|(key, _)| key == "name")
                        .and_then(|(_, value)| value.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args = function_call_entries
                        .iter()
                        .find(|(key, _)| key == "args")
                        .map(|(_, value)| value.clone())
                        .unwrap_or(Value::Map(Vec::new()));
                    let provided_id = function_call_entries
                        .iter()
                        .find(|(key, _)| key == "id")
                        .and_then(|(_, value)| value.as_str())
                        .map(|s| s.to_string());

                    let needs_new_id = match &provided_id {
                        None => true,
                        Some(id) => output
                            .content
                            .iter()
                            .any(|block| matches!(block, Content::ToolCall(tool_call) if tool_call.id == *id)),
                    };
                    let tool_call_id = if needs_new_id {
                        let counter = TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed);
                        format!("{name}_{}_{}", now_ms() as u64, counter)
                    } else {
                        provided_id.expect("checked above")
                    };

                    let part_thought_signature = part
                        .as_map()
                        .and_then(|entries| {
                            entries
                                .iter()
                                .find(|(key, _)| key == "thoughtSignature")
                                .and_then(|(_, value)| value.as_str())
                        })
                        .map(|s| s.to_string());

                    let tool_call = ToolCall {
                        id: tool_call_id,
                        name,
                        arguments: args,
                        thought_signature: part_thought_signature,
                        namespace: None,
                    };

                    output.content.push(Content::ToolCall(tool_call.clone()));
                    let content_index = output.content.len() - 1;
                    stream.push(crate::types::AssistantMessageEvent::ToolCallStart {
                        content_index: content_index as f64,
                        partial: output.clone(),
                    });
                    stream.push(crate::types::AssistantMessageEvent::ToolCallDelta {
                        content_index: content_index as f64,
                        delta: crate::utils::json::json_stringify(&tool_call.arguments),
                        partial: output.clone(),
                    });
                    stream.push(crate::types::AssistantMessageEvent::ToolCallEnd {
                        content_index: content_index as f64,
                        tool_call,
                        partial: output.clone(),
                    });
                }
            }
        }

        // finishReason
        if let Some(finish_reason) = candidate
            .as_map()
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|(key, _)| key == "finishReason")
                    .and_then(|(_, value)| value.as_str())
            })
        {
            output.raw_stop_reason = Some(finish_reason.to_string());
            output.stop_reason = map_stop_reason(finish_reason);
            if output.content.iter().any(|block| matches!(block, Content::ToolCall(_))) {
                output.stop_reason = StopReason::ToolUse;
            }
        }
    }

    apply_usage_metadata(model, output, chunk);
}

/// Stream function for the Google Vertex API. Spawns a worker thread that
/// performs the request and feeds the returned stream.
pub fn stream(
    model: &Model,
    context: &Context,
    options: Option<&GoogleVertexOptions>,
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
            let api_key = resolve_api_key(options.as_ref()).or_else(|| api_key.clone());

            let custom_base_url = resolve_custom_base_url(&model.base_url);
            let api_key_mode = api_key.is_some() && custom_base_url.is_none();
            let project = if api_key.is_none() {
                Some(resolve_project(options.as_ref())?)
            } else {
                None
            };
            let location = if api_key_mode {
                None
            } else {
                Some(resolve_location(options.as_ref())?)
            };

            // Authentication headers.
            let mut headers: Vec<(String, String)> = Vec::new();
            if let Some(api_key) = &api_key {
                if !custom_base_url.is_some() {
                    headers.push(("x-goog-api-key".to_string(), api_key.clone()));
                }
            } else if let Some(token) = get_provider_env_value(
                "GOOGLE_ACCESS_TOKEN",
                options.as_ref().and_then(|o| o.stream.request.env.as_ref()),
            ) {
                headers.push(("Authorization".to_string(), format!("Bearer {token}")));
            } else if get_provider_env_value(
                "GOOGLE_APPLICATION_CREDENTIALS",
                options.as_ref().and_then(|o| o.stream.request.env.as_ref()),
            )
            .is_some()
            {
                return Err(
                    "Vertex AI ADC is not available in the zero-dependency Rust port: set GOOGLE_ACCESS_TOKEN \
                     to a pre-fetched OAuth token (or provide an API key) instead of GOOGLE_APPLICATION_CREDENTIALS."
                        .to_string(),
                );
            } else if custom_base_url.is_none() {
                return Err(
                    "Vertex AI requires an API key or an access token. Set GOOGLE_ACCESS_TOKEN or provide an API key."
                        .to_string(),
                );
            }

            // Custom headers from the model/options (mirrors buildHttpOptions).
            let mut custom_headers: Vec<(String, String)> = Vec::new();
            if let Some(model_headers) = &model.headers {
                for (key, value) in model_headers {
                    custom_headers.push((key.clone(), value.clone()));
                }
            }
            if let Some(options_headers) = options.as_ref().and_then(|o| o.stream.request.headers.as_ref()) {
                for (key, value) in options_headers {
                    if let Some(value) = value {
                        custom_headers.push((key.clone(), value.clone()));
                    }
                }
            }
            headers.extend(custom_headers);

            let params = build_params(&model, &context, options.as_ref())?;
            let url = construct_url(
                &model.id,
                project.as_deref(),
                location.as_deref(),
                api_key_mode,
                custom_base_url.as_deref(),
            );

            let response = retry_provider_request(
                || {
                    client
                        .post_json(
                            &url,
                            &headers,
                            &params,
                            options.as_ref().and_then(|o| o.stream.request.timeout_ms),
                        )
                        .map(|response| response)
                        .map_err(|error| ProviderError::new(error.status, error.headers.clone(), error.message.clone()))
                },
                ProviderRetryOptions {
                    max_retries: options.as_ref().and_then(|o| o.stream.request.max_retries),
                    max_retry_delay_ms: options.as_ref().and_then(|o| o.stream.request.max_retry_delay_ms).map(|v| v as f64),
                    token: None,
                },
            )
            .map_err(|failure| match failure {
                crate::utils::provider_retry::ProviderRetryFailure::Error(error) => format_vertex_error(&error),
                crate::utils::provider_retry::ProviderRetryFailure::Aborted => "Request was aborted".to_string(),
            })?;

            stream.push(crate::types::AssistantMessageEvent::Start {
                partial: output.clone(),
            });

            let mut current_block: Option<CurrentBlock> = None;
            let mut parser = crate::http::sse::SseParser::new();
            let mut reader = response.reader;
            let mut buffer = [0u8; 8192];
            loop {
                use std::io::Read;
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        for sse in parser.push(&buffer[..n]) {
                            if let Ok(chunk) = crate::utils::json::parse_json_with_repair::<Value>(&sse.data) {
                                process_chunk(&stream, &mut output, &mut current_block, &model, &chunk);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            parser.end();

            // Flush the final block.
            if let Some(block) = current_block {
                let content_index = output.content.len() - 1;
                emit_block_end(&stream, &mut output, Some(&block), content_index);
            }

            if output.stop_reason == StopReason::Pending {
                return Err("Google Vertex stream ended without a finish reason".to_string());
            }
            if output.stop_reason == StopReason::Aborted || output.stop_reason == StopReason::Error {
                let error_message = match &output.raw_stop_reason {
                    Some(reason) => format!("Provider stopped with: {reason}"),
                    None => "An unknown error occurred".to_string(),
                };
                return Err(error_message);
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

/// Simple-stream variant: builds base options and delegates to `stream`.
pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<&SimpleStreamOptions>,
    api_key: Option<&str>,
    client: &HttpClient,
) -> AssistantMessageEventStream {
    let base = build_base_options(model, context, options, api_key);

    let reasoning = options.and_then(|o| o.reasoning.clone());
    let Some(reasoning) = reasoning else {
        let vertex_options = GoogleVertexOptions {
            stream: base,
            thinking: Some(VertexThinking {
                enabled: false,
                budget_tokens: None,
                level: None,
            }),
            ..GoogleVertexOptions::default()
        };
        return stream(model, context, Some(&vertex_options), api_key, client);
    };

    let clamped_reasoning = clamp_thinking_level(model, &reasoning);
    let effort = if clamped_reasoning == "off" {
        "high"
    } else {
        clamped_reasoning.as_str()
    };

    if is_gemini3_pro_model(&model.id) || is_gemini3_flash_model(&model.id) {
        let vertex_options = GoogleVertexOptions {
            stream: base,
            thinking: Some(VertexThinking {
                enabled: true,
                budget_tokens: None,
                level: Some(get_gemini3_thinking_level(effort, &model.id)),
            }),
            ..GoogleVertexOptions::default()
        };
        return stream(model, context, Some(&vertex_options), api_key, client);
    }

    let vertex_options = GoogleVertexOptions {
        stream: base,
        thinking: Some(VertexThinking {
            enabled: true,
            budget_tokens: Some(get_google_budget(
                &model.id,
                effort,
                options.and_then(|o| o.thinking_budgets.as_ref()),
            )),
            level: None,
        }),
        ..GoogleVertexOptions::default()
    };
    stream(model, context, Some(&vertex_options), api_key, client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_base_url_without_location_placeholder_is_kept() {
        assert_eq!(
            resolve_custom_base_url("https://example.com"),
            Some("https://example.com".to_string())
        );
        assert_eq!(resolve_custom_base_url(""), None);
        assert_eq!(
            resolve_custom_base_url("https://{location}-aiplatform.googleapis.com"),
            None
        );
    }

    #[test]
    fn detects_api_version_in_base_url() {
        assert!(base_url_includes_api_version("https://example.com/v1"));
        assert!(base_url_includes_api_version("https://example.com/v1beta1"));
        assert!(base_url_includes_api_version("https://example.com/v1beta"));
        assert!(!base_url_includes_api_version("https://example.com/v"));
        assert!(!base_url_includes_api_version("https://example.com/abc"));
    }

    #[test]
    fn constructs_vertex_urls() {
        // API-key mode (no project/location): global endpoint.
        let url = construct_url("gemini-2.5-pro", None, None, true, None);
        assert_eq!(
            url,
            "https://aiplatform.googleapis.com/v1/publishers/google/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );

        // Project/location mode.
        let url = construct_url(
            "gemini-2.5-pro",
            Some("my-project"),
            Some("us-central1"),
            false,
            None,
        );
        assert_eq!(
            url,
            "https://us-central1-aiplatform.googleapis.com/v1/projects/my-project/locations/us-central1/publishers/google/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );

        // Multi-regional location.
        let url = construct_url("gemini-2.5-pro", Some("p"), Some("us"), false, None);
        assert_eq!(
            url,
            "https://aiplatform.us.rep.googleapis.com/v1/projects/p/locations/us/publishers/google/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );

        // Custom base URL (collection scope): no project/location prefix.
        let url = construct_url(
            "gemini-2.5-pro",
            None,
            None,
            false,
            Some("https://my-proxy.example.com"),
        );
        assert_eq!(
            url,
            "https://my-proxy.example.com/v1/publishers/google/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );

        // Custom base URL with version: no version segment appended.
        let url = construct_url(
            "gemini-2.5-pro",
            None,
            None,
            false,
            Some("https://my-proxy.example.com/v1beta1"),
        );
        assert_eq!(
            url,
            "https://my-proxy.example.com/v1beta1/publishers/google/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn model_path_transformation() {
        assert_eq!(
            construct_url("publishers/openai/models/gpt-oss", None, None, true, None),
            "https://aiplatform.googleapis.com/v1/publishers/openai/models/gpt-oss:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            construct_url("google/gemini-2.5-pro", None, None, true, None),
            "https://aiplatform.googleapis.com/v1/publishers/google/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn thinking_level_mapping() {
        assert_eq!(get_gemini3_thinking_level("low", "gemini-3-pro-preview"), "LOW");
        assert_eq!(get_gemini3_thinking_level("high", "gemini-3-pro-preview"), "HIGH");
        assert_eq!(get_gemini3_thinking_level("minimal", "gemini-3-flash"), "MINIMAL");
        assert_eq!(get_gemini3_thinking_level("medium", "gemini-3-flash"), "MEDIUM");
    }

    #[test]
    fn disabled_thinking_configs() {
        let config = get_disabled_thinking_config("gemini-3-pro-preview");
        assert_eq!(
            config,
            Value::Map(vec![("thinkingLevel".to_string(), Value::String("LOW".to_string()))])
        );
        let config = get_disabled_thinking_config("gemini-2.5-pro");
        assert_eq!(
            config,
            Value::Map(vec![("thinkingBudget".to_string(), Value::Number(0.0))])
        );
    }

    #[test]
    fn google_budgets() {
        assert_eq!(get_google_budget("gemini-2.5-pro", "high", None), 32768.0);
        assert_eq!(get_google_budget("gemini-2.5-flash", "high", None), 24576.0);
        assert_eq!(get_google_budget("gemini-2.0", "high", None), -1.0);
        let custom = ThinkingBudgets {
            minimal: None,
            low: Some(500.0),
            medium: None,
            high: None,
        };
        assert_eq!(get_google_budget("gemini-2.5-pro", "low", Some(&custom)), 500.0);
    }
}
