//! Remote pi.dev catalog overlay, port of the pure logic of
//! `core/remote-catalog-provider.ts`. The Provider-object refreshModels flow
//! needs the pi-ai Provider composition (not ported), so the refresh decision
//! and merge logic are exposed as functions; the network fetch is deferred.

use pi_ai::models_store::ModelsStoreEntry;
use pi_ai::types::Model;

pub const REMOTE_CATALOG_REFRESH_INTERVAL_MS: f64 = 4.0 * 60.0 * 60.0 * 1000.0;

/// Merge dynamic models over a baseline, replacing by id.
pub fn merge_models(baseline: &[Model], dynamic: &[Model]) -> Vec<Model> {
    let mut merged: Vec<Model> = baseline.to_vec();
    for model in dynamic {
        if let Some(index) = merged.iter().position(|entry| entry.id == model.id) {
            merged[index] = model.clone();
        } else {
            merged.push(model.clone());
        }
    }
    merged
}

/// Parse a catalog response body into models for a provider.
pub fn parse_catalog(provider_id: &str, value: &pi_protocol::Value) -> Result<Vec<Model>, String> {
    let entries: Option<Vec<&pi_protocol::Value>> = match value {
        pi_protocol::Value::Array(items) => Some(items.iter().collect()),
        pi_protocol::Value::Map(fields) => fields
            .iter()
            .find(|(key, _)| key == "models")
            .and_then(|(_, value)| value.as_array())
            .map(|items| items.iter().collect())
            .or_else(|| Some(fields.iter().map(|(_, value)| value).collect())),
        _ => None,
    };
    let Some(entries) = entries else {
        return Err(format!("Invalid model catalog for provider \"{provider_id}\""));
    };
    let mut models: Vec<Model> = Vec::new();
    for entry in entries {
        // JS accepts any object with an id and forces the provider; the Rust
        // catalog parser requires provider, so inject it like the JS map does.
        let normalized = match entry {
            pi_protocol::Value::Map(fields) if fields.iter().any(|(key, _)| key == "id") => {
                let mut fields = fields.clone();
                fields.retain(|(key, _)| key != "provider");
                fields.push(("provider".into(), pi_protocol::Value::String(provider_id.into())));
                pi_protocol::Value::Map(fields)
            }
            other => other.clone(),
        };
        let Some(model) = crate::core::model_json::json_to_model(&normalized) else {
            continue;
        };
        models.push(model);
    }
    Ok(models)
}

/// Whether a stored entry contributes remote models given a local baseline.
pub fn remote_models(
    entry: Option<&ModelsStoreEntry>,
    local_generated_at: Option<f64>,
) -> Vec<Model> {
    let Some(entry) = entry else {
        return Vec::new();
    };
    if let Some(local_generated_at) = local_generated_at {
        if entry.last_modified.is_none() || entry.last_modified.unwrap_or(0.0) <= local_generated_at {
            return Vec::new();
        }
    }
    entry.models.clone()
}

/// Refresh decision: whether a remote check is due.
/// Mirrors the early-exit conditions of refreshModels.
pub fn refresh_due(
    stored: Option<&ModelsStoreEntry>,
    force: bool,
    allow_network: bool,
    now_ms: f64,
) -> bool {
    if !allow_network {
        return false;
    }
    if force {
        return true;
    }
    let Some(stored) = stored else {
        return true;
    };
    match (stored.checked_at, stored.last_modified) {
        (Some(checked_at), Some(_)) => now_ms - checked_at >= REMOTE_CATALOG_REFRESH_INTERVAL_MS,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str) -> Model {
        let mut model = Model {
            id: id.into(),
            name: id.into(),
            api: "openai".into(),
            provider: "acme".into(),
            base_url: String::new(),
            reasoning: false,
            thinking_level_map: None,
            input: Vec::new(),
            cost: pi_ai::types::ModelCost {
                rates: pi_ai::types::ModelCostRates {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                tiers: None,
            },
            context_window: 0.0,
            max_tokens: 0.0,
            sampling_params: None,
            headers: None,
            compat: None,
        };
        model.provider = "acme".into();
        model
    }

    #[test]
    fn merge_replaces_by_id() {
        let baseline = vec![model("a"), model("b")];
        let dynamic = vec![model("b"), model("c")];
        let merged = merge_models(&baseline, &dynamic);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[1].id, "b");
        assert_eq!(merged[2].id, "c");
    }

    #[test]
    fn parse_catalog_accepts_array_and_models_object() {
        let array = pi_protocol::Value::Array(vec![
            pi_protocol::Value::Map(vec![
                ("id".into(), pi_protocol::Value::String("m1".into())),
                ("api".into(), pi_protocol::Value::String("openai".into())),
            ]),
            pi_protocol::Value::Map(vec![("api".into(), pi_protocol::Value::String("openai".into()))]),
        ]);
        let models = parse_catalog("acme", &array).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].provider, "acme");

        let object = pi_protocol::Value::Map(vec![(
            "models".into(),
            pi_protocol::Value::Array(vec![pi_protocol::Value::Map(vec![
                ("id".into(), pi_protocol::Value::String("m2".into())),
                ("api".into(), pi_protocol::Value::String("openai".into())),
            ])]),
        )]);
        let models = parse_catalog("acme", &object).unwrap();
        assert_eq!(models[0].id, "m2");

        assert!(parse_catalog("acme", &pi_protocol::Value::Number(1.0)).is_err());
    }

    #[test]
    fn remote_models_gating() {
        let entry = ModelsStoreEntry {
            models: vec![model("dyn")],
            last_modified: Some(100.0),
            checked_at: Some(0.0),
            etag: None,
        };
        // Newer than local baseline: contributes.
        assert_eq!(remote_models(Some(&entry), Some(50.0)).len(), 1);
        // Older or equal: empty.
        assert!(remote_models(Some(&entry), Some(200.0)).is_empty());
        // No local baseline timestamp: contributes.
        assert_eq!(remote_models(Some(&entry), None).len(), 1);
        assert!(remote_models(None, None).is_empty());
    }

    #[test]
    fn refresh_due_interval() {
        let entry = ModelsStoreEntry {
            models: vec![],
            last_modified: Some(1.0),
            checked_at: Some(0.0),
            etag: None,
        };
        // Fresh check (within interval): not due.
        assert!(!refresh_due(Some(&entry), false, true, 1000.0));
        // Old check (past interval): due.
        assert!(refresh_due(Some(&entry), false, true, 4.0 * 60.0 * 60.0 * 1000.0 + 1.0));
        // Force overrides.
        assert!(refresh_due(Some(&entry), true, true, 1000.0));
        // No network: never due.
        assert!(!refresh_due(Some(&entry), false, false, 9999999999.0));
    }
}
