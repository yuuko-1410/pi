//! OpenAI Responses API provider, port of
//! `packages/ai/src/api/openai-responses.ts`.
//!
//! This is the reference provider adapter: request assembly, HTTP dispatch
//! with retries, SSE streaming into `processResponsesStream`, and error
//! normalization. Other OpenAI-compatible adapters follow the same shape.

use pi_protocol::Value;

use crate::api::constrained_sampling::create_grammar_tool_input_properties;
use crate::api::github_copilot_headers::{
    build_copilot_dynamic_headers, has_copilot_vision_input, CopilotDynamicHeadersParams,
};
use crate::api::openai_responses_shared::{
    convert_responses_messages, convert_responses_tools, ConvertResponsesToolsOptions,
};
use crate::api::openai_stream::{
    parse_stream_event, process_responses_stream, OpenAIResponsesStreamOptions as StreamProcessOptions,
};
use crate::api::prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::simple_options::{build_base_options, clamp_max_tokens_to_context};
use crate::event_stream::AssistantMessageEventStream;
use crate::http::client::HttpClient;
use crate::http::sse::SseEvent;
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::types::{
    AssistantMessage, CacheRetention, Context, Model, OpenAIResponsesCompat, ProviderHeaders,
    SimpleStreamOptions, StopReason, StreamOptions, Usage, UsageCost,
};
use crate::utils::deferred_tools::split_deferred_tools;
use crate::utils::error_body::{format_provider_error, NormalizedProviderError};
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{retry_provider_request, ProviderError, ProviderRetryOptions};

const OPENAI_TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "openai-codex", "opencode"];
// OpenAI Responses rejects max_output_tokens below 16.
const OPENAI_RESPONSES_MIN_OUTPUT_TOKENS: f64 = 16.0;

fn has_header(headers: Option<&ProviderHeaders>, name: &str) -> bool {
    let Some(headers) = headers else {
        return false;
    };
    headers.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case(name)
            && value.as_deref().is_some_and(|value| !value.trim().is_empty())
    })
}

fn get_client_api_key(provider: &str, api_key: Option<&str>, headers: Option<&ProviderHeaders>) -> Result<String, String> {
    if let Some(api_key) = api_key {
        if !api_key.is_empty() {
            return Ok(api_key.to_string());
        }
    }
    if has_header(headers, "authorization") || has_header(headers, "cf-aig-authorization") {
        return Ok("unused".to_string());
    }
    Err(format!("No API key for provider: {provider}"))
}

fn detect_session_affinity_format(model: &Model) -> &'static str {
    if model.provider == "openrouter" || model.base_url.contains("openrouter.ai") {
        "openrouter"
    } else {
        "openai"
    }
}

/// Resolve cache retention preference. Defaults to "short" and uses
/// PI_CACHE_RETENTION for backward compatibility.
fn resolve_cache_retention(
    cache_retention: Option<&CacheRetention>,
    env: Option<&crate::types::ProviderEnv>,
) -> CacheRetention {
    if let Some(cache_retention) = cache_retention {
        return cache_retention.clone();
    }
    if get_provider_env_value("PI_CACHE_RETENTION", env).as_deref() == Some("long") {
        return "long".to_string();
    }
    "short".to_string()
}

#[derive(Clone, Debug)]
pub struct RequiredResponsesCompat {
    pub supports_developer_role: bool,
    pub session_affinity_format: String,
    pub supports_long_cache_retention: bool,
    pub supports_strict_mode: bool,
    pub supports_openai_grammar_tools: bool,
    pub supports_additional_tools: bool,
    pub supports_tool_search: bool,
    pub supports_explicit_prompt_cache_mode: bool,
}

impl RequiredResponsesCompat {
    pub fn new(model: &Model) -> Self {
        let compat = match &model.compat {
            Some(crate::types::ModelCompat::OpenAiResponses(compat)) => compat.clone(),
            _ => OpenAIResponsesCompat::default(),
        };
        Self {
            supports_developer_role: compat.supports_developer_role.unwrap_or(true),
            session_affinity_format: compat
                .session_affinity_format
                .unwrap_or_else(|| detect_session_affinity_format(model).to_string()),
            supports_long_cache_retention: compat.supports_long_cache_retention.unwrap_or(true),
            supports_strict_mode: compat.supports_strict_mode.unwrap_or(false),
            supports_openai_grammar_tools: compat.supports_openai_grammar_tools.unwrap_or(false),
            supports_additional_tools: compat.supports_additional_tools.unwrap_or(false),
            supports_tool_search: compat.supports_tool_search.unwrap_or(false),
            supports_explicit_prompt_cache_mode: compat.supports_explicit_prompt_cache_mode.unwrap_or(false),
        }
    }
}

fn get_prompt_cache_retention(compat: &RequiredResponsesCompat, cache_retention: &CacheRetention) -> Option<String> {
    if cache_retention == "long" && compat.supports_long_cache_retention {
        Some("24h".to_string())
    } else {
        None
    }
}

/// OpenAI Responses-specific stream options.
#[derive(Clone, Debug, Default)]
pub struct OpenAIResponsesOptions {
    pub stream: StreamOptions,
    pub reasoning_effort: Option<String>,
    pub reasoning_summary: Option<String>,
    pub service_tier: Option<String>,
    pub tool_choice: Option<Value>,
}

impl From<OpenAIResponsesOptions> for StreamOptions {
    fn from(options: OpenAIResponsesOptions) -> Self {
        options.stream
    }
}

fn format_openai_responses_error(error: &ProviderError) -> String {
    let normalized = NormalizedProviderError::new(error.message.clone(), error.status, None);
    format_provider_error(&normalized, Some("OpenAI API error"))
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

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as f64
}

fn create_client_headers(
    model: &Model,
    context: &Context,
    options_headers: Option<&ProviderHeaders>,
    session_id: Option<&str>,
) -> Vec<(String, String)> {
    let compat = RequiredResponsesCompat::new(model);
    let mut headers: Vec<(String, String)> = model.headers.clone().unwrap_or_default();
    if model.provider == "github-copilot" {
        let has_images = has_copilot_vision_input(&context.messages);
        let copilot_headers = build_copilot_dynamic_headers(CopilotDynamicHeadersParams {
            messages: &context.messages,
            has_images,
        });
        for (key, value) in copilot_headers {
            if let Some(existing) = headers.iter_mut().find(|(k, _)| k == &key) {
                existing.1 = value;
            } else {
                headers.push((key, value));
            }
        }
    }

    if let Some(session_id) = session_id {
        if compat.session_affinity_format == "openrouter" {
            headers.push(("x-session-id".to_string(), session_id.to_string()));
        } else {
            if compat.session_affinity_format == "openai" {
                headers.push(("session_id".to_string(), session_id.to_string()));
            }
            headers.push(("x-client-request-id".to_string(), session_id.to_string()));
        }
    }

    // Merge options headers last so they can override defaults.
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

fn build_params(
    model: &Model,
    context: &Context,
    options: Option<&OpenAIResponsesOptions>,
    compat: &RequiredResponsesCompat,
    grammar_tool_input_properties: &[(String, String)],
    cache_retention: &CacheRetention,
    session_id: Option<&str>,
) -> Value {
    let deferred_tools_mode = if compat.supports_additional_tools {
        Some("additional-tools")
    } else if compat.supports_tool_search {
        Some("tool-search")
    } else {
        None
    };
    let tool_placement = split_deferred_tools(context, deferred_tools_mode.is_some(), None);
    let allowed: std::collections::HashSet<String> =
        OPENAI_TOOL_CALL_PROVIDERS.iter().map(|p| p.to_string()).collect();
    let messages = convert_responses_messages(model, context, &allowed, Some(&{
        let conversion_options = crate::api::openai_responses_shared::ConvertResponsesMessagesOptions {
            include_system_prompt: Some(true),
            grammar_tool_input_properties: Some(grammar_tool_input_properties.to_vec()),
            deferred_tools: Some(tool_placement.deferred.clone()),
            deferred_tools_mode: deferred_tools_mode.map(|mode| mode.to_string()),
            tool_options: Some(ConvertResponsesToolsOptions {
                supports_strict_mode: Some(compat.supports_strict_mode),
                supports_openai_grammar_tools: Some(compat.supports_openai_grammar_tools),
                ..ConvertResponsesToolsOptions::default()
            }),
        };
        conversion_options
    }));

    let disable_implicit_prompt_cache =
        cache_retention == "none" && compat.supports_explicit_prompt_cache_mode;

    let mut entries: Vec<(String, Value)> = vec![
        ("model".to_string(), Value::String(model.id.clone())),
        (
            "input".to_string(),
            Value::Array(messages.iter().map(|item| item.to_value()).collect()),
        ),
        ("stream".to_string(), Value::Bool(true)),
        (
            "prompt_cache_key".to_string(),
            match cache_retention.as_str() {
                "none" => Value::Null,
                _ => match clamp_openai_prompt_cache_key(session_id) {
                    Some(key) => Value::String(key),
                    None => Value::Null,
                },
            },
        ),
        (
            "prompt_cache_retention".to_string(),
            match get_prompt_cache_retention(compat, cache_retention) {
                Some(retention) => Value::String(retention),
                None => Value::Null,
            },
        ),
        (
            "prompt_cache_options".to_string(),
            if disable_implicit_prompt_cache {
                Value::Map(vec![("mode".to_string(), Value::String("explicit".to_string()))])
            } else {
                Value::Null
            },
        ),
        ("store".to_string(), Value::Bool(false)),
    ];

    if let Some(max_tokens) = options.and_then(|o| o.stream.max_tokens) {
        entries.push((
            "max_output_tokens".to_string(),
            Value::Number(max_tokens.max(OPENAI_RESPONSES_MIN_OUTPUT_TOKENS)),
        ));
    }

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
                    supports_strict_mode: Some(compat.supports_strict_mode),
                    supports_openai_grammar_tools: Some(compat.supports_openai_grammar_tools),
                    ..ConvertResponsesToolsOptions::default()
                }))
                .iter()
                .map(|tool| tool.to_value())
                .collect(),
            ),
        ));
    }

    if let Some(tool_choice) = options.and_then(|o| o.tool_choice.clone()) {
        entries.push(("tool_choice".to_string(), tool_choice));
    }

    if model.reasoning {
        if let Some(reasoning_effort) = options.and_then(|o| o.reasoning_effort.clone()) {
            let effort = model
                .thinking_level_map
                .as_ref()
                .and_then(|map| {
                    map.iter()
                        .find(|(key, _)| key == &reasoning_effort)
                        .and_then(|(_, value)| value.clone())
                })
                .unwrap_or(reasoning_effort);
            entries.push((
                "reasoning".to_string(),
                Value::Map(vec![
                    ("effort".to_string(), Value::String(effort)),
                    (
                        "summary".to_string(),
                        Value::String(options.and_then(|o| o.reasoning_summary.clone()).unwrap_or_else(|| "auto".to_string())),
                    ),
                ]),
            ));
            entries.push((
                "include".to_string(),
                Value::Array(vec![Value::String("reasoning.encrypted_content".to_string())]),
            ));
        } else if model.provider != "github-copilot" {
            let off_mapping = model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.iter().find(|(key, _)| key == "off"))
                .and_then(|(_, value)| value.clone());
            if off_mapping.is_some() {
                entries.push((
                    "reasoning".to_string(),
                    Value::Map(vec![("effort".to_string(), Value::String(off_mapping.unwrap_or_else(|| "none".to_string())))]),
                ));
            }
        }
        if model.provider == "xai" {
            entries.push((
                "include".to_string(),
                Value::Array(vec![Value::String("reasoning.encrypted_content".to_string())]),
            ));
        }
    }

    // Last so custom keys override the named request fields.
    if let Some(sampling_params) = options.and_then(|o| o.stream.sampling_params.as_ref()) {
        for (key, value) in sampling_params {
            if let Some(existing) = entries.iter_mut().find(|(k, _)| k == key) {
                existing.1 = value.clone();
            } else {
                entries.push((key.clone(), value.clone()));
            }
        }
    }

    Value::Map(entries)
}

/// Stream function for the OpenAI Responses API. Spawns a worker thread that
/// performs the request and feeds the returned stream (mirroring the JS
/// async IIFE). Aborts surface as aborted messages.
pub fn stream(
    model: &Model,
    context: &Context,
    options: Option<&OpenAIResponsesOptions>,
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
            let api_key = get_client_api_key(&model.provider, api_key.as_deref(), options.as_ref().and_then(|o| o.stream.request.headers.as_ref()))?;
            let cache_retention = resolve_cache_retention(
                options.as_ref().and_then(|o| o.stream.cache_retention.as_ref()),
                options.as_ref().and_then(|o| o.stream.request.env.as_ref()),
            );
            let cache_session_id = if cache_retention == "none" {
                None
            } else {
                options.as_ref().and_then(|o| o.stream.session_id.clone())
            };
            let compat = RequiredResponsesCompat::new(&model);
            let grammar_tool_input_properties = create_grammar_tool_input_properties(
                context.tools.as_deref(),
                compat.supports_openai_grammar_tools,
            );
            let headers = create_client_headers(
                &model,
                &context,
                options.as_ref().and_then(|o| o.stream.request.headers.as_ref()),
                cache_session_id.as_deref(),
            );
            let params = build_params(
                &model,
                &context,
                options.as_ref(),
                &compat,
                &grammar_tool_input_properties,
                &cache_retention,
                cache_session_id.as_deref(),
            );

            let mut request_headers = vec![("Authorization".to_string(), format!("Bearer {api_key}"))];
            for (key, value) in headers {
                request_headers.push((key, value));
            }
            let url = format!("{}/responses", model.base_url.trim_end_matches('/'));

            let response = retry_provider_request(
                || {
                    client
                        .post_json(
                            &url,
                            &request_headers,
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
                crate::utils::provider_retry::ProviderRetryFailure::Error(error) => format_openai_responses_error(&error),
                crate::utils::provider_retry::ProviderRetryFailure::Aborted => "Request was aborted".to_string(),
            })?;

            stream.push(crate::types::AssistantMessageEvent::Start {
                partial: output.clone(),
            });

            let mut events = Vec::new();
            crate::http::client::read_sse_stream(response.reader, |sse: &SseEvent| {
                if let Some(event) = parse_stream_event(&sse.data) {
                    events.push(event);
                }
            });

            process_responses_stream(
                events,
                &mut output,
                &stream,
                &model,
                Some(&StreamProcessOptions {
                    service_tier: options.as_ref().and_then(|o| o.service_tier.clone()),
                    grammar_tool_input_properties: Some(grammar_tool_input_properties),
                    apply_service_tier_pricing: None,
                }),
            )?;

            // Calculate cost from the final usage.
            calculate_cost(&model, &mut output.usage);

            if output.stop_reason == StopReason::Pending {
                return Err("OpenAI Responses stream ended without a stop reason".to_string());
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

/// Simple-stream variant: builds base options and delegates to `stream`.
pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<&SimpleStreamOptions>,
    api_key: Option<&str>,
    client: &HttpClient,
) -> AssistantMessageEventStream {
    let _ = get_client_api_key(&model.provider, api_key, options.and_then(|o| o.stream.request.headers.as_ref()));

    let base = build_base_options(model, context, options, api_key);
    let clamped_reasoning = options
        .and_then(|o| o.reasoning.as_deref())
        .map(|level| clamp_thinking_level(model, level));
    let reasoning_effort = match clamped_reasoning.as_deref() {
        Some("off") => None,
        Some(level) => Some(level.to_string()),
        None => None,
    };

    let stream_options = OpenAIResponsesOptions {
        stream: base,
        reasoning_effort,
        ..OpenAIResponsesOptions::default()
    };
    stream(model, context, Some(&stream_options), api_key, client)
}

/// Mirrors `clampMaxTokensToContext` usage from buildBaseOptions.
pub fn clamp_max_tokens(model: &Model, context: &Context, max_tokens: f64) -> f64 {
    clamp_max_tokens_to_context(model, context, max_tokens)
}
