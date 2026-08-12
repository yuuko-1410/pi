//! Model catalog parsing, port of `packages/ai/src/model-catalog.ts` plus
//! the JSON shape produced by `scripts/generate-models.ts`.
//!
//! The JS side flattens grouped JSON into `Model` objects at module load;
//! Rust parses the same catalog JSON into `Model` values.

use pi_protocol::Value;

use crate::types::{Model, ModelCompat, ModelCost, ModelCostRates, ModelCostTier};

fn get_str<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a str> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_str())
}

fn get_num(entries: &[(String, Value)], key: &str) -> Option<f64> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_number())
}

fn get_bool(entries: &[(String, Value)], key: &str) -> Option<bool> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_bool())
}

fn get_obj<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a [(String, Value)]> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_map())
}

fn parse_cost(entries: &[(String, Value)]) -> Result<ModelCost, String> {
    let rates = ModelCostRates {
        input: get_num(entries, "input").unwrap_or(0.0),
        output: get_num(entries, "output").unwrap_or(0.0),
        cache_read: get_num(entries, "cacheRead").unwrap_or(0.0),
        cache_write: get_num(entries, "cacheWrite").unwrap_or(0.0),
    };
    let tiers = get_obj(entries, "tiers").map(|tiers| {
        tiers
            .iter()
            .filter_map(|(_, tier)| tier.as_map())
            .map(|tier| ModelCostTier {
                rates: ModelCostRates {
                    input: get_num(tier, "input").unwrap_or(0.0),
                    output: get_num(tier, "output").unwrap_or(0.0),
                    cache_read: get_num(tier, "cacheRead").unwrap_or(0.0),
                    cache_write: get_num(tier, "cacheWrite").unwrap_or(0.0),
                },
                input_tokens_above: get_num(tier, "inputTokensAbove").unwrap_or(0.0),
            })
            .collect()
    });
    Ok(ModelCost { rates, tiers })
}

fn parse_thinking_level_map(entries: &[(String, Value)]) -> Option<crate::types::ThinkingLevelMap> {
    let mut map = Vec::new();
    for (level, value) in entries {
        let mapped = match value {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            _ => continue,
        };
        map.push((level.clone(), mapped));
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

/// Parses one catalog model object. Mirrors the flattenModelCatalog output
/// shape: `{ id, name, api, provider, baseUrl, reasoning, input, cost,
/// contextWindow, maxTokens, thinkingLevelMap?, samplingParams?, headers?,
/// compat? }`.
pub fn model_from_json(value: &Value) -> Result<Model, String> {
    let entries = value.as_map().ok_or("model catalog entry is not an object")?;
    let id = get_str(entries, "id").ok_or("model missing id")?.to_string();
    let name = get_str(entries, "name").unwrap_or("").to_string();
    let api = get_str(entries, "api").ok_or("model missing api")?.to_string();
    let provider = get_str(entries, "provider").ok_or("model missing provider")?.to_string();
    let base_url = get_str(entries, "baseUrl").unwrap_or("").to_string();
    let reasoning = get_bool(entries, "reasoning").unwrap_or(false);
    let input = entries
        .iter()
        .find(|(k, _)| k == "input")
        .and_then(|(_, v)| v.as_array())
        .map(|input| {
            input
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    let cost = match get_obj(entries, "cost") {
        Some(cost) => parse_cost(cost)?,
        None => ModelCost {
            rates: ModelCostRates {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            tiers: None,
        },
    };
    let context_window = get_num(entries, "contextWindow").unwrap_or(0.0);
    let max_tokens = get_num(entries, "maxTokens").unwrap_or(0.0);
    let thinking_level_map = get_obj(entries, "thinkingLevelMap").map(parse_thinking_level_map).flatten();
    let sampling_params = get_obj(entries, "samplingParams").map(|params| params.to_vec());
    let headers = get_obj(entries, "headers")
        .map(|headers| {
            headers
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
                .collect()
        })
        .filter(|headers: &Vec<(String, String)>| !headers.is_empty());
    let compat = parse_compat(entries);

    Ok(Model {
        id,
        name,
        api,
        provider,
        base_url,
        reasoning,
        thinking_level_map,
        input,
        cost,
        context_window,
        max_tokens,
        sampling_params,
        headers,
        compat,
    })
}

fn parse_chat_template_kwargs(entries: &[(String, Value)]) -> Vec<(String, crate::types::ChatTemplateKwargValue)> {
    entries
        .iter()
        .filter_map(|(key, value)| {
            let parsed = match value {
                Value::String(s) => crate::types::ChatTemplateKwargValue::Str(s.clone()),
                Value::Number(n) => crate::types::ChatTemplateKwargValue::Number(*n),
                Value::Bool(b) => crate::types::ChatTemplateKwargValue::Bool(*b),
                Value::Null => crate::types::ChatTemplateKwargValue::Null,
                Value::Map(var) => {
                    let var_name = get_str(var, "$var")?;
                    let omit_when_off = get_bool(var, "omitWhenOff");
                    crate::types::ChatTemplateKwargValue::Var {
                        var: var_name.to_string(),
                        omit_when_off,
                    }
                }
                _ => return None,
            };
            Some((key.clone(), parsed))
        })
        .collect()
}

/// Parses the `compat` field into the API-specific compat type based on the
/// model's `api` value (the catalog serializes compat per API family).
fn parse_compat(entries: &[(String, Value)]) -> Option<ModelCompat> {
    let compat = get_obj(entries, "compat")?;
    let api = get_str(entries, "api")?;
    let known = |entries: &[(String, Value)], key: &str| -> Option<bool> {
        get_bool(entries, key)
    };
    match api {
        "openai-completions" => Some(ModelCompat::OpenAiCompletions(crate::types::OpenAICompletionsCompat {
            supports_store: known(compat, "supportsStore"),
            supports_developer_role: known(compat, "supportsDeveloperRole"),
            supports_reasoning_effort: known(compat, "supportsReasoningEffort"),
            supports_usage_in_streaming: known(compat, "supportsUsageInStreaming"),
            supports_finish_reason: known(compat, "supportsFinishReason"),
            max_tokens_field: get_str(compat, "maxTokensField").map(|s| s.to_string()),
            requires_tool_result_name: known(compat, "requiresToolResultName"),
            requires_assistant_after_tool_result: known(compat, "requiresAssistantAfterToolResult"),
            requires_thinking_as_text: known(compat, "requiresThinkingAsText"),
            requires_reasoning_content_on_assistant_messages: known(compat, "requiresReasoningContentOnAssistantMessages"),
            thinking_format: get_str(compat, "thinkingFormat").map(|s| s.to_string()),
            chat_template_kwargs: get_obj(compat, "chatTemplateKwargs").map(parse_chat_template_kwargs),
            chat_template_args: get_obj(compat, "chatTemplateArgs").map(parse_chat_template_kwargs),
            open_router_routing: None,
            vercel_gateway_routing: None,
            zai_tool_stream: known(compat, "zaiToolStream"),
            supports_thinking_token_budget: known(compat, "supportsThinkingTokenBudget"),
            supports_openai_grammar_tools: known(compat, "supportsOpenAIGrammarTools"),
            supports_strict_mode: known(compat, "supportsStrictMode"),
            cache_control_format: get_str(compat, "cacheControlFormat").map(|s| s.to_string()),
            send_session_affinity_headers: known(compat, "sendSessionAffinityHeaders"),
            deferred_tools_mode: get_str(compat, "deferredToolsMode").map(|s| s.to_string()),
            session_affinity_format: get_str(compat, "sessionAffinityFormat").map(|s| s.to_string()),
            supports_long_cache_retention: known(compat, "supportsLongCacheRetention"),
        })),
        "openai-responses" | "azure-openai-responses" | "openai-codex-responses" => {
            Some(ModelCompat::OpenAiResponses(crate::types::OpenAIResponsesCompat {
                supports_developer_role: known(compat, "supportsDeveloperRole"),
                session_affinity_format: get_str(compat, "sessionAffinityFormat").map(|s| s.to_string()),
                supports_long_cache_retention: known(compat, "supportsLongCacheRetention"),
                supports_strict_mode: known(compat, "supportsStrictMode"),
                supports_openai_grammar_tools: known(compat, "supportsOpenAIGrammarTools"),
                supports_additional_tools: known(compat, "supportsAdditionalTools"),
                supports_tool_search: known(compat, "supportsToolSearch"),
                supports_explicit_prompt_cache_mode: known(compat, "supportsExplicitPromptCacheMode"),
            }))
        }
        "anthropic-messages" => Some(ModelCompat::AnthropicMessages(crate::types::AnthropicMessagesCompat {
            supports_eager_tool_input_streaming: known(compat, "supportsEagerToolInputStreaming"),
            supports_long_cache_retention: known(compat, "supportsLongCacheRetention"),
            send_session_affinity_headers: known(compat, "sendSessionAffinityHeaders"),
            supports_cache_control_on_tools: known(compat, "supportsCacheControlOnTools"),
            supports_temperature: known(compat, "supportsTemperature"),
            force_adaptive_thinking: known(compat, "forceAdaptiveThinking"),
            allow_empty_signature: known(compat, "allowEmptySignature"),
            supports_strict_tools: known(compat, "supportsStrictTools"),
            supports_tool_references: known(compat, "supportsToolReferences"),
        })),
        "bedrock-converse-stream" => Some(ModelCompat::Bedrock(crate::types::BedrockCompat {
            supports_strict_mode: known(compat, "supportsStrictMode"),
        })),
        _ => None,
    }
}

/// Mirrors `flattenModelCatalog`: merges grouped model objects (keyed by
/// model id) into one list.
pub fn flatten_model_catalog(groups: &Value) -> Result<Vec<Model>, String> {
    let mut models = Vec::new();
    if let Some(group_map) = groups.as_map() {
        for (_, group) in group_map {
            if let Some(models_map) = group.as_map() {
                for (_, model_value) in models_map {
                    models.push(model_from_json(model_value)?);
                }
            } else if let Some(model) = group.as_map() {
                // Flat single-model group.
                let _ = model;
            }
        }
    } else if let Some(list) = groups.as_array() {
        for model in list {
            models.push(model_from_json(model)?);
        }
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_json() -> Value {
        Value::Map(vec![
            ("id".to_string(), Value::String("claude-sonnet-4".to_string())),
            ("name".to_string(), Value::String("Claude Sonnet 4".to_string())),
            ("api".to_string(), Value::String("anthropic-messages".to_string())),
            ("provider".to_string(), Value::String("anthropic".to_string())),
            ("baseUrl".to_string(), Value::String("https://api.anthropic.com".to_string())),
            ("reasoning".to_string(), Value::Bool(true)),
            ("input".to_string(), Value::Array(vec![Value::String("text".to_string())])),
            (
                "cost".to_string(),
                Value::Map(vec![
                    ("input".to_string(), Value::Number(3.0)),
                    ("output".to_string(), Value::Number(15.0)),
                    ("cacheRead".to_string(), Value::Number(0.3)),
                    ("cacheWrite".to_string(), Value::Number(3.0)),
                ]),
            ),
            ("contextWindow".to_string(), Value::Number(200_000.0)),
            ("maxTokens".to_string(), Value::Number(8192.0)),
            (
                "thinkingLevelMap".to_string(),
                Value::Map(vec![
                    ("off".to_string(), Value::Null),
                    ("high".to_string(), Value::String("high".to_string())),
                ]),
            ),
            (
                "compat".to_string(),
                Value::Map(vec![
                    ("supportsStrictTools".to_string(), Value::Bool(true)),
                    ("forceAdaptiveThinking".to_string(), Value::Bool(true)),
                ]),
            ),
        ])
    }

    #[test]
    fn parses_catalog_model_objects() {
        let model = model_from_json(&model_json()).unwrap();
        assert_eq!(model.id, "claude-sonnet-4");
        assert_eq!(model.api, "anthropic-messages");
        assert!(model.reasoning);
        assert_eq!(model.cost.rates.input, 3.0);
        assert_eq!(model.context_window, 200_000.0);
        assert_eq!(
            model.thinking_level_map,
            Some(vec![("off".to_string(), None), ("high".to_string(), Some("high".to_string()))])
        );
        let Some(ModelCompat::AnthropicMessages(compat)) = &model.compat else {
            panic!("expected anthropic compat");
        };
        assert_eq!(compat.supports_strict_tools, Some(true));
        assert_eq!(compat.force_adaptive_thinking, Some(true));
    }

    #[test]
    fn flattens_grouped_catalogs() {
        let groups = Value::Map(vec![(
            "anthropic-messages".to_string(),
            Value::Map(vec![("claude-sonnet-4".to_string(), model_json())]),
        )]);
        let models = flatten_model_catalog(&groups).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "claude-sonnet-4");
    }
}
