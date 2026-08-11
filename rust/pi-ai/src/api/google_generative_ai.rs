//! Google Generative AI provider, port of
//! `packages/ai/src/api/google-generative-ai.ts`.
//!
//! Gemini streams via SSE (`streamGenerateContent?alt=sse`, `data:`-only
//! events). The SDK's URL layout is mirrored: default base is
//! `https://generativelanguage.googleapis.com/v1beta`; a custom `baseUrl`
//! replaces it wholesale (already includes the version path).

use pi_protocol::Value;

use crate::api::google_shared::{
    convert_messages, convert_tools, map_stop_reason, resolve_google_function_calling_mode,
    retain_thought_signature, supports_google_strict_tool_sampling,
};
use crate::api::simple_options::build_base_options;
use crate::event_stream::AssistantMessageEventStream;
use crate::http::client::HttpClient;
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::types::{
    AssistantMessage, Context, Model, ProviderHeaders, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
    ThinkingContent, ThinkingBudgets, ToolCall, Usage, UsageCost,
};
use crate::utils::error_body::{format_provider_error, NormalizedProviderError};
use crate::utils::provider_retry::{retry_provider_request, ProviderError, ProviderRetryFailure, ProviderRetryOptions};

/// Google Generative AI-specific stream options.
#[derive(Clone, Debug, Default)]
pub struct GoogleOptions {
    pub stream: StreamOptions,
    pub tool_choice: Option<String>,
    pub thinking: Option<GoogleThinkingConfig>,
}

#[derive(Clone, Debug)]
pub struct GoogleThinkingConfig {
    pub enabled: bool,
    /// -1 for dynamic, 0 to disable.
    pub budget_tokens: Option<f64>,
    pub level: Option<String>,
}

// Counter for generating unique tool call IDs.
static TOOL_CALL_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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

fn sanitize_surrogates(text: &str) -> String {
    crate::utils::sanitize::sanitize_surrogates(text)
}

/// Provider headers from `model.headers` merged with options headers (null
/// values dropped, options win).
fn merged_headers(model: &Model, options_headers: Option<&ProviderHeaders>) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = model.headers.clone().unwrap_or_default();
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

fn request_url(model: &Model) -> String {
    let base = if model.base_url.is_empty() {
        "https://generativelanguage.googleapis.com/v1beta".to_string()
    } else {
        model.base_url.trim_end_matches('/').to_string()
    };
    format!("{base}/models/{}:streamGenerateContent?alt=sse", model.id)
}

// ---------------------------------------------------------------------------
// Model-family detection
// ---------------------------------------------------------------------------

fn is_gemma4_model(model: &Model) -> bool {
    let id = model.id.to_lowercase();
    id.contains("gemma4") || id.contains("gemma-4")
}

fn is_gemini3_pro_model(model: &Model) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"gemini-3(?:\.\d+)?-pro").expect("valid regex"));
    re.is_match(&model.id.to_lowercase())
}

fn is_gemini3_flash_model(model: &Model) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"gemini-3(?:\.\d+)?-flash").expect("valid regex"));
    let id = model.id.to_lowercase();
    re.is_match(&id) || id == "gemini-flash-latest" || id == "gemini-flash-lite-latest"
}

/// Type of thinking config: level-based (Gemini 3 / Gemma 4) or budget-based.
fn get_disabled_thinking_config(model: &Model) -> GoogleThinkingConfig {
    // Google docs: Gemini 3.1 Pro cannot disable thinking, and Gemini 3
    // Flash / Flash-Lite do not support full thinking-off either. Use the
    // lowest supported thinkingLevel without includeThoughts so hidden
    // thinking remains invisible.
    if is_gemini3_pro_model(model) {
        return GoogleThinkingConfig {
            enabled: false,
            budget_tokens: None,
            level: Some("LOW".to_string()),
        };
    }
    if is_gemini3_flash_model(model) {
        return GoogleThinkingConfig {
            enabled: false,
            budget_tokens: None,
            level: Some("MINIMAL".to_string()),
        };
    }
    if is_gemma4_model(model) {
        return GoogleThinkingConfig {
            enabled: false,
            budget_tokens: None,
            level: Some("MINIMAL".to_string()),
        };
    }
    // Gemini 2.x supports disabling via thinkingBudget = 0.
    GoogleThinkingConfig {
        enabled: false,
        budget_tokens: Some(0.0),
        level: None,
    }
}

type ClampedThinkingLevel = &'static str;

fn get_thinking_level(effort: &str, model: &Model) -> String {
    if is_gemini3_pro_model(model) {
        return match effort {
            "minimal" | "low" => "LOW".to_string(),
            _ => "HIGH".to_string(),
        };
    }
    if is_gemma4_model(model) {
        return match effort {
            "minimal" | "low" => "MINIMAL".to_string(),
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

fn get_google_budget(model: &Model, effort: &str, custom_budgets: Option<&ThinkingBudgets>) -> f64 {
    if let Some(custom_budgets) = custom_budgets {
        let budget = match effort {
            "minimal" => custom_budgets.minimal,
            "low" => custom_budgets.low,
            "medium" => custom_budgets.medium,
            _ => custom_budgets.high,
        };
        if let Some(budget) = budget {
            return budget;
        }
    }

    if model.id.contains("2.5-pro") {
        return match effort {
            "minimal" => 128.0,
            "low" => 2048.0,
            "medium" => 8192.0,
            _ => 32768.0,
        };
    }

    if model.id.contains("2.5-flash-lite") {
        return match effort {
            "minimal" => 512.0,
            "low" => 2048.0,
            "medium" => 8192.0,
            _ => 24576.0,
        };
    }

    if model.id.contains("2.5-flash") {
        return match effort {
            "minimal" => 128.0,
            "low" => 2048.0,
            "medium" => 8192.0,
            _ => 24576.0,
        };
    }

    -1.0
}

// ---------------------------------------------------------------------------
// Request assembly
// ---------------------------------------------------------------------------

fn build_params(model: &Model, context: &Context, options: Option<&GoogleOptions>) -> Value {
    let contents = convert_messages(model, context);

    let mut generation_config: Vec<(String, Value)> = Vec::new();
    if let Some(temperature) = options.and_then(|o| o.stream.temperature) {
        generation_config.push(("temperature".to_string(), Value::Number(temperature)));
    }
    if let Some(max_tokens) = options.and_then(|o| o.stream.max_tokens) {
        generation_config.push(("maxOutputTokens".to_string(), Value::Number(max_tokens)));
    }

    let tools = context.tools.as_deref().unwrap_or(&[]);
    let function_calling_mode = if !tools.is_empty() {
        resolve_google_function_calling_mode(
            tools,
            options.and_then(|o| o.tool_choice.as_deref()),
            supports_google_strict_tool_sampling(&model.id),
        )
    } else {
        None
    };

    let mut entries: Vec<(String, Value)> = vec![(
        "contents".to_string(),
        Value::Array(contents.iter().map(|content| content.to_value()).collect()),
    )];

    if !generation_config.is_empty() {
        entries.push(("generationConfig".to_string(), Value::Map(generation_config)));
    }
    if let Some(system_prompt) = &context.system_prompt {
        entries.push(("systemInstruction".to_string(), Value::String(sanitize_surrogates(system_prompt))));
    }
    if !tools.is_empty() {
        if let Some(converted) = convert_tools(tools, false) {
            entries.push(("tools".to_string(), converted));
        }
    }
    if let Some(mode) = function_calling_mode {
        entries.push((
            "toolConfig".to_string(),
            Value::Map(vec![(
                "functionCallingConfig".to_string(),
                Value::Map(vec![("mode".to_string(), Value::String(mode))]),
            )]),
        ));
    }

    let thinking = options.and_then(|o| o.thinking.as_ref());
    if thinking.is_some_and(|t| t.enabled) && model.reasoning {
        let mut thinking_config: Vec<(String, Value)> = vec![("includeThoughts".to_string(), Value::Bool(true))];
        if let Some(level) = thinking.and_then(|t| t.level.clone()) {
            thinking_config.push(("thinkingLevel".to_string(), Value::String(level)));
        } else if let Some(budget) = thinking.and_then(|t| t.budget_tokens) {
            thinking_config.push(("thinkingBudget".to_string(), Value::Number(budget)));
        }
        entries.push(("thinkingConfig".to_string(), Value::Map(thinking_config)));
    } else if model.reasoning && thinking.is_some_and(|t| !t.enabled) {
        let disabled = get_disabled_thinking_config(model);
        let mut thinking_config: Vec<(String, Value)> = Vec::new();
        if let Some(level) = disabled.level {
            thinking_config.push(("thinkingLevel".to_string(), Value::String(level)));
        }
        if let Some(budget) = disabled.budget_tokens {
            thinking_config.push(("thinkingBudget".to_string(), Value::Number(budget)));
        }
        if !thinking_config.is_empty() {
            entries.push(("thinkingConfig".to_string(), Value::Map(thinking_config)));
        }
    }

    Value::Map(entries)
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

struct ParsedPart {
    text: Option<(String, bool, Option<String>)>,
    function_call: Option<(String, Value, Option<String>, Option<String>)>,
}

fn parse_part(part: &Value) -> ParsedPart {
    let Some(entries) = part.as_map() else {
        return ParsedPart {
            text: None,
            function_call: None,
        };
    };
    let get_str = |key: &str| -> Option<String> {
        entries
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.as_str())
            .map(|s| s.to_string())
    };
    let thought = entries
        .iter()
        .find(|(k, _)| k == "thought")
        .and_then(|(_, v)| v.as_bool())
        .unwrap_or(false);
    let thought_signature = get_str("thoughtSignature");

    let text = get_str("text").map(|text| (text, thought, thought_signature.clone()));

    let function_call = entries
        .iter()
        .find(|(k, _)| k == "functionCall")
        .and_then(|(_, v)| v.as_map())
        .map(|fc| {
            let name = fc
                .iter()
                .find(|(k, _)| k == "name")
                .and_then(|(_, v)| v.as_str())
                .unwrap_or("")
                .to_string();
            let args = fc
                .iter()
                .find(|(k, _)| k == "args")
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| Value::Map(Vec::new()));
            let id = fc
                .iter()
                .find(|(k, _)| k == "id")
                .and_then(|(_, v)| v.as_str())
                .map(|s| s.to_string());
            (name, args, id, thought_signature)
        });

    ParsedPart {
        text,
        function_call,
    }
}

enum CurrentBlock {
    Text { content_index: usize },
    Thinking { content_index: usize },
}

/// Stream function for the Google Generative AI API. Spawns a worker thread
/// that performs the request and feeds the returned stream.
pub fn stream(
    model: &Model,
    context: &Context,
    options: Option<&GoogleOptions>,
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
            let api_key = api_key.clone().ok_or_else(|| format!("No API key for provider: {}", model.provider))?;
            let params = build_params(&model, &context, options.as_ref());

            let mut request_headers: Vec<(String, String)> =
                vec![("x-goog-api-key".to_string(), api_key)];
            for (key, value) in merged_headers(
                &model,
                options.as_ref().and_then(|o| o.stream.request.headers.as_ref()),
            ) {
                request_headers.push((key, value));
            }
            let url = request_url(&model);

            let response = retry_provider_request(
                || {
                    client
                        .post_json(
                            &url,
                            &request_headers,
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
                ProviderRetryFailure::Error(error) => format_provider_error(
                    &NormalizedProviderError::new(error.message.clone(), error.status, None),
                    None,
                ),
                ProviderRetryFailure::Aborted => "Request was aborted".to_string(),
            })?;

            stream.push(crate::types::AssistantMessageEvent::Start {
                partial: output.clone(),
            });

            let mut current_block: Option<CurrentBlock> = None;
            crate::http::client::read_sse_stream(response.reader, |sse: &crate::http::sse::SseEvent| {
                let Ok(chunk) = crate::utils::json::parse_json_with_repair::<Value>(&sse.data) else {
                    return;
                };
                let Some(chunk_entries) = chunk.as_map() else {
                    return;
                };
                let get_str = |key: &str| -> Option<String> {
                    chunk_entries
                        .iter()
                        .find(|(k, _)| k == key)
                        .and_then(|(_, v)| v.as_str())
                        .map(|s| s.to_string())
                };

                // responseId: keep the first non-empty one from the stream.
                if output.response_id.is_none() {
                    output.response_id = get_str("responseId").filter(|id| !id.is_empty());
                }

                let candidate = chunk_entries
                    .iter()
                    .find(|(k, _)| k == "candidates")
                    .and_then(|(_, v)| v.as_array())
                    .and_then(|candidates| candidates.first())
                    .and_then(|candidate| candidate.as_map());

                if let Some(candidate) = candidate {
                    if let Some(parts) = candidate
                        .iter()
                        .find(|(k, _)| k == "content")
                        .and_then(|(_, v)| v.as_map())
                        .and_then(|content| {
                            content
                                .iter()
                                .find(|(k, _)| k == "parts")
                                .and_then(|(_, v)| v.as_array())
                        })
                    {
                        for part_value in parts {
                            let part = parse_part(part_value);

                            if let Some((text, is_thinking, part_signature)) = &part.text {
                                let block_needs_flush = match &current_block {
                                    None => true,
                                    Some(CurrentBlock::Text { .. }) => *is_thinking,
                                    Some(CurrentBlock::Thinking { .. }) => !*is_thinking,
                                };
                                if block_needs_flush {
                                    flush_current_block(
                                        &stream,
                                        &mut output,
                                        &mut current_block,
                                        true,
                                    );
                                    if *is_thinking {
                                        output.content.push(Content::Thinking(ThinkingContent {
                                            thinking: String::new(),
                                            thinking_signature: None,
                                            redacted: None,
                                        }));
                                        let content_index = output.content.len() - 1;
                                        current_block = Some(CurrentBlock::Thinking { content_index });
                                        stream.push(crate::types::AssistantMessageEvent::ThinkingStart {
                                            content_index: content_index as f64,
                                            partial: output.clone(),
                                        });
                                    } else {
                                        output.content.push(Content::Text(TextContent {
                                            text: String::new(),
                                            text_signature: None,
                                        }));
                                        let content_index = output.content.len() - 1;
                                        current_block = Some(CurrentBlock::Text { content_index });
                                        stream.push(crate::types::AssistantMessageEvent::TextStart {
                                            content_index: content_index as f64,
                                            partial: output.clone(),
                                        });
                                    }
                                }
                                match &current_block {
                                    Some(CurrentBlock::Thinking { content_index }) => {
                                        if let Content::Thinking(block) = &mut output.content[*content_index] {
                                            block.thinking.push_str(text);
                                            block.thinking_signature = retain_thought_signature(
                                                block.thinking_signature.clone(),
                                                part_signature.as_deref(),
                                            );
                                        }
                                        stream.push(crate::types::AssistantMessageEvent::ThinkingDelta {
                                            content_index: *content_index as f64,
                                            delta: text.clone(),
                                            partial: output.clone(),
                                        });
                                    }
                                    Some(CurrentBlock::Text { content_index }) => {
                                        if let Content::Text(block) = &mut output.content[*content_index] {
                                            block.text.push_str(text);
                                            block.text_signature = retain_thought_signature(
                                                block.text_signature.clone(),
                                                part_signature.as_deref(),
                                            );
                                        }
                                        stream.push(crate::types::AssistantMessageEvent::TextDelta {
                                            content_index: *content_index as f64,
                                            delta: text.clone(),
                                            partial: output.clone(),
                                        });
                                    }
                                    None => {}
                                }
                            }

                            if let Some((name, args, provided_id, part_signature)) = &part.function_call {
                                flush_current_block(&stream, &mut output, &mut current_block, true);

                                // Generate unique ID if not provided or duplicate.
                                let needs_new_id = match provided_id {
                                    None => true,
                                    Some(id) => output.content.iter().any(|block| {
                                        matches!(block, Content::ToolCall(tool_call) if &tool_call.id == id)
                                    }),
                                };
                                let tool_call_id = if needs_new_id {
                                    let counter = TOOL_CALL_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                                    format!("{name}_{}_{counter}", now_ms() as u64)
                                } else {
                                    provided_id.clone().unwrap_or_default()
                                };

                                let tool_call = ToolCall {
                                    id: tool_call_id,
                                    name: name.clone(),
                                    arguments: args.clone(),
                                    thought_signature: part_signature.clone(),
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
                                    delta: crate::utils::json::json_stringify(args),
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

                    if let Some(finish_reason) = candidate
                        .iter()
                        .find(|(k, _)| k == "finishReason")
                        .and_then(|(_, v)| v.as_str())
                    {
                        output.raw_stop_reason = Some(finish_reason.to_string());
                        output.stop_reason = map_stop_reason(finish_reason);
                        if output.content.iter().any(|block| matches!(block, Content::ToolCall(_))) {
                            output.stop_reason = StopReason::ToolUse;
                        }
                    }
                }

                if let Some(usage) = chunk_entries.iter().find(|(k, _)| k == "usageMetadata") {
                    if let Some(usage_entries) = usage.1.as_map() {
                        let num = |key: &str| -> f64 {
                            usage_entries
                                .iter()
                                .find(|(k, _)| k == key)
                                .and_then(|(_, v)| v.as_number())
                                .unwrap_or(0.0)
                        };
                        let cached = num("cachedContentTokenCount");
                        let thoughts = num("thoughtsTokenCount");
                        output.usage = Usage {
                            input: (num("promptTokenCount") - cached).max(0.0),
                            output: num("candidatesTokenCount") + thoughts,
                            cache_read: cached,
                            cache_write: 0.0,
                            cache_write_1h: None,
                            reasoning: Some(thoughts),
                            total_tokens: num("totalTokenCount"),
                            cost: UsageCost {
                                input: 0.0,
                                output: 0.0,
                                cache_read: 0.0,
                                cache_write: 0.0,
                                total: 0.0,
                            },
                        };
                        calculate_cost(&model, &mut output.usage);
                    }
                }
            });

            flush_current_block(&stream, &mut output, &mut current_block, false);

            if output.stop_reason == StopReason::Pending {
                return Err("Google stream ended without a finish reason".to_string());
            }
            if output.stop_reason == StopReason::Aborted || output.stop_reason == StopReason::Error {
                let error_message = output
                    .raw_stop_reason
                    .as_deref()
                    .map(|reason| format!("Provider stopped with: {reason}"))
                    .unwrap_or_else(|| "An unknown error occurred".to_string());
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

use crate::types::Content;

/// Emits text_end/thinking_end for the current block and clears it.
/// Mirrors the JS flush points (before a new block and at stream end).
fn flush_current_block(
    stream: &AssistantMessageEventStream,
    output: &mut AssistantMessage,
    current_block: &mut Option<CurrentBlock>,
    _emit_end: bool,
) {
    match current_block.take() {
        Some(CurrentBlock::Text { content_index }) => {
            let content = match &output.content[content_index] {
                Content::Text(block) => block.text.clone(),
                _ => String::new(),
            };
            stream.push(crate::types::AssistantMessageEvent::TextEnd {
                content_index: content_index as f64,
                content,
                partial: output.clone(),
            });
        }
        Some(CurrentBlock::Thinking { content_index }) => {
            let content = match &output.content[content_index] {
                Content::Thinking(block) => block.thinking.clone(),
                _ => String::new(),
            };
            stream.push(crate::types::AssistantMessageEvent::ThinkingEnd {
                content_index: content_index as f64,
                content,
                partial: output.clone(),
            });
        }
        None => {}
    }
}

/// Simple-stream variant: builds base options and delegates to `stream`.
pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<&SimpleStreamOptions>,
    api_key: Option<&str>,
    client: &HttpClient,
) -> AssistantMessageEventStream {
    let api_key = api_key.map(|s| s.to_string());
    let _ = api_key
        .as_deref()
        .ok_or_else(|| format!("No API key for provider: {}", model.provider));

    let base = build_base_options(model, context, options, api_key.as_deref());
    let Some(reasoning) = options.and_then(|o| o.reasoning.as_deref()) else {
        return stream(
            model,
            context,
            Some(&GoogleOptions {
                stream: base,
                thinking: Some(GoogleThinkingConfig {
                    enabled: false,
                    budget_tokens: None,
                    level: None,
                }),
                ..GoogleOptions::default()
            }),
            api_key.as_deref(),
            client,
        );
    };

    let clamped_reasoning = clamp_thinking_level(model, reasoning);
    let effort = if clamped_reasoning == "off" { "high" } else { clamped_reasoning.as_str() };

    if is_gemini3_pro_model(model) || is_gemini3_flash_model(model) || is_gemma4_model(model) {
        return stream(
            model,
            context,
            Some(&GoogleOptions {
                stream: base,
                thinking: Some(GoogleThinkingConfig {
                    enabled: true,
                    budget_tokens: None,
                    level: Some(get_thinking_level(effort, model)),
                }),
                ..GoogleOptions::default()
            }),
            api_key.as_deref(),
            client,
        );
    }

    stream(
        model,
        context,
        Some(&GoogleOptions {
            stream: base,
            thinking: Some(GoogleThinkingConfig {
                enabled: true,
                budget_tokens: Some(get_google_budget(
                    model,
                    effort,
                    options.and_then(|o| o.thinking_budgets.as_ref()),
                )),
                level: None,
            }),
            ..GoogleOptions::default()
        }),
        api_key.as_deref(),
        client,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str) -> Model {
        Model {
            id: id.to_string(),
            name: id.to_string(),
            api: "google-generative-ai".to_string(),
            provider: "google".to_string(),
            base_url: String::new(),
            reasoning: true,
            thinking_level_map: None,
            input: vec!["text".to_string()],
            cost: crate::types::ModelCost {
                rates: crate::types::ModelCostRates {
                    input: 1.25,
                    output: 5.0,
                    cache_read: 0.1,
                    cache_write: 1.25,
                },
                tiers: None,
            },
            context_window: 1_000_000.0,
            max_tokens: 8192.0,
            sampling_params: None,
            headers: None,
            compat: None,
        }
    }

    #[test]
    fn detects_model_families() {
        assert!(is_gemini3_pro_model(&model("gemini-3-pro")));
        assert!(is_gemini3_pro_model(&model("gemini-3.1-pro")));
        assert!(!is_gemini3_pro_model(&model("gemini-2.5-pro")));
        assert!(is_gemini3_flash_model(&model("gemini-3-flash")));
        assert!(is_gemini3_flash_model(&model("gemini-flash-latest")));
        assert!(is_gemma4_model(&model("gemma-4-27b-it")));
        assert!(is_gemma4_model(&model("gemma4-9b")));
    }

    #[test]
    fn maps_thinking_levels() {
        assert_eq!(get_thinking_level("minimal", &model("gemini-2.5-pro")), "MINIMAL");
        assert_eq!(get_thinking_level("high", &model("gemini-2.5-pro")), "HIGH");
        assert_eq!(get_thinking_level("low", &model("gemini-3-pro")), "LOW");
        assert_eq!(get_thinking_level("medium", &model("gemini-3-pro")), "HIGH");
        assert_eq!(get_thinking_level("low", &model("gemma-4-9b")), "MINIMAL");
    }

    #[test]
    fn computes_google_budgets() {
        assert_eq!(get_google_budget(&model("gemini-2.5-pro"), "high", None), 32768.0);
        assert_eq!(get_google_budget(&model("gemini-2.5-flash"), "medium", None), 8192.0);
        assert_eq!(get_google_budget(&model("gemini-2.5-flash-lite"), "low", None), 2048.0);
        assert_eq!(get_google_budget(&model("other"), "high", None), -1.0);
        let custom = ThinkingBudgets {
            minimal: None,
            low: Some(100.0),
            medium: None,
            high: None,
        };
        assert_eq!(get_google_budget(&model("other"), "low", Some(&custom)), 100.0);
    }

    #[test]
    fn builds_disabled_thinking_configs() {
        let config = get_disabled_thinking_config(&model("gemini-3-pro"));
        assert_eq!(config.level.as_deref(), Some("LOW"));
        assert_eq!(config.budget_tokens, None);
        let config = get_disabled_thinking_config(&model("gemini-3-flash"));
        assert_eq!(config.level.as_deref(), Some("MINIMAL"));
        let config = get_disabled_thinking_config(&model("gemini-2.5-pro"));
        assert_eq!(config.budget_tokens, Some(0.0));
        assert_eq!(config.level, None);
    }

    #[test]
    fn parses_function_call_parts() {
        let part = Value::Map(vec![
            (
                "functionCall".to_string(),
                Value::Map(vec![
                    ("name".to_string(), Value::String("read".to_string())),
                    ("args".to_string(), Value::Map(vec![("path".to_string(), Value::String("/tmp".to_string()))])),
                ]),
            ),
            ("thoughtSignature".to_string(), Value::String("sig".to_string())),
        ]);
        let parsed = parse_part(&part);
        let (name, args, id, signature) = parsed.function_call.unwrap();
        assert_eq!(name, "read");
        assert_eq!(id, None);
        assert_eq!(signature.as_deref(), Some("sig"));
        assert!(matches!(args, Value::Map(_)));
    }

    #[test]
    fn builds_request_url() {
        let m = model("gemini-2.5-pro");
        assert_eq!(
            request_url(&m),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );
        let mut m = model("gemini-2.5-pro");
        m.base_url = "https://gateway.example.com/v1beta".to_string();
        assert_eq!(
            request_url(&m),
            "https://gateway.example.com/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );
    }
}
