//! OpenAI-compatible Chat Completions provider, port of
//! `packages/ai/src/api/openai-completions.ts`.
//!
//! SSE stream: `data:` lines without event names, terminated by
//! `data: [DONE]`; `choices[].delta` carries content/reasoning/tool-call
//! increments. Mirrors the JS implementation: message conversion
//! (developer/system roles, per-provider thinking formats, tool-call ID
//! normalization, synthetic assistant bridging, consecutive tool-result
//! merging), compat auto-detection from provider/baseUrl, chunk usage
//! parsing with cache semantics, stop-reason mapping, and the streaming
//! state machine.

use pi_protocol::Value;

use crate::api::constrained_sampling::{
    append_grammar_tool_input_json_delta, create_grammar_tool_input_properties, get_grammar_tool_input,
    resolve_grammar_constrained_sampling, resolve_json_schema_strict_sampling, GrammarToolInputJsonBuffer,
};
use crate::api::github_copilot_headers::{
    build_copilot_dynamic_headers, has_copilot_vision_input, CopilotDynamicHeadersParams,
};
use crate::api::prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::simple_options::{
    build_base_options, clamp_max_tokens_to_context, clamp_reasoning, MIN_ANSWER_TOKENS,
};
use crate::api::transform_messages::transform_messages;
use crate::event_stream::AssistantMessageEventStream;
use crate::http::client::HttpClient;
use crate::http::sse::SseEvent;
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::types::{
    AssistantMessage, CacheRetention, ChatTemplateKwargValue, Content, Context, Message, Model,
    OpenRouterRouting, ProviderHeaders, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
    ThinkingBudgets, ThinkingContent, Tool, ToolCall, Usage, UsageCost, UserMessageContent,
    VercelGatewayRouting,
};
use crate::utils::error_body::{format_provider_error, NormalizedProviderError};
use crate::utils::json::{json_stringify, parse_json_with_repair, parse_streaming_json};
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{retry_provider_request, ProviderError, ProviderRetryFailure, ProviderRetryOptions};

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// OpenAI-completions-specific stream options.
#[derive(Clone, Debug, Default)]
pub struct OpenAICompletionsOptions {
    pub stream: StreamOptions,
    pub tool_choice: Option<Value>,
    pub reasoning_effort: Option<String>,
    /// Token budgets per thinking level. Only used when
    /// `compat.supports_thinking_token_budget` is set.
    pub thinking_budgets: Option<ThinkingBudgets>,
}

impl From<OpenAICompletionsOptions> for StreamOptions {
    fn from(options: OpenAICompletionsOptions) -> Self {
        options.stream
    }
}

// ---------------------------------------------------------------------------
// Compat resolution
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ResolvedOpenAICompletionsCompat {
    pub supports_store: bool,
    pub supports_developer_role: bool,
    pub supports_reasoning_effort: bool,
    pub supports_usage_in_streaming: bool,
    pub supports_finish_reason: bool,
    pub max_tokens_field: String,
    pub requires_tool_result_name: bool,
    pub requires_assistant_after_tool_result: bool,
    pub requires_thinking_as_text: bool,
    pub requires_reasoning_content_on_assistant_messages: bool,
    pub thinking_format: String,
    pub open_router_routing: Option<OpenRouterRouting>,
    pub vercel_gateway_routing: Option<VercelGatewayRouting>,
    pub chat_template_kwargs: Option<Vec<(String, ChatTemplateKwargValue)>>,
    pub chat_template_args: Option<Vec<(String, ChatTemplateKwargValue)>>,
    pub zai_tool_stream: bool,
    pub supports_thinking_token_budget: bool,
    pub supports_strict_mode: bool,
    pub supports_openai_grammar_tools: bool,
    pub cache_control_format: Option<String>,
    pub send_session_affinity_headers: bool,
    pub deferred_tools_mode: Option<String>,
    pub session_affinity_format: String,
    pub supports_long_cache_retention: bool,
}

/// Auto-detect compatibility settings from provider name and baseUrl.
fn detect_compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    let provider = model.provider.as_str();
    let base_url = model.base_url.as_str();

    let is_zai = provider == "zai"
        || provider == "zai-coding-cn"
        || base_url.contains("api.z.ai")
        || base_url.contains("open.bigmodel.cn");
    let is_together =
        provider == "together" || base_url.contains("api.together.ai") || base_url.contains("api.together.xyz");
    let is_moonshot = provider == "moonshotai"
        || provider == "moonshotai-cn"
        || base_url.contains("api.moonshot.");
    let is_open_router = provider == "openrouter" || base_url.contains("openrouter.ai");
    let is_cloudflare_workers_ai = provider == "cloudflare-workers-ai" || base_url.contains("api.cloudflare.com");
    let is_cloudflare_ai_gateway =
        provider == "cloudflare-ai-gateway" || base_url.contains("gateway.ai.cloudflare.com");
    let is_nvidia = provider == "nvidia" || base_url.contains("integrate.api.nvidia.com");
    let is_ant_ling = provider == "ant-ling" || base_url.contains("api.ant-ling.com");

    let is_non_standard = is_nvidia
        || provider == "cerebras"
        || base_url.contains("cerebras.ai")
        || provider == "xai"
        || base_url.contains("api.x.ai")
        || is_together
        || base_url.contains("chutes.ai")
        || base_url.contains("deepseek.com")
        || is_zai
        || is_moonshot
        || provider == "opencode"
        || base_url.contains("opencode.ai")
        || is_cloudflare_workers_ai
        || is_cloudflare_ai_gateway
        || is_ant_ling;

    let is_deep_seek = provider == "deepseek" || base_url.contains("deepseek.com");
    let use_max_tokens = base_url.contains("chutes.ai")
        || is_deep_seek
        || is_moonshot
        || is_cloudflare_ai_gateway
        || is_together
        || is_nvidia
        || is_ant_ling
        || is_zai;

    let is_grok = provider == "xai" || base_url.contains("api.x.ai");
    let is_open_router_developer_role_model =
        is_open_router && (model.id.starts_with("anthropic/") || model.id.starts_with("openai/"));
    let cache_control_format =
        if provider == "openrouter" && model.id.starts_with("anthropic/") {
            Some("anthropic".to_string())
        } else {
            None
        };

    ResolvedOpenAICompletionsCompat {
        supports_store: !is_non_standard,
        supports_developer_role: is_open_router_developer_role_model || (!is_non_standard && !is_open_router),
        supports_reasoning_effort: !is_grok
            && !is_zai
            && !is_moonshot
            && !is_together
            && !is_cloudflare_ai_gateway
            && !is_nvidia
            && !is_ant_ling,
        supports_usage_in_streaming: true,
        supports_finish_reason: true,
        max_tokens_field: if use_max_tokens {
            "max_tokens".to_string()
        } else {
            "max_completion_tokens".to_string()
        },
        requires_tool_result_name: false,
        requires_assistant_after_tool_result: false,
        requires_thinking_as_text: false,
        requires_reasoning_content_on_assistant_messages: is_deep_seek,
        thinking_format: if is_deep_seek {
            "deepseek"
        } else if is_zai {
            "zai"
        } else if is_together {
            "together"
        } else if is_ant_ling {
            "ant-ling"
        } else if is_open_router {
            "openrouter"
        } else {
            "openai"
        }
        .to_string(),
        open_router_routing: None,
        vercel_gateway_routing: None,
        chat_template_kwargs: None,
        chat_template_args: None,
        zai_tool_stream: false,
        supports_thinking_token_budget: false,
        supports_strict_mode: !is_moonshot && !is_together && !is_cloudflare_ai_gateway && !is_nvidia,
        supports_openai_grammar_tools: false,
        cache_control_format,
        send_session_affinity_headers: false,
        deferred_tools_mode: None,
        session_affinity_format: if is_open_router {
            "openrouter".to_string()
        } else {
            "openai".to_string()
        },
        supports_long_cache_retention: !(is_together
            || is_cloudflare_workers_ai
            || is_cloudflare_ai_gateway
            || is_nvidia
            || is_ant_ling),
    }
}

/// Get resolved compatibility settings for a model: auto-detect from
/// provider/URL, then override with explicit model.compat entries.
pub fn get_compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    let detected = detect_compat(model);
    let Some(compat) = (match &model.compat {
        Some(crate::types::ModelCompat::OpenAiCompletions(compat)) => Some(compat.clone()),
        _ => None,
    }) else {
        return detected;
    };

    ResolvedOpenAICompletionsCompat {
        supports_store: compat.supports_store.unwrap_or(detected.supports_store),
        supports_developer_role: compat
            .supports_developer_role
            .unwrap_or(detected.supports_developer_role),
        supports_reasoning_effort: compat
            .supports_reasoning_effort
            .unwrap_or(detected.supports_reasoning_effort),
        supports_usage_in_streaming: compat
            .supports_usage_in_streaming
            .unwrap_or(detected.supports_usage_in_streaming),
        supports_finish_reason: compat
            .supports_finish_reason
            .unwrap_or(detected.supports_finish_reason),
        max_tokens_field: compat.max_tokens_field.unwrap_or(detected.max_tokens_field),
        requires_tool_result_name: compat
            .requires_tool_result_name
            .unwrap_or(detected.requires_tool_result_name),
        requires_assistant_after_tool_result: compat
            .requires_assistant_after_tool_result
            .unwrap_or(detected.requires_assistant_after_tool_result),
        requires_thinking_as_text: compat
            .requires_thinking_as_text
            .unwrap_or(detected.requires_thinking_as_text),
        requires_reasoning_content_on_assistant_messages: compat
            .requires_reasoning_content_on_assistant_messages
            .unwrap_or(detected.requires_reasoning_content_on_assistant_messages),
        thinking_format: compat.thinking_format.unwrap_or(detected.thinking_format),
        open_router_routing: compat.open_router_routing.or(detected.open_router_routing),
        vercel_gateway_routing: compat.vercel_gateway_routing.or(detected.vercel_gateway_routing),
        chat_template_kwargs: compat.chat_template_kwargs.or(detected.chat_template_kwargs),
        chat_template_args: compat.chat_template_args.or(detected.chat_template_args),
        zai_tool_stream: compat.zai_tool_stream.unwrap_or(detected.zai_tool_stream),
        supports_thinking_token_budget: compat
            .supports_thinking_token_budget
            .unwrap_or(detected.supports_thinking_token_budget),
        supports_strict_mode: compat
            .supports_strict_mode
            .unwrap_or(detected.supports_strict_mode),
        supports_openai_grammar_tools: compat
            .supports_openai_grammar_tools
            .unwrap_or(detected.supports_openai_grammar_tools),
        cache_control_format: compat.cache_control_format.or(detected.cache_control_format),
        send_session_affinity_headers: compat
            .send_session_affinity_headers
            .unwrap_or(detected.send_session_affinity_headers),
        deferred_tools_mode: compat.deferred_tools_mode.or(detected.deferred_tools_mode),
        session_affinity_format: compat
            .session_affinity_format
            .unwrap_or(detected.session_affinity_format),
        supports_long_cache_retention: compat
            .supports_long_cache_retention
            .unwrap_or(detected.supports_long_cache_retention),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

fn has_tool_history(messages: &[Message]) -> bool {
    for message in messages {
        match message {
            Message::ToolResult(_) => return true,
            Message::Assistant(assistant) => {
                if assistant
                    .content
                    .iter()
                    .any(|block| matches!(block, Content::ToolCall(_)))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn get_deferred_tool_names(messages: &[Message]) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    for message in messages {
        if let Message::ToolResult(tool) = message {
            for name in tool.added_tool_names.iter().flatten() {
                names.insert(name.clone());
            }
        }
    }
    names
}

fn get_tools_by_name(tools: Option<&[Tool]>, names: &std::collections::HashSet<String>) -> Vec<Tool> {
    let Some(tools) = tools else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for name in names {
        if let Some(tool) = tools.iter().find(|tool| &tool.name == name) {
            result.push(tool.clone());
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Cache control (Anthropic-style markers on OpenAI-compatible requests)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct OpenAICompatCacheControl {
    pub type_: String,
    pub ttl: Option<String>,
}

fn get_compat_cache_control(
    compat: &ResolvedOpenAICompletionsCompat,
    cache_retention: &CacheRetention,
) -> Option<OpenAICompatCacheControl> {
    if compat.cache_control_format.as_deref() != Some("anthropic") || cache_retention == "none" {
        return None;
    }
    let ttl = if cache_retention == "long" && compat.supports_long_cache_retention {
        Some("1h".to_string())
    } else {
        None
    };
    Some(OpenAICompatCacheControl {
        type_: "ephemeral".to_string(),
        ttl,
    })
}

fn cache_control_to_value(cache_control: &OpenAICompatCacheControl) -> Value {
    match &cache_control.ttl {
        Some(ttl) => Value::Map(vec![
            ("type".to_string(), Value::String(cache_control.type_.clone())),
            ("ttl".to_string(), Value::String(ttl.clone())),
        ]),
        None => Value::Map(vec![("type".to_string(), Value::String(cache_control.type_.clone()))]),
    }
}

/// Adds `cache_control` to the last text part of a message content value.
/// Returns true when applied.
fn add_cache_control_to_text_content(content: &mut Value, cache_control: &OpenAICompatCacheControl) -> bool {
    match content {
        Value::String(text) => {
            if text.is_empty() {
                return false;
            }
            *content = Value::Array(vec![Value::Map(vec![
                ("type".to_string(), Value::String("text".to_string())),
                ("text".to_string(), Value::String(text.clone())),
                ("cache_control".to_string(), cache_control_to_value(cache_control)),
            ])]);
            true
        }
        Value::Array(parts) => {
            for part in parts.iter_mut().rev() {
                if let Value::Map(entries) = part {
                    let is_text = entries
                        .iter()
                        .find(|(key, _)| key == "type")
                        .and_then(|(_, value)| value.as_str())
                        == Some("text");
                    if is_text {
                        if let Some(existing) = entries.iter_mut().find(|(key, _)| key == "cache_control") {
                            existing.1 = cache_control_to_value(cache_control);
                        } else {
                            entries.push(("cache_control".to_string(), cache_control_to_value(cache_control)));
                        }
                        return true;
                    }
                }
            }
            false
        }
        _ => false,
    }
}

fn add_cache_control_to_message(message: &mut Value, cache_control: &OpenAICompatCacheControl) -> bool {
    let Value::Map(entries) = message else {
        return false;
    };
    let role = entries
        .iter()
        .find(|(key, _)| key == "role")
        .and_then(|(_, value)| value.as_str());
    match role {
        Some("user") | Some("assistant") | Some("tool") => {
            if let Some((_, content)) = entries.iter_mut().find(|(key, _)| key == "content") {
                return add_cache_control_to_text_content(content, cache_control);
            }
            false
        }
        _ => false,
    }
}

fn add_cache_control_to_system_prompt(messages: &mut [Value], cache_control: &OpenAICompatCacheControl) {
    for message in messages.iter_mut() {
        let is_instruction = message
            .as_map()
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|(key, _)| key == "role")
                    .and_then(|(_, value)| value.as_str())
            })
            .is_some_and(|role| role == "system" || role == "developer");
        if is_instruction {
            let _ = add_cache_control_to_message(message, cache_control);
            return;
        }
    }
}

fn add_cache_control_to_last_conversation_message(messages: &mut [Value], cache_control: &OpenAICompatCacheControl) {
    for message in messages.iter_mut().rev() {
        let role = message
            .as_map()
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|(key, _)| key == "role")
                    .and_then(|(_, value)| value.as_str())
            });
        if matches!(role, Some("user") | Some("assistant") | Some("tool")) {
            if add_cache_control_to_message(message, cache_control) {
                return;
            }
        }
    }
}

fn add_cache_control_to_last_tool(tools: &mut [Value], cache_control: &OpenAICompatCacheControl) {
    if let Some(last_tool) = tools.last_mut() {
        if let Value::Map(entries) = last_tool {
            entries.push(("cache_control".to_string(), cache_control_to_value(cache_control)));
        }
    }
}

// ---------------------------------------------------------------------------
// Message conversion
// ---------------------------------------------------------------------------

pub fn normalize_tool_call_id_for_completions(id: &str, model: &Model) -> String {
    if id.contains('|') {
        let separator_index = id.find('|').expect("contains |");
        let call_id = &id[..separator_index];
        let item_id = &id[separator_index + 1..];
        let sanitized_call: String = sanitize_tool_id(call_id);
        let sanitized_item: String = sanitize_tool_id(item_id);
        let combined = if !sanitized_item.is_empty() {
            format!("{sanitized_call}_{sanitized_item}")
        } else {
            sanitized_call.clone()
        };
        if combined.chars().count() <= 40 {
            return combined;
        }
        let hash = crate::utils::hash::short_hash(id);
        let hash = hash.chars().take(8).collect::<String>();
        let prefix_len = 40usize.saturating_sub(hash.chars().count() + 1).max(1);
        let prefix: String = sanitized_call.chars().take(prefix_len).collect();
        return format!("{prefix}_{hash}");
    }

    if model.provider == "openai" {
        if id.chars().count() > 40 {
            return id.chars().take(40).collect();
        }
        return id.to_string();
    }
    id.to_string()
}

fn sanitize_tool_id(part: &str) -> String {
    part.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub struct ConvertCompletionsMessagesOptions {
    pub grammar_tool_input_properties: Option<Vec<(String, String)>>,
}

fn is_encrypted_reasoning_detail(value: &Value) -> bool {
    let Some(entries) = value.as_map() else {
        return false;
    };
    let type_ = entries
        .iter()
        .find(|(key, _)| key == "type")
        .and_then(|(_, value)| value.as_str());
    let id = entries
        .iter()
        .find(|(key, _)| key == "id")
        .and_then(|(_, value)| value.as_str());
    let data = entries
        .iter()
        .find(|(key, _)| key == "data")
        .and_then(|(_, value)| value.as_str());
    type_ == Some("reasoning.encrypted") && id.is_some_and(|id| !id.is_empty()) && data.is_some_and(|data| !data.is_empty())
}

fn sanitize_surrogates(text: &str) -> String {
    crate::utils::sanitize::sanitize_surrogates(text)
}

/// Converts pi messages into Chat Completions wire messages (Value maps).
pub fn convert_messages(
    model: &Model,
    context: &Context,
    compat: &ResolvedOpenAICompletionsCompat,
    options: Option<&ConvertCompletionsMessagesOptions>,
) -> Vec<Value> {
    let mut params: Vec<Value> = Vec::new();

    let normalized = transform_messages(
        context.messages.clone(),
        model,
        Some(&|id, model, _source| normalize_tool_call_id_for_completions(id, model)),
    );

    if let Some(system_prompt) = &context.system_prompt {
        let use_developer_role = model.reasoning && compat.supports_developer_role;
        let role = if use_developer_role { "developer" } else { "system" };
        params.push(message_value(role, Value::String(sanitize_surrogates(system_prompt)), None));
    }

    let mut last_role: Option<String> = None;
    let mut i = 0usize;
    while i < normalized.len() {
        let msg = &normalized[i];
        // Some providers don't allow user messages directly after tool results;
        // insert a synthetic assistant message to bridge the gap.
        if compat.requires_assistant_after_tool_result
            && last_role.as_deref() == Some("toolResult")
            && matches!(msg, Message::User(_))
        {
            params.push(message_value(
                "assistant",
                Value::String("I have processed the tool results.".to_string()),
                None,
            ));
        }

        match msg {
            Message::User(user) => {
                match &user.content {
                    UserMessageContent::Text(text) => {
                        params.push(message_value("user", Value::String(sanitize_surrogates(text)), None));
                    }
                    UserMessageContent::Blocks(blocks) => {
                        let content: Vec<Value> = blocks
                            .iter()
                            .filter_map(|item| match item {
                                Content::Text(text) => Some(Value::Map(vec![
                                    ("type".to_string(), Value::String("text".to_string())),
                                    ("text".to_string(), Value::String(sanitize_surrogates(&text.text))),
                                ])),
                                Content::Image(image) => Some(Value::Map(vec![
                                    ("type".to_string(), Value::String("image_url".to_string())),
                                    (
                                        "image_url".to_string(),
                                        Value::Map(vec![(
                                            "url".to_string(),
                                            Value::String(format!("data:{};base64,{}", image.mime_type, image.data)),
                                        )]),
                                    ),
                                ])),
                                _ => None,
                            })
                            .collect();
                        if content.is_empty() {
                            i += 1;
                            continue;
                        }
                        params.push(message_value("user", Value::Array(content), None));
                    }
                }
            }
            Message::Assistant(assistant) => {
                let mut assistant_msg: Vec<(String, Value)> = vec![
                    ("role".to_string(), Value::String("assistant".to_string())),
                    (
                        "content".to_string(),
                        if compat.requires_assistant_after_tool_result {
                            Value::String(String::new())
                        } else {
                            Value::Null
                        },
                    ),
                ];

                let assistant_text_parts: Vec<Value> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        Content::Text(text) if !text.text.trim().is_empty() => Some(Value::Map(vec![
                            ("type".to_string(), Value::String("text".to_string())),
                            ("text".to_string(), Value::String(sanitize_surrogates(&text.text))),
                        ])),
                        _ => None,
                    })
                    .collect();
                let assistant_text: String = assistant_text_parts
                    .iter()
                    .filter_map(|part| part.as_map().and_then(|entries| {
                        entries
                            .iter()
                            .find(|(key, _)| key == "text")
                            .and_then(|(_, value)| value.as_str())
                    }))
                    .collect();

                let non_empty_thinking_blocks: Vec<&ThinkingContent> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        Content::Thinking(thinking) if !thinking.thinking.trim().is_empty() => Some(thinking),
                        _ => None,
                    })
                    .collect();

                if !non_empty_thinking_blocks.is_empty() {
                    if compat.requires_thinking_as_text {
                        let thinking_text = non_empty_thinking_blocks
                            .iter()
                            .map(|block| sanitize_surrogates(&block.thinking))
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        let mut content: Vec<Value> = vec![Value::Map(vec![
                            ("type".to_string(), Value::String("text".to_string())),
                            ("text".to_string(), Value::String(thinking_text)),
                        ])];
                        content.extend(assistant_text_parts.clone());
                        set_entry(&mut assistant_msg, "content", Value::Array(content));
                    } else {
                        // Always send assistant content as a plain string.
                        if !assistant_text.is_empty() {
                            set_entry(
                                &mut assistant_msg,
                                "content",
                                Value::String(assistant_text.clone()),
                            );
                        }

                        // Use the signature from the first thinking block if available.
                        let mut signature = non_empty_thinking_blocks[0].thinking_signature.clone();
                        if model.provider == "opencode-go" && signature.as_deref() == Some("reasoning") {
                            signature = Some("reasoning_content".to_string());
                        }
                        if let Some(signature) = signature {
                            if !signature.is_empty() {
                                let joined = non_empty_thinking_blocks
                                    .iter()
                                    .map(|block| block.thinking.clone())
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                set_entry(&mut assistant_msg, &signature, Value::String(joined));
                            }
                        }
                    }
                } else if !assistant_text.is_empty() {
                    set_entry(&mut assistant_msg, "content", Value::String(assistant_text));
                }

                let tool_calls: Vec<&ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        Content::ToolCall(tool_call) => Some(tool_call),
                        _ => None,
                    })
                    .collect();
                if !tool_calls.is_empty() {
                    let converted: Vec<Value> = tool_calls
                        .iter()
                        .map(|tool_call| {
                            let custom_input_property = options
                                .and_then(|o| o.grammar_tool_input_properties.as_ref())
                                .and_then(|properties| {
                                    properties
                                        .iter()
                                        .find(|(name, _)| name == &tool_call.name)
                                        .map(|(_, property)| property.clone())
                                });
                            match custom_input_property {
                                Some(property) => {
                                    let input = get_grammar_tool_input(&tool_call.name, &tool_call.arguments, &property)
                                        .unwrap_or_default();
                                    Value::Map(vec![
                                        ("id".to_string(), Value::String(tool_call.id.clone())),
                                        ("type".to_string(), Value::String("custom".to_string())),
                                        (
                                            "custom".to_string(),
                                            Value::Map(vec![
                                                ("name".to_string(), Value::String(tool_call.name.clone())),
                                                ("input".to_string(), Value::String(sanitize_surrogates(&input))),
                                            ]),
                                        ),
                                    ])
                                }
                                None => Value::Map(vec![
                                    ("id".to_string(), Value::String(tool_call.id.clone())),
                                    ("type".to_string(), Value::String("function".to_string())),
                                    (
                                        "function".to_string(),
                                        Value::Map(vec![
                                            ("name".to_string(), Value::String(tool_call.name.clone())),
                                            (
                                                "arguments".to_string(),
                                                Value::String(json_stringify(&tool_call.arguments)),
                                            ),
                                        ]),
                                    ),
                                ]),
                            }
                        })
                        .collect();
                    set_entry(&mut assistant_msg, "tool_calls", Value::Array(converted));

                    let reasoning_details: Vec<Value> = tool_calls
                        .iter()
                        .filter_map(|tool_call| tool_call.thought_signature.as_ref())
                        .filter_map(|signature| parse_json_with_repair::<Value>(signature).ok())
                        .collect();
                    if !reasoning_details.is_empty() {
                        set_entry(&mut assistant_msg, "reasoning_details", Value::Array(reasoning_details));
                    }
                }
                if compat.requires_reasoning_content_on_assistant_messages
                    && model.reasoning
                    && !assistant_msg.iter().any(|(key, _)| key == "reasoning_content")
                {
                    set_entry(&mut assistant_msg, "reasoning_content", Value::String(String::new()));
                }
                // Skip assistant messages that have no content and no tool calls.
                let has_content = match assistant_msg
                    .iter()
                    .find(|(key, _)| key == "content")
                    .map(|(_, value)| value)
                {
                    Some(Value::String(text)) => !text.is_empty(),
                    Some(Value::Array(items)) => !items.is_empty(),
                    _ => false,
                };
                let has_tool_calls = assistant_msg.iter().any(|(key, _)| key == "tool_calls");
                if !has_content && !has_tool_calls {
                    i += 1;
                    continue;
                }
                params.push(Value::Map(assistant_msg));
            }
            Message::ToolResult(_) => {
                let mut image_blocks: Vec<Value> = Vec::new();
                let mut deferred_tool_names = std::collections::HashSet::new();
                let mut j = i;

                while j < normalized.len() && matches!(normalized[j], Message::ToolResult(_)) {
                    let Message::ToolResult(current) = &normalized[j] else {
                        unreachable!()
                    };

                    let text_result: Vec<&str> = current
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            Content::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect();
                    let has_images = current
                        .content
                        .iter()
                        .any(|block| matches!(block, Content::Image(_)));

                    let has_text = text_result.iter().any(|text| !text.is_empty());
                    let tool_result_text = if has_text {
                        text_result.join("\n")
                    } else if has_images {
                        "(see attached image)".to_string()
                    } else {
                        "(no tool output)".to_string()
                    };
                    let mut tool_result_msg = vec![
                        ("role".to_string(), Value::String("tool".to_string())),
                        ("content".to_string(), Value::String(sanitize_surrogates(&tool_result_text))),
                        ("tool_call_id".to_string(), Value::String(current.tool_call_id.clone())),
                    ];
                    if compat.requires_tool_result_name && !current.tool_name.is_empty() {
                        tool_result_msg.push(("name".to_string(), Value::String(current.tool_name.clone())));
                    }
                    params.push(Value::Map(tool_result_msg));

                    if compat.deferred_tools_mode.as_deref() == Some("kimi") {
                        for name in current.added_tool_names.iter().flatten() {
                            deferred_tool_names.insert(name.clone());
                        }
                    }

                    if has_images && model.input.iter().any(|kind| kind == "image") {
                        for block in &current.content {
                            if let Content::Image(image) = block {
                                image_blocks.push(Value::Map(vec![
                                    ("type".to_string(), Value::String("image_url".to_string())),
                                    (
                                        "image_url".to_string(),
                                        Value::Map(vec![(
                                            "url".to_string(),
                                            Value::String(format!("data:{};base64,{}", image.mime_type, image.data)),
                                        )]),
                                    ),
                                ]));
                            }
                        }
                    }
                    j += 1;
                }

                i = j - 1;

                if !image_blocks.is_empty() {
                    if compat.requires_assistant_after_tool_result {
                        params.push(message_value(
                            "assistant",
                            Value::String("I have processed the tool results.".to_string()),
                            None,
                        ));
                    }

                    let mut content: Vec<Value> = vec![Value::Map(vec![
                        ("type".to_string(), Value::String("text".to_string())),
                        (
                            "text".to_string(),
                            Value::String("Attached image(s) from tool result:".to_string()),
                        ),
                    ])];
                    content.extend(image_blocks);
                    params.push(message_value("user", Value::Array(content), None));
                    last_role = Some("user".to_string());
                } else {
                    last_role = Some("toolResult".to_string());
                }

                if !deferred_tool_names.is_empty() {
                    let deferred_tools = get_tools_by_name(context.tools.as_deref(), &deferred_tool_names);
                    if !deferred_tools.is_empty() {
                        // Kimi accepts a system message with tools but omits the
                        // standard content field.
                        let kimi_message = Value::Map(vec![
                            ("role".to_string(), Value::String("system".to_string())),
                            (
                                "tools".to_string(),
                                Value::Array(
                                    convert_tools(&deferred_tools, compat)
                                        .into_iter()
                                        .map(|tool| tool)
                                        .collect(),
                                ),
                            ),
                        ]);
                        params.push(kimi_message);
                    }
                }
                i += 1;
                continue;
            }
        }

        last_role = match msg {
            Message::User(_) => Some("user".to_string()),
            Message::Assistant(_) => Some("assistant".to_string()),
            Message::ToolResult(_) => Some("toolResult".to_string()),
        };
        i += 1;
    }

    params
}

fn message_value(role: &str, content: Value, tool_call_id: Option<&str>) -> Value {
    let mut entries = vec![
        ("role".to_string(), Value::String(role.to_string())),
        ("content".to_string(), content),
    ];
    if let Some(tool_call_id) = tool_call_id {
        entries.push(("tool_call_id".to_string(), Value::String(tool_call_id.to_string())));
    }
    Value::Map(entries)
}

fn set_entry(entries: &mut Vec<(String, Value)>, key: &str, value: Value) {
    if let Some(existing) = entries.iter_mut().find(|(k, _)| k == key) {
        existing.1 = value;
    } else {
        entries.push((key.to_string(), value));
    }
}

// ---------------------------------------------------------------------------
// Tool conversion
// ---------------------------------------------------------------------------

pub fn convert_tools(tools: &[Tool], compat: &ResolvedOpenAICompletionsCompat) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            if let Ok(Some(grammar)) = resolve_grammar_constrained_sampling(tool, compat.supports_openai_grammar_tools)
            {
                return Value::Map(vec![
                    ("type".to_string(), Value::String("custom".to_string())),
                    (
                        "custom".to_string(),
                        Value::Map(vec![
                            ("name".to_string(), Value::String(tool.name.clone())),
                            ("description".to_string(), Value::String(tool.description.clone())),
                            (
                                "format".to_string(),
                                Value::Map(vec![
                                    ("type".to_string(), Value::String("grammar".to_string())),
                                    (
                                        "grammar".to_string(),
                                        Value::Map(vec![
                                            ("syntax".to_string(), Value::String(grammar.format)),
                                            ("definition".to_string(), Value::String(grammar.definition)),
                                        ]),
                                    ),
                                ]),
                            ),
                        ]),
                    ),
                ]);
            }

            let strict = resolve_json_schema_strict_sampling(tool, compat.supports_strict_mode)
                .unwrap_or(None);
            let mut function_entries = vec![
                ("name".to_string(), Value::String(tool.name.clone())),
                ("description".to_string(), Value::String(tool.description.clone())),
                ("parameters".to_string(), tool.parameters.to_value()),
            ];
            if compat.supports_strict_mode {
                function_entries.push(("strict".to_string(), Value::Bool(strict.unwrap_or(false))));
            }
            Value::Map(vec![
                ("type".to_string(), Value::String("function".to_string())),
                ("function".to_string(), Value::Map(function_entries)),
            ])
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Thinking template values
// ---------------------------------------------------------------------------

fn resolve_chat_template_kwarg_value(
    model: &Model,
    options: Option<&OpenAICompletionsOptions>,
    value: &ChatTemplateKwargValue,
) -> Option<Value> {
    match value {
        ChatTemplateKwargValue::Str(s) => Some(Value::String(s.clone())),
        ChatTemplateKwargValue::Number(n) => Some(Value::Number(*n)),
        ChatTemplateKwargValue::Bool(b) => Some(Value::Bool(*b)),
        ChatTemplateKwargValue::Null => Some(Value::Null),
        ChatTemplateKwargValue::Var {
            var,
            omit_when_off,
        } => {
            let reasoning_effort = options.and_then(|o| o.reasoning_effort.clone());
            if reasoning_effort.is_none() && *omit_when_off == Some(true) {
                return None;
            }
            if var == "thinking.enabled" {
                return Some(Value::Bool(reasoning_effort.is_some()));
            }
            let mapped_value = match &reasoning_effort {
                Some(effort) => model
                    .thinking_level_map
                    .as_ref()
                    .and_then(|map| {
                        map.iter()
                            .find(|(key, _)| key == effort)
                            .and_then(|(_, value)| value.clone())
                    }),
                None => model
                    .thinking_level_map
                    .as_ref()
                    .and_then(|map| {
                        map.iter()
                            .find(|(key, _)| key == "off")
                            .and_then(|(_, value)| value.clone())
                    }),
            };
            match mapped_value {
                None => reasoning_effort.map(Value::String),
                Some(value) => Some(Value::String(value)),
            }
        }
    }
}

fn build_chat_template_values(
    model: &Model,
    options: Option<&OpenAICompletionsOptions>,
    values: Option<&[(String, ChatTemplateKwargValue)]>,
) -> Option<Vec<(String, Value)>> {
    let values = values?;
    let mut resolved: Vec<(String, Value)> = Vec::new();
    for (key, value) in values {
        if let Some(resolved_value) = resolve_chat_template_kwarg_value(model, options, value) {
            resolved.push((key.clone(), resolved_value));
        }
    }
    if resolved.is_empty() {
        None
    } else {
        Some(resolved)
    }
}

// ---------------------------------------------------------------------------
// Request params
// ---------------------------------------------------------------------------

fn thinking_level_map_value(model: &Model, level: &str) -> Option<String> {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| {
            map.iter()
                .find(|(key, _)| key == level)
                .and_then(|(_, value)| value.clone())
        })
}

fn routing_to_value(routing: &OpenRouterRouting) -> Value {
    let mut entries: Vec<(String, Value)> = Vec::new();
    if let Some(value) = &routing.allow_fallbacks {
        entries.push(("allow_fallbacks".to_string(), Value::Bool(*value)));
    }
    if let Some(value) = &routing.require_parameters {
        entries.push(("require_parameters".to_string(), Value::Bool(*value)));
    }
    if let Some(value) = &routing.data_collection {
        entries.push(("data_collection".to_string(), Value::String(value.clone())));
    }
    if let Some(value) = &routing.zdr {
        entries.push(("zdr".to_string(), Value::Bool(*value)));
    }
    if let Some(value) = &routing.enforce_distillable_text {
        entries.push(("enforce_distillable_text".to_string(), Value::Bool(*value)));
    }
    if let Some(value) = &routing.order {
        entries.push((
            "order".to_string(),
            Value::Array(value.iter().map(|s| Value::String(s.clone())).collect()),
        ));
    }
    if let Some(value) = &routing.only {
        entries.push((
            "only".to_string(),
            Value::Array(value.iter().map(|s| Value::String(s.clone())).collect()),
        ));
    }
    if let Some(value) = &routing.ignore {
        entries.push((
            "ignore".to_string(),
            Value::Array(value.iter().map(|s| Value::String(s.clone())).collect()),
        ));
    }
    if let Some(value) = &routing.quantizations {
        entries.push((
            "quantizations".to_string(),
            Value::Array(value.iter().map(|s| Value::String(s.clone())).collect()),
        ));
    }
    Value::Map(entries)
}

fn vercel_gateway_routing_to_value(routing: &VercelGatewayRouting) -> Value {
    let mut gateway: Vec<(String, Value)> = Vec::new();
    if let Some(only) = &routing.only {
        gateway.push((
            "only".to_string(),
            Value::Array(only.iter().map(|s| Value::String(s.clone())).collect()),
        ));
    }
    if let Some(order) = &routing.order {
        gateway.push((
            "order".to_string(),
            Value::Array(order.iter().map(|s| Value::String(s.clone())).collect()),
        ));
    }
    Value::Map(vec![("gateway".to_string(), Value::Map(gateway))])
}

fn build_params(
    model: &Model,
    context: &Context,
    options: Option<&OpenAICompletionsOptions>,
    compat: &ResolvedOpenAICompletionsCompat,
    cache_retention: &CacheRetention,
    grammar_tool_input_properties: &[(String, String)],
) -> Value {
    let messages = convert_messages(
        model,
        context,
        compat,
        Some(&ConvertCompletionsMessagesOptions {
            grammar_tool_input_properties: Some(grammar_tool_input_properties.to_vec()),
        }),
    );
    let cache_control = get_compat_cache_control(compat, cache_retention);

    let prompt_cache_key = if (model.base_url.contains("api.openai.com") && cache_retention != "none")
        || (cache_retention == "long" && compat.supports_long_cache_retention)
    {
        clamp_openai_prompt_cache_key(options.and_then(|o| o.stream.session_id.as_deref()))
    } else {
        None
    };
    let prompt_cache_retention = if cache_retention == "long" && compat.supports_long_cache_retention {
        Some("24h".to_string())
    } else {
        None
    };

    let mut params: Vec<(String, Value)> = vec![
        ("model".to_string(), Value::String(model.id.clone())),
        (
            "messages".to_string(),
            Value::Array(messages.iter().map(|message| message.clone()).collect()),
        ),
        ("stream".to_string(), Value::Bool(true)),
    ];
    if let Some(key) = prompt_cache_key {
        params.push(("prompt_cache_key".to_string(), Value::String(key)));
    }
    if let Some(retention) = prompt_cache_retention {
        params.push(("prompt_cache_retention".to_string(), Value::String(retention)));
    }

    if compat.supports_usage_in_streaming {
        params.push((
            "stream_options".to_string(),
            Value::Map(vec![("include_usage".to_string(), Value::Bool(true))]),
        ));
    }

    if compat.supports_store {
        params.push(("store".to_string(), Value::Bool(false)));
    }

    if let Some(max_tokens) = options.and_then(|o| o.stream.max_tokens) {
        if compat.max_tokens_field == "max_tokens" {
            params.push(("max_tokens".to_string(), Value::Number(max_tokens)));
        } else {
            params.push(("max_completion_tokens".to_string(), Value::Number(max_tokens)));
        }
    }

    if let Some(temperature) = options.and_then(|o| o.stream.temperature) {
        params.push(("temperature".to_string(), Value::Number(temperature)));
    }

    let deferred_tool_names = if compat.deferred_tools_mode.as_deref() == Some("kimi") {
        get_deferred_tool_names(&context.messages)
    } else {
        std::collections::HashSet::new()
    };
    let active_tools: Vec<Tool> = context
        .tools
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|tool| !deferred_tool_names.contains(&tool.name))
        .cloned()
        .collect();
    if !active_tools.is_empty() {
        let converted = convert_tools(&active_tools, compat);
        params.push((
            "tools".to_string(),
            Value::Array(converted),
        ));
        if compat.zai_tool_stream {
            params.push(("tool_stream".to_string(), Value::Bool(true)));
        }
    } else if has_tool_history(&context.messages) {
        // Anthropic (via LiteLLM/proxy) requires tools param when the
        // conversation has tool_calls/tool_results.
        params.push(("tools".to_string(), Value::Array(Vec::new())));
    }

    let mut messages_clone = messages.clone();
    let mut tools_clone: Vec<Value> = params
        .iter()
        .find(|(key, _)| key == "tools")
        .and_then(|(_, value)| value.as_array())
        .map(|tools| tools.to_vec())
        .unwrap_or_default();
    if let Some(cache_control) = &cache_control {
        add_cache_control_to_system_prompt(&mut messages_clone, cache_control);
        add_cache_control_to_last_tool(&mut tools_clone, cache_control);
        add_cache_control_to_last_conversation_message(&mut messages_clone, cache_control);
    }
    set_entry(&mut params, "messages", Value::Array(messages_clone));
    if params.iter().any(|(key, _)| key == "tools") {
        set_entry(&mut params, "tools", Value::Array(tools_clone));
    }

    if let Some(tool_choice) = options.and_then(|o| o.tool_choice.clone()) {
        params.push(("tool_choice".to_string(), tool_choice));
    }

    // Thinking formats.
    if compat.thinking_format == "zai" && model.reasoning {
        let reasoning_effort = options.and_then(|o| o.reasoning_effort.clone());
        params.push((
            "thinking".to_string(),
            Value::Map(vec![(
                "type".to_string(),
                Value::String(if reasoning_effort.is_some() {
                    "enabled".to_string()
                } else {
                    "disabled".to_string()
                }),
            )]),
        ));
        if reasoning_effort.is_some() && compat.supports_reasoning_effort {
            let mapped = thinking_level_map_value(model, &reasoning_effort.clone().unwrap_or_default())
                .unwrap_or_else(|| reasoning_effort.clone().unwrap_or_default());
            params.push(("reasoning_effort".to_string(), Value::String(mapped)));
        }
    } else if compat.thinking_format == "qwen" && model.reasoning {
        let reasoning_effort = options.and_then(|o| o.reasoning_effort.clone());
        params.push(("enable_thinking".to_string(), Value::Bool(reasoning_effort.is_some())));
        if reasoning_effort.is_some() && compat.supports_reasoning_effort {
            let effort = thinking_level_map_value(model, &reasoning_effort.clone().unwrap_or_default())
                .unwrap_or_else(|| reasoning_effort.clone().unwrap_or_default());
            params.push(("reasoning_effort".to_string(), Value::String(effort)));
        }
    } else if compat.thinking_format == "qwen-chat-template" && model.reasoning {
        let reasoning_effort = options.and_then(|o| o.reasoning_effort.clone());
        params.push((
            "chat_template_kwargs".to_string(),
            Value::Map(vec![
                ("enable_thinking".to_string(), Value::Bool(reasoning_effort.is_some())),
                ("preserve_thinking".to_string(), Value::Bool(true)),
            ]),
        ));
    } else if compat.thinking_format == "chat-template" && model.reasoning {
        if let Some(values) = build_chat_template_values(model, options, compat.chat_template_kwargs.as_deref()) {
            params.push((
                "chat_template_kwargs".to_string(),
                Value::Map(values),
            ));
        }
    } else if compat.thinking_format == "baseten" && model.reasoning {
        if let Some(values) = build_chat_template_values(model, options, compat.chat_template_args.as_deref()) {
            params.push(("chat_template_args".to_string(), Value::Map(values)));
        }
        if compat.supports_reasoning_effort {
            let requested_effort = options.and_then(|o| o.reasoning_effort.clone());
            let mapped_effort = match &requested_effort {
                Some(effort) => thinking_level_map_value(model, effort),
                None => thinking_level_map_value(model, "off"),
            };
            let effort = mapped_effort.or(requested_effort);
            if let Some(effort) = effort {
                params.push(("reasoning_effort".to_string(), Value::String(effort)));
            }
        }
    } else if compat.thinking_format == "deepseek" && model.reasoning {
        if options.and_then(|o| o.reasoning_effort.as_ref()).is_some() {
            params.push((
                "thinking".to_string(),
                Value::Map(vec![("type".to_string(), Value::String("enabled".to_string()))]),
            ));
        } else if thinking_level_map_value(model, "off").is_some() {
            params.push((
                "thinking".to_string(),
                Value::Map(vec![("type".to_string(), Value::String("disabled".to_string()))]),
            ));
        }
        if options.and_then(|o| o.reasoning_effort.as_ref()).is_some() && compat.supports_reasoning_effort {
            let effort = options
                .and_then(|o| o.reasoning_effort.clone())
                .and_then(|effort| thinking_level_map_value(model, &effort))
                .unwrap_or_else(|| {
                    options
                        .and_then(|o| o.reasoning_effort.clone())
                        .unwrap_or_default()
                });
            params.push(("reasoning_effort".to_string(), Value::String(effort)));
        }
    } else if compat.thinking_format == "openrouter" && model.reasoning {
        let reasoning_effort = options.and_then(|o| o.reasoning_effort.clone());
        if reasoning_effort.is_some() {
            let effort = thinking_level_map_value(model, &reasoning_effort.clone().unwrap_or_default())
                .unwrap_or_else(|| reasoning_effort.clone().unwrap_or_default());
            params.push((
                "reasoning".to_string(),
                Value::Map(vec![("effort".to_string(), Value::String(effort))]),
            ));
        } else if thinking_level_map_value(model, "off").is_some() {
            let effort = thinking_level_map_value(model, "off").unwrap_or_else(|| "none".to_string());
            params.push((
                "reasoning".to_string(),
                Value::Map(vec![("effort".to_string(), Value::String(effort))]),
            ));
        }
    } else if compat.thinking_format == "ant-ling" && model.reasoning {
        if let Some(effort) = options.and_then(|o| o.reasoning_effort.clone()) {
            if let Some(mapped) = thinking_level_map_value(model, &effort) {
                params.push((
                    "reasoning".to_string(),
                    Value::Map(vec![("effort".to_string(), Value::String(mapped))]),
                ));
            }
        }
    } else if compat.thinking_format == "together" && model.reasoning {
        let reasoning_effort = options.and_then(|o| o.reasoning_effort.clone());
        params.push((
            "reasoning".to_string(),
            Value::Map(vec![("enabled".to_string(), Value::Bool(reasoning_effort.is_some()))]),
        ));
        if reasoning_effort.is_some() && compat.supports_reasoning_effort {
            let effort = thinking_level_map_value(model, &reasoning_effort.clone().unwrap_or_default())
                .unwrap_or_else(|| reasoning_effort.clone().unwrap_or_default());
            params.push(("reasoning_effort".to_string(), Value::String(effort)));
        }
    } else if compat.thinking_format == "string-thinking" && model.reasoning {
        let reasoning_effort = options.and_then(|o| o.reasoning_effort.clone());
        if reasoning_effort.is_some() {
            let thinking = thinking_level_map_value(model, &reasoning_effort.clone().unwrap_or_default())
                .unwrap_or_else(|| reasoning_effort.clone().unwrap_or_default());
            params.push(("thinking".to_string(), Value::String(thinking)));
        } else if thinking_level_map_value(model, "off").is_some() {
            let thinking = thinking_level_map_value(model, "off").unwrap_or_else(|| "none".to_string());
            params.push(("thinking".to_string(), Value::String(thinking)));
        }
    } else if options.and_then(|o| o.reasoning_effort.as_ref()).is_some()
        && model.reasoning
        && compat.supports_reasoning_effort
    {
        let effort = options
            .and_then(|o| o.reasoning_effort.clone())
            .and_then(|effort| thinking_level_map_value(model, &effort))
            .unwrap_or_else(|| {
                options
                    .and_then(|o| o.reasoning_effort.clone())
                    .unwrap_or_default()
            });
        params.push(("reasoning_effort".to_string(), Value::String(effort)));
    } else if options.and_then(|o| o.reasoning_effort.as_ref()).is_none()
        && model.reasoning
        && compat.supports_reasoning_effort
    {
        if let Some(off_value) = thinking_level_map_value(model, "off") {
            params.push(("reasoning_effort".to_string(), Value::String(off_value)));
        }
    }

    // vLLM caps reasoning with a top-level thinking_token_budget.
    if compat.supports_thinking_token_budget
        && options.and_then(|o| o.reasoning_effort.as_ref()).is_some()
        && model.reasoning
    {
        let level = clamp_reasoning(options.and_then(|o| o.reasoning_effort.as_ref()))
            .expect("clamped");
        let default_budgets = ThinkingBudgets {
            minimal: Some(1024.0),
            low: Some(2048.0),
            medium: Some(8192.0),
            high: Some(16384.0),
        };
        let custom = options.and_then(|o| o.thinking_budgets.clone());
        let budgets = ThinkingBudgets {
            minimal: custom.as_ref().and_then(|b| b.minimal).or(default_budgets.minimal),
            low: custom.as_ref().and_then(|b| b.low).or(default_budgets.low),
            medium: custom.as_ref().and_then(|b| b.medium).or(default_budgets.medium),
            high: custom.as_ref().and_then(|b| b.high).or(default_budgets.high),
        };
        let budget_for_level = match level.as_str() {
            "minimal" => budgets.minimal,
            "low" => budgets.low,
            "medium" => budgets.medium,
            _ => budgets.high,
        }
        .unwrap_or(0.0);
        let ceiling = params
            .iter()
            .find(|(key, _)| key == "max_tokens")
            .and_then(|(_, value)| value.as_number())
            .or_else(|| {
                params
                    .iter()
                    .find(|(key, _)| key == "max_completion_tokens")
                    .and_then(|(_, value)| value.as_number())
            })
            .unwrap_or(model.max_tokens);
        // Always leave room for the answer.
        let budget = budget_for_level.min((ceiling - MIN_ANSWER_TOKENS).max(0.0));
        if budget > 0.0 {
            params.push(("thinking_token_budget".to_string(), Value::Number(budget)));
        }
    }

    // OpenRouter provider routing preferences.
    if let Some(routing) = &compat.open_router_routing {
        params.push(("provider".to_string(), routing_to_value(routing)));
    }

    // Vercel AI Gateway provider routing preferences.
    if let Some(routing) = &compat.vercel_gateway_routing {
        if routing.only.is_some() || routing.order.is_some() {
            params.push(("providerOptions".to_string(), vercel_gateway_routing_to_value(routing)));
        }
    }

    // Last so custom keys override the named request fields.
    if let Some(sampling_params) = options.and_then(|o| o.stream.sampling_params.as_ref()) {
        for (key, value) in sampling_params {
            if let Some(existing) = params.iter_mut().find(|(k, _)| k == key) {
                existing.1 = value.clone();
            } else {
                params.push((key.clone(), value.clone()));
            }
        }
    }

    Value::Map(params)
}

// ---------------------------------------------------------------------------
// Chunk parsing
// ---------------------------------------------------------------------------

fn parse_chunk_usage(raw_usage: &Value, model: &Model) -> Usage {
    let entries: Vec<(String, Value)> = raw_usage.as_map().map(|entries| entries.to_vec()).unwrap_or_default();
    let prompt_tokens = get_number(&entries, "prompt_tokens").unwrap_or(0.0);
    let prompt_details = get_object(&entries, "prompt_tokens_details");
    let cache_read_tokens = prompt_details
        .and_then(|d| get_number(d, "cached_tokens"))
        .or_else(|| get_number(&entries, "prompt_cache_hit_tokens"))
        .unwrap_or(0.0);
    let cache_write_tokens = prompt_details
        .and_then(|d| get_number(d, "cache_write_tokens"))
        .unwrap_or(0.0);

    // Follow documented OpenAI/OpenRouter semantics: cached_tokens is
    // cache-read tokens (hits). Do not subtract writes from cached_tokens.
    let input = (prompt_tokens - cache_read_tokens - cache_write_tokens).max(0.0);
    let output_tokens = get_number(&entries, "completion_tokens").unwrap_or(0.0);
    let completion_details = get_object(&entries, "completion_tokens_details");
    let reasoning = completion_details
        .and_then(|d| get_number(d, "reasoning_tokens"))
        .unwrap_or(0.0);

    let mut usage = Usage {
        input,
        output: output_tokens,
        cache_read: cache_read_tokens,
        cache_write: cache_write_tokens,
        cache_write_1h: None,
        reasoning: Some(reasoning),
        total_tokens: input + output_tokens + cache_read_tokens + cache_write_tokens,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
    };
    calculate_cost(model, &mut usage);
    usage
}

fn get_number(entries: &[(String, Value)], key: &str) -> Option<f64> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, value)| value.as_number())
}

fn get_object<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a [(String, Value)]> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, value)| value.as_map())
}

fn get_string<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, value)| value.as_str())
}

pub fn map_stop_reason(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" | "end" => (StopReason::Stop, None),
        "length" => (StopReason::Length, None),
        "function_call" | "tool_calls" => (StopReason::ToolUse, None),
        "content_filter" => (
            StopReason::Error,
            Some("Provider finish_reason: content_filter".to_string()),
        ),
        "network_error" => (
            StopReason::Error,
            Some("Provider finish_reason: network_error".to_string()),
        ),
        other => (
            StopReason::Error,
            Some(format!("Provider finish_reason: {other}")),
        ),
    }
}

// ---------------------------------------------------------------------------
// Streaming state machine
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum StreamingBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(StreamingToolCallBlock),
}

#[derive(Clone)]
struct StreamingToolCallBlock {
    tool_call: ToolCall,
    partial_args: Option<String>,
    custom_input: Option<CustomInputState>,
}

#[derive(Clone)]
struct CustomInputState {
    property: String,
    json_buffer: GrammarToolInputJsonBuffer,
}

struct StreamingToolCallDelta {
    index: Option<f64>,
    id: Option<String>,
    name: Option<String>,
    function_arguments: Option<String>,
    custom_input: Option<String>,
    is_custom: bool,
}

fn parse_tool_call_delta(value: &Value) -> Option<StreamingToolCallDelta> {
    let entries = value.as_map()?;
    let index = get_number(entries, "index");
    let id = get_string(entries, "id").map(|s| s.to_string());
    let function = get_object(entries, "function");
    let custom = get_object(entries, "custom");
    let function_name = function.and_then(|f| get_string(f, "name")).map(|s| s.to_string());
    let function_arguments = function
        .and_then(|f| get_string(f, "arguments"))
        .map(|s| s.to_string());
    let custom_name = custom.and_then(|c| get_string(c, "name")).map(|s| s.to_string());
    let custom_input = custom
        .and_then(|c| get_string(c, "input"))
        .map(|s| s.to_string());
    Some(StreamingToolCallDelta {
        index,
        id,
        name: function_name.or(custom_name),
        function_arguments,
        custom_input,
        is_custom: custom.is_some(),
    })
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

fn format_openai_completions_error(error: &ProviderError) -> String {
    let normalized = NormalizedProviderError::new(error.message.clone(), error.status, None);
    format_provider_error(&normalized, None)
}

// ---------------------------------------------------------------------------
// stream
// ---------------------------------------------------------------------------

/// Stream function for the OpenAI-compatible Chat Completions API. Spawns a
/// worker thread that performs the request and feeds the returned stream.
pub fn stream(
    model: &Model,
    context: &Context,
    options: Option<&OpenAICompletionsOptions>,
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
            let api_key = get_client_api_key(
                &model.provider,
                api_key.as_deref(),
                options.as_ref().and_then(|o| o.stream.request.headers.as_ref()),
            )?;
            let compat = get_compat(&model);
            let grammar_tool_input_properties = create_grammar_tool_input_properties(
                context.tools.as_deref(),
                compat.supports_openai_grammar_tools,
            );
            let cache_retention = resolve_cache_retention(
                options.as_ref().and_then(|o| o.stream.cache_retention.as_ref()),
                options.as_ref().and_then(|o| o.stream.request.env.as_ref()),
            );
            let cache_session_id = if cache_retention == "none" {
                None
            } else {
                options.as_ref().and_then(|o| o.stream.session_id.clone())
            };
            let params = build_params(
                &model,
                &context,
                options.as_ref(),
                &compat,
                &cache_retention,
                &grammar_tool_input_properties,
            );

            let mut headers: Vec<(String, String)> = vec![("Authorization".to_string(), format!("Bearer {api_key}"))];
            // Client headers (model headers, copilot dynamic, session affinity).
            let mut client_headers: Vec<(String, String)> = model.headers.clone().unwrap_or_default();
            if model.provider == "github-copilot" {
                let has_images = has_copilot_vision_input(&context.messages);
                let copilot_headers = build_copilot_dynamic_headers(CopilotDynamicHeadersParams {
                    messages: &context.messages,
                    has_images,
                });
                for (key, value) in copilot_headers {
                    if let Some(existing) = client_headers.iter_mut().find(|(k, _)| k == &key) {
                        existing.1 = value;
                    } else {
                        client_headers.push((key, value));
                    }
                }
            }
            if let Some(session_id) = &cache_session_id {
                if compat.send_session_affinity_headers {
                    if compat.session_affinity_format == "openrouter" {
                        client_headers.push(("x-session-id".to_string(), session_id.clone()));
                    } else {
                        if compat.session_affinity_format == "openai" {
                            client_headers.push(("session_id".to_string(), session_id.clone()));
                        }
                        client_headers.push(("x-client-request-id".to_string(), session_id.clone()));
                        client_headers.push(("x-session-affinity".to_string(), session_id.clone()));
                    }
                }
            }
            // Merge options headers last so they can override defaults.
            if let Some(options_headers) = options.as_ref().and_then(|o| o.stream.request.headers.as_ref()) {
                for (key, value) in options_headers {
                    if let Some(value) = value {
                        if let Some(existing) = client_headers.iter_mut().find(|(k, _)| k == key) {
                            existing.1 = value.clone();
                        } else {
                            client_headers.push((key.clone(), value.clone()));
                        }
                    }
                }
            }
            headers.extend(client_headers);
            let url = format!("{}/chat/completions", model.base_url.trim_end_matches('/'));

            let response = retry_provider_request(
                || {
                    client
                        .post_json(
                            &url,
                            &headers,
                            &params,
                            options.as_ref().and_then(|o| o.stream.request.timeout_ms),
                        )
                        .map_err(|error| {
                            ProviderError::new(error.status, error.headers.clone(), error.message.clone())
                        })
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
                ProviderRetryFailure::Error(error) => format_openai_completions_error(&error),
                ProviderRetryFailure::Aborted => "Request was aborted".to_string(),
            })?;

            stream.push(crate::types::AssistantMessageEvent::Start {
                partial: output.clone(),
            });

            // Stream state.
            let mut text_block: Option<TextContent> = None;
            let mut thinking_block: Option<ThinkingContent> = None;
            let mut has_finish_reason = false;
            let mut tool_call_blocks_by_index: std::collections::HashMap<u64, StreamingBlock> =
                std::collections::HashMap::new();
            let mut tool_call_blocks_by_id: std::collections::HashMap<String, StreamingBlock> =
                std::collections::HashMap::new();
            let mut pending_reasoning_details_by_tool_call_id: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();

            let mut chunk_parsing_errors = Vec::new();
            crate::http::client::read_sse_stream(response.reader, |sse: &SseEvent| {
                if sse.data == "[DONE]" {
                    return;
                }
                let chunk: Value = match parse_json_with_repair(&sse.data) {
                    Ok(value) => value,
                    Err(_) => return,
                };
                if chunk_parsing_errors.is_empty() {
                    if let Some(error) = process_chunk(
                        &chunk,
                        &mut output,
                        &stream,
                        &model,
                        &compat,
                        &grammar_tool_input_properties,
                        &mut text_block,
                        &mut thinking_block,
                        &mut has_finish_reason,
                        &mut tool_call_blocks_by_index,
                        &mut tool_call_blocks_by_id,
                        &mut pending_reasoning_details_by_tool_call_id,
                    ) {
                        chunk_parsing_errors.push(error);
                    }
                }
            });

            if let Some(error) = chunk_parsing_errors.first() {
                return Err(error.clone());
            }

            // Finish blocks.
            let blocks: Vec<StreamingBlock> = output
                .content
                .iter()
                .map(|block| match block {
                    Content::Text(text) => StreamingBlock::Text(text.clone()),
                    Content::Thinking(thinking) => StreamingBlock::Thinking(thinking.clone()),
                    Content::ToolCall(tool_call) => {
                        let index = output
                            .content
                            .iter()
                            .position(|b| std::ptr::eq(b, block))
                            .unwrap_or(0);
                        let _ = index;
                        let scratch = tool_call_blocks_by_index
                            .values()
                            .find(|candidate| matches!(candidate, StreamingBlock::ToolCall(c) if c.tool_call.id == tool_call.id))
                            .cloned();
                        match scratch {
                            Some(StreamingBlock::ToolCall(scratched)) => StreamingBlock::ToolCall(scratched),
                            _ => StreamingBlock::ToolCall(StreamingToolCallBlock {
                                tool_call: tool_call.clone(),
                                partial_args: None,
                                custom_input: None,
                            }),
                        }
                    }
                    _ => unreachable!("assistant content only has text/thinking/toolCall"),
                })
                .collect();

            let mut errors: Vec<String> = Vec::new();
            for block in &blocks {
                if let Err(error) = finish_block(&block, &mut output, &stream, &compat) {
                    errors.push(error);
                }
            }
            if let Some(error) = errors.first() {
                return Err(error.clone());
            }

            if output.stop_reason == StopReason::Aborted {
                return Err("Request was aborted".to_string());
            }
            if !has_finish_reason && !compat.supports_finish_reason {
                output.stop_reason = if output.content.iter().any(|block| matches!(block, Content::ToolCall(_))) {
                    StopReason::ToolUse
                } else {
                    StopReason::Stop
                };
            }
            if output.stop_reason == StopReason::Error {
                return Err(output.error_message.clone().unwrap_or_else(|| {
                    "Provider returned an error stop reason".to_string()
                }));
            }
            if (compat.supports_finish_reason && !has_finish_reason) || output.stop_reason == StopReason::Pending {
                return Err("Stream ended without finish_reason".to_string());
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

#[allow(clippy::too_many_arguments)]
fn process_chunk(
    chunk: &Value,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
    compat: &ResolvedOpenAICompletionsCompat,
    grammar_tool_input_properties: &[(String, String)],
    text_block: &mut Option<TextContent>,
    thinking_block: &mut Option<ThinkingContent>,
    has_finish_reason: &mut bool,
    tool_call_blocks_by_index: &mut std::collections::HashMap<u64, StreamingBlock>,
    tool_call_blocks_by_id: &mut std::collections::HashMap<String, StreamingBlock>,
    pending_reasoning_details_by_tool_call_id: &mut std::collections::HashMap<String, String>,
) -> Option<String> {
    let Some(chunk_entries) = chunk.as_map() else {
        return None;
    };

    if output.response_id.is_none() {
        if let Some(id) = get_string(chunk_entries, "id") {
            if !id.is_empty() {
                output.response_id = Some(id.to_string());
            }
        }
    }
    if let Some(chunk_model) = get_string(chunk_entries, "model") {
        if !chunk_model.is_empty() && chunk_model != model.id && output.response_model.is_none() {
            output.response_model = Some(chunk_model.to_string());
        }
    }
    if let Some(usage) = get_object(chunk_entries, "usage") {
        output.usage = parse_chunk_usage(&Value::Map(usage.to_vec()), model);
    }

    let Some(choices) = get_array(chunk_entries, "choices") else {
        return None;
    };
    let Some(choice) = choices.first() else {
        return None;
    };
    let Some(choice_entries) = choice.as_map() else {
        return None;
    };

    // Fallback: some providers return usage in choice.usage.
    if get_object(chunk_entries, "usage").is_none() {
        if let Some(choice_usage) = get_object(choice_entries, "usage") {
            output.usage = parse_chunk_usage(&Value::Map(choice_usage.to_vec()), model);
        }
    }

    if let Some(finish_reason) = get_string(choice_entries, "finish_reason") {
        if !finish_reason.is_empty() {
            output.raw_stop_reason = Some(finish_reason.to_string());
            let (stop_reason, error_message) = map_stop_reason(finish_reason);
            output.stop_reason = stop_reason;
            if let Some(error_message) = error_message {
                output.error_message = Some(error_message);
            }
            *has_finish_reason = true;
        }
    }

    let Some(delta) = get_object(choice_entries, "delta") else {
        return None;
    };

    if let Some(content) = get_string(delta, "content") {
        if !content.is_empty() {
            if text_block.is_none() {
                let block = TextContent {
                    text: String::new(),
                    text_signature: None,
                };
                output.content.push(Content::Text(block.clone()));
                let content_index = (output.content.len() - 1) as f64;
                *text_block = Some(block);
                stream.push(crate::types::AssistantMessageEvent::TextStart {
                    content_index,
                    partial: output.clone(),
                });
            }
            let block = text_block.as_mut().expect("set above");
            block.text.push_str(content);
            let content_index = (output.content.len() - 1) as f64;
            stream.push(crate::types::AssistantMessageEvent::TextDelta {
                content_index,
                delta: content.to_string(),
                partial: output.clone(),
            });
        }
    }

    // Reasoning fields: use the first non-empty field to avoid duplication.
    let reasoning_fields = ["reasoning_content", "reasoning", "reasoning_text"];
    let mut found_reasoning: Option<(String, String)> = None;
    for field in reasoning_fields {
        if let Some(value) = get_string(delta, field) {
            if !value.is_empty() {
                found_reasoning = Some((field.to_string(), value.to_string()));
                break;
            }
        }
    }
    if let Some((field, delta_text)) = found_reasoning {
        let thinking_signature = if model.provider == "opencode-go" && field == "reasoning" {
            "reasoning_content".to_string()
        } else {
            field
        };
        if thinking_block.is_none() {
            let block = ThinkingContent {
                thinking: String::new(),
                thinking_signature: Some(thinking_signature.clone()),
                redacted: None,
            };
            output.content.push(Content::Thinking(block.clone()));
            *thinking_block = Some(block);
            let content_index = (output.content.len() - 1) as f64;
            stream.push(crate::types::AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: output.clone(),
            });
        }
        let block = thinking_block.as_mut().expect("set above");
        block.thinking.push_str(&delta_text);
        let content_index = (output.content.len() - 1) as f64;
        stream.push(crate::types::AssistantMessageEvent::ThinkingDelta {
            content_index,
            delta: delta_text,
            partial: output.clone(),
        });
    }

    if let Some(tool_calls) = get_array(delta, "tool_calls") {
        for tool_call_value in tool_calls {
            let Some(tool_call) = parse_tool_call_delta(tool_call_value) else {
                continue;
            };
            let block_index = ensure_tool_call_block(
                &tool_call,
                output,
                stream,
                grammar_tool_input_properties,
                tool_call_blocks_by_index,
                tool_call_blocks_by_id,
                pending_reasoning_details_by_tool_call_id,
            );
            let Some(block_index) = block_index else {
                continue;
            };
            let name = tool_call.name.clone();
            let mut delta = String::new();
            if let Some(arguments) = tool_call.function_arguments {
                delta = arguments.clone();
                let block = tool_call_blocks_by_index
                    .get_mut(&(block_index as u64))
                    .and_then(|b| match b {
                        StreamingBlock::ToolCall(block) => Some(block),
                        _ => None,
                    });
                if let Some(block) = block {
                    let partial = block.partial_args.clone().unwrap_or_default() + &arguments;
                    block.partial_args = Some(partial.clone());
                    block.tool_call.arguments = parse_streaming_json(Some(&partial));
                }
            } else if let Some(custom_input) = tool_call.custom_input {
                let block = tool_call_blocks_by_index
                    .get_mut(&(block_index as u64))
                    .and_then(|b| match b {
                        StreamingBlock::ToolCall(block) => Some(block),
                        _ => None,
                    });
                if let Some(block) = block {
                    let current = get_custom_tool_call_input(block);
                    let next_input = format!("{current}{custom_input}");
                    let delta_out = append_custom_tool_call_input(block, &next_input, false)
                        .unwrap_or_default()
                        .unwrap_or_default();
                    delta = delta_out;
                }
            }
            let _ = name;
            stream.push(crate::types::AssistantMessageEvent::ToolCallDelta {
                content_index: block_index,
                delta,
                partial: output.clone(),
            });
        }
    }

    if let Some(reasoning_details) = get_array(delta, "reasoning_details") {
        for detail in reasoning_details {
            if is_encrypted_reasoning_detail(detail) {
                let serialized = json_stringify(detail);
                if let Some(id) = detail.as_map().and_then(|entries| get_string(entries, "id")) {
                    if let Some(block) = tool_call_blocks_by_id.get_mut(id) {
                        if let StreamingBlock::ToolCall(block) = block {
                            block.tool_call.thought_signature = Some(serialized);
                        }
                    } else {
                        pending_reasoning_details_by_tool_call_id
                            .insert(id.to_string(), serialized);
                    }
                }
            }
        }
    }

    let _ = compat;
    None
}

fn get_array<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a [Value]> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, value)| value.as_array())
}

fn get_custom_tool_call_input(block: &StreamingToolCallBlock) -> String {
    let Some(property) = block.custom_input.as_ref().map(|c| c.property.clone()) else {
        return String::new();
    };
    match &block.tool_call.arguments {
        Value::Map(entries) => entries
            .iter()
            .find(|(key, _)| key == &property)
            .and_then(|(_, value)| value.as_str())
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

fn append_custom_tool_call_input(
    block: &mut StreamingToolCallBlock,
    next_input: &str,
    close: bool,
) -> Result<Option<String>, String> {
    let Some(custom_input) = &mut block.custom_input else {
        return Ok(None);
    };
    let delta = append_grammar_tool_input_json_delta(
        &mut custom_input.json_buffer,
        &custom_input.property,
        next_input,
        close,
    )?;
    block.tool_call.arguments =
        Value::Map(vec![(custom_input.property.clone(), Value::String(next_input.to_string()))]);
    Ok(delta)
}

#[allow(clippy::too_many_arguments)]
fn ensure_tool_call_block(
    tool_call: &StreamingToolCallDelta,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    grammar_tool_input_properties: &[(String, String)],
    tool_call_blocks_by_index: &mut std::collections::HashMap<u64, StreamingBlock>,
    tool_call_blocks_by_id: &mut std::collections::HashMap<String, StreamingBlock>,
    pending_reasoning_details_by_tool_call_id: &mut std::collections::HashMap<String, String>,
) -> Option<f64> {
    let stream_index = tool_call.index;
    let name = tool_call.name.clone().unwrap_or_default();
    let mut existing: Option<StreamingBlock> = stream_index
        .and_then(|index| tool_call_blocks_by_index.get(&(index as u64)))
        .cloned();
    if existing.is_none() {
        if let Some(id) = &tool_call.id {
            existing = tool_call_blocks_by_id.get(id).cloned();
        }
    }
    let (block, content_index) = match existing {
        Some(StreamingBlock::ToolCall(block)) => {
            let content_index = find_content_index(output, &block.tool_call.id);
            (block, content_index)
        }
        Some(_) => return None,
        None => {
            let is_custom = tool_call.is_custom && tool_call.function_arguments.is_none();
            let custom_input_property = if is_custom {
                Some(
                    grammar_tool_input_properties
                        .iter()
                        .find(|(tool_name, _)| tool_name == &name)
                        .map(|(_, property)| property.clone())
                        .unwrap_or_else(|| "input".to_string()),
                )
            } else {
                None
            };
            let has_custom_input = custom_input_property.is_some();
            let block = StreamingToolCallBlock {
                tool_call: ToolCall {
                    id: tool_call.id.clone().unwrap_or_default(),
                    name: name.clone(),
                    arguments: match &custom_input_property {
                        Some(property) => Value::Map(vec![(property.clone(), Value::String(String::new()))]),
                        None => Value::Map(Vec::new()),
                    },
                    thought_signature: None,
                    namespace: None,
                },
                partial_args: if has_custom_input { None } else { Some(String::new()) },
                custom_input: custom_input_property.map(|property| CustomInputState {
                    property,
                    json_buffer: GrammarToolInputJsonBuffer::default(),
                }),
            };
            output.content.push(Content::ToolCall(block.tool_call.clone()));
            let content_index = (output.content.len() - 1) as f64;
            let streaming = StreamingBlock::ToolCall(block);
            if let Some(index) = stream_index {
                tool_call_blocks_by_index.insert(index as u64, streaming.clone());
            }
            if let Some(id) = &tool_call.id {
                tool_call_blocks_by_id.insert(id.clone(), streaming.clone());
            }
            stream.push(crate::types::AssistantMessageEvent::ToolCallStart {
                content_index,
                partial: output.clone(),
            });
            match streaming {
                StreamingBlock::ToolCall(block) => (block, content_index),
                _ => unreachable!(),
            }
        }
    };
    // Restore the block into the maps (they hold cloned state).
    if let Some(index) = stream_index {
        tool_call_blocks_by_index.insert(index as u64, StreamingBlock::ToolCall(block.clone()));
    }
    if let Some(id) = &tool_call.id {
        tool_call_blocks_by_id.insert(id.clone(), StreamingBlock::ToolCall(block.clone()));
    }
    let _ = pending_reasoning_details_by_tool_call_id;
    Some(content_index)
}

fn find_content_index(output: &AssistantMessage, tool_call_id: &str) -> f64 {
    output
        .content
        .iter()
        .position(|block| match block {
            Content::ToolCall(tool_call) => tool_call.id == tool_call_id,
            _ => false,
        })
        .map(|index| index as f64)
        .unwrap_or(0.0)
}

fn finish_block(
    block: &StreamingBlock,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    _compat: &ResolvedOpenAICompletionsCompat,
) -> Result<(), String> {
    let content_index = match block {
        StreamingBlock::Text(text) => output
            .content
            .iter()
            .position(|b| matches!(b, Content::Text(c) if c.text == text.text))
            .map(|index| index as f64)
            .unwrap_or(0.0),
        StreamingBlock::Thinking(thinking) => output
            .content
            .iter()
            .position(|b| matches!(b, Content::Thinking(c) if c.thinking == thinking.thinking))
            .map(|index| index as f64)
            .unwrap_or(0.0),
        StreamingBlock::ToolCall(tool_call) => find_content_index(output, &tool_call.tool_call.id),
    };

    match block {
        StreamingBlock::Text(text) => {
            stream.push(crate::types::AssistantMessageEvent::TextEnd {
                content_index,
                content: text.text.clone(),
                partial: output.clone(),
            });
        }
        StreamingBlock::Thinking(thinking) => {
            stream.push(crate::types::AssistantMessageEvent::ThinkingEnd {
                content_index,
                content: thinking.thinking.clone(),
                partial: output.clone(),
            });
        }
        StreamingBlock::ToolCall(block) => {
            if block.custom_input.is_some() {
                let current = get_custom_tool_call_input(block);
                let mut scratch_block = block.clone();
                if let Ok(Some(delta)) = append_custom_tool_call_input(&mut scratch_block, &current, true) {
                    stream.push(crate::types::AssistantMessageEvent::ToolCallDelta {
                        content_index,
                        delta,
                        partial: output.clone(),
                    });
                }
            } else {
                if let Some(partial_args) = &block.partial_args {
                    if let Content::ToolCall(tool_call) = &mut output.content[content_index as usize] {
                        tool_call.arguments = parse_streaming_json(Some(partial_args));
                    }
                }
            }
            // Finalize in place and strip scratch buffers.
            if let Content::ToolCall(tool_call) = &mut output.content[content_index as usize] {
                let finalized = tool_call.clone();
                stream.push(crate::types::AssistantMessageEvent::ToolCallEnd {
                    content_index,
                    tool_call: finalized,
                    partial: output.clone(),
                });
            }
        }
    }
    Ok(())
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

    let stream_options = OpenAICompletionsOptions {
        stream: base,
        reasoning_effort,
        thinking_budgets: options.and_then(|o| o.thinking_budgets.clone()),
        ..OpenAICompletionsOptions::default()
    };
    stream(model, context, Some(&stream_options), api_key, client)
}

/// Mirrors `clampMaxTokensToContext` usage from buildBaseOptions.
pub fn clamp_max_tokens(model: &Model, context: &Context, max_tokens: f64) -> f64 {
    clamp_max_tokens_to_context(model, context, max_tokens)
}
