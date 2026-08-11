//! OpenAI Codex Responses API provider, port of
//! `packages/ai/src/api/openai-codex-responses.ts`.
//!
//! Scope notes (differences from the JS implementation):
//! - Only the SSE transport is ported. The WebSocket transport (connection
//!   pooling, session cache, continuation via previous_response_id, debug
//!   stats) requires a WebSocket client and is not available in this
//!   dependency-free crate; `transport` options other than "sse" fall back
//!   to SSE, which the JS implementation also does after a WebSocket
//!   failure.
//! - zstd request compression is skipped (the JS implementation also falls
//!   back to uncompressed JSON when node:zlib is unavailable).
//! - The `_os` User-Agent uses Rust compile-time platform constants.

use pi_protocol::Value;

use crate::api::constrained_sampling::create_grammar_tool_input_properties;
use crate::api::openai_responses_shared::{
    convert_responses_messages, convert_responses_tools, ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions,
};
use crate::api::openai_stream::{parse_stream_event, process_responses_stream, OpenAIResponsesStreamOptions};
use crate::api::prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::simple_options::build_base_options;
use crate::event_stream::AssistantMessageEventStream;
use crate::http::client::HttpClient;
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::types::{
    AssistantMessage, CacheRetention, Context, Model, ProviderHeaders, SimpleStreamOptions, StopReason, StreamOptions,
    Usage, UsageCost,
};
use crate::utils::abort::{abortable_sleep, CancellationToken};
use crate::utils::deferred_tools::split_deferred_tools;

const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
const DEFAULT_MAX_RETRIES: u64 = 0;
const BASE_DELAY_MS: u64 = 1000;
const DEFAULT_MAX_RETRY_DELAY_MS: f64 = 60_000.0;
const CODEX_TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "openai-codex", "opencode"];

const CODEX_RESPONSE_STATUSES: [&str; 6] = ["completed", "incomplete", "failed", "cancelled", "queued", "in_progress"];

/// OpenAI Codex Responses-specific stream options.
#[derive(Clone, Debug, Default)]
pub struct OpenAICodexResponsesOptions {
    pub stream: StreamOptions,
    pub reasoning_effort: Option<String>,
    pub reasoning_summary: Option<String>,
    pub service_tier: Option<String>,
    pub text_verbosity: Option<String>,
    pub tool_choice: Option<String>,
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

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as f64
}

fn output_message(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: vec![],
        api: "openai-codex-responses".to_string(),
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
// Retry helpers (mirroring the JS custom retry loop)
// ---------------------------------------------------------------------------

pub fn is_terminal_rate_limit_error(error_text: &str) -> bool {
    let pattern = regex::Regex::new(
        r"GoUsageLimitError|FreeUsageLimitError|Monthly usage limit reached|available balance|insufficient_quota|out of budget|quota exceeded|billing",
    )
    .expect("static pattern");
    pattern.is_match(error_text)
}

pub fn is_retryable_error(status: Option<u16>, error_text: &str) -> bool {
    match status {
        Some(429) => {
            if is_terminal_rate_limit_error(error_text) {
                return false;
            }
            return true;
        }
        Some(500) | Some(502) | Some(503) | Some(504) => return true,
        _ => {}
    }
    let pattern = regex::Regex::new(r"rate.?limit|overloaded|service.?unavailable|upstream.?connect|connection.?refused")
        .expect("static pattern");
    pattern.is_match(error_text)
}

fn get_retry_after_delay_ms(headers: &[(String, String)]) -> Option<f64> {
    let get = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    if let Some(retry_after_ms) = get("retry-after-ms") {
        if let Ok(millis) = retry_after_ms.parse::<f64>() {
            if millis.is_finite() {
                return Some(millis.max(0.0));
            }
        }
    }
    let retry_after = get("retry-after")?;
    if let Ok(seconds) = retry_after.parse::<f64>() {
        if seconds.is_finite() {
            return Some((seconds * 1000.0).max(0.0));
        }
    }
    // HTTP-date fallback (IMF-fixdate).
    if let Some(date_ms) = crate::utils::provider_retry::parse_http_date(retry_after) {
        return Some((date_ms - now_ms()).max(0.0));
    }
    None
}

fn validate_retry_delay_ms(delay_ms: f64, options: Option<&OpenAICodexResponsesOptions>) -> Result<f64, String> {
    let max_retry_delay_ms = options
        .and_then(|o| o.stream.request.max_retry_delay_ms)
        .map(|v| v as f64)
        .unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS);
    if max_retry_delay_ms > 0.0 && delay_ms > max_retry_delay_ms {
        return Err(format!(
            "Server requested {}s retry delay (max: {}s)",
            (delay_ms / 1000.0).ceil(),
            (max_retry_delay_ms / 1000.0).ceil()
        ));
    }
    Ok(delay_ms)
}

// ---------------------------------------------------------------------------
// URL resolution
// ---------------------------------------------------------------------------

pub fn resolve_codex_url(base_url: Option<&str>) -> String {
    let raw = match base_url {
        Some(base_url) if !base_url.trim().is_empty() => base_url,
        _ => DEFAULT_CODEX_BASE_URL,
    };
    let normalized = raw.trim_end_matches('/');
    if normalized.ends_with("/codex/responses") {
        normalized.to_string()
    } else if normalized.ends_with("/codex") {
        format!("{normalized}/responses")
    } else {
        format!("{normalized}/codex/responses")
    }
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

fn base64url_decode(input: &str) -> Result<Vec<u8>, String> {
    let mut cleaned: String = input.chars().filter(|c| *c != '=').collect();
    cleaned = cleaned.replace('-', "+").replace('_', "/");
    match cleaned.len() % 4 {
        2 => cleaned.push_str("=="),
        3 => cleaned.push_str("="),
        1 => return Err("invalid base64 length".to_string()),
        _ => {}
    }
    // Manual base64 decode (zero deps).
    let table: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = cleaned.as_bytes();
    let mut result = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for &byte in bytes {
        if byte == b'=' {
            break; // padding reached; all remaining bytes are padding
        }
        let value = table.iter().position(|c| *c == byte).ok_or("invalid base64 character")? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            result.push((buffer >> bits) as u8);
            // Clear the consumed bits so later groups decode correctly.
            buffer &= (1 << bits) - 1;
        }
    }
    Ok(result)
}

/// Mirrors `extractAccountId`: decodes the JWT payload and reads
/// `https://api.openai.com/auth.chatgpt_account_id`.
pub fn extract_account_id(token: &str) -> Result<String, String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err("Failed to extract accountId from token".to_string());
    }
    let payload = base64url_decode(parts[1]).map_err(|_| "Failed to extract accountId from token".to_string())?;
    let parsed: Value = crate::utils::json::parse_json_with_repair(&String::from_utf8_lossy(&payload))
        .map_err(|_| "Failed to extract accountId from token".to_string())?;
    let Value::Map(entries) = &parsed else {
        return Err("Failed to extract accountId from token".to_string());
    };
    let Some(Value::Map(auth)) = entries.iter().find(|(key, _)| key == JWT_CLAIM_PATH).map(|(_, v)| v) else {
        return Err("Failed to extract accountId from token".to_string());
    };
    match auth.iter().find(|(key, _)| key == "chatgpt_account_id") {
        Some((_, Value::String(account_id))) if !account_id.is_empty() => Ok(account_id.clone()),
        _ => Err("Failed to extract accountId from token".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------

fn build_base_codex_headers(
    init_headers: Option<&[(String, String)]>,
    additional_headers: Option<&ProviderHeaders>,
    account_id: &str,
    token: &str,
) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = init_headers
        .map(|headers| headers.to_vec())
        .unwrap_or_default();
    if let Some(additional) = additional_headers {
        for (key, value) in additional {
            match value {
                None => {
                    headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(key));
                }
                Some(value) => {
                    if let Some(existing) = headers.iter_mut().find(|(existing, _)| existing.eq_ignore_ascii_case(key)) {
                        existing.1 = value.clone();
                    } else {
                        headers.push((key.clone(), value.clone()));
                    }
                }
            }
        }
    }
    headers.push(("Authorization".to_string(), format!("Bearer {token}")));
    headers.push(("chatgpt-account-id".to_string(), account_id.to_string()));
    headers.push(("originator".to_string(), "pi".to_string()));
    headers.push((
        "User-Agent".to_string(),
        format!("pi ({}; {})", std::env::consts::OS, std::env::consts::ARCH),
    ));
    headers
}

fn build_sse_headers(
    init_headers: Option<&[(String, String)]>,
    additional_headers: Option<&ProviderHeaders>,
    account_id: &str,
    token: &str,
    session_id: Option<&str>,
) -> Vec<(String, String)> {
    let mut headers = build_base_codex_headers(init_headers, additional_headers, account_id, token);
    headers.push(("OpenAI-Beta".to_string(), "responses=experimental".to_string()));
    headers.push(("accept".to_string(), "text/event-stream".to_string()));
    headers.push(("content-type".to_string(), "application/json".to_string()));
    if let Some(session_id) = session_id {
        headers.push(("session-id".to_string(), session_id.to_string()));
        headers.push(("x-client-request-id".to_string(), session_id.to_string()));
    }
    headers
}

// ---------------------------------------------------------------------------
// Request body
// ---------------------------------------------------------------------------

fn resolve_cache_retention(cache_retention: Option<&CacheRetention>) -> CacheRetention {
    cache_retention.cloned().unwrap_or_else(|| "short".to_string())
}

/// Test-exposed wrapper for `build_request_body` (the internal helper is
/// private; this mirrors the JS function shape for direct unit testing).
pub fn build_request_body_for_test(
    model: &Model,
    context: &Context,
    options: Option<&OpenAICodexResponsesOptions>,
    cache_session_id: Option<&str>,
    grammar_tool_input_properties: &[(String, String)],
) -> Value {
    build_request_body(model, context, options, cache_session_id, grammar_tool_input_properties)
}

fn build_request_body(
    model: &Model,
    context: &Context,
    options: Option<&OpenAICodexResponsesOptions>,
    cache_session_id: Option<&str>,
    grammar_tool_input_properties: &[(String, String)],
) -> Value {
    let supports_strict_mode = match &model.compat {
        Some(crate::types::ModelCompat::OpenAiResponses(compat)) => compat.supports_strict_mode.unwrap_or(true),
        _ => true,
    };
    let supports_openai_grammar_tools = match &model.compat {
        Some(crate::types::ModelCompat::OpenAiResponses(compat)) => {
            compat.supports_openai_grammar_tools.unwrap_or(false)
        }
        _ => false,
    };
    let supports_additional_tools = match &model.compat {
        Some(crate::types::ModelCompat::OpenAiResponses(compat)) => {
            compat.supports_additional_tools.unwrap_or(false)
        }
        _ => false,
    };
    let supports_tool_search = match &model.compat {
        Some(crate::types::ModelCompat::OpenAiResponses(compat)) => compat.supports_tool_search.unwrap_or(false),
        _ => false,
    };
    let deferred_tools_mode = if supports_additional_tools {
        Some("additional-tools")
    } else if supports_tool_search {
        Some("tool-search")
    } else {
        None
    };
    let tool_placement = split_deferred_tools(context, deferred_tools_mode.is_some(), None);
    let allowed: std::collections::HashSet<String> = CODEX_TOOL_CALL_PROVIDERS.iter().map(|p| p.to_string()).collect();
    let messages = convert_responses_messages(model, context, &allowed, Some(&ConvertResponsesMessagesOptions {
        include_system_prompt: Some(false),
        grammar_tool_input_properties: Some(grammar_tool_input_properties.to_vec()),
        deferred_tools: Some(tool_placement.deferred.clone()),
        deferred_tools_mode: deferred_tools_mode.map(|mode| mode.to_string()),
        tool_options: Some(ConvertResponsesToolsOptions {
            strict: Some(false),
            supports_strict_mode: Some(supports_strict_mode),
            supports_openai_grammar_tools: Some(supports_openai_grammar_tools),
            ..ConvertResponsesToolsOptions::default()
        }),
    }));

    let mut entries: Vec<(String, Value)> = vec![
        ("model".to_string(), Value::String(model.id.clone())),
        ("store".to_string(), Value::Bool(false)),
        ("stream".to_string(), Value::Bool(true)),
        (
            "instructions".to_string(),
            Value::String(
                context
                    .system_prompt
                    .clone()
                    .unwrap_or_else(|| "You are a helpful assistant.".to_string()),
            ),
        ),
        (
            "input".to_string(),
            Value::Array(messages.iter().map(|item| item.to_value()).collect()),
        ),
        (
            "text".to_string(),
            Value::Map(vec![(
                "verbosity".to_string(),
                Value::String(
                    options
                        .and_then(|o| o.text_verbosity.clone())
                        .unwrap_or_else(|| "low".to_string()),
                ),
            )]),
        ),
        (
            "include".to_string(),
            Value::Array(vec![Value::String("reasoning.encrypted_content".to_string())]),
        ),
        (
            "prompt_cache_key".to_string(),
            match cache_session_id {
                Some(key) => Value::String(key.to_string()),
                None => Value::Null,
            },
        ),
        (
            "tool_choice".to_string(),
            Value::String(
                options
                    .and_then(|o| o.tool_choice.clone())
                    .unwrap_or_else(|| "auto".to_string()),
            ),
        ),
        ("parallel_tool_calls".to_string(), Value::Bool(true)),
    ];

    if let Some(temperature) = options.and_then(|o| o.stream.temperature) {
        entries.push(("temperature".to_string(), Value::Number(temperature)));
    }

    if let Some(service_tier) = options.and_then(|o| o.service_tier.clone()) {
        entries.push(("service_tier".to_string(), Value::String(service_tier)));
    }

    if !tool_placement.immediate.is_empty() {
        entries.push((
            "tools".to_string(),
            Value::Array(
                convert_responses_tools(&tool_placement.immediate, Some(&ConvertResponsesToolsOptions {
                    strict: Some(false),
                    supports_strict_mode: Some(supports_strict_mode),
                    supports_openai_grammar_tools: Some(supports_openai_grammar_tools),
                    ..ConvertResponsesToolsOptions::default()
                }))
                .iter()
                .map(|tool| tool.to_value())
                .collect(),
            ),
        ));
    }

    if let Some(reasoning_effort) = options.and_then(|o| o.reasoning_effort.clone()) {
        let effort = if reasoning_effort == "none" {
            model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.iter().find(|(key, _)| key == "off"))
                .and_then(|(_, value)| value.clone())
                .unwrap_or_else(|| "none".to_string())
        } else {
            model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.iter().find(|(key, _)| key == &reasoning_effort))
                .and_then(|(_, value)| value.clone())
                .unwrap_or(reasoning_effort)
        };
        entries.push((
            "reasoning".to_string(),
            Value::Map(vec![
                ("effort".to_string(), Value::String(effort)),
                (
                    "summary".to_string(),
                    Value::String(
                        options
                            .and_then(|o| o.reasoning_summary.clone())
                            .unwrap_or_else(|| "auto".to_string()),
                    ),
                ),
            ]),
        ));
    }

    Value::Map(entries)
}

// ---------------------------------------------------------------------------
// Service tier pricing
// ---------------------------------------------------------------------------

fn get_service_tier_cost_multiplier(model_id: &str, service_tier: Option<&str>) -> f64 {
    match service_tier {
        Some("flex") => 0.5,
        Some("priority") => {
            if model_id == "gpt-5.5" {
                2.5
            } else {
                2.0
            }
        }
        _ => 1.0,
    }
}

fn apply_service_tier_pricing(usage: &mut Usage, service_tier: Option<&str>, model_id: &str) {
    let multiplier = get_service_tier_cost_multiplier(model_id, service_tier);
    if multiplier == 1.0 {
        return;
    }
    usage.cost.input *= multiplier;
    usage.cost.output *= multiplier;
    usage.cost.cache_read *= multiplier;
    usage.cost.cache_write *= multiplier;
    usage.cost.total = usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
}

fn resolve_codex_service_tier(response_service_tier: Option<&str>, request_service_tier: Option<&str>) -> Option<String> {
    if response_service_tier == Some("default")
        && (request_service_tier == Some("flex") || request_service_tier == Some("priority"))
    {
        return request_service_tier.map(|tier| tier.to_string());
    }
    response_service_tier
        .or(request_service_tier)
        .map(|tier| tier.to_string())
}

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

fn normalize_codex_status(status: Option<&str>) -> Option<String> {
    match status {
        Some(status) if CODEX_RESPONSE_STATUSES.contains(&status) => Some(status.to_string()),
        _ => None,
    }
}

fn extract_codex_event_error(event: &Value) -> (Option<String>, Option<String>) {
    let Value::Map(entries) = event else {
        return (None, None);
    };
    let code = entries
        .iter()
        .find(|(key, _)| key == "code")
        .and_then(|(_, value)| value.as_str())
        .map(|s| s.to_string());
    let message = entries
        .iter()
        .find(|(key, _)| key == "message")
        .and_then(|(_, value)| value.as_str())
        .map(|s| s.to_string());
    let (nested_code, nested_message) = match entries.iter().find(|(key, _)| key == "error").map(|(_, value)| value) {
        Some(Value::Map(nested)) => (
            nested
                .iter()
                .find(|(key, _)| key == "code")
                .and_then(|(_, value)| value.as_str())
                .map(|s| s.to_string()),
            nested
                .iter()
                .find(|(key, _)| key == "message")
                .and_then(|(_, value)| value.as_str())
                .map(|s| s.to_string()),
        ),
        _ => (None, None),
    };
    (code.or(nested_code), message.or(nested_message))
}

/// Parses the error response body with the Codex-friendly usage-limit
/// message, mirroring `parseErrorResponse`.
pub fn parse_error_response(body: &str, status: Option<u16>) -> String {
    let mut message = if !body.is_empty() { body.to_string() } else { "Request failed".to_string() };
    let parsed: Option<Value> = crate::utils::json::parse_json_with_repair(body).ok();
    if let Some(Value::Map(entries)) = &parsed {
        if let Some(Value::Map(error)) = entries.iter().find(|(key, _)| key == "error").map(|(_, value)| value) {
            let code = error
                .iter()
                .find(|(key, _)| key == "code")
                .and_then(|(_, value)| value.as_str())
                .or_else(|| {
                    error
                        .iter()
                        .find(|(key, _)| key == "type")
                        .and_then(|(_, value)| value.as_str())
                })
                .unwrap_or("");
            if status == Some(429)
                || regex::Regex::new(r"usage_limit_reached|usage_not_included|rate_limit_exceeded")
                    .expect("static")
                    .is_match(code)
            {
                let plan = error
                    .iter()
                    .find(|(key, _)| key == "plan_type")
                    .and_then(|(_, value)| value.as_str())
                    .map(|plan_type| format!(" ({} plan)", plan_type.to_lowercase()))
                    .unwrap_or_default();
                let when = error
                    .iter()
                    .find(|(key, _)| key == "resets_at")
                    .and_then(|(_, value)| value.as_number())
                    .map(|resets_at| {
                        let mins = ((resets_at * 1000.0 - now_ms()) / 60000.0).round().max(0.0);
                        format!(" Try again in ~{} min.", mins as u64)
                    })
                    .unwrap_or_default();
                let friendly = format!("You have hit your ChatGPT usage limit{plan}.{when}").trim().to_string();
                // JS throws `friendlyMessage || message` for usage-limit errors.
                if !friendly.is_empty() {
                    message = friendly;
                } else if let Some(error_message) = error
                    .iter()
                    .find(|(key, _)| key == "message")
                    .and_then(|(_, value)| value.as_str())
                {
                    if !error_message.is_empty() {
                        message = error_message.to_string();
                    }
                }
            } else if let Some(error_message) = error
                .iter()
                .find(|(key, _)| key == "message")
                .and_then(|(_, value)| value.as_str())
            {
                if !error_message.is_empty() {
                    message = error_message.to_string();
                }
            }
        }
    }
    message
}

// ---------------------------------------------------------------------------
// SSE parsing + event mapping
// ---------------------------------------------------------------------------

/// Mirrors `parseSSE` + `mapCodexEvents`: reads data: lines, parses JSON,
/// normalizes terminal events to `response.completed`, and raises on
/// Codex-specific error events. Returns the mapped stream events.
fn parse_codex_stream(
    reader: impl std::io::Read,
    output: &mut AssistantMessage,
) -> Result<Vec<crate::api::openai_stream::ResponseStreamEvent>, String> {
    let mut raw_events: Vec<Value> = Vec::new();
    let mut parse_error: Option<String> = None;
    crate::http::client::read_sse_stream(reader, |sse| {
        let data = sse.data.trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        match crate::utils::json::parse_json_with_repair::<Value>(data) {
            Ok(value) => raw_events.push(value),
            Err(cause) => {
                if parse_error.is_none() {
                    parse_error = Some(format!(
                        "Invalid Codex SSE JSON: {}",
                        crate::utils::diagnostics::format_thrown_value(&cause)
                    ));
                }
            }
        }
    });
    if let Some(error) = parse_error {
        return Err(error);
    }

    let mut mapped = Vec::new();
    for event in raw_events {
        let Value::Map(entries) = &event else {
            continue;
        };
        let type_ = entries
            .iter()
            .find(|(key, _)| key == "type")
            .and_then(|(_, value)| value.as_str());
        let Some(type_) = type_ else {
            continue;
        };

        match type_ {
            "error" => {
                let (code, message) = extract_codex_event_error(&event);
                let text = message
                    .or(code)
                    .unwrap_or_else(|| crate::utils::json::json_stringify(&event));
                return Err(format!("Codex error: {text}"));
            }
            "response.failed" => {
                let (_code, message) = match entries.iter().find(|(key, _)| key == "response").map(|(_, value)| value) {
                    Some(Value::Map(response)) => (
                        response
                            .iter()
                            .find(|(key, _)| key == "error")
                            .and_then(|(_, value)| value.as_map())
                            .and_then(|error| {
                                error
                                    .iter()
                                    .find(|(key, _)| key == "code")
                                    .and_then(|(_, value)| value.as_str())
                            })
                            .map(|s| s.to_string()),
                        response
                            .iter()
                            .find(|(key, _)| key == "error")
                            .and_then(|(_, value)| value.as_map())
                            .and_then(|error| {
                                error
                                    .iter()
                                    .find(|(key, _)| key == "message")
                                    .and_then(|(_, value)| value.as_str())
                            })
                            .map(|s| s.to_string()),
                    ),
                    _ => (None, None),
                };
                return Err(message.unwrap_or_else(|| "Codex response failed".to_string()));
            }
            "response.done" | "response.completed" | "response.incomplete" => {
                // Normalize to response.completed and capture end_turn.
                let response_entries = entries
                    .iter()
                    .find(|(key, _)| key == "response")
                    .map(|(_, value)| value.as_map().map(|entries| entries.to_vec()))
                    .flatten();
                if let Some(response_entries) = &response_entries {
                    if let Some(Value::Bool(end_turn)) = response_entries
                        .iter()
                        .find(|(key, _)| key == "end_turn")
                        .map(|(_, value)| value)
                    {
                        output.end_turn = Some(*end_turn);
                    }
                }
                // Rewrite: type -> response.completed, status normalized.
                let mut rewritten = event;
                if let Value::Map(ref mut entries) = rewritten {
                    if let Some((_, value)) = entries.iter_mut().find(|(key, _)| key == "type") {
                        *value = Value::String("response.completed".to_string());
                    }
                    if let Some((_, Value::Map(response))) = entries.iter_mut().find(|(key, _)| key == "response") {
                        let status = response
                            .iter()
                            .find(|(key, _)| key == "status")
                            .and_then(|(_, value)| value.as_str());
                        match normalize_codex_status(status) {
                            Some(normalized) => {
                                if let Some((_, value)) = response.iter_mut().find(|(key, _)| key == "status") {
                                    *value = Value::String(normalized);
                                }
                            }
                            None => {
                                response.retain(|(key, _)| key != "status");
                            }
                        }
                    }
                }
                if let Some(stream_event) = parse_stream_event(&crate::utils::json::json_stringify(&rewritten)) {
                    mapped.push(stream_event);
                }
                return Ok(mapped);
            }
            _ => {
                if let Some(stream_event) = parse_stream_event(&crate::utils::json::json_stringify(&event)) {
                    mapped.push(stream_event);
                }
            }
        }
    }
    Ok(mapped)
}

// ---------------------------------------------------------------------------
// Main stream functions
// ---------------------------------------------------------------------------

fn assert_successful_output(output: &AssistantMessage) -> Result<(), String> {
    if output.stop_reason == StopReason::Pending {
        return Err("Codex stream ended without a stop reason".to_string());
    }
    if output.stop_reason == StopReason::Error || output.stop_reason == StopReason::Aborted {
        return Err(output
            .error_message
            .clone()
            .unwrap_or_else(|| "An unknown error occurred".to_string()));
    }
    Ok(())
}

fn process_stream(
    reader: impl std::io::Read + Send + 'static,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
    grammar_tool_input_properties: &[(String, String)],
    options: Option<&OpenAICodexResponsesOptions>,
) -> Result<(), String> {
    let events = parse_codex_stream(reader, output)?;
    let request_service_tier = options.and_then(|o| o.service_tier.clone());
    let model_id = model.id.clone();
    let stream_options = OpenAIResponsesStreamOptions {
        service_tier: request_service_tier.clone(),
        grammar_tool_input_properties: Some(grammar_tool_input_properties.to_vec()),
        apply_service_tier_pricing: Some(Box::new(move |usage, response_tier, request_tier| {
            let resolved = resolve_codex_service_tier(response_tier, request_tier);
            apply_service_tier_pricing(usage, resolved.as_deref(), &model_id);
        })),
    };
    process_responses_stream(events, output, stream, model, Some(&stream_options))
}

/// Stream function for the OpenAI Codex Responses API (SSE transport).
pub fn stream(
    model: &Model,
    context: &Context,
    options: Option<&OpenAICodexResponsesOptions>,
    api_key: Option<&str>,
    client: &HttpClient,
    token: Option<&CancellationToken>,
) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let worker_stream = stream.clone();
    let model = model.clone();
    let context = context.clone();
    let options = options.cloned();
    let api_key = api_key.map(|s| s.to_string());
    let client = client.clone();
    let token = token.cloned();

    std::thread::spawn(move || {
        let stream = worker_stream;
        let mut output = output_message(&model);
        let result = (|| -> Result<(), String> {
            let api_key = api_key.ok_or_else(|| format!("No API key for provider: {}", model.provider))?;
            let account_id = extract_account_id(&api_key)?;
            let grammar_tool_input_properties = create_grammar_tool_input_properties(
                context.tools.as_deref(),
                match &model.compat {
                    Some(crate::types::ModelCompat::OpenAiResponses(compat)) => {
                        compat.supports_openai_grammar_tools.unwrap_or(false)
                    }
                    _ => false,
                },
            );
            let cache_retention = resolve_cache_retention(options.as_ref().and_then(|o| o.stream.cache_retention.as_ref()));
            let cache_session_id = if cache_retention == "none" {
                None
            } else {
                options.as_ref().and_then(|o| o.stream.session_id.clone())
            };
            let codex_session_id = clamp_openai_prompt_cache_key(cache_session_id.as_deref());
            let body = build_request_body(
                &model,
                &context,
                options.as_ref(),
                codex_session_id.as_deref(),
                &grammar_tool_input_properties,
            );
            let sse_headers = build_sse_headers(
                model.headers.as_deref(),
                options.as_ref().and_then(|o| o.stream.request.headers.as_ref()),
                &account_id,
                &api_key,
                codex_session_id.as_deref(),
            );

            let url = resolve_codex_url(Some(&model.base_url));
            let max_retries = options.as_ref().and_then(|o| o.stream.request.max_retries).unwrap_or(DEFAULT_MAX_RETRIES);
            let timeout_ms = options.as_ref().and_then(|o| o.stream.request.timeout_ms);

            // Fetch with retry logic for rate limits and transient errors.
            let mut response: Option<crate::http::client::HttpResponse> = None;
            let mut last_error: Option<String> = None;
            let mut attempt: u64 = 0;
            loop {
                if let Some(token) = &token {
                    if token.is_aborted() {
                        return Err("Request was aborted".to_string());
                    }
                }
                match client.post_json(&url, &sse_headers, &body, timeout_ms) {
                    Ok(http_response) => {
                        // post_json raises on non-2xx; a success here is 2xx.
                        response = Some(http_response);
                        break;
                    }
                    Err(error) => {
                        if attempt < max_retries && is_retryable_error(error.status, &error.message) {
                            let delay_ms = match get_retry_after_delay_ms(&error.headers) {
                                Some(delay) => match validate_retry_delay_ms(delay, options.as_ref()) {
                                    Ok(delay) => delay,
                                    Err(message) => return Err(message),
                                },
                                None => (BASE_DELAY_MS * 2u64.pow(attempt as u32)) as f64,
                            };
                            if abortable_sleep(delay_ms as u64, token.as_ref()).is_err() {
                                return Err("Request was aborted".to_string());
                            }
                            attempt += 1;
                            continue;
                        }
                        // Parse error for a friendly message on the final
                        // attempt or a non-retryable error.
                        last_error = Some(parse_error_response(&error.message, error.status));
                        break;
                    }
                }
            }

            let response = response.ok_or_else(|| last_error.unwrap_or_else(|| "Failed after retries".to_string()))?;

            stream.push(crate::types::AssistantMessageEvent::Start {
                partial: output.clone(),
            });

            process_stream(
                response.reader,
                &mut output,
                &stream,
                &model,
                &grammar_tool_input_properties,
                options.as_ref(),
            )?;

            if let Some(token) = &token {
                if token.is_aborted() {
                    return Err("Request was aborted".to_string());
                }
            }

            assert_successful_output(&output)?;

            stream.push(crate::types::AssistantMessageEvent::Done {
                reason: output.stop_reason.as_str().to_string(),
                message: output.clone(),
            });
            stream.end(None);
            Ok(())
        })();

        if let Err(error) = result {
            let aborted = token.as_ref().is_some_and(|token| token.is_aborted());
            output.stop_reason = if aborted { StopReason::Aborted } else { StopReason::Error };
            output.error_message = Some(error);
            stream.push(crate::types::AssistantMessageEvent::Error {
                reason: output.stop_reason.as_str().to_string(),
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
    token: Option<&CancellationToken>,
) -> AssistantMessageEventStream {
    let api_key = api_key.ok_or_else(|| format!("No API key for provider: {}", model.provider)).expect("api key required");
    let base = build_base_options(model, context, options, Some(&api_key));
    let clamped_reasoning = options
        .and_then(|o| o.reasoning.as_deref())
        .map(|level| clamp_thinking_level(model, level));
    let reasoning_effort = match clamped_reasoning.as_deref() {
        Some("off") => None,
        Some(level) => Some(level.to_string()),
        None => None,
    };

    let stream_options = OpenAICodexResponsesOptions {
        stream: base,
        reasoning_effort,
        ..OpenAICodexResponsesOptions::default()
    };
    stream(model, context, Some(&stream_options), Some(&api_key), client, token)
}

// Keep calculate_cost referenced for parity with the responses provider.
#[allow(dead_code)]
fn _cost_reference(model: &Model, usage: &mut Usage) {
    calculate_cost(model, usage);
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Context;
    use pi_protocol::Value;

    fn model() -> Model {
        Model {
            id: "gpt-5-codex".to_string(),
            name: "gpt-5-codex".to_string(),
            api: "openai-codex-responses".to_string(),
            provider: "openai-codex".to_string(),
            base_url: "https://chatgpt.com/backend-api".to_string(),
            reasoning: true,
            thinking_level_map: Some(vec![
                ("off".to_string(), Some("none".to_string())),
                ("high".to_string(), Some("high".to_string())),
            ]),
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
            context_window: 200_000.0,
            max_tokens: 8192.0,
            sampling_params: None,
            headers: None,
            compat: None,
        }
    }

    fn base64url(input: &str) -> String {
        const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bytes = input.as_bytes();
        let mut result = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            result.push(TABLE[(n >> 18) as usize & 63] as char);
            result.push(TABLE[(n >> 12) as usize & 63] as char);
            if chunk.len() > 1 { result.push(TABLE[(n >> 6) as usize & 63] as char); } else { result.push('='); }
            if chunk.len() > 2 { result.push(TABLE[n as usize & 63] as char); } else { result.push('='); }
        }
        result.replace('+', "-").replace('/', "_").trim_end_matches('=').to_string()
    }

    #[test]
    fn resolves_codex_urls() {
        assert_eq!(resolve_codex_url(Some("https://chatgpt.com/backend-api")), "https://chatgpt.com/backend-api/codex/responses");
        assert_eq!(resolve_codex_url(Some("https://x.example/codex")), "https://x.example/codex/responses");
        assert_eq!(resolve_codex_url(Some("https://x.example/codex/responses/")), "https://x.example/codex/responses");
        assert_eq!(resolve_codex_url(None), "https://chatgpt.com/backend-api/codex/responses");
    }

    #[test]
    fn extracts_account_id_from_jwt() {
        let header = base64url("{\"alg\":\"HS256\"}");
        let payload = base64url("{\"https://api.openai.com/auth\":{\"chatgpt_account_id\":\"acct_123\"}}");
        let token = format!("{header}.{payload}.sig");
        assert_eq!(extract_account_id(&token).unwrap(), "acct_123");
        assert!(extract_account_id("not-a-token").is_err());
        let bad_payload = base64url("{\"other\": 1}");
        assert!(extract_account_id(&format!("{header}.{bad_payload}.sig")).is_err());
    }

    #[test]
    fn classifies_retryable_errors() {
        assert!(is_retryable_error(Some(429), "rate limit"));
        assert!(is_retryable_error(Some(500), "server error"));
        assert!(is_retryable_error(Some(503), ""));
        assert!(!is_retryable_error(Some(400), "bad request"));
        assert!(!is_retryable_error(Some(429), "GoUsageLimitError"));
        assert!(is_retryable_error(None, "connection refused"));
        assert!(is_terminal_rate_limit_error("insufficient_quota"));
        assert!(!is_terminal_rate_limit_error("rate limit"));
    }

    #[test]
    fn parses_error_responses() {
        let body = r#"{"error":{"code":"usage_limit_reached","message":"limit","plan_type":"plus","resets_at":0}}"#;
        let message = parse_error_response(body, Some(429));
        assert!(message.contains("usage limit"), "{message}");
        let body = r#"{"error":{"message":"boom"}}"#;
        assert_eq!(parse_error_response(body, Some(400)), "boom");
        assert_eq!(parse_error_response("plain text", Some(500)), "plain text");
    }

    #[test]
    fn builds_request_body_with_codex_fields() {
        let context = Context { system_prompt: Some("Be concise.".to_string()), ..Context::default() };
        let options = OpenAICodexResponsesOptions {
            text_verbosity: Some("high".to_string()),
            reasoning_effort: Some("high".to_string()),
            tool_choice: Some("required".to_string()),
            ..Default::default()
        };
        let body = build_request_body(&model(), &context, Some(&options), Some("session-1"), &[]);
        let Value::Map(entries) = &body else { panic!("expected map"); };
        let get = |key: &str| entries.iter().find(|(k, _)| k == key).map(|(_, v)| v);
        assert_eq!(get("model"), Some(&Value::String("gpt-5-codex".to_string())));
        assert_eq!(get("store"), Some(&Value::Bool(false)));
        assert_eq!(get("stream"), Some(&Value::Bool(true)));
        assert_eq!(get("instructions"), Some(&Value::String("Be concise.".to_string())));
        assert_eq!(get("prompt_cache_key"), Some(&Value::String("session-1".to_string())));
        assert_eq!(get("tool_choice"), Some(&Value::String("required".to_string())));
        assert_eq!(get("parallel_tool_calls"), Some(&Value::Bool(true)));
        assert_eq!(get("text"), Some(&Value::Map(vec![("verbosity".to_string(), Value::String("high".to_string()))])));
        if let Some(Value::Map(reasoning)) = get("reasoning") {
            assert_eq!(reasoning.iter().find(|(k, _)| k == "effort").map(|(_, v)| v), Some(&Value::String("high".to_string())));
        } else {
            panic!("expected reasoning block");
        }
    }
}
