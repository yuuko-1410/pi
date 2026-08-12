//! Immutable, credential-blind models.json snapshot, port of
//! `core/model-config.ts`. The typebox schema validation is a hand-written
//! structural check over the parsed JSON (same accepted shapes, same error
//! message style). deepFreeze is unnecessary in Rust (immutable by default).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use pi_protocol::Value;

use crate::utils::basics::strip_json_comments;

#[derive(Clone, Debug, PartialEq)]
pub struct ModelsJsonModel {
    pub id: String,
    pub name: Option<String>,
    pub api: Option<String>,
    pub base_url: Option<String>,
    pub reasoning: Option<bool>,
    pub input: Option<Vec<String>>,
    pub context_window: Option<f64>,
    pub max_tokens: Option<f64>,
    pub headers: Option<Vec<(String, String)>>,
    /// Raw compat object (validated structurally, kept as JSON).
    pub compat: Option<Value>,
    /// Raw model JSON (samplingParams etc. preserved verbatim).
    pub raw: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelsJsonProvider {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api: Option<String>,
    pub oauth: Option<String>,
    pub headers: Option<Vec<(String, String)>>,
    pub compat: Option<Value>,
    pub auth_header: Option<bool>,
    pub models: Vec<ModelsJsonModel>,
    pub model_overrides: Vec<(String, Value)>,
    pub raw: Value,
}

/// One immutable load of models.json.
pub struct ModelConfig {
    providers: HashMap<String, ModelsJsonProvider>,
    error: Option<String>,
}

impl ModelConfig {
    fn new(providers: HashMap<String, ModelsJsonProvider>, error: Option<String>) -> Self {
        Self { providers, error }
    }

    pub fn load(models_json_path: Option<&str>) -> ModelConfig {
        let Some(models_json_path) = models_json_path else {
            return Self::new(HashMap::new(), None);
        };
        let path = models_json_path;
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self::new(HashMap::new(), None);
            }
            Err(error) => {
                return Self::new(
                    HashMap::new(),
                    Some(format!("Failed to load models.json: {error}\n\nFile: {path}")),
                );
            }
        };

        let parsed: Value = match pi_ai::utils::json::parse_json_with_repair(&strip_json_comments(&content)) {
            Ok(value) => value,
            Err(error) => {
                return Self::new(
                    HashMap::new(),
                    Some(format!("Failed to parse models.json: {error}\n\nFile: {path}")),
                );
            }
        };

        match validate_and_build(&parsed) {
            Ok(providers) => Self::new(providers, None),
            Err(errors) => Self::new(
                HashMap::new(),
                Some(format!("Invalid models.json schema:\n{errors}\n\nFile: {path}")),
            ),
        }
    }

    pub fn get_provider(&self, provider_id: &str) -> Option<&ModelsJsonProvider> {
        self.providers.get(provider_id)
    }

    pub fn get_provider_ids(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    pub fn get_error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

fn is_string(value: &Value) -> bool {
    matches!(value, Value::String(_))
}

fn is_number(value: &Value) -> bool {
    matches!(value, Value::Number(_))
}

fn is_bool(value: &Value) -> bool {
    matches!(value, Value::Bool(_))
}

fn field<'a>(map: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    map.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Validate a provider-scoped models.json against the typebox schema shapes
/// and build the typed snapshot. Returns a list of validation errors.
fn validate_and_build(parsed: &Value) -> Result<HashMap<String, ModelsJsonProvider>, String> {
    let root = parsed
        .as_map()
        .ok_or_else(|| "  - root: must be an object".to_string())?;
    let providers_value = field(root, "providers").ok_or_else(|| "  - providers: must be an object".to_string())?;
    let providers_map = providers_value
        .as_map()
        .ok_or_else(|| "  - providers: must be an object".to_string())?;

    let mut errors: Vec<String> = Vec::new();
    let mut providers: HashMap<String, ModelsJsonProvider> = HashMap::new();
    for (provider_id, provider_value) in providers_map {
        let Some(provider_entries) = provider_value.as_map() else {
            errors.push(format!("  - providers.{provider_id}: must be an object"));
            continue;
        };
        let mut provider = ModelsJsonProvider {
            name: optional_string(provider_entries, "name"),
            base_url: optional_string(provider_entries, "baseUrl"),
            api_key: optional_string(provider_entries, "apiKey"),
            api: optional_string(provider_entries, "api"),
            oauth: match field(provider_entries, "oauth") {
                Some(Value::String(value)) if value == "radius" => Some(value.clone()),
                _ => None,
            },
            headers: optional_string_map(provider_entries, "headers"),
            compat: field(provider_entries, "compat").cloned(),
            auth_header: field(provider_entries, "authHeader").and_then(|v| v.as_bool()),
            models: Vec::new(),
            model_overrides: Vec::new(),
            raw: provider_value.clone(),
        };

        if let Some(models_value) = field(provider_entries, "models") {
            match models_value {
                Value::Array(models) => {
                    for model in models {
                        match validate_model(model, provider_id) {
                            Ok(model) => provider.models.push(model),
                            Err(message) => errors.push(message),
                        }
                    }
                }
                _ => errors.push(format!("  - providers.{provider_id}.models: must be an array")),
            }
        }
        if let Some(overrides_value) = field(provider_entries, "modelOverrides") {
            match overrides_value {
                Value::Map(overrides) => {
                    for (model_id, override_value) in overrides {
                        if !is_valid_override(override_value) {
                            errors.push(format!("  - providers.{provider_id}.modelOverrides.{model_id}: invalid override"));
                        }
                        provider.model_overrides.push((model_id.clone(), override_value.clone()));
                    }
                }
                _ => errors.push(format!("  - providers.{provider_id}.modelOverrides: must be an object")),
            }
        }
        providers.insert(provider_id.clone(), provider);
    }

    if errors.is_empty() {
        Ok(providers)
    } else {
        Err(errors.join("\n"))
    }
}

fn optional_string(entries: &[(String, Value)], key: &str) -> Option<String> {
    field(entries, key).and_then(|value| value.as_str()).map(|value| value.to_string())
}

fn optional_string_map(entries: &[(String, Value)], key: &str) -> Option<Vec<(String, String)>> {
    field(entries, key).and_then(|value| value.as_map()).map(|map| {
        map.iter()
            .filter_map(|(k, v)| v.as_str().map(|value| (k.clone(), value.to_string())))
            .collect()
    })
}

fn validate_model(model: &Value, provider_id: &str) -> Result<ModelsJsonModel, String> {
    let Some(entries) = model.as_map() else {
        return Err(format!("  - providers.{provider_id}.models: each model must be an object"));
    };
    let id = match field(entries, "id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => {
            return Err(format!(
                "  - providers.{provider_id}.models.id: must be a non-empty string"
            ));
        }
    };
    let mut errors: Vec<String> = Vec::new();
    if let Some(value) = field(entries, "name") {
        if !is_string(value) || value.as_str().is_some_and(|v| v.is_empty()) {
            errors.push(format!("  - providers.{provider_id}.models.{id}.name: must be a non-empty string"));
        }
    }
    if let Some(value) = field(entries, "api") {
        if !is_string(value) {
            errors.push(format!("  - providers.{provider_id}.models.{id}.api: must be a string"));
        }
    }
    if let Some(value) = field(entries, "baseUrl") {
        if !is_string(value) {
            errors.push(format!("  - providers.{provider_id}.models.{id}.baseUrl: must be a string"));
        }
    }
    if let Some(value) = field(entries, "reasoning") {
        if !is_bool(value) {
            errors.push(format!("  - providers.{provider_id}.models.{id}.reasoning: must be a boolean"));
        }
    }
    if let Some(Value::Array(input)) = field(entries, "input") {
        for block in input {
            let valid = block.as_str().is_some_and(|v| v == "text" || v == "image");
            if !valid {
                errors.push(format!("  - providers.{provider_id}.models.{id}.input: must be \"text\" or \"image\""));
                break;
            }
        }
    }
    for key in ["contextWindow", "maxTokens"] {
        if let Some(value) = field(entries, key) {
            if !is_number(value) {
                errors.push(format!("  - providers.{provider_id}.models.{id}.{key}: must be a number"));
            }
        }
    }
    if let Some(value) = field(entries, "cost") {
        if !is_valid_cost(value, false) {
            errors.push(format!("  - providers.{provider_id}.models.{id}.cost: invalid cost"));
        }
    }
    if let Some(value) = field(entries, "headers") {
        if value.as_map().is_none() {
            errors.push(format!("  - providers.{provider_id}.models.{id}.headers: must be an object"));
        }
    }

    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    Ok(ModelsJsonModel {
        id,
        name: optional_string(entries, "name"),
        api: optional_string(entries, "api"),
        base_url: optional_string(entries, "baseUrl"),
        reasoning: field(entries, "reasoning").and_then(|v| v.as_bool()),
        input: field(entries, "input").and_then(|v| v.as_array()).map(|array| {
            array
                .iter()
                .filter_map(|v| v.as_str().map(|value| value.to_string()))
                .collect()
        }),
        context_window: field(entries, "contextWindow").and_then(|v| v.as_number()),
        max_tokens: field(entries, "maxTokens").and_then(|v| v.as_number()),
        headers: optional_string_map(entries, "headers"),
        compat: field(entries, "compat").cloned(),
        raw: model.clone(),
    })
}

fn is_valid_cost(value: &Value, allow_optional_rates: bool) -> bool {
    let Some(entries) = value.as_map() else {
        return false;
    };
    for key in ["input", "output", "cacheRead", "cacheWrite"] {
        match field(entries, key) {
            Some(Value::Number(_)) => {}
            _ if allow_optional_rates => {}
            _ => return false,
        }
    }
    true
}

fn is_valid_override(value: &Value) -> bool {
    let Some(entries) = value.as_map() else {
        return false;
    };
    for key in ["name", "reasoning", "contextWindow", "maxTokens"] {
        if let Some(field) = field(entries, key) {
            let valid = match key {
                "name" => is_string(field),
                "reasoning" => is_bool(field),
                _ => is_number(field),
            };
            if !valid {
                return false;
            }
        }
    }
    if let Some(value) = field(entries, "cost") {
        if !is_valid_cost(value, true) {
            return false;
        }
    }
    true
}

/// Path helper kept for callers that need a normalized models.json path.
pub fn normalize_path(path: &str) -> String {
    if Path::new(path).is_absolute() {
        path.to_string()
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(path)
            .to_string_lossy()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn load_from(content: &str) -> ModelConfig {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("pi-models-{}-{n}.json", std::process::id()));
        std::fs::write(&path, content).unwrap();
        let config = ModelConfig::load(Some(&path.to_string_lossy()));
        let _ = std::fs::remove_file(&path);
        config
    }

    #[test]
    fn loads_providers_and_models() {
        let config = load_from(
            r#"{"providers":{"acme":{"name":"Acme","baseUrl":"https://a.example","apiKey":"$ACME_KEY","models":[{"id":"m1","name":"Model One","reasoning":true,"input":["text","image"],"contextWindow":1000}]}}}"#,
        );
        assert!(config.get_error().is_none());
        let provider = config.get_provider("acme").unwrap();
        assert_eq!(provider.name.as_deref(), Some("Acme"));
        assert_eq!(provider.models.len(), 1);
        assert_eq!(provider.models[0].id, "m1");
        assert_eq!(provider.models[0].reasoning, Some(true));
        assert_eq!(config.get_provider_ids(), vec!["acme".to_string()]);
    }

    #[test]
    fn missing_file_yields_empty() {
        let config = ModelConfig::load(Some("/nonexistent/models.json"));
        assert!(config.get_error().is_none());
        assert!(config.get_provider_ids().is_empty());
    }

    #[test]
    fn invalid_schema_reports_error() {
        let config = load_from(r#"{"providers":{"p":{"models":[{"id":123}]}}}"#);
        let error = config.get_error().unwrap();
        assert!(error.contains("Invalid models.json schema"));
        assert!(error.contains("models.id"));
    }

    #[test]
    fn comment_stripping() {
        let config = load_from("// header\n{\"providers\":{}}");
        assert!(config.get_error().is_none());
    }
}
