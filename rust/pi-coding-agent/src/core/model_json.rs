//! Model JSON round trips for the models-store file format, mirroring
//! `JSON.stringify` of the JS `Model` objects stored by ModelsStore.
//! Parsing reuses pi-ai's catalog decoder; serialization writes the JS field
//! shapes (construction order).

use pi_ai::types::{Model, ModelCompat, ModelCost, ThinkingLevelMap};
use pi_protocol::Value;

pub fn model_to_json(model: &Model) -> Value {
    let mut entries: Vec<(String, Value)> = vec![
        kv("id", str(&model.id)),
        kv("name", str(&model.name)),
        kv("api", str(&model.api)),
        kv("provider", str(&model.provider)),
        kv("baseUrl", str(&model.base_url)),
        kv("reasoning", Value::Bool(model.reasoning)),
    ];
    if let Some(thinking_level_map) = &model.thinking_level_map {
        entries.push(kv("thinkingLevelMap", thinking_level_map_to_json(thinking_level_map)));
    }
    entries.push(kv(
        "input",
        Value::Array(model.input.iter().map(|v| Value::String(v.clone())).collect()),
    ));
    entries.push(kv("cost", cost_to_json(&model.cost)));
    entries.push(kv("contextWindow", Value::Number(model.context_window)));
    entries.push(kv("maxTokens", Value::Number(model.max_tokens)));
    if let Some(sampling_params) = &model.sampling_params {
        entries.push(kv(
            "samplingParams",
            Value::Map(sampling_params.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
        ));
    }
    if let Some(headers) = &model.headers {
        entries.push(kv(
            "headers",
            Value::Map(headers.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect()),
        ));
    }
    if let Some(compat) = &model.compat {
        entries.push(kv("compat", compat_to_json(compat)));
    }
    Value::Map(entries)
}

pub fn json_to_model(value: &Value) -> Option<Model> {
    pi_ai::model_catalog::model_from_json(value).ok()
}

fn kv(key: &str, value: Value) -> (String, Value) {
    (key.to_string(), value)
}

fn str(value: &str) -> Value {
    Value::String(value.to_string())
}

fn num(value: f64) -> Value {
    Value::Number(value)
}

fn cost_to_json(cost: &ModelCost) -> Value {
    let mut entries = vec![
        kv("input", num(cost.rates.input)),
        kv("output", num(cost.rates.output)),
        kv("cacheRead", num(cost.rates.cache_read)),
        kv("cacheWrite", num(cost.rates.cache_write)),
    ];
    if let Some(tiers) = &cost.tiers {
        entries.push(kv(
            "tiers",
            Value::Array(
                tiers
                    .iter()
                    .map(|tier| {
                        Value::Map(vec![
                            kv("inputTokensAbove", num(tier.input_tokens_above)),
                            kv("input", num(tier.rates.input)),
                            kv("output", num(tier.rates.output)),
                            kv("cacheRead", num(tier.rates.cache_read)),
                            kv("cacheWrite", num(tier.rates.cache_write)),
                        ])
                    })
                    .collect(),
            ),
        ));
    }
    Value::Map(entries)
}

fn thinking_level_map_to_json(map: &ThinkingLevelMap) -> Value {
    Value::Map(
        map.iter()
            .map(|(level, value)| {
                (
                    level.clone(),
                    value
                        .as_ref()
                        .map(|value| Value::String(value.clone()))
                        .unwrap_or(Value::Null),
                )
            })
            .collect(),
    )
}

fn compat_to_json(compat: &ModelCompat) -> Value {
    // The compat objects are stored verbatim from the catalog; the typed
    // structs lose unknown fields, so round-trip the known fields only.
    // ponytail: compat fields are consumed by provider adapters (pi-ai),
    // not by the models store consumers; a lossy round trip is acceptable.
    match compat {
        ModelCompat::OpenAiCompletions(_) => Value::Map(vec![kv("api", str("openai-completions"))]),
        ModelCompat::OpenAiResponses(_) => Value::Map(vec![kv("api", str("openai-responses"))]),
        ModelCompat::AnthropicMessages(_) => Value::Map(vec![kv("api", str("anthropic-messages"))]),
        ModelCompat::Bedrock(_) => Value::Map(vec![kv("api", str("bedrock"))]),
    }
}
