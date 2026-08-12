//! Locked JSON-backed storage for dynamically refreshed provider catalogs,
//! port of `core/models-store.ts` (FileModelsStore + the coding-agent
//! in-memory store). Reuses the auth-storage file backend.

use std::collections::HashMap;
use std::sync::Mutex;

use pi_ai::models_store::{ModelsStore, ModelsStoreEntry};

use super::auth_storage::{get_file_revision, AuthStorageBackend, FileAuthStorageBackend};

type StoredModels = HashMap<String, ModelsStoreEntry>;

#[derive(Default)]
pub struct InMemoryCodingAgentModelsStore {
    entries: Mutex<HashMap<String, ModelsStoreEntry>>,
}

impl InMemoryCodingAgentModelsStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ModelsStore for InMemoryCodingAgentModelsStore {
    fn read(&self, provider_id: &str) -> Option<ModelsStoreEntry> {
        self.entries.lock().unwrap().get(provider_id).cloned()
    }

    fn write(&self, provider_id: &str, entry: ModelsStoreEntry) {
        self.entries.lock().unwrap().insert(provider_id.to_string(), entry);
    }

    fn delete(&self, provider_id: &str) {
        self.entries.lock().unwrap().remove(provider_id);
    }
}

/// JSON-file-backed model store (models-store.json in the agent dir).
pub struct FileModelsStore {
    storage: AuthStorageBackend,
    path: String,
    data: Mutex<StoredModels>,
    revision: Mutex<Option<String>>,
}

impl FileModelsStore {
    pub fn new(path: Option<String>) -> Self {
        let path = path.unwrap_or_else(|| crate::config::get_agent_dir() + "/models-store.json");
        let storage = AuthStorageBackend::File(FileAuthStorageBackend::new(Some(path.clone())));
        let store = Self {
            storage,
            path,
            data: Mutex::new(StoredModels::new()),
            revision: Mutex::new(None),
        };
        store.reload();
        store
    }

    fn parse(content: Option<&str>) -> StoredModels {
        let mut data = StoredModels::new();
        let Some(content) = content else {
            return data;
        };
        let Ok(value) = pi_ai::utils::json::parse_json_with_repair::<pi_protocol::Value>(content) else {
            return data;
        };
        let Some(entries) = value.as_map() else {
            return data;
        };
        for (provider_id, entry) in entries {
            if let Some(entry) = json_to_entry(entry) {
                data.insert(provider_id.clone(), entry);
            }
        }
        data
    }

    fn reload(&self) {
        let mut content: Option<String> = None;
        let mut update = |current: Option<&str>| {
            content = current.map(|value| value.to_string());
            super::auth_storage::LockResult {
                result: (),
                next: None,
            }
        };
        self.storage.with_lock(&mut update);
        *self.data.lock().unwrap() = Self::parse(content.as_deref());
        *self.revision.lock().unwrap() = get_file_revision(&self.path);
    }

    fn read_latest(&self) -> StoredModels {
        let current_revision = get_file_revision(&self.path);
        let cached_revision = self.revision.lock().unwrap().clone();
        if current_revision.is_some() && current_revision == cached_revision {
            return self.data.lock().unwrap().clone();
        }
        self.reload();
        self.data.lock().unwrap().clone()
    }
}

impl ModelsStore for FileModelsStore {
    fn read(&self, provider_id: &str) -> Option<ModelsStoreEntry> {
        self.read_latest().get(provider_id).cloned()
    }

    fn write(&self, provider_id: &str, entry: ModelsStoreEntry) {
        let mut latest: Option<StoredModels> = None;
        let entry_json = entry_to_json(&entry);
        let mut update = |current: Option<&str>| {
            let mut current = Self::parse(current);
            current.insert(provider_id.to_string(), entry.clone());
            latest = Some(current.clone());
            super::auth_storage::LockResult {
                result: (),
                next: Some(serialize(&current)),
            }
        };
        let _ = entry_json;
        self.storage.with_lock(&mut update);
        if let Some(latest) = latest {
            *self.data.lock().unwrap() = latest;
        }
    }

    fn delete(&self, provider_id: &str) {
        let mut latest: Option<StoredModels> = None;
        let mut update = |current: Option<&str>| {
            let mut current = Self::parse(current);
            current.remove(provider_id);
            latest = Some(current.clone());
            super::auth_storage::LockResult {
                result: (),
                next: Some(serialize(&current)),
            }
        };
        self.storage.with_lock(&mut update);
        if let Some(latest) = latest {
            *self.data.lock().unwrap() = latest;
        }
    }
}

fn serialize(data: &StoredModels) -> String {
    let entries: Vec<(String, pi_protocol::Value)> = data
        .iter()
        .map(|(provider_id, entry)| (provider_id.clone(), entry_to_json(entry)))
        .collect();
    pi_ai::utils::json::json_stringify_pretty(&pi_protocol::Value::Map(entries))
}

fn entry_to_json(entry: &ModelsStoreEntry) -> pi_protocol::Value {
    let mut fields: Vec<(String, pi_protocol::Value)> = Vec::new();
    // Models serialized via the shared model JSON encoding.
    let models: Vec<pi_protocol::Value> = entry
        .models
        .iter()
        .map(|model| crate::core::model_json::model_to_json(model))
        .collect();
    fields.push(("models".to_string(), pi_protocol::Value::Array(models)));
    if let Some(last_modified) = entry.last_modified {
        fields.push(("lastModified".to_string(), pi_protocol::Value::Number(last_modified)));
    }
    if let Some(checked_at) = entry.checked_at {
        fields.push(("checkedAt".to_string(), pi_protocol::Value::Number(checked_at)));
    }
    if let Some(etag) = &entry.etag {
        fields.push(("etag".to_string(), pi_protocol::Value::String(etag.clone())));
    }
    pi_protocol::Value::Map(fields)
}

fn json_to_entry(value: &pi_protocol::Value) -> Option<ModelsStoreEntry> {
    let entries = value.as_map()?;
    let models = entries
        .iter()
        .find(|(k, _)| k == "models")
        .and_then(|(_, v)| v.as_array())
        .map(|array| {
            array
                .iter()
                .filter_map(crate::core::model_json::json_to_model)
                .collect()
        })
        .unwrap_or_default();
    Some(ModelsStoreEntry {
        models,
        last_modified: entries
            .iter()
            .find(|(k, _)| k == "lastModified")
            .and_then(|(_, v)| v.as_number()),
        checked_at: entries
            .iter()
            .find(|(k, _)| k == "checkedAt")
            .and_then(|(_, v)| v.as_number()),
        etag: entries
            .iter()
            .find(|(k, _)| k == "etag")
            .and_then(|(_, v)| v.as_str())
            .map(|value| value.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::Model;

    fn sample_model() -> Model {
        Model {
            id: "m1".into(),
            name: "Model".into(),
            api: "openai".into(),
            provider: "acme".into(),
            base_url: "https://a.example".into(),
            reasoning: false,
            thinking_level_map: None,
            input: vec!["text".into()],
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
        }
    }

    #[test]
    fn in_memory_round_trip() {
        let store = InMemoryCodingAgentModelsStore::new();
        assert_eq!(store.read("p"), None);
        let entry = ModelsStoreEntry {
            models: vec![sample_model()],
            last_modified: Some(1.0),
            checked_at: Some(2.0),
            etag: Some("\"abc\"".into()),
        };
        store.write("p", entry.clone());
        assert_eq!(store.read("p"), Some(entry));
        store.delete("p");
        assert_eq!(store.read("p"), None);
    }

    #[test]
    fn file_round_trip() {
        let counter = std::sync::atomic::AtomicU64::new(0);
        let n = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-mstore-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("models-store.json").to_string_lossy().to_string();

        let store = FileModelsStore::new(Some(path.clone()));
        let entry = ModelsStoreEntry {
            models: vec![sample_model()],
            last_modified: Some(123.0),
            checked_at: Some(456.0),
            etag: None,
        };
        store.write("acme", entry.clone());
        assert_eq!(store.read("acme"), Some(entry));

        // Reload from a fresh instance sees persisted data.
        let reloaded = FileModelsStore::new(Some(path.clone()));
        assert_eq!(reloaded.read("acme").unwrap().models[0].id, "m1");
        assert_eq!(reloaded.read("acme").unwrap().last_modified, Some(123.0));

        reloaded.delete("acme");
        assert_eq!(reloaded.read("acme"), None);
    }
}
