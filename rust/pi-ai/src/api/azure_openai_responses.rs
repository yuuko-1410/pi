//! Azure OpenAI Responses API provider, port of
//! `packages/ai/src/api/azure-openai-responses.ts`.
//!
//! Reuses the OpenAI Responses message/tool conversion and the stream
//! processing state machine; the differences are the Azure endpoint layout
//! (`{baseUrl}/openai/responses?api-version=...`), the `api-key` auth header,
//! deployment-name resolution, and a smaller parameter set (no service tier,
//! no deferred tools, no session affinity).

use pi_protocol::Value;

use crate::api::constrained_sampling::create_grammar_tool_input_properties;
use crate::api::openai_responses_shared::{
    convert_responses_messages, convert_responses_tools, ConvertResponsesToolsOptions,
};
use crate::api::openai_stream::{
    parse_stream_event, process_responses_stream, OpenAIResponsesStreamOptions as StreamProcessOptions,
};
use crate::api::prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::simple_options::build_base_options;
use crate::event_stream::AssistantMessageEventStream;
use crate::http::client::HttpClient;
use crate::http::sse::SseEvent;
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::types::{
    AssistantMessage, Context, Model, ProviderHeaders, SimpleStreamOptions, StopReason, StreamOptions, Usage,
    UsageCost,
};
use crate::utils::error_body::{format_provider_error, NormalizedProviderError};
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{retry_provider_request, ProviderError, ProviderRetryOptions};

const DEFAULT_AZURE_API_VERSION: &str = "v1";
const AZURE_TOOL_CALL_PROVIDERS: [&str; 4] = ["openai", "openai-codex", "opencode", "azure-openai-responses"];
// OpenAI Responses rejects max_output_tokens below 16: https://github.com/earendil-works/pi/issues/6265
const OPENAI_RESPONSES_MIN_OUTPUT_TOKENS: f64 = 16.0;

/// Azure OpenAI Responses-specific options.
#[derive(Clone, Debug, Default)]
pub struct AzureOpenAIResponsesOptions {
    pub stream: StreamOptions,
    pub reasoning_effort: Option<String>,
    pub reasoning_summary: Option<String>,
    pub azure_api_version: Option<String>,
    pub azure_resource_name: Option<String>,
    pub azure_base_url: Option<String>,
    pub azure_deployment_name: Option<String>,
}

fn parse_deployment_name_map(value: Option<&str>) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Some(value) = value else {
        return map;
    };
    for entry in value.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((model_id, deployment_name)) = trimmed.split_once('=') else {
            continue;
        };
        if model_id.is_empty() || deployment_name.is_empty() {
            continue;
        }
        map.insert(model_id.trim().to_string(), deployment_name.trim().to_string());
    }
    map
}

fn resolve_deployment_name(model: &Model, options: Option<&AzureOpenAIResponsesOptions>) -> String {
    if let Some(name) = options.and_then(|o| o.azure_deployment_name.clone()) {
        return name;
    }
    let mapped = parse_deployment_name_map(
        get_provider_env_value(
            "AZURE_OPENAI_DEPLOYMENT_NAME_MAP",
            options.and_then(|o| o.stream.request.env.as_ref()),
        )
        .as_deref(),
    )
    .get(&model.id)
    .cloned();
    mapped.unwrap_or_else(|| model.id.clone())
}

fn format_azure_openai_error(error: &ProviderError) -> String {
    let normalized = NormalizedProviderError::new(error.message.clone(), error.status, None);
    format_provider_error(&normalized, Some("Azure OpenAI API error"))
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
        api: "azure-openai-responses".to_string(),
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

fn compat_supports_strict_mode(model: &Model) -> bool {
    match &model.compat {
        Some(crate::types::ModelCompat::OpenAiResponses(compat)) => compat.supports_strict_mode.unwrap_or(true),
        _ => true,
    }
}

fn compat_supports_openai_grammar_tools(model: &Model) -> bool {
    match &model.compat {
        Some(crate::types::ModelCompat::OpenAiResponses(compat)) => {
            compat.supports_openai_grammar_tools.unwrap_or(false)
        }
        _ => false,
    }
}

/// Mirrors `normalizeAzureBaseUrl`: Azure hosts with a missing/partial path
/// are normalized to `/openai/v1` (so the SDK-style `/openai/responses`
/// suffix resolves); other URLs pass through unchanged. Trailing slashes are
/// stripped.
fn normalize_azure_base_url(base_url: &str) -> Result<String, String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let (scheme, rest) = match trimmed.split_once("://") {
        Some((scheme, rest)) => (format!("{scheme}://"), rest),
        None => return Err(format!("Invalid Azure OpenAI base URL: {base_url}")),
    };
    let (authority, tail) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    let hostname = authority.split(':').next().unwrap_or(authority);
    let (path, query) = match tail.find('?') {
        Some(index) => (&tail[..index], Some(&tail[index..])),
        None => (tail, None),
    };
    let normalized_path = path.trim_end_matches('/');
    let is_azure_host = hostname.ends_with(".openai.azure.com")
        || hostname.ends_with(".cognitiveservices.azure.com")
        || hostname.ends_with(".ai.azure.com");
    if is_azure_host
        && (normalized_path.is_empty()
            || normalized_path == "/"
            || normalized_path == "/openai"
            || normalized_path == "/openai/v1/responses")
    {
        return Ok(format!("{scheme}{authority}/openai/v1"));
    }
    let result = match query {
        Some(query) => format!("{scheme}{authority}{path}{query}"),
        None => format!("{scheme}{authority}{path}"),
    };
    Ok(result.trim_end_matches('/').to_string())
}

fn build_default_base_url(resource_name: &str) -> String {
    format!("https://{resource_name}.openai.azure.com/openai/v1")
}

fn resolve_azure_config(
    model: &Model,
    options: Option<&AzureOpenAIResponsesOptions>,
) -> Result<(String, String), String> {
    let api_version = options
        .and_then(|o| o.azure_api_version.clone())
        .or_else(|| {
            get_provider_env_value("AZURE_OPENAI_API_VERSION", options.and_then(|o| o.stream.request.env.as_ref()))
        })
        .unwrap_or_else(|| DEFAULT_AZURE_API_VERSION.to_string());

    let base_url = options
        .and_then(|o| o.azure_base_url.as_deref().map(str::trim))
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .or_else(|| {
            get_provider_env_value("AZURE_OPENAI_BASE_URL", options.and_then(|o| o.stream.request.env.as_ref()))
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        });
    let resource_name = options
        .and_then(|o| o.azure_resource_name.clone())
        .or_else(|| {
            get_provider_env_value("AZURE_OPENAI_RESOURCE_NAME", options.and_then(|o| o.stream.request.env.as_ref()))
        });

    let mut resolved_base_url = base_url;

    if resolved_base_url.is_none() {
        if let Some(resource_name) = &resource_name {
            resolved_base_url = Some(build_default_base_url(resource_name));
        }
    }

    if resolved_base_url.is_none() && !model.base_url.is_empty() {
        resolved_base_url = Some(model.base_url.clone());
    }

    let Some(resolved_base_url) = resolved_base_url else {
        return Err(
            "Azure OpenAI base URL is required. Set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME, or pass azureBaseUrl, azureResourceName, or model.baseUrl."
                .to_string(),
        );
    };

    Ok((normalize_azure_base_url(&resolved_base_url)?, api_version))
}

fn create_client_headers(model: &Model, options_headers: Option<&ProviderHeaders>) -> Vec<(String, String)> {
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

fn build_params(
    model: &Model,
    context: &Context,
    options: Option<&AzureOpenAIResponsesOptions>,
    deployment_name: &str,
    grammar_tool_input_properties: &[(String, String)],
) -> Value {
    let allowed: std::collections::HashSet<String> =
        AZURE_TOOL_CALL_PROVIDERS.iter().map(|p| p.to_string()).collect();
    let messages = convert_responses_messages(model, context, &allowed, Some(&{
        let conversion_options = crate::api::openai_responses_shared::ConvertResponsesMessagesOptions {
            include_system_prompt: Some(true),
            grammar_tool_input_properties: Some(grammar_tool_input_properties.to_vec()),
            deferred_tools: None,
            deferred_tools_mode: None,
            tool_options: None,
        };
        conversion_options
    }));

    let mut entries: Vec<(String, Value)> = vec![
        ("model".to_string(), Value::String(deployment_name.to_string())),
        (
            "input".to_string(),
            Value::Array(messages.iter().map(|item| item.to_value()).collect()),
        ),
        ("stream".to_string(), Value::Bool(true)),
        (
            "prompt_cache_key".to_string(),
            match clamp_openai_prompt_cache_key(options.and_then(|o| o.stream.session_id.as_deref())) {
                Some(key) => Value::String(key),
                None => Value::Null,
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

    if let Some(tools) = &context.tools {
        if !tools.is_empty() {
            entries.push((
                "tools".to_string(),
                Value::Array(
                    convert_responses_tools(tools, Some(&ConvertResponsesToolsOptions {
                        supports_strict_mode: Some(compat_supports_strict_mode(model)),
                        supports_openai_grammar_tools: Some(compat_supports_openai_grammar_tools(model)),
                        ..ConvertResponsesToolsOptions::default()
                    }))
                    .iter()
                    .map(|tool| tool.to_value())
                    .collect(),
                ),
            ));
        }
    }

    if model.reasoning {
        let has_reasoning_effort = options
            .and_then(|o| o.reasoning_effort.as_deref())
            .is_some_and(|value| !value.is_empty());
        let has_reasoning_summary = options
            .and_then(|o| o.reasoning_summary.as_deref())
            .is_some_and(|value| !value.is_empty());
        if has_reasoning_effort || has_reasoning_summary {
            let effort = options
                .and_then(|o| o.reasoning_effort.clone())
                .map(|level| {
                    model
                        .thinking_level_map
                        .as_ref()
                        .and_then(|map| map.iter().find(|(key, _)| key == &level))
                        .and_then(|(_, value)| value.clone())
                        .unwrap_or(level)
                })
                .unwrap_or_else(|| "medium".to_string());
            let summary = options
                .and_then(|o| o.reasoning_summary.clone())
                .unwrap_or_else(|| "auto".to_string());
            entries.push((
                "reasoning".to_string(),
                Value::Map(vec![
                    ("effort".to_string(), Value::String(effort)),
                    ("summary".to_string(), Value::String(summary)),
                ]),
            ));
            entries.push((
                "include".to_string(),
                Value::Array(vec![Value::String("reasoning.encrypted_content".to_string())]),
            ));
        } else {
            // JS: model.thinkingLevelMap?.off !== null
            let off_is_null = model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.iter().find(|(key, _)| key == "off"))
                .map(|(_, value)| value.is_none())
                .unwrap_or(false);
            if !off_is_null {
                let off_mapping = model
                    .thinking_level_map
                    .as_ref()
                    .and_then(|map| map.iter().find(|(key, _)| key == "off"))
                    .and_then(|(_, value)| value.clone())
                    .unwrap_or_else(|| "none".to_string());
                entries.push((
                    "reasoning".to_string(),
                    Value::Map(vec![("effort".to_string(), Value::String(off_mapping))]),
                ));
            }
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

/// Stream function for the Azure OpenAI Responses API. Spawns a worker thread
/// that performs the request and feeds the returned stream (mirroring the JS
/// async IIFE).
pub fn stream(
    model: &Model,
    context: &Context,
    options: Option<&AzureOpenAIResponsesOptions>,
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
            let api_key = api_key.ok_or_else(|| format!("No API key for provider: {}", model.provider))?;
            let deployment_name = resolve_deployment_name(&model, options.as_ref());
            let grammar_tool_input_properties = create_grammar_tool_input_properties(
                context.tools.as_deref(),
                compat_supports_openai_grammar_tools(&model),
            );
            let headers = create_client_headers(
                &model,
                options.as_ref().and_then(|o| o.stream.request.headers.as_ref()),
            );
            let params = build_params(
                &model,
                &context,
                options.as_ref(),
                &deployment_name,
                &grammar_tool_input_properties,
            );
            let (base_url, api_version) = resolve_azure_config(&model, options.as_ref())?;

            // AzureOpenAI SDK: api-key header, URL {baseUrl}/openai/responses?api-version=...
            let mut request_headers = vec![("api-key".to_string(), api_key)];
            for (key, value) in headers {
                request_headers.push((key, value));
            }
            let separator = if base_url.contains('?') { "&" } else { "?" };
            let url = format!("{base_url}/openai/responses{separator}api-version={api_version}");

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
                    max_retry_delay_ms: options
                        .as_ref()
                        .and_then(|o| o.stream.request.max_retry_delay_ms)
                        .map(|value| value as f64),
                    token: None,
                },
            )
            .map_err(|failure| match failure {
                crate::utils::provider_retry::ProviderRetryFailure::Error(error) => format_azure_openai_error(&error),
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
                    service_tier: None,
                    grammar_tool_input_properties: Some(grammar_tool_input_properties),
                    apply_service_tier_pricing: None,
                }),
            )?;

            // processResponsesStream computes cost in JS; the Rust port keeps
            // costs zero there, so apply them here (mirrors the reference
            // provider adapter).
            calculate_cost(&model, &mut output.usage);

            if output.stop_reason == StopReason::Pending {
                return Err("Azure OpenAI Responses stream ended without a stop reason".to_string());
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

/// Simple-stream variant: builds base options and delegates to `stream`.
/// Mirrors `streamSimple`; the API key requirement surfaces as an error event
/// from `stream` when the key is missing.
pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<&SimpleStreamOptions>,
    api_key: Option<&str>,
    client: &HttpClient,
) -> AssistantMessageEventStream {
    let base = build_base_options(model, context, options, api_key);
    let clamped_reasoning = options
        .and_then(|o| o.reasoning.as_deref())
        .map(|level| clamp_thinking_level(model, level));
    let reasoning_effort = match clamped_reasoning.as_deref() {
        Some("off") => None,
        Some(level) => Some(level.to_string()),
        None => None,
    };

    let stream_options = AzureOpenAIResponsesOptions {
        stream: base,
        reasoning_effort,
        ..AzureOpenAIResponsesOptions::default()
    };
    stream(model, context, Some(&stream_options), api_key, client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModelCompat, ModelCost, ModelCostRates, OpenAIResponsesCompat};

    fn model(id: &str) -> Model {
        Model {
            id: id.to_string(),
            name: id.to_string(),
            api: "azure-openai-responses".to_string(),
            provider: "azure-openai-responses".to_string(),
            base_url: String::new(),
            reasoning: false,
            thinking_level_map: None,
            input: vec!["text".to_string()],
            cost: ModelCost {
                rates: ModelCostRates {
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
        }
    }

    fn context() -> Context {
        Context::default()
    }

    #[test]
    fn parses_deployment_name_maps() {
        let map = parse_deployment_name_map(Some("gpt-4o=my-deploy, gpt-4.1=deploy2=extra ,badentry"));
        assert_eq!(map.get("gpt-4o"), Some(&"my-deploy".to_string()));
        assert_eq!(map.get("gpt-4.1"), Some(&"deploy2=extra".to_string()));
        assert_eq!(map.get("badentry"), None);
        assert!(parse_deployment_name_map(None).is_empty());
        assert!(parse_deployment_name_map(Some("  ")).is_empty());
    }

    #[test]
    fn resolves_deployment_names() {
        // No option: falls back to the model id.
        let m = model("gpt-4o");
        assert_eq!(resolve_deployment_name(&m, None), "gpt-4o");
        // Explicit option wins.
        let options = AzureOpenAIResponsesOptions {
            azure_deployment_name: Some("explicit".to_string()),
            ..AzureOpenAIResponsesOptions::default()
        };
        assert_eq!(resolve_deployment_name(&m, Some(&options)), "explicit");
    }

    #[test]
    fn normalizes_azure_base_urls() {
        // Azure host with empty path -> /openai/v1.
        assert_eq!(
            normalize_azure_base_url("https://res.openai.azure.com/").unwrap(),
            "https://res.openai.azure.com/openai/v1"
        );
        // Already correct path stays.
        assert_eq!(
            normalize_azure_base_url("https://res.openai.azure.com/openai/v1").unwrap(),
            "https://res.openai.azure.com/openai/v1"
        );
        // /openai and /openai/v1/responses normalize too.
        assert_eq!(
            normalize_azure_base_url("https://res.openai.azure.com/openai").unwrap(),
            "https://res.openai.azure.com/openai/v1"
        );
        assert_eq!(
            normalize_azure_base_url("https://res.openai.azure.com/openai/v1/responses").unwrap(),
            "https://res.openai.azure.com/openai/v1"
        );
        // Custom path passes through.
        assert_eq!(
            normalize_azure_base_url("https://res.openai.azure.com/custom/path/").unwrap(),
            "https://res.openai.azure.com/custom/path"
        );
        // Non-azure host passes through with query preserved.
        assert_eq!(
            normalize_azure_base_url("https://gateway.example.com/v1/").unwrap(),
            "https://gateway.example.com/v1"
        );
        // Invalid URL errors.
        assert!(normalize_azure_base_url("not a url").is_err());
    }

    #[test]
    fn resolves_azure_config_with_model_base_url() {
        let mut m = model("gpt-4o");
        m.base_url = "https://res.openai.azure.com/".to_string();
        let (base_url, api_version) = resolve_azure_config(&m, None).unwrap();
        assert_eq!(base_url, "https://res.openai.azure.com/openai/v1");
        assert_eq!(api_version, "v1");
    }

    #[test]
    fn resolves_azure_config_with_options() {
        let m = model("gpt-4o");
        let options = AzureOpenAIResponsesOptions {
            azure_base_url: Some("https://res.cognitiveservices.azure.com/".to_string()),
            azure_api_version: Some("2024-10-21".to_string()),
            ..AzureOpenAIResponsesOptions::default()
        };
        let (base_url, api_version) = resolve_azure_config(&m, Some(&options)).unwrap();
        assert_eq!(base_url, "https://res.cognitiveservices.azure.com/openai/v1");
        assert_eq!(api_version, "2024-10-21");
    }

    #[test]
    fn azure_config_requires_a_base_url() {
        let m = model("gpt-4o");
        let error = resolve_azure_config(&m, None).unwrap_err();
        assert!(error.contains("base URL is required"), "{error}");
    }

    #[test]
    fn build_params_sets_deployment_and_clamps_output_tokens() {
        let m = model("gpt-4o");
        let options = AzureOpenAIResponsesOptions {
            stream: StreamOptions {
                max_tokens: Some(5.0),
                ..StreamOptions::default()
            },
            ..AzureOpenAIResponsesOptions::default()
        };
        let params = build_params(&m, &context(), Some(&options), "deployment-1", &[]);
        let entries = params.as_map().expect("object");
        assert_eq!(
            entries.iter().find(|(k, _)| k == "model").map(|(_, v)| v),
            Some(&Value::String("deployment-1".to_string()))
        );
        // max_output_tokens clamps to the 16-token floor.
        assert_eq!(
            entries.iter().find(|(k, _)| k == "max_output_tokens").map(|(_, v)| v),
            Some(&Value::Number(16.0))
        );
        // Non-reasoning model: no reasoning param.
        assert!(entries.iter().all(|(k, _)| k != "reasoning"));
    }

    #[test]
    fn build_params_reasoning_uses_thinking_level_map() {
        let mut m = model("gpt-4o");
        m.reasoning = true;
        m.thinking_level_map = Some(vec![("high".to_string(), Some("high".to_string()))]);
        let options = AzureOpenAIResponsesOptions {
            reasoning_effort: Some("high".to_string()),
            ..AzureOpenAIResponsesOptions::default()
        };
        let params = build_params(&m, &context(), Some(&options), "deployment-1", &[]);
        let entries = params.as_map().expect("object");
        let reasoning = entries.iter().find(|(k, _)| k == "reasoning").expect("reasoning");
        assert_eq!(
            reasoning.1.clone(),
            Value::Map(vec![
                ("effort".to_string(), Value::String("high".to_string())),
                ("summary".to_string(), Value::String("auto".to_string())),
            ])
        );
        assert_eq!(
            entries.iter().find(|(k, _)| k == "include").map(|(_, v)| v),
            Some(&Value::Array(vec![Value::String("reasoning.encrypted_content".to_string())]))
        );
    }

    #[test]
    fn build_params_skips_reasoning_when_off_is_null() {
        let mut m = model("gpt-4o");
        m.reasoning = true;
        m.thinking_level_map = Some(vec![("off".to_string(), None)]);
        let params = build_params(&m, &context(), None, "deployment-1", &[]);
        let entries = params.as_map().expect("object");
        assert!(entries.iter().all(|(k, _)| k != "reasoning"));
    }

    #[test]
    fn build_params_defaults_reasoning_effort_to_none_when_off_missing() {
        let m = model("gpt-4o");
        let mut reasoning_model = m;
        reasoning_model.reasoning = true;
        // thinkingLevelMap missing entirely -> JS `?.off` is undefined, which
        // is !== null, so reasoning { effort: "none" } is sent.
        let params = build_params(&reasoning_model, &context(), None, "deployment-1", &[]);
        let entries = params.as_map().expect("object");
        assert_eq!(
            entries.iter().find(|(k, _)| k == "reasoning").map(|(_, v)| v),
            Some(&Value::Map(vec![("effort".to_string(), Value::String("none".to_string()))]))
        );
    }

    #[test]
    fn build_params_uses_strict_mode_compat() {
        let mut m = model("gpt-4o");
        m.compat = Some(ModelCompat::OpenAiResponses(OpenAIResponsesCompat {
            supports_strict_mode: Some(true),
            ..OpenAIResponsesCompat::default()
        }));
        let tools = vec![crate::types::Tool {
            name: "add".to_string(),
            description: "Add".to_string(),
            parameters: crate::types::JsonSchemaObject {
                type_: Some(vec!["object".to_string()]),
                ..crate::types::JsonSchemaObject::default()
            },
            constrained_sampling: None,
        }];
        let mut ctx = context();
        ctx.tools = Some(tools);
        let params = build_params(&m, &ctx, None, "deployment-1", &[]);
        let entries = params.as_map().expect("object");
        let tools_value = entries.iter().find(|(k, _)| k == "tools").expect("tools");
        assert!(matches!(tools_value.1, Value::Array(_)));
    }
}
