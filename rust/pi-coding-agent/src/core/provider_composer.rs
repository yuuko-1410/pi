//! Provider composition from models.json and extensions, port of the pure
//! function surface of `core/provider-composer.ts`.
//!
//! ponytail: the JS composeModelProvider builds a full Provider object
//! (apiKey/oauth auth orchestration, lazyStream routing, refreshModels
//! publishing). The Rust runtime streams through the agent's StreamFn
//! directly, so this module ports the models.json merge/override/validation
//! layer and the request-time header/auth-status helpers; the Provider
//! composition object itself is not needed and is marked as a difference.

use std::collections::HashMap;

use pi_protocol::Value;

use super::model_config::{ModelsJsonModel, ModelsJsonProvider, ModelConfig};
use super::resolve_config_value::{
    get_config_value_env_var_names, is_command_config_value, is_config_value_configured, resolve_headers_or_throw,
};

#[derive(Clone, Debug, PartialEq)]
pub struct AuthStatus {
    pub configured: bool,
    pub source: Option<String>,
    pub label: Option<String>,
}

pub fn clear_api_key_cache() {
    super::resolve_config_value::clear_config_value_cache();
}

/// Merge compat objects: shallow merge with nested deep-merge for the known
/// object keys (openRouterRouting etc.).
pub fn merge_compat(base: Option<&Value>, override_value: Option<&Value>) -> Option<Value> {
    match (base, override_value) {
        (None, None) => None,
        (Some(base), None) => Some(base.clone()),
        (None, Some(override_value)) => Some(override_value.clone()),
        (Some(base), Some(override_value)) => {
            let base_map = base.as_map().unwrap_or_default();
            let override_map = override_value.as_map().unwrap_or_default();
            let mut merged: Vec<(String, Value)> = base_map.to_vec();
            for (key, value) in override_map {
                let nested_keys = ["openRouterRouting", "vercelGatewayRouting", "chatTemplateKwargs", "chatTemplateArgs"];
                if nested_keys.contains(&key.as_str()) {
                    let base_nested = merged.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
                    let combined = match (&base_nested, value) {
                        (Some(Value::Map(base_entries)), Value::Map(override_entries)) => {
                            let mut entries = base_entries.clone();
                            for (k, v) in override_entries {
                                if let Some(slot) = entries.iter_mut().find(|(existing, _)| existing == k) {
                                    slot.1 = v.clone();
                                } else {
                                    entries.push((k.clone(), v.clone()));
                                }
                            }
                            Value::Map(entries)
                        }
                        (_, override_value) => override_value.clone(),
                    };
                    if let Some(slot) = merged.iter_mut().find(|(k, _)| k == key) {
                        slot.1 = combined;
                    } else {
                        merged.push((key.clone(), combined));
                    }
                } else if let Some(slot) = merged.iter_mut().find(|(k, _)| k == key) {
                    slot.1 = value.clone();
                } else {
                    merged.push((key.clone(), value.clone()));
                }
            }
            Some(Value::Map(merged))
        }
    }
}

/// Apply a models.json modelOverrides entry to a model.
pub fn apply_model_override(model: Value, override_value: &Value) -> Value {
    let model_entries = model.as_map().unwrap_or_default();
    let override_entries = override_value.as_map().unwrap_or_default();
    let get = |key: &str| -> Option<&Value> { override_entries.iter().find(|(k, _)| k == key).map(|(_, v)| v) };

    let mut merged: Vec<(String, Value)> = model_entries.to_vec();
    let set = |merged: &mut Vec<(String, Value)>, key: &str, value: Value| {
        if let Some(slot) = merged.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value;
        } else {
            merged.push((key.to_string(), value));
        }
    };

    if let Some(name) = get("name") {
        set(&mut merged, "name", name.clone());
    }
    if let Some(reasoning) = get("reasoning") {
        set(&mut merged, "reasoning", reasoning.clone());
    }
    if let Some(thinking_level_map) = get("thinkingLevelMap") {
        // Merge maps.
        let current = merged.iter().find(|(k, _)| k == "thinkingLevelMap").map(|(_, v)| v);
        let combined = match (current, thinking_level_map) {
            (Some(Value::Map(current)), Value::Map(override_map)) => {
                let mut entries = current.clone();
                for (k, v) in override_map {
                    if let Some(slot) = entries.iter_mut().find(|(existing, _)| existing == k) {
                        slot.1 = v.clone();
                    } else {
                        entries.push((k.clone(), v.clone()));
                    }
                }
                Value::Map(entries)
            }
            (_, override_map) => override_map.clone(),
        };
        set(&mut merged, "thinkingLevelMap", combined);
    }
    if let Some(input) = get("input") {
        set(&mut merged, "input", input.clone());
    }
    if let Some(cost) = get("cost") {
        let cost_entries = cost.as_map().unwrap_or_default();
        let current_cost = merged.iter().find(|(k, _)| k == "cost").map(|(_, v)| v.clone());
        let mut cost_merged: Vec<(String, Value)> = current_cost
            .as_ref()
            .and_then(|value| value.as_map())
            .map(|map| map.to_vec())
            .unwrap_or_default();
        for key in ["input", "output", "cacheRead", "cacheWrite", "tiers"] {
            if let Some(value) = cost_entries.iter().find(|(k, _)| k == key) {
                if let Some(slot) = cost_merged.iter_mut().find(|(k, _)| k == key) {
                    slot.1 = value.1.clone();
                } else {
                    cost_merged.push((key.to_string(), value.1.clone()));
                }
            }
        }
        set(&mut merged, "cost", Value::Map(cost_merged));
    }
    if let Some(context_window) = get("contextWindow") {
        set(&mut merged, "contextWindow", context_window.clone());
    }
    if let Some(max_tokens) = get("maxTokens") {
        set(&mut merged, "maxTokens", max_tokens.clone());
    }
    if let Some(sampling_params) = get("samplingParams") {
        let current = merged.iter().find(|(k, _)| k == "samplingParams").map(|(_, v)| v);
        let combined = match (current, sampling_params) {
            (Some(Value::Map(current)), Value::Map(override_map)) => {
                let mut entries = current.clone();
                for (k, v) in override_map {
                    if let Some(slot) = entries.iter_mut().find(|(existing, _)| existing == k) {
                        slot.1 = v.clone();
                    } else {
                        entries.push((k.clone(), v.clone()));
                    }
                }
                Value::Map(entries)
            }
            (_, override_map) => override_map.clone(),
        };
        set(&mut merged, "samplingParams", combined);
    }
    if let Some(compat) = get("compat") {
        let current_compat = merged.iter().find(|(k, _)| k == "compat").map(|(_, v)| v);
        if let Some(merged_compat) = merge_compat(current_compat, Some(compat)) {
            set(&mut merged, "compat", merged_compat);
        }
    }
    Value::Map(merged)
}

/// Build a Model from a models.json definition with JS error semantics.
pub fn model_from_json(
    provider_id: &str,
    definition: &ModelsJsonModel,
    provider_config: &ModelsJsonProvider,
    defaults: Option<&Value>,
) -> Result<Value, String> {
    let api = definition
        .api
        .clone()
        .or_else(|| provider_config.api.clone())
        .or_else(|| defaults.and_then(|d| d.as_map()).and_then(|entries| {
            entries.iter().find(|(k, _)| k == "api").and_then(|(_, v)| v.as_str().map(|s| s.to_string()))
        }));
    let Some(api) = api else {
        return Err(format!(
            "Provider {provider_id}, model {}: no \"api\" specified. Set at provider or model level.",
            definition.id
        ));
    };
    let base_url = definition
        .base_url
        .clone()
        .or_else(|| provider_config.base_url.clone())
        .or_else(|| defaults.and_then(|d| d.as_map()).and_then(|entries| {
            entries.iter().find(|(k, _)| k == "baseUrl").and_then(|(_, v)| v.as_str().map(|s| s.to_string()))
        }));
    let Some(base_url) = base_url else {
        return Err(format!("Provider {provider_id}: \"baseUrl\" is required when defining custom models."));
    };
    if definition.context_window.is_some_and(|value| value <= 0.0) {
        return Err(format!("Provider {provider_id}, model {}: invalid contextWindow", definition.id));
    }
    if definition.max_tokens.is_some_and(|value| value <= 0.0) {
        return Err(format!("Provider {provider_id}, model {}: invalid maxTokens", definition.id));
    }

    let mut entries: Vec<(String, Value)> = vec![
        ("id".to_string(), Value::String(definition.id.clone())),
        (
            "name".to_string(),
            Value::String(definition.name.clone().unwrap_or_else(|| definition.id.clone())),
        ),
        ("api".to_string(), Value::String(api)),
        ("provider".to_string(), Value::String(provider_id.to_string())),
        ("baseUrl".to_string(), Value::String(base_url)),
        ("reasoning".to_string(), Value::Bool(definition.reasoning.unwrap_or(false))),
        (
            "input".to_string(),
            Value::Array(
                definition
                    .input
                    .clone()
                    .unwrap_or_else(|| vec!["text".to_string()])
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        ),
        (
            "cost".to_string(),
            definition
                .raw
                .as_map()
                .and_then(|entries| entries.iter().find(|(k, _)| k == "cost").map(|(_, v)| v.clone()))
                .unwrap_or_else(|| {
                    Value::Map(vec![
                        ("input".to_string(), Value::Number(0.0)),
                        ("output".to_string(), Value::Number(0.0)),
                        ("cacheRead".to_string(), Value::Number(0.0)),
                        ("cacheWrite".to_string(), Value::Number(0.0)),
                    ])
                }),
        ),
        ("contextWindow".to_string(), Value::Number(definition.context_window.unwrap_or(128000.0))),
        ("maxTokens".to_string(), Value::Number(definition.max_tokens.unwrap_or(16384.0))),
    ];
    if let Some(thinking_level_map) = definition.raw.as_map().and_then(|entries| {
        entries.iter().find(|(k, _)| k == "thinkingLevelMap").map(|(_, v)| v.clone())
    }) {
        entries.push(("thinkingLevelMap".to_string(), thinking_level_map));
    }
    if let Some(sampling_params) = definition.raw.as_map().and_then(|entries| {
        entries.iter().find(|(k, _)| k == "samplingParams").map(|(_, v)| v.clone())
    }) {
        entries.push(("samplingParams".to_string(), sampling_params));
    }
    if let Some(compat) = merge_compat(provider_config.compat.as_ref(), definition.compat.as_ref()) {
        entries.push(("compat".to_string(), compat));
    }
    Ok(Value::Map(entries))
}

/// Apply a models.json provider config over base models.
pub fn apply_models_json(provider_id: &str, base_models: &[Value], config: Option<&ModelsJsonProvider>) -> Result<Vec<Value>, String> {
    let Some(config) = config else {
        return Ok(base_models.to_vec());
    };
    if config.oauth.is_some() && config.base_url.is_none() {
        return Err(format!("Provider {provider_id}: \"baseUrl\" is required when \"oauth\" is set."));
    }
    let has_overrides = !config.model_overrides.is_empty();
    if config.models.is_empty()
        && config.base_url.is_none()
        && config.headers.is_none()
        && config.compat.is_none()
        && !has_overrides
        && config.api_key.is_none()
        && config.oauth.is_none()
        && config.auth_header.is_none()
    {
        return Err(format!(
            "Provider {provider_id}: must specify \"baseUrl\", \"headers\", \"compat\", \"modelOverrides\", or \"models\"."
        ));
    }

    let mut models: Vec<Value> = base_models
        .iter()
        .map(|model| {
            let mut entries = model.as_map().unwrap_or_default().to_vec();
            if config.oauth.as_deref() != Some("radius") {
                if let Some(base_url) = &config.base_url {
                    if let Some(slot) = entries.iter_mut().find(|(k, _)| k == "baseUrl") {
                        slot.1 = Value::String(base_url.clone());
                    }
                }
            }
            if let Some(compat) = merge_compat(
                model.as_map().and_then(|entries| entries.iter().find(|(k, _)| k == "compat").map(|(_, v)| v)),
                config.compat.as_ref(),
            ) {
                if let Some(slot) = entries.iter_mut().find(|(k, _)| k == "compat") {
                    slot.1 = compat;
                } else {
                    entries.push(("compat".to_string(), compat));
                }
            }
            Value::Map(entries)
        })
        .collect();

    for definition in &config.models {
        let existing_index = models.iter().position(|model| {
            model
                .as_map()
                .and_then(|entries| entries.iter().find(|(k, _)| k == "id"))
                .and_then(|(_, v)| v.as_str())
                == Some(definition.id.as_str())
        });
        let defaults = match existing_index {
            Some(index) => models.get(index).cloned(),
            None => models.first().cloned(),
        };
        let model = model_from_json(provider_id, definition, config, defaults.as_ref())?;
        match existing_index {
            Some(index) => models[index] = model,
            None => models.push(model),
        }
    }
    Ok(models)
}

/// Resolve configured headers for a model from provider config and extension
/// definitions, using the same resolution logic as API keys.
pub fn resolve_configured_model_headers(
    model: &Value,
    config: Option<&ModelsJsonProvider>,
    env: Option<&HashMap<String, String>>,
) -> Result<Option<Vec<(String, String)>>, String> {
    let model_id = model
        .as_map()
        .and_then(|entries| entries.iter().find(|(k, _)| k == "id"))
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("");
    let provider = model
        .as_map()
        .and_then(|entries| entries.iter().find(|(k, _)| k == "provider"))
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("");
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(config) = config {
        for (key, value) in config.headers.clone().unwrap_or_default() {
            headers.push((key, value));
        }
        if let Some((_, override_value)) = config.model_overrides.iter().find(|(id, _)| id == model_id) {
            if let Some(override_headers) = override_value
                .as_map()
                .and_then(|entries| entries.iter().find(|(k, _)| k == "headers"))
                .and_then(|(_, v)| v.as_map())
            {
                for (key, value) in override_headers {
                    if let Some(value) = value.as_str() {
                        headers.push((key.clone(), value.to_string()));
                    }
                }
            }
        }
        if let Some(definition) = config.models.iter().find(|definition| definition.id == model_id) {
            for (key, value) in definition.headers.clone().unwrap_or_default() {
                headers.push((key, value));
            }
        }
    }
    if headers.is_empty() {
        return Ok(None);
    }
    resolve_headers_or_throw(&headers, &format!("model \"{provider}/{model_id}\""), env).map(Some)
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompatibilityRequestConfig {
    pub headers: Option<Vec<(String, Option<String>)>>,
    pub auth_header: bool,
}

/// Resolve the request-time compatibility config for a model.
pub fn resolve_compatibility_request_config(
    model: &Value,
    config: Option<&ModelsJsonProvider>,
) -> Result<CompatibilityRequestConfig, String> {
    let model_id = model
        .as_map()
        .and_then(|entries| entries.iter().find(|(k, _)| k == "id"))
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("");
    let provider = model
        .as_map()
        .and_then(|entries| entries.iter().find(|(k, _)| k == "provider"))
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("");

    let mut configured: Vec<(String, String)> = Vec::new();
    if let Some(config) = config {
        for (key, value) in config.headers.clone().unwrap_or_default() {
            configured.push((key, value));
        }
        if let Some(definition) = config.models.iter().find(|definition| definition.id == model_id) {
            for (key, value) in definition.headers.clone().unwrap_or_default() {
                configured.push((key, value));
            }
        }
    }
    let configured = resolve_headers_or_throw(&configured, &format!("model \"{provider}/{model_id}\""), None)?;

    let model_headers: Option<Vec<(String, Option<String>)>> = model
        .as_map()
        .and_then(|entries| entries.iter().find(|(k, _)| k == "headers"))
        .and_then(|(_, v)| v.as_map())
        .map(|map| {
            map.iter()
                .map(|(key, value)| (key.clone(), value.as_str().map(|value| value.to_string())))
                .collect()
        })
        .filter(|headers: &Vec<(String, Option<String>)>| !headers.is_empty());

    let headers = match (&model_headers, configured.is_empty()) {
        (Some(model_headers), false) => {
            let mut merged = model_headers.clone();
            for (key, value) in &configured {
                if let Some(slot) = merged.iter_mut().find(|(existing, _)| existing == key) {
                    slot.1 = Some(value.clone());
                } else {
                    merged.push((key.clone(), Some(value.clone())));
                }
            }
            Some(merged)
        }
        (Some(model_headers), true) => Some(model_headers.clone()),
        (None, false) => Some(
            configured
                .iter()
                .map(|(key, value)| (key.clone(), Some(value.clone())))
                .collect(),
        ),
        (None, true) => None,
    };

    Ok(CompatibilityRequestConfig {
        headers,
        auth_header: config.and_then(|config| config.auth_header).unwrap_or(false),
    })
}

/// Request auth status from models.json config values (no credential reads).
pub fn configured_request_auth_status(config: Option<&ModelsJsonProvider>) -> Option<AuthStatus> {
    let value = config.and_then(|config| config.api_key.clone())?;
    if is_command_config_value(&value) {
        return Some(AuthStatus {
            configured: true,
            source: Some("models_json_command".to_string()),
            label: None,
        });
    }
    let names = get_config_value_env_var_names(&value);
    if !names.is_empty() {
        if is_config_value_configured(&value, None) {
            Some(AuthStatus {
                configured: true,
                source: Some("environment".to_string()),
                label: Some(names.join(", ")),
            })
        } else {
            Some(AuthStatus {
                configured: false,
                source: None,
                label: None,
            })
        }
    } else {
        Some(AuthStatus {
            configured: true,
            source: Some("models_json_key".to_string()),
            label: None,
        })
    }
}

/// Validate an extension provider registration (structural checks only).
pub fn validate_extension_provider(
    provider_id: &str,
    extension: &Value,
    _model_config: &ModelConfig,
) -> Result<(), String> {
    let extension_entries = extension.as_map().unwrap_or_default();
    let has_stream_simple = extension_entries.iter().any(|(k, _)| k == "streamSimple");
    let api = extension_entries
        .iter()
        .find(|(k, _)| k == "api")
        .and_then(|(_, v)| v.as_str());
    if has_stream_simple && api.is_none() {
        return Err(format!("Provider {provider_id}: \"api\" is required when registering streamSimple."));
    }
    Ok(())
}

/// Compose models for a provider from base + models.json + extension
/// (synchronous, without building the full Provider object).
pub fn compose_provider_models(
    provider_id: &str,
    base_models: &[Value],
    model_config: &ModelConfig,
    extension: Option<&Value>,
) -> Result<Vec<Value>, String> {
    let config = model_config.get_provider(provider_id);
    let mut models = apply_models_json(provider_id, base_models, config)?;
    if let Some(extension) = extension {
        // Extension model definitions replace base models with matching ids.
        let extension_models: Vec<Value> = extension
            .as_map()
            .and_then(|entries| entries.iter().find(|(k, _)| k == "models"))
            .and_then(|(_, v)| v.as_array())
            .map(|array| array.to_vec())
            .unwrap_or_default();
        if !extension_models.is_empty() {
            let mut replaced: Vec<Value> = Vec::new();
            for definition in &extension_models {
                let def_entries = definition.as_map().unwrap_or_default();
                let id = def_entries
                    .iter()
                    .find(|(k, _)| k == "id")
                    .and_then(|(_, v)| v.as_str())
                    .unwrap_or("");
                let defaults = models
                    .iter()
                    .find(|model| {
                        model
                            .as_map()
                            .and_then(|entries| entries.iter().find(|(k, _)| k == "id"))
                            .and_then(|(_, v)| v.as_str())
                            == Some(id)
                    })
                    .or_else(|| models.first())
                    .cloned();
                let mut model = definition.clone();
                let mut entries = model.as_map().unwrap_or_default().to_vec();
                if let Some(slot) = entries.iter_mut().find(|(k, _)| k == "api") {
                    // keep extension api
                } else if let Some(defaults) = &defaults {
                    if let Some(default_api) = defaults.as_map().and_then(|entries| {
                        entries.iter().find(|(k, _)| k == "api").map(|(_, v)| v.clone())
                    }) {
                        entries.push(("api".to_string(), default_api));
                    }
                }
                if !entries.iter().any(|(k, _)| k == "baseUrl") {
                    if let Some(defaults) = &defaults {
                        if let Some(base_url) = defaults.as_map().and_then(|entries| {
                            entries.iter().find(|(k, _)| k == "baseUrl").map(|(_, v)| v.clone())
                        }) {
                            entries.push(("baseUrl".to_string(), base_url));
                        }
                    }
                }
                if let Some(slot) = entries.iter_mut().find(|(k, _)| k == "provider") {
                    slot.1 = Value::String(provider_id.to_string());
                } else {
                    entries.push(("provider".to_string(), Value::String(provider_id.to_string())));
                }
                model = Value::Map(entries);
                replaced.push(model);
            }
            models = replaced;
        }
    }
    // Apply modelOverrides last.
    if let Some(config) = config {
        for (model_id, override_value) in &config.model_overrides {
            if let Some(model) = models.iter_mut().find(|model| {
                model
                    .as_map()
                    .and_then(|entries| entries.iter().find(|(k, _)| k == "id"))
                    .and_then(|(_, v)| v.as_str())
                    == Some(model_id.as_str())
            }) {
                *model = apply_model_override(model.clone(), override_value);
            }
        }
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_config(json: &str) -> ModelsJsonProvider {
        let config = ModelConfig::load_from_content(json);
        config.get_provider("acme").unwrap().clone()
    }

    fn base_model(id: &str) -> Value {
        Value::Map(vec![
            ("id".to_string(), Value::String(id.to_string())),
            ("name".to_string(), Value::String(id.to_string())),
            ("api".to_string(), Value::String("openai".to_string())),
            ("provider".to_string(), Value::String("acme".to_string())),
            ("baseUrl".to_string(), Value::String("https://base.example".to_string())),
            ("reasoning".to_string(), Value::Bool(false)),
            ("input".to_string(), Value::Array(vec![Value::String("text".to_string())])),
            ("cost".to_string(), Value::Map(vec![
                ("input".to_string(), Value::Number(1.0)),
                ("output".to_string(), Value::Number(2.0)),
                ("cacheRead".to_string(), Value::Number(0.1)),
                ("cacheWrite".to_string(), Value::Number(0.2)),
            ])),
            ("contextWindow".to_string(), Value::Number(128000.0)),
            ("maxTokens".to_string(), Value::Number(16384.0)),
        ])
    }

    #[test]
    fn model_override_applies_fields() {
        let model = base_model("m1");
        let override_value = Value::Map(vec![
            ("name".to_string(), Value::String("Renamed".to_string())),
            ("reasoning".to_string(), Value::Bool(true)),
            ("contextWindow".to_string(), Value::Number(200000.0)),
        ]);
        let merged = apply_model_override(model, &override_value);
        let entries = merged.as_map().unwrap();
        assert_eq!(
            entries.iter().find(|(k, _)| k == "name").map(|(_, v)| v.as_str().unwrap()),
            Some("Renamed")
        );
        assert_eq!(
            entries.iter().find(|(k, _)| k == "reasoning").map(|(_, v)| v.as_bool().unwrap()),
            Some(true)
        );
        assert_eq!(
            entries.iter().find(|(k, _)| k == "contextWindow").map(|(_, v)| v.as_number().unwrap()),
            Some(200000.0)
        );
    }

    #[test]
    fn compat_nested_merge() {
        let base = Value::Map(vec![(
            "openRouterRouting".to_string(),
            Value::Map(vec![("allow_fallbacks".to_string(), Value::Bool(true))]),
        )]);
        let override_value = Value::Map(vec![(
            "openRouterRouting".to_string(),
            Value::Map(vec![("zdr".to_string(), Value::Bool(true))]),
        )]);
        let merged = merge_compat(Some(&base), Some(&override_value)).unwrap();
        let routing = merged
            .as_map()
            .unwrap()
            .iter()
            .find(|(k, _)| k == "openRouterRouting")
            .unwrap()
            .1
            .as_map()
            .unwrap();
        assert_eq!(routing.len(), 2);
    }

    #[test]
    fn models_json_requires_base_url_for_custom() {
        // Missing api errors first (JS order), missing baseUrl second.
        let config = provider_config(r#"{"providers":{"acme":{"models":[{"id":"custom","name":"C"}]}}}"#);
        let result = apply_models_json("acme", &[], Some(&config));
        assert!(result.unwrap_err().contains("no \"api\""));

        let config = provider_config(r#"{"providers":{"acme":{"api":"openai","models":[{"id":"custom","name":"C"}]}}}"#);
        let result = apply_models_json("acme", &[], Some(&config));
        assert!(result.unwrap_err().contains("baseUrl"));
    }

    #[test]
    fn models_json_validates_context_window() {
        let config = provider_config(
            r#"{"providers":{"acme":{"baseUrl":"https://x","models":[{"id":"m","contextWindow":-5}]}}}"#,
        );
        let result = apply_models_json("acme", &[base_model("other")], Some(&config));
        assert!(result.unwrap_err().contains("invalid contextWindow"));
    }

    #[test]
    fn models_json_upserts_and_overrides() {
        let config = provider_config(
            r#"{"providers":{"acme":{"baseUrl":"https://x","models":[{"id":"m1","name":"NewName"},{"id":"m2"}]}}}"#,
        );
        let result = apply_models_json("acme", &[base_model("m1")], Some(&config)).unwrap();
        assert_eq!(result.len(), 2);
        // Existing model replaced with the custom definition (baseUrl from config).
        let m1 = result[0].as_map().unwrap();
        assert_eq!(
            m1.iter().find(|(k, _)| k == "name").map(|(_, v)| v.as_str().unwrap()),
            Some("NewName")
        );
        assert_eq!(
            m1.iter().find(|(k, _)| k == "baseUrl").map(|(_, v)| v.as_str().unwrap()),
            Some("https://x")
        );
    }

    #[test]
    fn auth_status_from_config() {
        // ACME_KEY is not set in the test env: not configured.
        let config = provider_config(r#"{"providers":{"acme":{"apiKey":"$ACME_KEY"}}}"#);
        let status = configured_request_auth_status(Some(&config)).unwrap();
        assert!(!status.configured);

        // Set the env var: configured with the environment source label.
        std::env::set_var("ACME_KEY", "secret");
        let config = provider_config(r#"{"providers":{"acme":{"apiKey":"$ACME_KEY"}}}"#);
        let status = configured_request_auth_status(Some(&config)).unwrap();
        assert!(status.configured);
        assert_eq!(status.source.as_deref(), Some("environment"));
        assert_eq!(status.label.as_deref(), Some("ACME_KEY"));
        std::env::remove_var("ACME_KEY");

        let config = provider_config(r#"{"providers":{"acme":{"apiKey":"!echo hi"}}}"#);
        let status = configured_request_auth_status(Some(&config)).unwrap();
        assert_eq!(status.source.as_deref(), Some("models_json_command"));

        let config = provider_config(r#"{"providers":{"acme":{"apiKey":"plain-key"}}}"#);
        let status = configured_request_auth_status(Some(&config)).unwrap();
        assert_eq!(status.source.as_deref(), Some("models_json_key"));

        assert!(configured_request_auth_status(None).is_none());
    }

    #[test]
    fn compose_models_applies_overrides() {
        let config = provider_config(
            r#"{"providers":{"acme":{"modelOverrides":{"m1":{"reasoning":true}}}}}"#,
        );
        let config_obj = ModelConfig::load_from_content(r#"{"providers":{"acme":{"modelOverrides":{"m1":{"reasoning":true}}}}}"#);
        let result = compose_provider_models("acme", &[base_model("m1")], &config_obj, None).unwrap();
        let m1 = result[0].as_map().unwrap();
        assert_eq!(
            m1.iter().find(|(k, _)| k == "reasoning").map(|(_, v)| v.as_bool().unwrap()),
            Some(true)
        );
        let _ = &config;
    }
}
